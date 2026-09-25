// wattcoin_name_service/src/network.rs

use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};

use wots::{Wots, WotsSignature};
use wattcoin_core::mixnet::OnionPacket; // 💡 Import du Mixnet
use crate::state::SharedL2State;
use crate::transaction::{L2Transaction, WnsBlock};

// Passage au binaire pur (Vec<u8>) pour une vitesse maximale et la compatibilité Mixnet
pub type ActiveWnsPeers = Arc<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>>;

#[derive(Serialize, Deserialize, Debug)]
pub enum WnsP2PMessage {
    Handshake { block_index: u64 },
    SyncResponse { state: crate::state::L2State },
    BroadcastTx { tx: L2Transaction },
    BroadcastBlock { block: WnsBlock },
    RelayOnion { packet: OnionPacket }, // Le WNS devient un relais Mixnet incensurable !
}

// TCP Framing : Lecture binaire sécurisée (Préfixe de taille 4 octets)
async fn read_wns_message<R: AsyncReadExt + std::marker::Unpin>(reader: &mut R) -> Option<WnsP2PMessage> {
    let mut len_buf = [0u8; 4];
    if reader.read_exact(&mut len_buf).await.is_err() { return None; }
    
    let length = u32::from_be_bytes(len_buf) as usize;
    // Bouclier mémoire : 33 Mo maximum par message (comme sur le L1)
    if length > 34_603_008 { 
        println!("🚨 [SÉCURITÉ WNS] Message trop volumineux ignoré ({} octets).", length);
        return None; 
    }
    
    let mut payload = vec![0u8; length];
    if reader.read_exact(&mut payload).await.is_err() { return None; }
    
    bincode::deserialize(&payload).ok()
}

pub async fn start_wns_p2p_server(port: u16, state: SharedL2State, active_peers: ActiveWnsPeers, node_kyber_secret: String) {
    let address = format!("0.0.0.0:{}", port);
    let listener = TcpListener::bind(&address).await.unwrap();
    println!("🌐 [WNS P2P] Serveur Gossip L2 (Binaire & Mixnet) à l'écoute sur TCP/{}...", port);

    loop {
        let (socket, peer_addr) = listener.accept().await.unwrap();
        let peer_ip = peer_addr.ip().to_string();
        
        println!("🤝 [WNS P2P] Connexion entrante : {}", peer_ip);
        start_peer_connection(socket, peer_ip, Arc::clone(&state), Arc::clone(&active_peers), node_kyber_secret.clone());
    }
}

pub fn start_peer_connection(
    socket: TcpStream, 
    peer_ip: String, 
    state: SharedL2State, 
    active_peers: ActiveWnsPeers,
    node_kyber_secret: String 
) {
    let (mut read_half, mut write_half) = socket.into_split();
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(10_000);

    let peer_id = format!("{}_{}", peer_ip, rand::random::<u32>());
    active_peers.lock().unwrap().insert(peer_id.clone(), tx.clone());

    // Handshake initial (Binaire)
    let my_index = state.lock().unwrap().block_index;
    let hs = WnsP2PMessage::Handshake { block_index: my_index };
    if let Ok(payload) = bincode::serialize(&hs) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);
        let _ = tx.try_send(framed);
    }

    tokio::spawn(async move {
        while let Some(msg_bytes) = rx.recv().await {
            if write_half.write_all(&msg_bytes).await.is_err() { break; }
            let _ = write_half.flush().await;
        }
    });

    let active_peers_clone = Arc::clone(&active_peers);
    
    tokio::spawn(async move {
        while let Some(message) = read_wns_message(&mut read_half).await {
            match message {
                WnsP2PMessage::Handshake { block_index } => {
                    let my_state = state.lock().unwrap();
                    if my_state.block_index > block_index {
                        println!("📤 [SYNC] Pair en retard détecté ({} vs {}). Envoi du snapshot L2...", block_index, my_state.block_index);
                        let env = WnsP2PMessage::SyncResponse { state: my_state.clone() };
                        broadcast_message(&env, &active_peers_clone, &peer_id); // On utilise broadcast_message pour formater correctement
                    }
                },
                WnsP2PMessage::SyncResponse { state: new_state } => {
                    let mut my_state = state.lock().unwrap();
                    
                    if new_state.block_index > my_state.block_index {
                        if new_state.last_l1_block >= my_state.last_l1_block {
                            println!("📥 [SYNC] Snapshot L2 téléchargé ! Mise à jour du Bloc {} ➡ {}", my_state.block_index, new_state.block_index);
                            
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
                        
                        let is_valid = if let Ok(sig) = serde_json::from_str::<WotsSignature>(&in_tx.signature) {
                            hex::encode(&sig.public_key) == in_tx.sender_pubkey && Wots::verify(&sig, &hash)
                        } else {
                            false
                        };

                        if is_valid {
                            state_guard.mempool.push(in_tx.clone());
                            println!("📡 [WNS P2P] Nouvelle TX relayée reçue : Action {:?} sur '{}'", in_tx.action, in_tx.domain_name);

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
                        
                        let mut hasher = sha2::Sha256::new(); 
                        use sha2::Digest;
                        hasher.update(block.state_root.as_bytes());
                        let mut hash_array = [0u8; 32];
                        hash_array.copy_from_slice(&hasher.finalize());
                        
                        let is_valid = if let Ok(sig) = serde_json::from_str::<WotsSignature>(&block.signature) {
                            hex::encode(&sig.public_key) == block.sequencer_pubkey && Wots::verify(&sig, &hash_array)
                        } else {
                            false
                        };

                        if !is_valid {
                            println!("❌ [WNS P2P] Signature du bloc invalide !");
                            continue; 
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
                WnsP2PMessage::RelayOnion { packet } => {
                    // 💡 Utilisation de la VRAIE clé dynamique du nœud !
                    match packet.peel(&node_kyber_secret) {
                        Ok(hop_payload) => {
                            if hop_payload.next_hop_address.is_empty() {
                                println!("🎯 [MIXNET WNS] Destination finale atteinte.");
                            } else {
                                println!("🧅 [MIXNET WNS] Couche épluchée. Relais P2P vers : {}", hop_payload.next_hop_address);
                                
                                if let Ok(next_packet) = bincode::deserialize::<OnionPacket>(&hop_payload.inner_data) {
                                    let target_ip = hop_payload.next_hop_address.clone();
                                    tokio::spawn(async move {
                                        if let Ok(mut stream) = tokio::net::TcpStream::connect(&target_ip).await {
                                            let envelope = WnsP2PMessage::RelayOnion { packet: next_packet };
                                            if let Ok(payload) = bincode::serialize(&envelope) {
                                                let length = (payload.len() as u32).to_be_bytes();
                                                let mut framed = Vec::with_capacity(4 + payload.len());
                                                framed.extend_from_slice(&length);
                                                framed.extend_from_slice(&payload);
                                                let _ = stream.write_all(&framed).await;
                                            }
                                        }
                                    });
                                }
                            }
                        },
                        Err(e) => { println!("❌ [MIXNET WNS] Rejet du paquet en oignon : {}", e); }
                    }
                }
            }
        }
        active_peers.lock().unwrap().remove(&peer_id);
    });
}

// L'envoi est désormais structuré avec le TCP Framing binaire
pub fn broadcast_message(msg: &WnsP2PMessage, active_peers: &ActiveWnsPeers, skip_peer: &str) {
    if let Ok(payload) = bincode::serialize(msg) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);
        
        let peers = active_peers.lock().unwrap().clone();
        for (id, sender) in peers.iter() {
            if id != skip_peer {
                let _ = sender.try_send(framed.clone());
            }
        }
    }
}