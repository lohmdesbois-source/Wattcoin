use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use rand::Rng;
use crate::block::Block;
use crate::blockchain::Blockchain;
use crate::transaction::{Transaction, TransactionType};
use crate::api::{Order, SharedPool};
use arti_client::{TorClient, TorClientConfig};
use tokio::io;

pub type ActivePeers = Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>;

#[derive(Serialize, Deserialize, Debug)]
pub enum P2PMessage {
    Handshake { genesis_hash: String, current_height: u64, sender_port: String },
    SyncRequest { current_height: u64, last_hash: String, sender_port: String }, 
    SyncResponse { blocks: Vec<Block> },
    NewBlock { block: Block, sender_port: String }, 
    WhisperTransaction { tx: Transaction },    
    BroadcastTransaction { tx: Transaction },  
    BroadcastOrder { order: Order },
    GetMempool,
    MempoolSync { txs: Vec<Transaction> },
}

async fn read_p2p_message<R: AsyncBufReadExt + std::marker::Unpin>(reader: &mut R) -> Option<P2PMessage> {
    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) => None,
        Ok(_) => serde_json::from_str::<P2PMessage>(&line.trim()).ok(),
        Err(_) => None,
    }
}

async fn send_message_to_channel(sender: &mpsc::Sender<String>, message: P2PMessage) {
    let mut json_str = serde_json::to_string(&message).unwrap();
    json_str.push('\n'); 
    let _ = sender.send(json_str).await;
}

pub async fn start_p2p_server(host_ip: &str, port: &str, blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool, known_peers: crate::SharedPeers, active_peers: ActivePeers) {
    let address = format!("{}:{}", host_ip, port);
    let listener = TcpListener::bind(&address).await.unwrap();
    println!("📡 Serveur P2P (Tunnels Persistants) à l'écoute sur TCP/{}...", port);
    
    let my_port = port.to_string(); 

    loop {
        let (socket, peer_addr) = listener.accept().await.unwrap();
        let peer_ip = peer_addr.ip().to_string();
        
        // 💡 NOUVEAU : Log du serrage de main pour le Relais
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        println!("🤝 [{}] Nouvelle connexion P2P entrante depuis {} !", now, peer_ip);
        
        start_peer_connection(
            socket, peer_ip, my_port.clone(), 
            Arc::clone(&blockchain), Arc::clone(&mempool), Arc::clone(&dex_pool), 
            Arc::clone(&known_peers), Arc::clone(&active_peers)
        );
    }
}

fn start_peer_connection(
    socket: TcpStream, peer_ip: String, my_port: String,
    blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool,
    known_peers: crate::SharedPeers, active_peers: ActivePeers
) {
    let (read_half, mut write_half) = socket.into_split();
    let mut reader = BufReader::new(read_half);
    let (tx, mut rx) = mpsc::channel::<String>(100);

    let temp_peer_id = format!("{}:incoming", peer_ip);
    active_peers.lock().unwrap().insert(temp_peer_id.clone(), tx.clone());

    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if write_half.write_all(msg.as_bytes()).await.is_err() { break; }
            let _ = write_half.flush().await;
        }
    });

    tokio::spawn(async move {
        let mut actual_peer_id = temp_peer_id.clone();

        while let Some(message) = read_p2p_message(&mut reader).await {
            match message {
                P2PMessage::Handshake { genesis_hash, current_height, sender_port } => {
                    actual_peer_id = format!("{}:{}", peer_ip, sender_port);
                    known_peers.lock().unwrap().insert(actual_peer_id.clone());
                    
                    {
                        let mut ap = active_peers.lock().unwrap();
                        if let Some(sender) = ap.remove(&temp_peer_id) {
                            ap.insert(actual_peer_id.clone(), sender);
                        }
                    } 

                    let (is_behind, i_am_ahead, my_height, my_hash, genesis_valid) = {
                        let chain = blockchain.lock().unwrap(); 
                        let my_h = chain.chain.len() as u64;
                        (
                            current_height > my_h, 
                            my_h > current_height, 
                            my_h, 
                            chain.chain.last().unwrap().header.hash.clone(),
                            genesis_hash == chain.chain[0].header.hash
                        )
                    }; 

                    if !genesis_valid { break; }

                    if is_behind {
                        send_message_to_channel(&tx, P2PMessage::SyncRequest { current_height: my_height, last_hash: my_hash, sender_port: my_port.clone() }).await;
                    } else if i_am_ahead {
                        send_message_to_channel(&tx, P2PMessage::Handshake { genesis_hash, current_height: my_height, sender_port: my_port.clone() }).await;
                    }
                },

                P2PMessage::SyncRequest { current_height, last_hash, sender_port: _ } => {
                    let blocks_to_send = {
                        let chain = blockchain.lock().unwrap(); 
                        let my_height = chain.chain.len() as u64;

                        if my_height > current_height {
                            let mut start_idx = current_height as usize;
                            let check_idx = start_idx.saturating_sub(1); 

                            if check_idx < chain.chain.len() && chain.chain[check_idx].header.hash == last_hash {
                                Some(chain.chain[start_idx..].to_vec())
                            } else {
                                start_idx = start_idx.saturating_sub(10);
                                if start_idx == 0 { start_idx = 1; } 
                                Some(chain.chain[start_idx..].to_vec())
                            }
                        } else { None }
                    }; 

                    if let Some(blocks) = blocks_to_send {
                        println!("📤 [SYNC] Le nœud distant est en retard. Envoi de {} blocs manquants...", blocks.len());
                        send_message_to_channel(&tx, P2PMessage::SyncResponse { blocks }).await;
                    }
                },
                
                P2PMessage::SyncResponse { blocks } => {
                    if blocks.is_empty() { continue; }
                    println!("📥 [SYNC] Lot de {} blocs téléchargé ! (Index {} à {}). Tentative de fusion...", blocks.len(), blocks.first().unwrap().header.index, blocks.last().unwrap().header.index);
                    let mut chain = blockchain.lock().unwrap(); 
                    
                    if chain.resolve_partial_fork(blocks.clone()) { 
                        println!("✅ [SYNC] Rattrapage réussi ! La blockchain locale est à jour (Taille: {}).", chain.chain.len());
                        let mut mp = mempool.lock().unwrap();
                        mp.retain(|tx| { !blocks.iter().any(|b| b.transactions.iter().any(|mined_tx| mined_tx.outputs[0].kyber_capsule == tx.outputs[0].kyber_capsule)) });
                    } else {
                        println!("❌ [SYNC] Échec de la fusion ! La fonction 'resolve_partial_fork' a refusé d'intégrer les blocs.");
                    }
                },

                P2PMessage::NewBlock { block, sender_port } => {
                    let reject_info = {
                        let mut chain = blockchain.lock().unwrap();
                        if let Err(_) = chain.validate_and_add_external_block(block.clone()) {
                            Some((chain.chain[0].header.hash.clone(), chain.chain.len() as u64))
                        } else { None }
                    };

                    if let Some((my_genesis, my_height)) = reject_info {
                        send_message_to_channel(&tx, P2PMessage::Handshake { genesis_hash: my_genesis, current_height: my_height, sender_port: my_port.clone() }).await;
                    } else {
                        // 💡 NOUVEAU : Log enrichi pour le serveur TCP
                        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                        let tx_count = block.transactions.len();
                        let tx_detail = if tx_count == 1 { "1 Coinbase".to_string() } else { format!("1 Coinbase + {} Publique/Swap", tx_count - 1) };

                        println!("\n====================================================================");
                        println!("🌍 [RÉSEAU] NOUVEAU BLOC {} REÇU VIA P2P ! (Source: {})", block.header.index, sender_port);
                        println!("🕒 Reçu le : {}", now);
                        println!("🔗 Hash    : {}", block.header.hash);
                        println!("📝 Contenu : {} transactions incluses ({})", tx_count, tx_detail);
                        println!("====================================================================");
                        println!("✅ Bloc {} validé et ajouté à la chaîne locale.", block.header.index);
                        
                        mempool.lock().unwrap().retain(|t| { !block.transactions.iter().any(|mined_tx| mined_tx.outputs[0].kyber_capsule == t.outputs[0].kyber_capsule) });
                        
                        let env = P2PMessage::NewBlock { block: block.clone(), sender_port: my_port.clone() };
                        let mut json_str = serde_json::to_string(&env).unwrap();
                        json_str.push('\n');
                        
                        let ap = active_peers.lock().unwrap().clone();
                        for (peer_id, sender) in ap.iter() {
                            if peer_id != &actual_peer_id {
                                let _ = sender.try_send(json_str.clone());
                            }
                        }
                    }
                },

                P2PMessage::WhisperTransaction { tx: in_tx } => {
                    let mut rng = rand::thread_rng();
                    if rng.gen_range(1..=10) <= 2 {
                        mempool.lock().unwrap().push(in_tx);
                    } 
                },

                P2PMessage::BroadcastTransaction { tx: in_tx } => {
                    if in_tx.is_valid() {
                        let mut pool = mempool.lock().unwrap();
                        if !pool.iter().any(|t| t.dilithium_signature == in_tx.dilithium_signature) {
                            println!("📥 [MEMPOOL] Nouvelle transaction publique reçue !");
                            pool.push(in_tx);
                        }
                    }
                },

                P2PMessage::GetMempool => {
                    let pool = mempool.lock().unwrap().clone();
                    send_message_to_channel(&tx, P2PMessage::MempoolSync { txs: pool }).await;
                },

                P2PMessage::MempoolSync { txs } => {
                    let mut local_mp = mempool.lock().unwrap();
                    let chain = blockchain.lock().unwrap(); 
                    let mut added = 0;
                    for t in txs {
                        let mut spent = false;
                        if t.tx_type != TransactionType::Coinbase {
                            for input in &t.inputs {
                                if chain.spent_key_images.contains(&input.pq_ring_signature.key_image) {
                                    spent = true;
                                    break;
                                }
                            }
                        }
                        if !local_mp.iter().any(|x| x.outputs[0].kyber_capsule == t.outputs[0].kyber_capsule) && !spent {
                            local_mp.push(t);
                            added += 1;
                        }
                    }
                    if added > 0 { println!("📥 [PULL] {} transaction(s) aspirée(s) !", added); }
                },

                P2PMessage::BroadcastOrder { order } => {
                    let mut pool = dex_pool.lock().unwrap();
                    if !pool.iter().any(|o| o.id == order.id) {
                        println!("🌊 [P2P DEX] Ordre reçu du réseau : {} {} WATT", order.order_type, order.amount_flames);
                        pool.push(order);
                    }
                },
            }
        }
        
        println!("🔌 [P2P] Connexion perdue avec {}.", actual_peer_id);
        active_peers.lock().unwrap().remove(&actual_peer_id);
    });
}

pub async fn connect_to_network(target_peer: &str, my_port: &str, blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool, known_peers: crate::SharedPeers, active_peers: ActivePeers) {
    let address = if target_peer.contains(':') { target_peer.to_string() } else { format!("127.0.0.1:{}", target_peer) };
    
    println!("🧅 [ARTI-TOR] Initialisation du client Tor embarqué (sans démon externe)...");

    let config = TorClientConfig::default();
    
    match TorClient::create_bootstrapped(config).await {
        Ok(tor_client) => {
            println!("✅ [ARTI-TOR] Nœud anonymisé ! Création du circuit Onion vers {}...", address);

            let mut connected = false;
            let mut retries = 0;
            let max_retries = 5;

            while !connected && retries < max_retries {
                println!("⏳ Tentative de percée du tunnel Tor (Essai {}/{}) ...", retries + 1, max_retries);
                
                match tor_client.connect(address.clone()).await {
                    Ok(tor_stream) => {
                        connected = true;
                        println!("🛡️ [ARTI-TOR] Tunnel fantôme établi ! L'IP du mineur est désormais intraçable.");
                        
                        let (my_genesis, my_height) = {
                            let chain = blockchain.lock().unwrap();
                            (chain.chain[0].header.hash.clone(), chain.chain.len() as u64)
                        };

                        let (read_half, mut write_half) = io::split(tor_stream);
                        let mut reader = BufReader::new(read_half);
                        let (tx, mut rx) = mpsc::channel::<String>(100);

                        let temp_peer_id = format!("{}:incoming_tor", address);
                        active_peers.lock().unwrap().insert(temp_peer_id.clone(), tx.clone());

                        tokio::spawn(async move {
                            while let Some(msg) = rx.recv().await {
                                if write_half.write_all(msg.as_bytes()).await.is_err() { break; }
                                let _ = write_half.flush().await; 
                            }
                        });

                        let ap_clone = Arc::clone(&active_peers);
                        let kp_clone = Arc::clone(&known_peers);
                        let bc_clone = Arc::clone(&blockchain);
                        let mp_clone = Arc::clone(&mempool);
                        let dp_clone = Arc::clone(&dex_pool);
                        let my_port_clone = my_port.to_string();
                        let temp_peer_id_for_task = temp_peer_id.clone();
                        
                        let address_for_task = address.clone();

                        tokio::spawn(async move {
                            let mut actual_peer_id = temp_peer_id_for_task;

                            while let Some(message) = read_p2p_message(&mut reader).await {
                                match message {
                                    P2PMessage::Handshake { genesis_hash, current_height, sender_port } => {
                                        actual_peer_id = format!("{}:{}", address_for_task, sender_port); 
                                        kp_clone.lock().unwrap().insert(actual_peer_id.clone());
                                        
                                        {
                                            let mut ap = ap_clone.lock().unwrap();
                                            let old_id = format!("{}:incoming_tor", address_for_task); 
                                            if let Some(sender) = ap.remove(&old_id) {
                                                ap.insert(actual_peer_id.clone(), sender);
                                            }
                                        } 

                                        let (is_behind, i_am_ahead, my_height, my_hash, genesis_valid) = {
                                            let chain = bc_clone.lock().unwrap(); 
                                            let my_h = chain.chain.len() as u64;
                                            (current_height > my_h, my_h > current_height, my_h, chain.chain.last().unwrap().header.hash.clone(), genesis_hash == chain.chain[0].header.hash)
                                        }; 

                                        if !genesis_valid { break; }

                                        if is_behind {
                                            send_message_to_channel(&tx, P2PMessage::SyncRequest { current_height: my_height, last_hash: my_hash, sender_port: my_port_clone.clone() }).await;
                                        } else if i_am_ahead {
                                            send_message_to_channel(&tx, P2PMessage::Handshake { genesis_hash, current_height: my_height, sender_port: my_port_clone.clone() }).await;
                                        }
                                    },
                                    P2PMessage::SyncRequest { current_height, last_hash, sender_port: _ } => {
                                        let blocks_to_send = {
                                            let chain = bc_clone.lock().unwrap(); 
                                            if (chain.chain.len() as u64) > current_height {
                                                let mut start_idx = current_height as usize;
                                                if start_idx.saturating_sub(1) < chain.chain.len() && chain.chain[start_idx.saturating_sub(1)].header.hash == last_hash {
                                                    Some(chain.chain[start_idx..].to_vec())
                                                } else {
                                                    start_idx = start_idx.saturating_sub(10);
                                                    if start_idx == 0 { start_idx = 1; } 
                                                    Some(chain.chain[start_idx..].to_vec())
                                                }
                                            } else { None }
                                        }; 
                                        if let Some(blocks) = blocks_to_send {
                                            println!("📤 [SYNC TOR] Envoi de {} blocs manquants...", blocks.len());
                                            send_message_to_channel(&tx, P2PMessage::SyncResponse { blocks }).await;
                                        }
                                    },
                                    P2PMessage::SyncResponse { blocks } => {
                                        if !blocks.is_empty() {
                                            println!("📥 [SYNC TOR] Lot de {} blocs reçu ! Tentative de fusion...", blocks.len());
                                            let mut chain = bc_clone.lock().unwrap(); 
                                            if chain.resolve_partial_fork(blocks.clone()) { 
                                                println!("✅ [SYNC TOR] Rattrapage réussi !");
                                                let mut mp = mp_clone.lock().unwrap();
                                                mp.retain(|t| { !blocks.iter().any(|b| b.transactions.iter().any(|mined_tx| mined_tx.outputs[0].kyber_capsule == t.outputs[0].kyber_capsule)) });
                                            } else {
                                                println!("❌ [SYNC TOR] Échec de la fusion !");
                                            }
                                        }
                                    },
                                    P2PMessage::NewBlock { block, sender_port } => {
                                        let reject_info = {
                                            let mut chain = bc_clone.lock().unwrap();
                                            if let Err(_) = chain.validate_and_add_external_block(block.clone()) {
                                                Some((chain.chain[0].header.hash.clone(), chain.chain.len() as u64))
                                            } else { None }
                                        };
                                        
                                        if let Some((my_genesis, my_height)) = reject_info {
                                            send_message_to_channel(&tx, P2PMessage::Handshake { genesis_hash: my_genesis, current_height: my_height, sender_port: my_port_clone.clone() }).await;
                                        } else {
                                            // 💡 NOUVEAU : Log enrichi pour le mineur sous Tor
                                            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                                            let tx_count = block.transactions.len();
                                            let tx_detail = if tx_count == 1 { "1 Coinbase".to_string() } else { format!("1 Coinbase + {} Publique/Swap", tx_count - 1) };

                                            println!("\n====================================================================");
                                            println!("🌍 [RÉSEAU] NOUVEAU BLOC {} REÇU VIA TOR ! (Source: {})", block.header.index, sender_port);
                                            println!("🕒 Reçu le : {}", now);
                                            println!("🔗 Hash    : {}", block.header.hash);
                                            println!("📝 Contenu : {} transactions incluses ({})", tx_count, tx_detail);
                                            println!("====================================================================");
                                            
                                            mp_clone.lock().unwrap().retain(|t| { !block.transactions.iter().any(|mined_tx| mined_tx.outputs[0].kyber_capsule == t.outputs[0].kyber_capsule) });
                                            
                                            let env = P2PMessage::NewBlock { block: block.clone(), sender_port: my_port_clone.clone() };
                                            let mut json_str = serde_json::to_string(&env).unwrap();
                                            json_str.push('\n');
                                            
                                            let ap = ap_clone.lock().unwrap().clone();
                                            for (_peer_id, sender) in ap.iter() {
                                                if _peer_id != &actual_peer_id { let _ = sender.try_send(json_str.clone()); }
                                            }
                                        }
                                    },
                                    P2PMessage::WhisperTransaction { tx: in_tx } => {
                                        if rand::thread_rng().gen_range(1..=10) <= 2 { mp_clone.lock().unwrap().push(in_tx); } 
                                    },
                                    P2PMessage::BroadcastTransaction { tx: in_tx } => {
                                        if in_tx.is_valid() {
                                            let mut pool = mp_clone.lock().unwrap();
                                            if !pool.iter().any(|t| t.dilithium_signature == in_tx.dilithium_signature) {
                                                println!("📥 [MEMPOOL] Nouvelle TX reçue via Tor !");
                                                pool.push(in_tx);
                                            }
                                        }
                                    },
                                    P2PMessage::GetMempool => {
                                        let pool = mp_clone.lock().unwrap().clone();
                                        send_message_to_channel(&tx, P2PMessage::MempoolSync { txs: pool }).await;
                                    },
                                    P2PMessage::MempoolSync { txs } => {
                                        let mut local_mp = mp_clone.lock().unwrap();
                                        let chain = bc_clone.lock().unwrap(); 
                                        for t in txs {
                                            let mut spent = false;
                                            if t.tx_type != TransactionType::Coinbase {
                                                for input in &t.inputs {
                                                    if chain.spent_key_images.contains(&input.pq_ring_signature.key_image) { spent = true; break; }
                                                }
                                            }
                                            if !local_mp.iter().any(|x| x.outputs[0].kyber_capsule == t.outputs[0].kyber_capsule) && !spent {
                                                local_mp.push(t);
                                            }
                                        }
                                    },
                                    P2PMessage::BroadcastOrder { order } => {
                                        let mut pool = dp_clone.lock().unwrap();
                                        if !pool.iter().any(|o| o.id == order.id) { pool.push(order); }
                                    },
                                }
                            }
                            
                            println!("🔌 [ARTI-TOR] Connexion perdue avec {}.", actual_peer_id);
                            ap_clone.lock().unwrap().remove(&actual_peer_id);
                        });

                        let sender_opt = { active_peers.lock().unwrap().get(&temp_peer_id).cloned() };
                        if let Some(sender) = sender_opt {
                            send_message_to_channel(&sender, P2PMessage::Handshake { 
                                genesis_hash: my_genesis, 
                                current_height: my_height, 
                                sender_port: my_port.to_string() 
                            }).await;
                        }
                    },
                    Err(e) => {
                        retries += 1;
                        println!("⚠️ [ARTI-TOR] Timeout du Nœud de Sortie ({}). Nouvelle tentative dans 5s...", e);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    }
                }
            }
            
            // 💡 REPLI TCP (FALLBACK)
            if !connected {
                println!("🛑 [ARTI-TOR] Échec définitif après {} tentatives. L'hébergeur distant bloque probablement les nœuds de sortie Tor.", max_retries);
                println!("⚠️ [REPLI] Activation du mode survie : Tentative de connexion en clair (TCP direct)...");
                
                if let Ok(socket) = TcpStream::connect(&address).await {
                    println!("✅ [REPLI] Connecté au réseau en clair via {} ! L'IP n'est plus masquée.", address);
                    
                    let (my_genesis, my_height) = {
                        let chain = blockchain.lock().unwrap();
                        (chain.chain[0].header.hash.clone(), chain.chain.len() as u64)
                    };

                    start_peer_connection(
                        socket, address.clone(), my_port.to_string(), 
                        Arc::clone(&blockchain), Arc::clone(&mempool), Arc::clone(&dex_pool), 
                        Arc::clone(&known_peers), Arc::clone(&active_peers)
                    );

                    let ip_only = address.split(':').next().unwrap_or(&address).to_string();
                    let sender_opt = {
                        active_peers.lock().unwrap().get(&format!("{}:incoming", ip_only)).cloned()
                    };
                    if let Some(sender) = sender_opt {
                        send_message_to_channel(&sender, P2PMessage::Handshake { 
                            genesis_hash: my_genesis, 
                            current_height: my_height, 
                            sender_port: my_port.to_string() 
                        }).await;
                    }
                } else {
                    println!("💀 [FATAL] Impossible de joindre le nœud {} ni via Tor, ni via TCP. Le serveur est hors ligne.", address);
                }
            }
        }
        Err(e) => println!("🛑 [ARTI-TOR] Échec de l'initialisation du circuit Tor local : {}", e),
    }
}

pub async fn broadcast_mined_block(my_port: &str, block: Block, active_peers: ActivePeers) {
    let envelope = P2PMessage::NewBlock { block, sender_port: my_port.to_string() };
    let mut json_str = serde_json::to_string(&envelope).unwrap();
    json_str.push('\n');

    let peers = active_peers.lock().unwrap().clone();
    for (_peer_id, sender) in peers.iter() {
        let _ = sender.try_send(json_str.clone());
    }
}

pub async fn broadcast_transaction(tx: Transaction, active_peers: ActivePeers) {
    let envelope = P2PMessage::BroadcastTransaction { tx };
    let mut json_str = serde_json::to_string(&envelope).unwrap();
    json_str.push('\n');

    let peers = active_peers.lock().unwrap().clone();
    for (_peer_id, sender) in peers.iter() {
        let _ = sender.try_send(json_str.clone());
    }
}

pub async fn broadcast_order(order: Order, active_peers: ActivePeers) {
    let envelope = P2PMessage::BroadcastOrder { order };
    let mut json_str = serde_json::to_string(&envelope).unwrap();
    json_str.push('\n');

    let peers = active_peers.lock().unwrap().clone();
    for (_peer_id, sender) in peers.iter() {
        let _ = sender.try_send(json_str.clone());
    }
}