// wattcoin_name_service/src/network.rs
use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};

use wots::{Wots, WotsSignature};
use crate::state::SharedL2State;
use crate::transaction::{L2Transaction, WnsBlock};


pub type ActiveWnsPeers = Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>;

#[derive(Serialize, Deserialize, Debug)]
pub enum WnsP2PMessage {
    Handshake { block_index: u64 },
    SyncResponse { state: crate::state::L2State },
    BroadcastTx { tx: L2Transaction },
    BroadcastBlock { block: WnsBlock },
}

pub async fn start_wns_p2p_server(port: u16, state: SharedL2State, active_peers: ActiveWnsPeers) {
    let address = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&address).await.unwrap();
    println!("🌐 [WNS P2P] Serveur Gossip L2 à l'écoute sur TCP/{}...", port);

    loop {
        let (socket, peer_addr) = listener.accept().await.unwrap();
        let peer_ip = peer_addr.ip().to_string();
        
        println!("🤝 [WNS P2P] Connexion d'un autre Séquenceur : {}", peer_ip);
        start_peer_connection(socket, peer_ip, Arc::clone(&state), Arc::clone(&active_peers));
    }
}

pub fn start_peer_connection(
    socket: TcpStream, 
    peer_ip: String, 
    state: SharedL2State, 
    active_peers: ActiveWnsPeers
) {
    let (read_half, mut write_half) = socket.into_split();
    let mut reader = BufReader::new(read_half);
    let (tx, mut rx) = mpsc::channel::<String>(1000);

    let peer_id = format!("{}_{}", peer_ip, rand::random::<u32>());
    active_peers.lock().unwrap().insert(peer_id.clone(), tx.clone());

    let my_index = state.lock().unwrap().block_index;
    let hs = WnsP2PMessage::Handshake { block_index: my_index };
    let mut hs_str = serde_json::to_string(&hs).unwrap();
    hs_str.push('\n');
    let _ = tx.try_send(hs_str);

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_half.write_all(msg.as_bytes()).await.is_err() { break; }
            let _ = write_half.flush().await;
        }
    });

    let active_peers_clone = Arc::clone(&active_peers);
    tokio::spawn(async move {
        let mut line = String::new();
        while let Ok(n) = reader.read_line(&mut line).await {
            if n == 0 { break; }
            
            if let Ok(message) = serde_json::from_str::<WnsP2PMessage>(&line.trim()) {
                match message {
                    WnsP2PMessage::Handshake { block_index } => {
                        let my_state = state.lock().unwrap();
                        if my_state.block_index > block_index {
                            println!("📤 [SYNC] Pair en retard détecté ({} vs {}). Envoi du snapshot L2...", block_index, my_state.block_index);
                            let env = WnsP2PMessage::SyncResponse { state: my_state.clone() };
                            let mut json_str = serde_json::to_string(&env).unwrap();
                            json_str.push('\n');
                            let _ = tx.try_send(json_str);
                        }
                    },
                    WnsP2PMessage::SyncResponse { state: new_state } => {
                        let mut my_state = state.lock().unwrap();
                        
                        if new_state.block_index > my_state.block_index {
                            if new_state.last_l1_block >= my_state.last_l1_block {
                                println!("📥 [SYNC] Snapshot L2 téléchargé ! Mise à jour du Bloc {} ➡ {}", my_state.block_index, new_state.block_index);
                                
                                // 💡 CRITIQUE : on récupère notre poignée de connexion DB avant d'écraser la RAM par le nouveau snapshot réseau
                                let db_handle = my_state.db.clone();
                                *my_state = new_state; 
                                my_state.db = db_handle;
                                my_state.save_to_disk(); 
                            } else {
                                println!("❌ [SYNC] Rejet : Un pair a tenté d'injecter un état périmé (Incohérence L1) !");
                            }
                        }
                    },
                    WnsP2PMessage::BroadcastTx { tx: in_tx } => {
                        let mut state_guard = state.lock().unwrap();
                        
                        if !state_guard.mempool.iter().any(|t| t.signature == in_tx.signature) {
                            
                            let hash = in_tx.hash_data();
                            
                            // 💡 VRAIE VÉRIFICATION WOTS+
                            let is_valid = if let Ok(sig) = serde_json::from_str::<WotsSignature>(&in_tx.signature) {
                                hex::encode(&sig.public_key) == in_tx.sender_pubkey && Wots::verify(&sig, &hash)
                            } else {
                                false
                            };

                            if is_valid {
                                state_guard.mempool.push(in_tx.clone());

                                println!("📡 [WNS P2P] Nouvelle TX relayée reçue : Action {:?} sur '{}'", 
                                    in_tx.action, in_tx.domain_name);

                                let env = WnsP2PMessage::BroadcastTx { tx: in_tx };
                                broadcast_message(&env, &active_peers_clone, &peer_id);
                            } else {
                                println!("❌ [WNS P2P] TX relayée rejetée : Signature invalide !");
                            }
                        }
                    },
                    WnsP2PMessage::BroadcastBlock { block } => {
                        let mut state_guard = state.lock().unwrap();
                        
                        if block.index == state_guard.block_index + 1 {
                            println!("📦 [WNS P2P] Nouveau bloc L2 reçu (Index {}) !", block.index);
                            
                            let mut hasher = sha2::Sha256::new(); // WOTS+ exige SHA-256
                            use sha2::Digest;
                            hasher.update(block.state_root.as_bytes());
                            let mut hash_array = [0u8; 32];       // 32 octets !
                            hash_array.copy_from_slice(&hasher.finalize());
                            
                            // 💡 VRAIE VÉRIFICATION WOTS+ POUR LE BLOC
                            let is_valid = if let Ok(sig) = serde_json::from_str::<WotsSignature>(&block.signature) {
                                hex::encode(&sig.public_key) == block.sequencer_pubkey && Wots::verify(&sig, &hash_array)
                            } else {
                                false
                            };

                            if !is_valid {
                                println!("❌ [WNS P2P] Signature du bloc invalide !");
                                return; 
                            }
                            
                            if let Ok(_) = state_guard.apply_incoming_wns_block(&block) {
                                println!("✅ [WNS P2P] Bloc L2 validé ! Registre mis à jour.");
                                let env = WnsP2PMessage::BroadcastBlock { block };
                                broadcast_message(&env, &active_peers_clone, &peer_id);
                            } else {
                                println!("❌ [WNS P2P] Fraude détectée. Bloc rejeté.");
                            }
                        }
                    },
                }
            }
            line.clear();
        }
        active_peers.lock().unwrap().remove(&peer_id);
    });
}

pub fn broadcast_message(msg: &WnsP2PMessage, active_peers: &ActiveWnsPeers, skip_peer: &str) {
    let mut json_str = serde_json::to_string(msg).unwrap();
    json_str.push('\n');
    let peers = active_peers.lock().unwrap().clone();
    for (id, sender) in peers.iter() {
        if id != skip_peer {
            let _ = sender.try_send(json_str.clone());
        }
    }
}