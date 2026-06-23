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
	BroadcastMicroBlock { micro_block: crate::block::MicroBlock },
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

pub async fn start_p2p_server(host_ip: &str, port: &str, blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool, known_peers: crate::SharedPeers, active_peers: ActivePeers, l2_db_file: String) {
    let address = format!("{}:{}", host_ip, port);
    let listener = TcpListener::bind(&address).await.unwrap();
    println!("📡 Serveur P2P (Tunnels Persistants) à l'écoute sur TCP/{}...", port);
    
    let my_port = port.to_string(); 

    loop {
        let (socket, peer_addr) = listener.accept().await.unwrap();
        let peer_ip = peer_addr.ip().to_string();
        
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        println!("🤝 [{}] Nouvelle connexion P2P entrante depuis {} !", now, peer_ip);
        
        start_peer_connection(
            socket, peer_ip, my_port.clone(), 
            Arc::clone(&blockchain), Arc::clone(&mempool), Arc::clone(&dex_pool), 
            Arc::clone(&known_peers), Arc::clone(&active_peers), l2_db_file.clone()
        );
    }
}

pub fn start_peer_connection(
    socket: TcpStream, peer_ip: String, my_port: String,
    blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool,
    known_peers: crate::SharedPeers, active_peers: ActivePeers, l2_db_file: String
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
					if blocks.is_empty() {
						println!("⚠️ [SYNC] Lot de blocs vide reçu, ignoré.");
						continue;
					}
					println!("📥 [SYNC] Lot de {} blocs téléchargé ! (Index {} à {})", blocks.len(), blocks[0].header.index, blocks.last().unwrap().header.index);
					let mut chain = blockchain.lock().unwrap(); 
					if chain.resolve_partial_fork(blocks.clone()) { 
						println!("✅ [SYNC] Rattrapage réussi ! La blockchain locale est à jour (Taille: {}).", chain.chain.len());
						let mut mp = mempool.lock().unwrap();
						mp.retain(|tx| { !blocks.iter().any(|b| b.transactions.iter().any(|mined_tx| mined_tx.public_key == tx.public_key)) });
					} else {
						println!("❌ [SYNC] Échec de la fusion !");
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
                        
                        
                        
                        mempool.lock().unwrap().retain(|t| { 
                            !block.transactions.iter().any(|mined_tx| {
                                mined_tx.public_key == t.public_key
                            }) 
                        });
                        
                        dex_pool.lock().unwrap().clear();
                        println!("🧹 [DEX] Nouveau bloc reçu : La session FBA est clôturée, Dark Pool vidé.");
						
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
						if !pool.iter().any(|t| t.public_key == in_tx.public_key) {
							println!("📥 [MEMPOOL] Nouvelle TX reçue via P2P !");   // ← Message unique et beau

							let tx_to_propagate = in_tx.clone();           // ← FIX du move
							pool.push(in_tx);                              // original dans le mempool local

							// Propagation aux autres nœuds
							let envelope = P2PMessage::BroadcastTransaction { tx: tx_to_propagate };
							let mut json_str = serde_json::to_string(&envelope).unwrap();
							json_str.push('\n');
							let ap = active_peers.lock().unwrap().clone();
							for (peer_id, sender) in ap.iter() {
								if peer_id != &actual_peer_id {
									let _ = sender.try_send(json_str.clone());
								}
							}
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
                                if chain.spent_key_images.contains(&input.mpc_ring.key_image) {
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
				
				P2PMessage::BroadcastMicroBlock { micro_block } => {
                    let (current_l1_hash, current_l2_root) = {
                        let chain = blockchain.lock().unwrap();
                        let last_block = chain.chain.last().unwrap();
                        (last_block.header.hash.clone(), last_block.header.l2_root.clone())
                    };

                    if micro_block.l1_parent_hash == current_l1_hash && micro_block.merkle_proof.len() == 128 {
                        let mut calculated_root = micro_block.merkle_proof[0].clone();
                        for i in 1..128 {
                            calculated_root = crate::merkle_ring::MpcRingSignature::hash_nodes(&calculated_root, &micro_block.merkle_proof[i]);
                        }
                        
                        if calculated_root == current_l2_root && micro_block.merkle_proof[micro_block.micro_index as usize] == micro_block.sequencer_pubkey {
                            let mb_data = format!("{}{}{}", micro_block.l1_parent_hash, micro_block.micro_index, micro_block.timestamp);
                            let mut hasher = sha2::Sha512::new();
                            use sha2::Digest;
                            hasher.update(mb_data.as_bytes());
                            let mut hash_arr = [0u8; 64];
                            hash_arr.copy_from_slice(&hasher.finalize());

                            if crate::wots::WotsKeyPair::verify(&micro_block.sequencer_pubkey, &micro_block.sequencer_sig, &hash_arr) {
                                                
                                // 🛡️ 1. TRIBUNAL DES FRAIS (Anti-Triche du Séquenceur)
                                if micro_block.transactions.is_empty() || micro_block.transactions[0].tx_type != TransactionType::MicroCoinbase {
                                    println!("❌ [L2 REJETÉ] Le séquenceur a oublié la MicroCoinbase !");
                                    continue;
                                }
                                let expected_fees = (micro_block.transactions.len() - 1) as u64 * 100;
                                let actual_fees: u64 = micro_block.transactions[0].outputs[0].aes_vault.parse().unwrap_or(u64::MAX);
                                                
                                if actual_fees > expected_fees {
                                    println!("❌ [L2 REJETÉ] Le séquenceur tente d'imprimer de l'argent ({} vs {}) !", actual_fees, expected_fees);
                                    continue;
                                }

                                println!("⚡ [L2 VERIFIÉ] MicroBloc {}/128 sauvegardé ! ({} TXs instantanées validées)", 
                                            micro_block.micro_index, micro_block.transactions.len() - 1);
                                                
                                // 🛡️ 2. MISE À JOUR DE L'ÉTAT L1/L2 UNIFIÉ (Anti-Double Dépense)
                                {
                                    let mut chain = blockchain.lock().unwrap();
                                    for tx in &micro_block.transactions {
                                        if tx.tx_type != TransactionType::MicroCoinbase {
                                            for input in &tx.inputs {
                                                chain.spent_key_images.insert(input.mpc_ring.key_image.clone());
                                            }
                                        }
                                    }
                                    // Sauvegarde directe sur HDD !
                                    crate::blockchain::Blockchain::save_microblock_to_disk(&l2_db_file, &micro_block);
                                }

                                // 🛡️ 3. NETTOYAGE DU MEMPOOL
                                {
                                    let mut mp = mempool.lock().unwrap();
                                    mp.retain(|tx| !micro_block.transactions.iter().any(|m_tx| m_tx.public_key == tx.public_key));
                                }
								
                                let envelope = P2PMessage::BroadcastMicroBlock { micro_block: micro_block.clone() };
                                let mut json_str = serde_json::to_string(&envelope).unwrap();
                                json_str.push('\n');
                                let ap = active_peers.lock().unwrap().clone();
                                for (peer_id, sender) in ap.iter() {
                                    if peer_id != &actual_peer_id { let _ = sender.try_send(json_str.clone()); }
                                }
                            }
                        }
                    }
                },
            }
        }
        
        println!("🔌 [P2P] Connexion perdue avec {}.", actual_peer_id);
        active_peers.lock().unwrap().remove(&actual_peer_id);
    });
}

pub async fn connect_to_network(target_peer: &str, my_port: &str, l2_db_file: &str, blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool, known_peers: crate::SharedPeers, active_peers: ActivePeers) {
    let address = if target_peer.contains(':') { target_peer.to_string() } else { format!("127.0.0.1:{}", target_peer) };
    
    println!("🧅 [ARTI-TOR] Initialisation du client Tor embarqué (sans démon externe)...");

    let config = TorClientConfig::default();
    
    match TorClient::create_bootstrapped(config).await {
        Ok(tor_client) => {
            println!("✅ [ARTI-TOR] Nœud anonymisé !");

            // 💡 BOUCLE INFINIE : Le Mineur n'abandonne jamais Tor et refuse le TCP !
            loop {
                println!("⏳ Tentative de création du circuit Onion vers {}...", address);
                
                let mut prefs = arti_client::StreamPrefs::new();
				prefs.connect_to_onion_services(arti_client::config::BoolOrAuto::Explicit(true));

				match tor_client.connect_with_prefs(address.clone(), &prefs).await {
                    Ok(tor_stream) => {
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

                        // Tâche d'écriture en arrière-plan
                        tokio::spawn(async move {
                            while let Some(msg) = rx.recv().await {
                                if write_half.write_all(msg.as_bytes()).await.is_err() { break; }
                                let _ = write_half.flush().await; 
                            }
                        });

                        // 💡 Envoi du Handshake INITIAL *avant* d'écouter
                        let sender_opt = { active_peers.lock().unwrap().get(&temp_peer_id).cloned() };
                        if let Some(sender) = sender_opt {
                            send_message_to_channel(&sender, P2PMessage::Handshake { 
                                genesis_hash: my_genesis, 
                                current_height: my_height, 
                                sender_port: my_port.to_string() 
                            }).await;
                        }

                        // Préparation des clones pour le cerveau de réception
                        let ap_clone = Arc::clone(&active_peers);
                        let kp_clone = Arc::clone(&known_peers);
                        let bc_clone = Arc::clone(&blockchain);
                        let mp_clone = Arc::clone(&mempool);
                        let dp_clone = Arc::clone(&dex_pool);
                        let my_port_clone = my_port.to_string();
                        let address_for_task = address.clone();
                        let mut actual_peer_id = temp_peer_id.clone();

                        // 💡 BOUCLE DE LECTURE (Bloquante)
                        // Tant que la connexion tient, on reste ici. Si elle casse, la boucle while s'arrête.
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
									if blocks.is_empty() {
										println!("⚠️ [SYNC TOR] Lot de blocs vide reçu, ignoré.");
										continue;
									}
									println!("📥 [SYNC TOR] Lot de {} blocs téléchargé ! (Index {} à {})", blocks.len(), blocks[0].header.index, blocks.last().unwrap().header.index);
									let mut chain = blockchain.lock().unwrap(); 
									if chain.resolve_partial_fork(blocks.clone()) { 
										println!("✅ [SYNC TOR] Rattrapage réussi ! La blockchain locale est à jour (Taille: {}).", chain.chain.len());
										let mut mp = mempool.lock().unwrap();
										mp.retain(|tx| { !blocks.iter().any(|b| b.transactions.iter().any(|mined_tx| mined_tx.public_key == tx.public_key)) });
									} else {
										println!("❌ [SYNC TOR] Échec de la fusion !");
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
                                        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                                        let tx_count = block.transactions.len();
                                        let tx_detail = if tx_count == 1 { "1 Coinbase".to_string() } else { format!("1 Coinbase + {} Publique/Swap", tx_count - 1) };

                                        println!("\n====================================================================");
                                        println!("🌍 [RÉSEAU] NOUVEAU BLOC {} REÇU VIA TOR ! (Source: {})", block.header.index, sender_port);
                                        println!("🕒 Reçu le : {}", now);
                                        println!("🔗 Hash    : {}", block.header.hash);
                                        println!("📝 Contenu : {} transactions incluses ({})", tx_count, tx_detail);
                                        println!("====================================================================");
                                        
                                        
                                        
                                        mp_clone.lock().unwrap().retain(|t| { 
                                            !block.transactions.iter().any(|mined_tx| {
                                                mined_tx.public_key == t.public_key
                                            }) 
                                        });
                                        
                                        dp_clone.lock().unwrap().clear();
                                        println!("🧹 [DEX] Nouveau bloc Tor reçu : La session FBA est clôturée, Dark Pool vidé.");

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
										if !pool.iter().any(|t| t.public_key == in_tx.public_key) {
											println!("📥 [MEMPOOL] Nouvelle TX reçue via Tor !");   // ← Le message que tu aimes

											let tx_to_propagate = in_tx.clone();          // ← FIX du move
											pool.push(in_tx);

											let envelope = P2PMessage::BroadcastTransaction { tx: tx_to_propagate };
											let mut json_str = serde_json::to_string(&envelope).unwrap();
											json_str.push('\n');
											let ap = ap_clone.lock().unwrap().clone();
											for (peer_id, sender) in ap.iter() {
												if peer_id != &actual_peer_id {
													let _ = sender.try_send(json_str.clone());
												}
											}
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
                                                if chain.spent_key_images.contains(&input.mpc_ring.key_image) { spent = true; break; }
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
								P2PMessage::BroadcastMicroBlock { micro_block } => {
                                    let (current_l1_hash, current_l2_root) = {
                                        let chain = bc_clone.lock().unwrap(); // ⚡ bc_clone ici
                                        let last_block = chain.chain.last().unwrap();
                                        (last_block.header.hash.clone(), last_block.header.l2_root.clone())
                                    };

                                    if micro_block.l1_parent_hash == current_l1_hash && micro_block.merkle_proof.len() == 128 {
                                        let mut calculated_root = micro_block.merkle_proof[0].clone();
                                        for i in 1..128 {
                                            calculated_root = crate::merkle_ring::MpcRingSignature::hash_nodes(&calculated_root, &micro_block.merkle_proof[i]);
                                        }
                                        
                                        if calculated_root == current_l2_root && micro_block.merkle_proof[micro_block.micro_index as usize] == micro_block.sequencer_pubkey {
                                            let mb_data = format!("{}{}{}", micro_block.l1_parent_hash, micro_block.micro_index, micro_block.timestamp);
                                            let mut hasher = sha2::Sha512::new();
                                            use sha2::Digest;
                                            hasher.update(mb_data.as_bytes());
                                            let mut hash_arr = [0u8; 64];
                                            hash_arr.copy_from_slice(&hasher.finalize());

                                            if crate::wots::WotsKeyPair::verify(&micro_block.sequencer_pubkey, &micro_block.sequencer_sig, &hash_arr) {
                                                
                                                // 🛡️ 1. TRIBUNAL DES FRAIS (Anti-Triche du Séquenceur)
                                                if micro_block.transactions.is_empty() || micro_block.transactions[0].tx_type != TransactionType::MicroCoinbase {
                                                    println!("❌ [L2 REJETÉ] Le séquenceur a oublié la MicroCoinbase !");
                                                    continue;
                                                }
                                                let expected_fees = (micro_block.transactions.len() - 1) as u64 * 100;
                                                let actual_fees: u64 = micro_block.transactions[0].outputs[0].aes_vault.parse().unwrap_or(u64::MAX);
                                                
                                                if actual_fees > expected_fees {
                                                    println!("❌ [L2 REJETÉ] Le séquenceur tente d'imprimer de l'argent ({} vs {}) !", actual_fees, expected_fees);
                                                    continue;
                                                }

                                                println!("⚡ [L2 VERIFIÉ] MicroBloc {}/128 sauvegardé ! ({} TXs instantanées validées)", 
                                                          micro_block.micro_index, micro_block.transactions.len() - 1);
                                                
                                                // 🛡️ 2. MISE À JOUR DE L'ÉTAT L1/L2 UNIFIÉ (Anti-Double Dépense)
                                                {
                                                    let mut chain = bc_clone.lock().unwrap();
                                                    for tx in &micro_block.transactions {
                                                        if tx.tx_type != TransactionType::MicroCoinbase {
                                                            for input in &tx.inputs {
                                                                chain.spent_key_images.insert(input.mpc_ring.key_image.clone());
                                                            }
                                                        }
                                                    }
                                                    // Sauvegarde directe sur HDD !
                                                    crate::blockchain::Blockchain::save_microblock_to_disk(&l2_db_file, &micro_block);
                                                }

                                                // 🛡️ 3. NETTOYAGE DU MEMPOOL
                                                {
                                                    let mut mp = mp_clone.lock().unwrap();
                                                    mp.retain(|tx| !micro_block.transactions.iter().any(|m_tx| m_tx.public_key == tx.public_key));
                                                }
												
                                                let envelope = P2PMessage::BroadcastMicroBlock { micro_block: micro_block.clone() };
                                                let mut json_str = serde_json::to_string(&envelope).unwrap();
                                                json_str.push('\n');
                                                let ap = ap_clone.lock().unwrap().clone(); // ⚡ ap_clone ici
                                                for (peer_id, sender) in ap.iter() {
                                                    if peer_id != &actual_peer_id { let _ = sender.try_send(json_str.clone()); }
                                                }
                                            }
                                        }
                                    }
                                },
                            }
                        }
                        
                        // Si on sort du "while", c'est que la connexion a été perdue !
                        println!("🔌 [ARTI-TOR] Connexion perdue avec {}.", actual_peer_id);
                        active_peers.lock().unwrap().remove(&actual_peer_id);
                    },
                    Err(e) => {
                        println!("⚠️ [ARTI-TOR] Échec de connexion ({}). L'hôte est peut-être temporairement injoignable.", e);
                    }
                }

                // Le cœur du repli : On attend 15 secondes, et la boucle 'loop' relance une attaque Tor !
                println!("⏳ [RETRY] Nouvelle tentative de percée Tor dans 15 secondes...");
                tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
            }
        }
        Err(e) => println!("🛑 [ARTI-TOR] Échec fatal de l'initialisation du circuit Tor local : {}", e),
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

pub async fn broadcast_micro_block(micro_block: crate::block::MicroBlock, active_peers: ActivePeers) {
    let envelope = P2PMessage::BroadcastMicroBlock { micro_block };
    let mut json_str = serde_json::to_string(&envelope).unwrap();
    json_str.push('\n');

    let peers = active_peers.lock().unwrap().clone();
    for (_peer_id, sender) in peers.iter() {
        let _ = sender.try_send(json_str.clone());
    }
}