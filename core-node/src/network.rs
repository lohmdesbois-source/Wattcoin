use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use serde::{Serialize, Deserialize};
use rand::Rng;
use crate::block::Block;
use crate::blockchain::Blockchain;
use crate::transaction::{Transaction, TransactionType};
use crate::api::{Order, SharedPool};
use crate::mixnet::OnionPacket;
// ===================================================================
// ANNUAIRE WNS CENTRALISÉ (Partagé entre le Nœud L1 et le Wallet)
// ===================================================================
use once_cell::sync::Lazy;
use tokio::sync::Mutex as AsyncMutex;

pub const WNS_RESOLVERS: &[&str] = &[
    "http://127.0.0.1:8200", // En local on tape direct sur le port 8200 !
    // "http://80.78.26.243/wns", // Pour la PROD plus tard
];

pub const NETWORK_SEEDS: &[&str] = &[
    "seed.watt", // Le nom de domaine fondateur par défaut
];

pub static WNS_CACHE: Lazy<AsyncMutex<HashMap<String, (String, String)>>> = Lazy::new(|| AsyncMutex::new(HashMap::new()));

#[derive(serde::Deserialize)]
pub struct WnsDirectory {
    pub domains: HashMap<String, String>,
    pub owners: HashMap<String, String>,
}

pub async fn sync_wns_directory(is_local_dev: bool) {
    let resolver = if is_local_dev {
        "http://127.0.0.1:8200"
    } else {
        "http://80.78.26.243/wns"
    };

    let url = format!("{}/directory", resolver);
    let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build().unwrap();
    
    if let Ok(res) = client.get(&url).send().await {
        if let Ok(directory) = res.json::<WnsDirectory>().await {
            let mut cache = WNS_CACHE.lock().await;
            cache.clear();
            for (domain, record) in directory.domains {
                if let Some(owner) = directory.owners.get(&domain) {
                    cache.insert(domain, (record, owner.clone()));
                }
            }
            println!("📖 [WNS] Annuaire téléchargé ({} domaines) depuis {}", cache.len(), resolver);
        }
    }
}



pub type ActivePeers = Arc<Mutex<HashMap<String, mpsc::Sender<String>>>>;
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
	// POUR L'ANNUAIRE !
    SharePeers { peers: Vec<String> },
}

async fn read_p2p_message<R: AsyncBufReadExt + std::marker::Unpin>(reader: &mut R) -> Option<P2PMessage> {
    let mut line = String::new();
    
    // Limite stricte à 3 Mo (3 * 1024 * 1024 octets)
    const MAX_MESSAGE_SIZE: u64 = 3_145_728; 
    
    // On enveloppe le lecteur pour qu'il refuse de lire au-delà de la limite
    let mut limited_reader = reader.take(MAX_MESSAGE_SIZE);
    
    match limited_reader.read_line(&mut line).await {
        Ok(0) => None, // Déconnexion propre ou fin de flux
        Ok(n) => {
            // Si on a atteint la limite stricte sans trouver de saut de ligne final, c'est une attaque
            if n as u64 == MAX_MESSAGE_SIZE && !line.ends_with('\n') {
                println!("🚨 [SÉCURITÉ] Flux TCP ignoré : Message P2P trop volumineux (Attaque OOM bloquée).");
                return None; 
            }
            
            serde_json::from_str::<P2PMessage>(line.trim()).ok()
        },
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
		
		// PEX : On donne notre carnet d'adresses au nouveau venu
        let my_known_peers: Vec<String> = {
            let kp = known_peers.lock().unwrap();
            kp.iter().cloned().collect()
        };
        if !my_known_peers.is_empty() {
            send_message_to_channel(&tx, P2PMessage::SharePeers { peers: my_known_peers }).await;
        }

        // La boucle d'écoute existante...
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
                                    found = true;
                                    break;
                                }
                            }
                            if found { break; }
                        }
                        
                        // On envoie uniquement les blocs APRÈS l'ancêtre commun
                        if (found_idx as u64) < chain.current_height {
                            let mut blocks = Vec::new();
                            for i in (found_idx as u64 + 1)..=chain.current_height {
                                blocks.push(chain.get_block_by_height(i).unwrap());
                            }
                            Some(blocks)
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
					
					let incoming_last = blocks.last().unwrap();
					let mut chain = blockchain.lock().unwrap(); 
					let current_height = chain.current_height + 1;

					// ====================================================================
					// BOUCLIER ANTI-SPAM & ANTI-FAUX POSITIF MESS
					// Si on a déjà dépassé cet index ET que le hash correspond à ce qu'on a,
					// c'est un lot en double. On le détruit silencieusement.
					// ====================================================================
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

						let mut mp = mempool.lock().unwrap();
						mp.retain(|tx| { 
							let not_in_block = !blocks.iter().any(|b| b.transactions.iter().any(|mined_tx| mined_tx.public_key == tx.public_key));
							let is_valid_share = match &tx.tx_type {
								TransactionType::MiningShare { timestamp, .. } => *timestamp >= cutoff_time,
								_ => true
							};
							not_in_block && is_valid_share
						});

						// On relaie la bonne nouvelle au reste du réseau !
						if let Some(last_block) = blocks.last() {
							
							// --- NOUVEL AFFICHAGE VISUEL POUR LE RELAIS ---
							let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
							let tx_count = last_block.transactions.len();
							let tx_detail = if tx_count == 1 { 
								"1 Coinbase".to_string() 
							} else { 
								format!("1 Coinbase + {} Publique/Swap", tx_count - 1) 
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
							let mut json_str = serde_json::to_string(&env).unwrap();
							json_str.push('\n');
							
							let ap = active_peers.lock().unwrap().clone();
							for (peer_id, sender) in ap.iter() {
								// On ne renvoie pas au pair qui vient de nous synchroniser
								if peer_id != &actual_peer_id {
									let _ = sender.try_send(json_str.clone());
								}
							}
						}

					} else {
						println!("❌ [SYNC] Échec de la fusion !");
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
                                let mut json_str = serde_json::to_string(&env).unwrap();
                                json_str.push('\n');
                                
                                let ap = active_peers_clone.lock().unwrap().clone();
                                for (peer_id, sender) in ap.iter() {
                                    if peer_id != &actual_peer_id_clone {
                                        let _ = sender.try_send(json_str.clone());
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
												// Séparation claire : icône de part de minage
												println!("⛏️ [P2POOL] Nouvelle part de minage relayée !");
												let tx_to_propagate = in_tx.clone();
												pool.push(in_tx);

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
						
						continue;
					}

					// 3. TRAITEMENT CLASSIQUE POUR LES AUTRES TRANSACTIONS (HTLC, Envoi Classique...)
					if in_tx.is_valid() {
						let mut pool = mempool.lock().unwrap(); // Ou mp_clone dans la boucle Tor
						if !pool.iter().any(|t| t.hash_data() == in_tx.hash_data()) {
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
                                // NETTOYAGE DU MEMPOOL
                                {
                                    let mut mp = mp_clone.lock().unwrap();
                                    mp.retain(|tx| !micro_block.transactions.iter().any(|m_tx| m_tx.hash_data() == tx.hash_data()));
                                }
                                
                                // GOSSIP P2P : On relaie aux autres !
                                let envelope = P2PMessage::BroadcastMicroBlock { micro_block: micro_block.clone() };
                                let mut json_str = serde_json::to_string(&envelope).unwrap();
                                json_str.push('\n');
                                let ap = ap_clone.lock().unwrap().clone();
                                for (peer_id, sender) in ap.iter() {
                                    if peer_id != &actual_peer_id_clone { 
                                        let _ = sender.try_send(json_str.clone()); 
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
				
				P2PMessage::SharePeers { peers } => {
                    let mut kp = known_peers.lock().unwrap();
                    let mut new_found = 0;
                    for peer in peers {
                        // On n'ajoute pas soi-même ni des IP mortes
                        if !kp.contains(&peer) && peer.contains(':') {
                            kp.insert(peer);
                            new_found += 1;
                        }
                    }
                    if new_found > 0 {
                        println!("🕸️ [PEX] Annuaire mis à jour : {} nouveaux pairs découverts !", new_found);
                        // On sauvegarde sur le disque immédiatement !
                        let db_dir = format!("{}/.wattcoin", std::env::var("HOME").unwrap_or_else(|_| ".".to_string()));
                        let peers_file = format!("{}/known_peers.json", db_dir);
                        let peers_list: Vec<String> = kp.iter().cloned().collect();
                        let _ = std::fs::write(&peers_file, serde_json::to_string(&peers_list).unwrap_or_default());
                    }
                },
                
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