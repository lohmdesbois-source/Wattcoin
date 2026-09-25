use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::time::Duration;
use serde::{Serialize, Deserialize};
use rand::Rng;
use crate::block::Block;
use crate::blockchain::Blockchain;
use crate::transaction::{Transaction, TransactionType};
use crate::api::{Order, SharedPool};
use crate::mixnet::OnionPacket;

// Le channel gère du binaire pur (Vec<u8>)
pub type ActivePeers = Arc<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>>;
pub static HIGHEST_KNOWN_BLOCK: AtomicU64 = AtomicU64::new(0);


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
	RelayOnion { packet: OnionPacket },
    NodeAnnouncement { 
        kyber_pubkey: String, 
        ip_port: String, 
        is_lighthouse: bool,
        timestamp: i64,
        pow_hash: String,     // Le hash RandomX
        nonce: u64,           // Le sel pour trouver le PoW
    },
}

// Le profil enregistré dans la base de données Sled
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DhtRecord {
    pub kyber_pubkey: String,
    pub ip_port: String,      // L'adresse IP et le port (vide si le nœud est caché)
    pub is_lighthouse: bool,  // Vrai si le nœud a passé le test TCP
    pub last_seen: i64,       // Timestamp de la dernière validation
}






// Lecture avec préfixe de taille (TCP Framing)
async fn read_p2p_message<R: AsyncReadExt + std::marker::Unpin>(reader: &mut R) -> Option<P2PMessage> {
    let mut len_buf = [0u8; 4];
    
    // 1. On lit exactement 4 octets pour connaître la taille
    if reader.read_exact(&mut len_buf).await.is_err() { return None; }
    let length = u32::from_be_bytes(len_buf) as usize;
    
    // 2. Limite stricte à 33 Mo (32 Mo de bloc + 1 Mo de marge Bincode) = 34_603_008 octets
    const MAX_MESSAGE_SIZE: usize = 34_603_008; 
    
    if length > MAX_MESSAGE_SIZE {
        println!("🚨 [SÉCURITÉ] Flux TCP ignoré : Message binaire trop volumineux ({} octets).", length);
        return None; 
    }
    
    // 3. On lit exactement le reste du message
    let mut payload = vec![0u8; length];
    if reader.read_exact(&mut payload).await.is_err() { return None; }
    
    bincode::deserialize(&payload).ok()
}

// Écriture Binaire avec préfixe
async fn send_message_to_channel(sender: &mpsc::Sender<Vec<u8>>, message: P2PMessage) {
    if let Ok(payload) = bincode::serialize(&message) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);
        let _ = sender.send(framed).await;
    }
}

pub async fn start_p2p_server(host_ip: &str, port: &str, blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool, known_peers: crate::SharedPeers, active_peers: ActivePeers) {
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
            Arc::clone(&known_peers), Arc::clone(&active_peers)
        );
    }
}

pub fn start_peer_connection(
    socket: TcpStream, peer_ip: String, my_port: String,
    blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool,
    known_peers: crate::SharedPeers, active_peers: ActivePeers
) {
    let (mut read_half, mut write_half) = socket.into_split();
    // Le channel transite du Vec<u8> !
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(10_000);

    let random_id: u32 = rand::random();
    let temp_peer_id = format!("{}:incoming_{}", peer_ip, random_id);
    active_peers.lock().unwrap().insert(temp_peer_id.clone(), tx.clone());

    tokio::spawn(async move {
        while let Some(msg_bytes) = rx.recv().await {
            if write_half.write_all(&msg_bytes).await.is_err() { break; }
        }
    });

    tokio::spawn(async move {
        let mut actual_peer_id = temp_peer_id.clone();

        // Le "Hello" immédiat ! Dès qu'on se connecte, on annonce notre hauteur.
        let (my_height, my_genesis) = {
            let chain = blockchain.lock().unwrap();
            (chain.current_height + 1, chain.get_block_by_height(0).unwrap().header.hash.clone())
        };
        send_message_to_channel(&tx, P2PMessage::Handshake { 
            genesis_hash: my_genesis, 
            current_height: my_height, 
            sender_port: my_port.clone() 
        }).await;

        // La boucle d'écoute existante...
        // On lit directement sur read_half
        while let Some(message) = read_p2p_message(&mut read_half).await {
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
                        let my_h = chain.current_height + 1;
                        (
                            current_height > my_h, 
                            my_h > current_height, 
                            my_h, 
                            genesis_hash == chain.get_block_by_height(0).unwrap().header.hash
                        )
                    }; 

                    if !genesis_valid { break; }

                    if is_behind {
                        // GÉNÉRATION DU LOCATOR (1, 2, puis de 5 en 5)
                        let locator_hashes = {
                            let chain = blockchain.lock().unwrap(); 
                            let mut locators = Vec::new();
                            let len = (chain.current_height + 1) as usize;
                            
                            if len > 0 {
                                locators.push(chain.get_block_by_height((len - 1) as u64).unwrap().header.hash.clone());
                                if len > 1 { locators.push(chain.get_block_by_height((len - 2) as u64).unwrap().header.hash.clone()); }
                                
                                let mut idx = len.saturating_sub(2).saturating_sub(5);
                                while idx > 0 && locators.len() < 10 {
                                    locators.push(chain.get_block_by_height(idx as u64).unwrap().header.hash.clone());
                                    idx = idx.saturating_sub(5);
                                }
                                // Le parachute final : on s'assure que le Genesis est toujours là
                                if locators.last() != Some(&chain.get_block_by_height(0).unwrap().header.hash) {
                                    locators.push(chain.get_block_by_height(0).unwrap().header.hash.clone()); 
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
                        
                        // RECHERCHE DYNAMIQUE DE L'ANCÊTRE
                        for locator in locator_hashes {
                            let mut found = false;
                            for i in (0..=chain.current_height).rev() {
                                if chain.get_block_by_height(i).unwrap().header.hash == locator {
                                    found_idx = i as usize;
                                    found = true; break;
                                }
                            }
                            if found { break; }
                        }
                        
                        // On envoie uniquement les blocs APRÈS l'ancêtre commun
                        // SYNC PING-PONG : 1 bloc binaire à la fois
                        if (found_idx as u64) < chain.current_height {
                            Some(vec![chain.get_block_by_height(found_idx as u64 + 1).unwrap()])
                        } else {
                            None
                        }
                    }; 

                    if let Some(blocks) = blocks_to_send {
                        send_message_to_channel(&tx, P2PMessage::SyncResponse { blocks }).await;
                    }
                },
                
                P2PMessage::SyncResponse { blocks } => {
                    if blocks.is_empty() {
                        println!("⚠️ [SYNC] Lot de blocs vide reçu, ignoré.");
                        continue;
                    }
                    
                    let incoming_last = blocks.last().unwrap();
                    let mut needs_sync_request = false;
                    let mut locators = Vec::new();

                    { // DÉBUT DE LA ZONE SOUS VERROU BLOCKCHAIN
                        let mut chain = blockchain.lock().unwrap(); 
                        let current_height = chain.current_height + 1;

                        if incoming_last.header.index < current_height {
                            let our_hash = &chain.get_block_by_height(incoming_last.header.index).unwrap().header.hash;
                            if our_hash == &incoming_last.header.hash {
                                continue;
                            }
                        }

                        println!("📥 [SYNC] Lot de {} blocs téléchargé ! (Index {} à {})", blocks.len(), blocks[0].header.index, incoming_last.header.index);
                        
                        if chain.resolve_partial_fork(blocks.clone()) { 
                            println!("✅ [SYNC] Rattrapage réussi ! La blockchain locale est à jour (Taille: {}).", chain.current_height + 1);
                            
                            let cutoff_time = if chain.current_height >= 1 { chain.get_block_by_height(chain.current_height - 1).unwrap().header.timestamp } else { 0 };

                            { // SOUS-VERROU MEMPOOL (isolé dans ses propres accolades)
                                let mut mp = mempool.lock().unwrap();
                                mp.retain(|tx| { 
                                    let not_in_block = !blocks.iter().any(|b| b.transactions.iter().any(|mined_tx| mined_tx.public_key == tx.public_key));
                                    let is_valid_share = match &tx.tx_type {
                                        TransactionType::MiningShare { timestamp, .. } => *timestamp >= cutoff_time,
                                        _ => true
                                    };
                                    not_in_block && is_valid_share
                                });
                            } // FIN VERROU MEMPOOL

                            // On relaie la bonne nouvelle au reste du réseau !
                            if let Some(last_block) = blocks.last() {
                                
                                // --- NOUVEL AFFICHAGE VISUEL POUR LE RELAIS ---
                                let now = chrono::Local::now().format("%d-%m-%Y %H:%M:%S");
                                let tx_count = last_block.transactions.len();
                                let tx_detail = if tx_count == 1 { 
                                    "1 Coinbase".to_string() 
                                } else { 
                                    format!("1 Coinbase + {} Publique/Swap/Loto", tx_count - 1) 
                                };

                                println!("\n====================================================================");
                                println!("🔄 [SYNC] BLOC {} RATTRAPÉ ET REDIFFUSÉ !", last_block.header.index);
                                println!("🕒 Synchronisé le : {}", now);
                                println!("🔗 Hash           : {}", last_block.header.hash);
                                println!("📝 Contenu        : {} transactions incluses ({})", tx_count, tx_detail);
                                println!("====================================================================");
                                // ----------------------------------------------

                                let env = P2PMessage::NewBlock { 
                                    block: last_block.clone(), 
                                    sender_port: my_port.clone() 
                                };
                                if let Ok(payload) = bincode::serialize(&env) {
                                    let length = (payload.len() as u32).to_be_bytes();
                                    let mut framed = Vec::with_capacity(4 + payload.len());
                                    framed.extend_from_slice(&length);
                                    framed.extend_from_slice(&payload);

                                    let ap = active_peers.lock().unwrap().clone();
                                    for (peer_id, sender) in ap.iter() {
                                        if peer_id != &actual_peer_id {
                                            let _ = sender.try_send(framed.clone()); 
                                        }
                                    }
                                }
                            }

                            if blocks.len() == 1 {
                                needs_sync_request = true;
                                locators = vec![incoming_last.header.hash.clone()];
                            }

                        } else {
                            println!("❌ [SYNC] Échec de la fusion !");
                        }
                    } // FIN DE LA ZONE SOUS VERROU BLOCKCHAIN (chain est 100% purgée)

                    // L'APPEL ASYNCHRONE EST TOTALEMENT ISOLÉ ICI
                    if needs_sync_request {
                        send_message_to_channel(&tx, P2PMessage::SyncRequest { locator_hashes: locators, sender_port: my_port.clone() }).await;
                    }
                },

                P2PMessage::NewBlock { block, sender_port } => {
					// DÉCLENCHEMENT DU KILL SWITCH
					crate::network::HIGHEST_KNOWN_BLOCK.fetch_max(block.header.index, Ordering::Relaxed);
                    // 1. Clones pour envoyer dans le thread d'arrière-plan
                    let bc_clone = Arc::clone(&blockchain);
                    let block_clone = block.clone();
                    let my_port_clone = my_port.clone();
                    let tx_clone = tx.clone();
                    let mempool_clone = Arc::clone(&mempool);
                    let dex_pool_clone = Arc::clone(&dex_pool);
                    let active_peers_clone = Arc::clone(&active_peers);
                    let actual_peer_id_clone = actual_peer_id.clone();

                    // 2. On libère IMMÉDIATEMENT le port TCP (le réseau respire)
                    tokio::spawn(async move {
                        
                        // VÉRIFICATION MATHÉMATIQUE HORS DU MUTEX !
                        // On vérifie tout le bloc sans bloquer le reste du nœud.
                        let mut all_math_valid = true;
                        for tx in &block_clone.transactions {
                            if tx.tx_type != TransactionType::Coinbase && tx.tx_type != TransactionType::MicroCoinbase {
                                if !tx.is_valid() {
                                    all_math_valid = false;
                                    break;
                                }
                            }
                        }
                        
                        if !all_math_valid {
                            println!("❌ [SÉCURITÉ] Bloc frauduleux ! Cryptographie invalide (Rejeté).");
                            return; // On jette le bloc sans jamais avoir bloqué le nœud !
                        }

                        // SEULEMENT MAINTENANT, on bloque la chaîne pour les vérifications de solde (ultra-rapide)
                        let bc_clone_blocking = Arc::clone(&bc_clone); 
                        let validation_result = tokio::task::spawn_blocking(move || {
							let mut chain = bc_clone_blocking.lock().unwrap();
							let current_height = chain.current_height + 1;
							
							// Anti-doublon ultra-rapide avant la grosse validation
							if block_clone.header.index < current_height {
								let our_hash = &chain.get_block_by_height(block_clone.header.index).unwrap().header.hash;
								if our_hash == &block_clone.header.hash {
									return Ok(false); // 👈 FAUX : Bloc déjà connu, on arrête les frais.
								}
							}

							if let Err(_) = chain.validate_and_add_external_block(block_clone) {
                                // Préparation des locators pour la synchro en cas de rejet
                                let mut locators = Vec::new();
                                let len = (chain.current_height + 1) as usize;
                                if len > 0 {
                                    locators.push(chain.get_block_by_height((len - 1) as u64).unwrap().header.hash.clone());
                                    if len > 1 { locators.push(chain.get_block_by_height((len - 2) as u64).unwrap().header.hash.clone()); }
                                    let mut idx = len.saturating_sub(2).saturating_sub(5);
                                    while idx > 0 && locators.len() < 10 {
                                        locators.push(chain.get_block_by_height(idx as u64).unwrap().header.hash.clone());
                                        idx = idx.saturating_sub(5);
                                    }
                                    if locators.last() != Some(&chain.get_block_by_height(0).unwrap().header.hash) {
                                        locators.push(chain.get_block_by_height(0).unwrap().header.hash.clone()); 
                                    }
                                }
                                Err((chain.get_block_by_height(0).unwrap().header.hash.clone(), len as u64, locators))
                            } else {
								Ok(true) // VRAI : C'est un vrai nouveau bloc validé !
							}
                        }).await.unwrap();

                        // 4. Gestion du résultat réseau
                        match validation_result {
                            Err((_, my_height, locator_hashes)) => {
                                // SI LE BLOC EST INVALIDE, ON RELÂCHE LE KILL SWITCH POUR LE MINEUR !
                                crate::network::HIGHEST_KNOWN_BLOCK.store(my_height.saturating_sub(1), Ordering::Relaxed);

                                send_message_to_channel(&tx_clone, P2PMessage::SyncRequest { locator_hashes, sender_port: my_port_clone.clone() }).await;
                            },
                            Ok(is_new) => {
								// BOUCLIER ANTI-TEMPÊTE
								if !is_new {
									return; // On coupe la propagation instantanément !
								}
								
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
                                
                                // LA RÈGLE D'OR : On récupère l'heure de l'avant-dernier bloc
                                let cutoff_time = {
                                    let chain = bc_clone.lock().unwrap(); 
                                    if chain.current_height >= 1 { chain.get_block_by_height(chain.current_height - 1).unwrap().header.timestamp } else { 0 }
                                };

                                let mined_hashes: Vec<_> = block.transactions.iter().map(|tx| tx.hash_data()).collect();
								mempool_clone.lock().unwrap().retain(|t| { 
									let not_in_block = !mined_hashes.contains(&t.hash_data());
                                    // PURGE KISS : On détruit les parts périmées
                                    let is_valid_share = match &t.tx_type {
                                        TransactionType::MiningShare { timestamp, .. } => *timestamp >= cutoff_time,
                                        _ => true
                                    };
                                    not_in_block && is_valid_share
                                });
                                
                                {
									let now = chrono::Utc::now().timestamp();
									let mut dp = dex_pool_clone.lock().unwrap();
									
									for tx in &block.transactions {
										if let TransactionType::DexSettlement { swaps, .. } = &tx.tx_type {
											for swap in swaps {
												if let Some(buy) = dp.iter_mut().find(|o| o.order_type == "buy" && o.htlc_hash.as_ref() == Some(&swap.htlc_hash)) {
													buy.amount_flames = buy.amount_flames.saturating_sub(swap.watt_amount_flames);
												}
												if let Some(sell) = dp.iter_mut().find(|o| o.order_type == "sell" && o.watt_address == swap.seller_watt_address && o.amount_flames >= swap.watt_amount_flames) {
													sell.amount_flames = sell.amount_flames.saturating_sub(swap.watt_amount_flames);
												}
											}
										}
									}
									dp.retain(|o| o.amount_flames > 0 && o.expires_at > now);
									println!("🧹 [DEX] Bloc reçu : Dark Pool synchronisé (ordres restants: {}).", dp.len());
								}
                                
                                let env = P2PMessage::NewBlock { block: block.clone(), sender_port: my_port_clone };
								if let Ok(payload) = bincode::serialize(&env) {
									let length = (payload.len() as u32).to_be_bytes();
									let mut framed = Vec::with_capacity(4 + payload.len());
									framed.extend_from_slice(&length);
									framed.extend_from_slice(&payload);

									let ap = active_peers_clone.lock().unwrap().clone();
									for (peer_id, sender) in ap.iter() {
										// CORRECTION : Utilisation des bonnes variables du scope
										if peer_id != &actual_peer_id_clone {
											let _ = sender.try_send(framed.clone());
										}
									}
								}
                            }
                        }
                    });
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
					// 1. REJET DES TRANSACTIONS SYSTÈMES EN P2P
					if matches!(in_tx.tx_type, TransactionType::Coinbase | TransactionType::MicroCoinbase | TransactionType::DexSettlement { .. } | TransactionType::LotteryPayout { .. }) {
						println!("🚨 [SÉCURITÉ] Tentative d'injection d'une transaction système via P2P. Bloquée.");
						continue; 
					}

					// 2. LE BOUCLIER QUALITATIF (P2Pool Mining Share)
					if let TransactionType::MiningShare { nonce, hash, timestamp, .. } = in_tx.tx_type.clone() {
						
						let (target, current_height, previous_hash, seed) = {
							let chain = blockchain.lock().unwrap();
							let height = chain.current_height + 1;
							let prev_hash = if height > 0 { chain.get_last_block().header.hash.clone() } else { String::new() };
							(chain.target.clone(), height, prev_hash, chain.get_epoch_seed(height))
						};

						// KILL SWITCH 1 : Si la part appartient à un vieux bloc, on la jette sans calcul !
						let highest_known = crate::network::HIGHEST_KNOWN_BLOCK.load(Ordering::Relaxed);
						if current_height < highest_known {
							continue; 
						}

						// Filtre Mathématique ultra-rapide (Instant Kill)
						let hash_bigint = num_bigint::BigUint::parse_bytes(hash.as_bytes(), 16).unwrap_or_default();
						if hash_bigint > (&target * 20u32) {
							continue;
						}

						// Filtre Anti-Exhaustion CPU
						{
							let pool = mempool.lock().unwrap();
							if pool.iter().filter(|t| matches!(t.tx_type, TransactionType::MiningShare { .. })).count() > 500 {
								continue; 
							}
						}

						let tx_clone = in_tx.clone();
						let mp_clone_bg = Arc::clone(&mempool);
						let ap_clone_bg = Arc::clone(&active_peers);
						let actual_peer_id_clone = actual_peer_id.clone();

						tokio::task::spawn_blocking(move || {
							// KILL SWITCH 2 : Juste avant de lancer le hachage lourd, on revérifie si le bloc a changé !
							if crate::network::HIGHEST_KNOWN_BLOCK.load(Ordering::Relaxed) > current_height {
								return; // On avorte la tâche pour libérer le CPU
							}

							let parts: Vec<&str> = tx_clone.public_key.split('_').collect();
							let l2_root = parts.get(0).cloned().unwrap_or("");
							let tx_root = parts.get(1).cloned().unwrap_or("");
							
							let header_data = format!("{}{}{}{}{}{}", current_height, timestamp, previous_hash, nonce, l2_root, tx_root);
							
							let flags = randomx_rs::RandomXFlag::get_recommended_flags();
							if let Ok(cache) = randomx_rs::RandomXCache::new(flags, seed.as_bytes()) {
								if let Ok(vm) = randomx_rs::RandomXVM::new(flags, Some(cache), None) {
									if let Ok(hash_bytes) = vm.calculate_hash(header_data.as_bytes()) {
										if hex::encode(&hash_bytes) == *hash {
											let mut pool = mp_clone_bg.lock().unwrap();
											let tx_hash = in_tx.hash_data();
											if !pool.iter().any(|t| t.hash_data() == tx_hash) {
												println!("⛏️ [P2POOL] Nouvelle part de minage relayée !");
												let tx_to_propagate = in_tx.clone();
												pool.push(in_tx);

												let envelope = P2PMessage::BroadcastTransaction { tx: tx_to_propagate };
												if let Ok(payload) = bincode::serialize(&envelope) {
													let length = (payload.len() as u32).to_be_bytes();
													let mut framed = Vec::with_capacity(4 + payload.len());
													framed.extend_from_slice(&length);
													framed.extend_from_slice(&payload);
													
													let ap = ap_clone_bg.lock().unwrap().clone();
													for (peer_id, sender) in ap.iter() {
														if peer_id != &actual_peer_id_clone {
															let _ = sender.try_send(framed.clone());
														}
													}
												}
											}
										}
									}
								}
							}
						});
						
						continue;
					}

					// 3. TRAITEMENT CLASSIQUE POUR LES AUTRES TRANSACTIONS (HTLC, Envoi Classique...)
					if in_tx.is_valid() {
						let mut pool = mempool.lock().unwrap(); 
						if !pool.iter().any(|t| t.hash_data() == in_tx.hash_data()) {
							println!("📥 [MEMPOOL] Nouvelle TX reçue via P2P !"); 
							let tx_to_propagate = in_tx.clone();          
							pool.push(in_tx);

							let envelope = P2PMessage::BroadcastTransaction { tx: tx_to_propagate };
							if let Ok(payload) = bincode::serialize(&envelope) {
								let length = (payload.len() as u32).to_be_bytes();
								let mut framed = Vec::with_capacity(4 + payload.len());
								framed.extend_from_slice(&length);
								framed.extend_from_slice(&payload);

								let ap = active_peers.lock().unwrap().clone();
								for (peer_id, sender) in ap.iter() {
									// CORRECTION : On envoie 'framed'
									if peer_id != &actual_peer_id {
										let _ = sender.try_send(framed.clone());
									}
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
                            if let Some(sig) = &t.wots_signature {
								let ki = hex::encode(&sig.public_key);
								if chain.spent_key_images.contains(&ki) {
									spent = true;
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
                    let bc_clone = Arc::clone(&blockchain);
                    let mp_clone = Arc::clone(&mempool);
                    let ap_clone = Arc::clone(&active_peers);
                    let actual_peer_id_clone = actual_peer_id.clone();

                    tokio::spawn(async move {
                        // 1. BOUCLIER DE LATENCE : On attend l'arrivée du parent L1 si besoin (Max 60 sec)
                        let mut parent_l1_ready = false;
                        for _ in 0..120 {
                            {
                                let chain = bc_clone.lock().unwrap();
                                for i in (0..=chain.current_height).rev().take(10) {
                                    if let Some(b) = chain.get_block_by_height(i) {
                                        if b.header.hash == micro_block.l1_parent_hash {
                                            parent_l1_ready = true;
                                            break;
                                        }
                                    }
                                }
                            }
                            if parent_l1_ready { break; }
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        }

                        if !parent_l1_ready {
                            println!("⚠️ [L2] Microbloc orphelin rejeté (Parent L1 '{}' inconnu).", micro_block.l1_parent_hash);
                            return;
                        }

                        // 2. LE TRIBUNAL CONSENSUS L2 (Dans un thread dédié pour ne pas bloquer le P2P)
                        let mb_clone = micro_block.clone();
                        let validation_result = tokio::task::spawn_blocking(move || {
                            let mut chain = bc_clone.lock().unwrap();
                            chain.validate_and_add_microblock(mb_clone)
                        }).await.unwrap();

                        match validation_result {
							Ok(()) => {
								{
									let mut mp = mp_clone.lock().unwrap();
									mp.retain(|tx| !micro_block.transactions.iter().any(|m_tx| m_tx.hash_data() == tx.hash_data()));
								}
								
								let envelope = P2PMessage::BroadcastMicroBlock { micro_block: micro_block.clone() };
								if let Ok(payload) = bincode::serialize(&envelope) {
									let length = (payload.len() as u32).to_be_bytes();
									let mut framed = Vec::with_capacity(4 + payload.len());
									framed.extend_from_slice(&length);
									framed.extend_from_slice(&payload);

									let ap = ap_clone.lock().unwrap().clone();
									for (peer_id, sender) in ap.iter() {
										if peer_id != &actual_peer_id_clone { 
											let _ = sender.try_send(framed.clone()); 
										}
									}
								}
							},
                            Err(e) => {
                                // Le Tribunal L2 a parlé. Le bloc est une fraude.
                                println!("{}", e); 
                            }
                        }
                    });
                },
				
				P2PMessage::RelayOnion { packet } => {
                    // Le code du Nœud Relais
                    let my_secret = "HEX_SECRET_KYBER_DU_NOEUD"; // Plus tard on chargera la vraie clé
                    match packet.peel(my_secret) {
                        Ok(hop_payload) => {
                            if hop_payload.next_hop_address.is_empty() {
                                println!("🎯 [MIXNET] Destination finale atteinte. Traitement de la requête.");
                            } else {
                                println!("🧅 [MIXNET] Couche épluchée. Transfert aveugle vers : {}", hop_payload.next_hop_address);
                                
                                // 💡 CORRECTION ICI : Décodage Binaire au lieu de JSON
                                if let Ok(next_packet) = bincode::deserialize::<OnionPacket>(&hop_payload.inner_data) {
                                    let target_ip = hop_payload.next_hop_address.clone();
                                    tokio::spawn(async move {
                                        if let Ok(mut stream) = tokio::net::TcpStream::connect(&target_ip).await {
                                            use tokio::io::AsyncWriteExt;
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
				
				P2PMessage::NodeAnnouncement { kyber_pubkey, ip_port, is_lighthouse, timestamp, pow_hash, nonce } => {
					// A. Filtre temporel (Anti-Rejeu) : On refuse les annonces qui ont plus de 2 heures
					// On ne les "périme" pas de la base une fois entrées, c'est juste pour l'admission.
					let now = chrono::Utc::now().timestamp();
					if timestamp < now - 7200 || timestamp > now + 3600 {
						println!("🚨 [DHT] Annonce ignorée (Timestamp invalide/rejeu).");
						continue;
					}

					// B. Validation du PoW RandomX (Mode Light, 0 RAM)
					let header_data = format!("{}{}{}{}", kyber_pubkey, ip_port, timestamp, nonce);
					let flags = randomx_rs::RandomXFlag::get_recommended_flags();
					
					let is_pow_valid = {
						let chain = blockchain.lock().unwrap();
						let seed = chain.get_epoch_seed(chain.current_height);
						
						if let Ok(cache) = randomx_rs::RandomXCache::new(flags, seed.as_bytes()) {
							if let Ok(vm) = randomx_rs::RandomXVM::new(flags, Some(cache), None) {
								if let Ok(hash_bytes) = vm.calculate_hash(header_data.as_bytes()) {
									let calculated_hash = hex::encode(&hash_bytes);
									// On vérifie que le hash correspond ET qu'il respecte la difficulté (les 12 premiers bits à zéro) !
									calculated_hash == pow_hash && hash_bytes[0] == 0 && hash_bytes[1] < 16
								} else { false }
							} else { false }
						} else { false }
					};

					if !is_pow_valid {
						println!("🚨 [DHT] Annonce ignorée (PoW Invalide). Spam détecté !");
						continue;
					}

					// C. Validation TCP Asynchrone & Écriture dans Sled
					// On clone les variables nécessaires pour le thread d'arrière-plan
					let kp_clone = kyber_pubkey.clone();
					let ip_clone = ip_port.clone();
					let chain_arc = Arc::clone(&blockchain); // On utilise la db de la blockchain
					
					tokio::spawn(async move {
						let mut is_valid = true;

						// Si le nœud se déclare "Phare", on le teste IMMÉDIATEMENT
						if is_lighthouse {
							// Timeout ultra court (2 secondes) pour ne pas engorger le réseau
							match tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(&ip_clone)).await {
								Ok(Ok(_stream)) => {
									println!("📡 [DHT] Ping réussi ! Nœud Phare validé : {}", ip_clone);
									// (La connexion se coupe toute seule à la fin du bloc)
								}
								_ => {
									println!("🚨 [DHT] Échec Ping TCP sur {}. Le nœud ment sur son statut de Phare.", ip_clone);
									is_valid = false;
								}
							}
						} else {
							println!("📡 [DHT] Annonce de Nœud Caché valide reçue : {}", kp_clone);
						}

						// Si tout est bon, on l'écrit de manière persistante
						if is_valid {
							let record = DhtRecord {
								kyber_pubkey: kp_clone.clone(),
								ip_port: ip_clone,
								is_lighthouse,
								last_seen: chrono::Utc::now().timestamp(),
							};

							// On récupère l'instance Sled depuis l'Arc de la Blockchain
							let db = { chain_arc.lock().unwrap().db.clone() }; 
							if let Ok(dht_tree) = db.open_tree("dht_nodes") {
								if let Ok(bincode_data) = bincode::serialize(&record) {
									let _ = dht_tree.insert(kp_clone.as_bytes(), bincode_data);
									let _ = dht_tree.flush(); // Force l'écriture sur le disque
									println!("💾 [DHT] Identité {} sauvegardée sur le disque.", kp_clone);
								}
							}
						}
					});
				},
                
            }
        }
        
        println!("🔌 [P2P] Connexion perdue avec {}.", actual_peer_id);
        active_peers.lock().unwrap().remove(&actual_peer_id);
    });
}

pub async fn broadcast_mined_block(my_port: &str, block: Block, active_peers: ActivePeers) {
    let envelope = P2PMessage::NewBlock { block, sender_port: my_port.to_string() };
    if let Ok(payload) = bincode::serialize(&envelope) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);

        let peers = active_peers.lock().unwrap().clone();
        for (_peer_id, sender) in peers.iter() {
            let _ = sender.try_send(framed.clone());
        }
    }
}

pub async fn broadcast_transaction(tx: Transaction, active_peers: ActivePeers) {
    let envelope = P2PMessage::BroadcastTransaction { tx };
    if let Ok(payload) = bincode::serialize(&envelope) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);

        let peers = active_peers.lock().unwrap().clone();
        for (_peer_id, sender) in peers.iter() {
            let _ = sender.try_send(framed.clone());
        }
    }
}

pub async fn broadcast_order(order: Order, active_peers: ActivePeers) {
    let envelope = P2PMessage::BroadcastOrder { order };
    if let Ok(payload) = bincode::serialize(&envelope) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);

        let peers = active_peers.lock().unwrap().clone();
        for (_peer_id, sender) in peers.iter() {
            let _ = sender.try_send(framed.clone());
        }
    }
}

pub async fn broadcast_micro_block(micro_block: crate::block::MicroBlock, active_peers: ActivePeers) {
    let envelope = P2PMessage::BroadcastMicroBlock { micro_block };
    if let Ok(payload) = bincode::serialize(&envelope) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);

        let peers = active_peers.lock().unwrap().clone();
        for (_peer_id, sender) in peers.iter() {
            let _ = sender.try_send(framed.clone());
        }
    }
}

pub fn setup_upnp(port: u16) {
    std::thread::spawn(move || {
        println!("🔌 [UPnP] Tentative de communication avec la box internet...");
        match igd::search_gateway(Default::default()) {
            Ok(gateway) => {
                // Astuce pour trouver notre propre IP locale sur le réseau
                let local_addr = match std::net::UdpSocket::bind("0.0.0.0:0") {
                    Ok(s) => {
                        if s.connect("8.8.8.8:53").is_ok() {
                            s.local_addr().ok().map(|a| a.ip())
                        } else { None }
                    },
                    Err(_) => None,
                };

                if let Some(std::net::IpAddr::V4(ipv4)) = local_addr {
                    let local_socket = std::net::SocketAddrV4::new(ipv4, port);
                    match gateway.add_port(igd::PortMappingProtocol::TCP, port, local_socket, 0, "Wattcoin Node") {
                        Ok(_) => println!("✅ [UPnP] Port TCP/{} ouvert automatiquement sur la box ! Votre nœud est joignable de l'extérieur.", port),
                        Err(e) => println!("⚠️ [UPnP] La box a refusé d'ouvrir le port (Erreur: {:?}). Les autres mineurs ne pourront pas initier la connexion vers vous.", e),
                    }
                }
            },
            Err(e) => println!("⚠️ [UPnP] Routeur introuvable ou UPnP désactivé ({:?}).", e),
        }
    });
}