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
use crate::dht::DhtRecord;
use crate::mixnet::OnionPacket;



pub type ActivePeers = Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>;

#[derive(Serialize, Deserialize, Debug)]
pub enum P2PMessage {
    Handshake { genesis_hash: String, current_height: u64, sender_port: String },
    // On remplace height et last_hash par la liste dynamique
    SyncRequest { locator_hashes: Vec<String>, sender_port: String },
    SyncResponse { blocks: Vec<Block> },
    NewBlock { block: Block, sender_port: String }, 
    WhisperTransaction { tx: Transaction },    
    BroadcastTransaction { tx: Transaction },  
    BroadcastOrder { order: Order },
    GetMempool,
    MempoolSync { txs: Vec<Transaction> },
	BroadcastMicroBlock { micro_block: crate::block::MicroBlock },
	/// Un nœud annonce qu'il héberge un site (ou relaie l'annonce d'un autre)
    DhtPublish { record: DhtRecord },
    /// Un navigateur demande "Qui connaît la route vers felps.watt ?"
    DhtLookup { domain_name: String, sender_port: String },
    /// Réponse contenant la route prouvée
    DhtResponse { record: Option<DhtRecord> },
	RelayOnion { packet: OnionPacket },
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
    let (tx, mut rx) = mpsc::channel::<String>(10_000);

    let random_id: u32 = rand::random();
    let temp_peer_id = format!("{}:incoming_{}", peer_ip, random_id);
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
                    actual_peer_id = format!("{}:{}_{}", peer_ip, sender_port, random_id);
                    known_peers.lock().unwrap().insert(actual_peer_id.clone());
                    
                    {
                        let mut ap = active_peers.lock().unwrap();
                        if let Some(sender) = ap.remove(&temp_peer_id) {
                            ap.insert(actual_peer_id.clone(), sender);
                        }
                    } 
					
                    let (is_behind, i_am_ahead, my_height, genesis_valid) = {
                        let chain = blockchain.lock().unwrap(); 
                        let my_h = chain.chain.len() as u64;
                        (
                            current_height > my_h, 
                            my_h > current_height, 
                            my_h, 
                            genesis_hash == chain.chain[0].header.hash
                        )
                    }; 

                    if !genesis_valid { break; }

                    if is_behind {
                        // ⚡ GÉNÉRATION DU LOCATOR (1, 2, puis de 5 en 5)
                        let locator_hashes = {
                            let chain = blockchain.lock().unwrap(); 
                            let mut locators = Vec::new();
                            let len = chain.chain.len();
                            
                            if len > 0 {
                                locators.push(chain.chain[len - 1].header.hash.clone());
                                if len > 1 { locators.push(chain.chain[len - 2].header.hash.clone()); }
                                
                                let mut idx = len.saturating_sub(2).saturating_sub(5);
                                while idx > 0 && locators.len() < 10 {
                                    locators.push(chain.chain[idx].header.hash.clone());
                                    idx = idx.saturating_sub(5);
                                }
                                // Le parachute final : on s'assure que le Genesis est toujours là
                                if locators.last() != Some(&chain.chain[0].header.hash) {
                                    locators.push(chain.chain[0].header.hash.clone()); 
                                }
                            }
                            locators
                        };

                        send_message_to_channel(&tx, P2PMessage::SyncRequest { locator_hashes, sender_port: my_port.clone() }).await;
                    } else if i_am_ahead {
                        send_message_to_channel(&tx, P2PMessage::Handshake { genesis_hash, current_height: my_height, sender_port: my_port.clone() }).await;
                    }
                },

                P2PMessage::SyncRequest { locator_hashes, sender_port: _ } => {
                    let blocks_to_send = {
                        let chain = blockchain.lock().unwrap(); 
                        let mut found_idx = 0; // Par défaut, on remonte au Genesis
                        
                        // ⚡ RECHERCHE DYNAMIQUE DE L'ANCÊTRE
                        for locator in locator_hashes {
                            if let Some(pos) = chain.chain.iter().position(|b| b.header.hash == locator) {
                                found_idx = pos;
                                break;
                            }
                        }
                        
                        // On envoie uniquement les blocs APRÈS l'ancêtre commun
                        if found_idx + 1 < chain.chain.len() {
                            Some(chain.chain[(found_idx + 1)..].to_vec())
                        } else {
                            None
                        }
                    }; 

                    if let Some(blocks) = blocks_to_send {
                        println!("📤 [SYNC] Le nœud distant est en retard. Envoi dynamique de {} blocs manquants...", blocks.len());
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
					// 🛡️ PATCH SÉCURITÉ P2P : On drop les Coinbase volantes !
					if in_tx.tx_type == TransactionType::Coinbase || in_tx.tx_type == TransactionType::MicroCoinbase {
						println!("🚨 [SÉCURITÉ] Drop d'une transaction Coinbase ou MicroCoinbase illégale reçue via P2P.");
						continue; 
					}
                    let mut rng = rand::thread_rng();
                    if rng.gen_range(1..=10) <= 2 {
                        mempool.lock().unwrap().push(in_tx);
                    } 
                },

                P2PMessage::BroadcastTransaction { tx: in_tx } => {
					// 1. 🛡️ REJET DES TRANSACTIONS SYSTÈMES EN P2P
					if matches!(in_tx.tx_type, TransactionType::Coinbase | TransactionType::MicroCoinbase | TransactionType::DexSettlement { .. } | TransactionType::LotteryPayout { .. }) {
						println!("🚨 [SÉCURITÉ] Tentative d'injection d'une transaction système via P2P. Bloquée.");
						continue; 
					}

					// 2. 🛡️ LE BOUCLIER QUALITATIF (P2Pool Mining Share)
					if let TransactionType::MiningShare { nonce, hash, timestamp, .. } = in_tx.tx_type.clone() {
						
						let (target, current_height, previous_hash, seed) = {
							let chain = blockchain.lock().unwrap(); // Ou bc_clone dans la boucle Tor
							let height = chain.chain.len() as u64;
							let prev_hash = if height > 0 { chain.chain.last().unwrap().header.hash.clone() } else { String::new() };
							(chain.target.clone(), height, prev_hash, chain.get_epoch_seed(height))
						};

						// Filtre Mathématique ultra-rapide (Instant Kill)
						let hash_bigint = num_bigint::BigUint::parse_bytes(hash.as_bytes(), 16).unwrap_or_default();
						if hash_bigint > (&target * 20u32) {
							// Un hacker a envoyé une part avec un hash qui n'est même pas gagnant
							continue;
						}

						// Filtre Anti-Exhaustion CPU (On limite le travail d'expertise)
						{
							let pool = mempool.lock().unwrap(); // Ou mp_clone dans la boucle Tor
							if pool.iter().filter(|t| matches!(t.tx_type, TransactionType::MiningShare { .. })).count() > 100 {
								continue; 
							}
						}

						// Expertise RandomX (Déportée en arrière-plan pour ne pas bloquer le réseau P2P)
						let tx_clone = in_tx.clone();
						let mp_clone_bg = Arc::clone(&mempool); // Ou mp_clone.clone() dans la boucle Tor
						let ap_clone_bg = Arc::clone(&active_peers); // Ou ap_clone.clone() dans la boucle Tor
						let actual_peer_id_clone = actual_peer_id.clone(); 

						tokio::task::spawn_blocking(move || {
							let parts: Vec<&str> = tx_clone.public_key.split('_').collect();
							let l2_root = parts.get(0).cloned().unwrap_or("");
							let tx_root = parts.get(1).cloned().unwrap_or("");
							
							let header_data = format!("{}{}{}{}{}{}", current_height, timestamp, previous_hash, nonce, l2_root, tx_root);
							
							let flags = randomx_rs::RandomXFlag::get_recommended_flags();
							// Initialisation légère du VM (Sans dataset = moins de RAM, idéal pour valider)
							if let Ok(cache) = randomx_rs::RandomXCache::new(flags, seed.as_bytes()) {
								if let Ok(vm) = randomx_rs::RandomXVM::new(flags, Some(cache), None) {
									if let Ok(hash_bytes) = vm.calculate_hash(header_data.as_bytes()) {
										
										if hex::encode(&hash_bytes) == *hash {
											let mut pool = mp_clone_bg.lock().unwrap();
											if !pool.iter().any(|t| t.public_key == tx_clone.public_key) {
												println!("⛏️ [P2POOL] Part de minage validée et ajoutée au réseau !");
												let tx_to_propagate = tx_clone.clone();
												pool.push(tx_clone);

												let envelope = P2PMessage::BroadcastTransaction { tx: tx_to_propagate };
												let mut json_str = serde_json::to_string(&envelope).unwrap();
												json_str.push('\n');
												
												let ap = ap_clone_bg.lock().unwrap().clone();
												for (peer_id, sender) in ap.iter() {
													if peer_id != &actual_peer_id_clone {
														let _ = sender.try_send(json_str.clone());
													}
												}
											}
										}
									}
								}
							}
						});
						
						continue; // On passe à l'écoute du message P2P suivant
					}

					// 3. TRAITEMENT CLASSIQUE POUR LES AUTRES TRANSACTIONS (HTLC, Envoi Classique...)
					if in_tx.is_valid() {
						let mut pool = mempool.lock().unwrap(); // Ou mp_clone dans la boucle Tor
						if !pool.iter().any(|t| t.public_key == in_tx.public_key) {
							println!("📥 [MEMPOOL] Nouvelle TX reçue via P2P !"); 
							let tx_to_propagate = in_tx.clone();          
							pool.push(in_tx);

							let envelope = P2PMessage::BroadcastTransaction { tx: tx_to_propagate };
							let mut json_str = serde_json::to_string(&envelope).unwrap();
							json_str.push('\n');
							
							let ap = active_peers.lock().unwrap().clone(); // Ou ap_clone dans la boucle Tor
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
                                        
                        // 💡 FIX : On utilise key_index pour vérifier l'appartenance à l'arbre !
                        if calculated_root == current_l2_root && micro_block.merkle_proof[micro_block.key_index as usize] == micro_block.sequencer_pubkey {
                                            
                            // 💡 FIX : On hache avec key_index
                            let mb_data = format!("{}{}{}{}", micro_block.l1_parent_hash, micro_block.micro_index, micro_block.key_index, micro_block.timestamp);
                            let mut hasher = sha2::Sha512::new();
                            use sha2::Digest;
                            hasher.update(mb_data.as_bytes());
                            let mut hash_arr = [0u8; 64];
                            hash_arr.copy_from_slice(&hasher.finalize());

                            if crate::wots::WotsKeyPair::verify(&micro_block.sequencer_pubkey, &micro_block.sequencer_sig, &hash_arr) {
                                                
                                // 1. TRIBUNAL DES FRAIS (Anti-Triche du Séquenceur)
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
                                                
                                // =======================================================================
								// 1.5 TRIBUNAL DES TRANSACTIONS L2 (La protection manquante !)
								// =======================================================================
								let mut all_valid = true;
								let mut temp_spent = std::collections::HashSet::new();

								{
									// Utilise bc_clone.lock().unwrap() pour la boucle Tor, 
									// ou blockchain.lock().unwrap() pour la boucle TCP.
									let chain_lock = blockchain.lock().unwrap(); 
									
									for (idx, tx) in micro_block.transactions.iter().enumerate() {
										if idx == 0 { continue; } // On ignore la MicroCoinbase (déjà vérifiée)
										
										// a) La transaction est-elle cryptographiquement valide ? (ZKP, Ring Signature, etc.)
										if !tx.is_valid() {
											println!("❌ [L2 REJETÉ] Le Séquenceur a inclus une transaction non valide cryptographiquement !");
											all_valid = false;
											break;
										}
										
										// b) Anti-Double Dépense strict (L1 + L2)
										let mut double_spend = false;
										for input in &tx.inputs {
											let ki = &input.mpc_ring.key_image;
											// On vérifie sur la blockchain L1 et dans les transactions précédentes de CE microbloc
											if chain_lock.spent_key_images.contains(ki) || temp_spent.contains(ki) {
												double_spend = true;
												break;
											}
											temp_spent.insert(ki.clone());
										}
										
										if double_spend {
											println!("❌ [L2 REJETÉ] Le Séquenceur tente de valider une double-dépense !");
											all_valid = false;
											break;
										}
									}
								}

								if !all_valid {
									println!("🚨 [SÉCURITÉ L2] MicroBloc frauduleux ignoré. Le séquenceur a perdu sa crédibilité.");
									continue; // On rejette brutalement tout le MicroBloc !
								}
								// =======================================================================
								
								// 2. MISE À JOUR DE L'ÉTAT L1/L2 UNIFIÉ (Anti-Double Dépense)
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

                                // 3. NETTOYAGE DU MEMPOOL
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
				
				P2PMessage::RelayOnion { packet } => {
                    // Le code du Nœud Relais que je t'ai donné tout à l'heure
                    let my_secret = "HEX_SECRET_KYBER_DU_NOEUD"; // Plus tard on chargera la vraie clé
                    match packet.peel(my_secret) {
                        Ok(hop_payload) => {
                            if hop_payload.next_hop_address.is_empty() {
                                println!("🎯 [MIXNET] Destination finale atteinte. Traitement de la requête.");
                            } else {
                                println!("🧅 [MIXNET] Couche épluchée. Transfert aveugle vers : {}", hop_payload.next_hop_address);
                                if let Ok(next_packet) = serde_json::from_str::<OnionPacket>(&hop_payload.inner_data) {
                                    let target_ip = hop_payload.next_hop_address.clone();
                                    tokio::spawn(async move {
                                        if let Ok(mut stream) = tokio::net::TcpStream::connect(&target_ip).await {
                                            let envelope = P2PMessage::RelayOnion { packet: next_packet };
                                            let mut json_str = serde_json::to_string(&envelope).unwrap();
                                            json_str.push('\n');
                                            let _ = stream.write_all(json_str.as_bytes()).await;
                                        }
                                    });
                                }
                            }
                        },
                        Err(e) => { println!("❌ [MIXNET] Rejet du paquet en oignon : {}", e); }
                    }
                },
                
                // Pour l'instant on ignore les requêtes DHT le temps de finir le Mixnet
                P2PMessage::DhtPublish { .. } | 
                P2PMessage::DhtLookup { .. } | 
                P2PMessage::DhtResponse { .. } => {
                    // TODO: Implémenter le stockage Kademlia plus tard
                }
            }
        }
        
        println!("🔌 [P2P] Connexion perdue avec {}.", actual_peer_id);
        active_peers.lock().unwrap().remove(&actual_peer_id);
    });
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