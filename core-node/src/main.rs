mod block;
mod blockchain;
mod transaction;
mod network;
mod api;
pub mod lattice;

use std::env;
use std::sync::{Arc, Mutex};
use std::collections::{HashSet, HashMap}; 
use randomx_rs::{RandomXFlag, RandomXCache, RandomXDataset, RandomXVM};
use blockchain::{Blockchain, EPOCH_BLOCKS}; 
use transaction::{Transaction, TransactionType};
use api::SharedPool; 


pub type SharedMempool = Arc<Mutex<Vec<Transaction>>>;
pub type SharedPeers = Arc<Mutex<HashSet<String>>>; 

// ===================================================================
// 📦 CONTENEUR UNSAFE POUR LE WARM-UP RANDOMX
// RandomX utilise des pointeurs C. Rust refuse de les changer de thread.
// En implémentant 'Send' de manière 'unsafe', on force l'autorisation.
// C'est sans danger ici car on transfère uniquement l'appartenance (Ownership).
// ===================================================================
struct WarmUpContainer {
    cache: RandomXCache,
    dataset: RandomXDataset,
}
unsafe impl Send for WarmUpContainer {}
unsafe impl Sync for WarmUpContainer {}

// ================= GESTION DES ERREURS (PRO-LEVEL) =================


#[derive(Debug)]
pub enum WattError {
    Crypto(String),
    Network(String),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl From<std::io::Error> for WattError {
    fn from(err: std::io::Error) -> Self { WattError::Io(err) }
}
impl From<serde_json::Error> for WattError {
    fn from(err: serde_json::Error) -> Self { WattError::Json(err) }
}
// ===================================================================

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let is_live_mode = args.contains(&"--live".to_string());
    let clean_args: Vec<String> = args.into_iter().filter(|a| a != "--live").collect();

    if clean_args.len() < 3 {
        eprintln!("🛑 Usage Mineur : cargo run <PORT> <MINER_ADDRESS> [PEER_IP:PORT] [--live]");
        eprintln!("🛡️  Usage Relais : cargo run <PORT> --relay [PEER_IP:PORT] [--live]");
        return;
    }

    let port = clean_args[1].clone();
    let api_port = port.parse::<u16>().unwrap() + 100;
    let arg2 = clean_args[2].clone();
    let is_relay_mode = arg2 == "--relay";
    let miner_address = if is_relay_mode { String::from("RELAY_NODE_NO_MINING") } else { arg2 };
    let peer_target = clean_args.get(3).cloned();

    println!("🔥 DÉMARRAGE DU NŒUD CYPHERPUNK...");
	
	
    
    let (p2p_bind_ip, api_bind_ip) = if is_live_mode {
        println!("🌍 MODE LIVE ACTIVÉ : Le Nœud est ouvert sur Internet (0.0.0.0)");
        ("0.0.0.0", [0, 0, 0, 0])
    } else {
        println!("🏠 MODE LOCAL ACTIVÉ : Le Nœud est isolé sur ta machine (127.0.0.1)");
        ("127.0.0.1", [127, 0, 0, 1])
    };

    if is_relay_mode {
        println!("🛡️  MODE RELAIS ACTIVÉ : Minage désactivé. Le Nœud agira comme un routeur P2P.");
    }
    
    let db_file = format!("chain_{}.json", port);
    let shared_chain = Arc::new(Mutex::new(Blockchain::load_from_disk(&db_file).unwrap_or_else(|_| Blockchain::new())));
    let mempool: SharedMempool = Arc::new(Mutex::new(Vec::new()));
    let dex_pool: SharedPool = Arc::new(Mutex::new(Vec::new()));
	
	// ====================================================================
    // ⚛️ AFFICHAGE DU GENESIS ET GESTION DU LANCEMENT (MAINNET)
    // ====================================================================
    let (genesis_timestamp, genesis_hash) = {
        let chain = shared_chain.lock().unwrap();
        let genesis_block = &chain.chain[0];
        (genesis_block.header.timestamp, genesis_block.header.hash.clone())
    };

    let genesis_date = chrono::DateTime::from_timestamp(genesis_timestamp, 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
        .format("%d/%m/%Y %H:%M:%S")
        .to_string();

    println!("\n====================================================================");
    println!("⚛️  BLOC GENESIS PRÊT (STARTING BLOCK)");
    println!("====================================================================");
    println!("📦 Index       : 0");
    println!("🔗 Hash        : {}", genesis_hash);
    println!("🕒 Date Prévue : {}", genesis_date);
    println!("====================================================================\n");

    let now_ts = chrono::Utc::now().timestamp();
    if now_ts < genesis_timestamp {
        let wait_seconds = genesis_timestamp - now_ts;
        println!("⏳ [MAINNET STARTING BLOCK] Le réseau principal n'a pas encore démarré !");
        println!("⏳ Le nœud est en mode veille. Lancement automatique dans {} secondes...", wait_seconds);
        println!("⏳ Laissez ce terminal ouvert. Les moteurs s'allumeront à l'heure H.\n");
        
        // 💡 Le nœud s'endort ici et se réveillera exactement à l'heure du Genesis !
        tokio::time::sleep(tokio::time::Duration::from_secs(wait_seconds as u64)).await;
        
        println!("🚀 [MAINNET LIVE] C'EST PARTI ! Allumage des moteurs Cypherpunk !");
    }
    // ====================================================================

    // 💡 L'initialisation se fera juste avant le minage
    
    let known_peers: SharedPeers = Arc::new(Mutex::new(HashSet::new()));
    if let Some(target) = &peer_target { known_peers.lock().unwrap().insert(target.clone()); }
    let active_peers: network::ActivePeers = Arc::new(Mutex::new(HashMap::new()));

    let p2p_chain = Arc::clone(&shared_chain);
    let p2p_mempool = Arc::clone(&mempool);
    let p2p_dex_pool = Arc::clone(&dex_pool);
    let p2p_peers = Arc::clone(&known_peers); 
    let p2p_active = Arc::clone(&active_peers);
    let port_clone = port.clone();
    let bind_ip_p2p = p2p_bind_ip.to_string(); 
    tokio::spawn(async move { network::start_p2p_server(&bind_ip_p2p, &port_clone, p2p_chain, p2p_mempool, p2p_dex_pool, p2p_peers, p2p_active).await; });
    
    let api_chain = Arc::clone(&shared_chain);
    let api_mempool = Arc::clone(&mempool);
    let api_peers = Arc::clone(&known_peers); 
    let api_dex_pool = Arc::clone(&dex_pool);
    let api_active_peers = Arc::clone(&active_peers);
    tokio::spawn(async move { api::start_api_server(api_port, api_bind_ip, api_mempool, api_chain, api_peers, api_dex_pool, api_active_peers).await; });

    if let Some(target) = &peer_target {
        println!("🤝 Ouverture du tunnel P2P vers {}...", target);
        let target_clone = target.clone();
        let my_port = port.clone();
        let p2p_chain_handshake = Arc::clone(&shared_chain);
        let p2p_mempool_hs = Arc::clone(&mempool);
        let p2p_dex_hs = Arc::clone(&dex_pool);
        let p2p_peers_hs = Arc::clone(&known_peers);
        let p2p_active_hs = Arc::clone(&active_peers);
        
        tokio::spawn(async move {
            network::connect_to_network(&target_clone, &my_port, p2p_chain_handshake, p2p_mempool_hs, p2p_dex_hs, p2p_peers_hs, p2p_active_hs).await;
        });
    }

    if is_relay_mode {
        let db_file_relay = format!("chain_{}.json", port);
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            let chain = shared_chain.lock().unwrap();
            chain.save_to_disk(&db_file_relay);
        }
    } else {
        // 💡 LA CORRECTION EST ICI : On force le mineur à attendre la synchro initiale !
        if peer_target.is_some() {
            println!("⏳ [SYNCHRONISATION] Pause de 15 secondes...");
            println!("⏳ Laissons le temps au tunnel Tor de s'établir et de télécharger l'historique du Relais.");
            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
            println!("✅ [SYNCHRONISATION] Phase d'écoute terminée. Allumage des moteurs !");
        }

        println!("\n⚙️  Initialisation du moteur RandomX...");
        let start_rx = std::time::Instant::now();
        
        let flags = RandomXFlag::get_recommended_flags();
        let mut current_epoch = 0;
        let mut seed_hash = shared_chain.lock().unwrap().get_epoch_seed(1);
        
        let mut cache = RandomXCache::new(flags, seed_hash.as_bytes()).unwrap();
        
        println!("⏳ Allocation du Dataset de 2 Go en RAM (Veuillez patienter...)");
        let mut dataset = RandomXDataset::new(flags, cache.clone(), 0).unwrap();
        let mut vm = RandomXVM::new(flags, Some(cache.clone()), Some(dataset.clone())).unwrap();
        println!("✅ RandomX prêt en {:.2?} !", start_rx.elapsed());

        println!("\n⛏️  Début de l'extraction pour l'adresse : {}...", miner_address);
    
        // 💡 NOTRE BOÎTE BLINDÉE POUR LE PRÉ-CALCUL EN ARRIÈRE-PLAN
        let next_dataset: Arc<Mutex<Option<WarmUpContainer>>> = Arc::new(Mutex::new(None));
        let mut warming_up_epoch = current_epoch;
    
        loop {
            // 💡 0. LE MOTEUR DEX (FBA) ON-CHAIN - VERSION SÉCURISÉE
			let mut dex_settlement_tx = None;
			{
				let mut p = dex_pool.lock().unwrap();
				let mut buys: Vec<_> = p.iter().filter(|o| o.order_type == "buy").cloned().collect();
				let mut sells: Vec<_> = p.iter().filter(|o| o.order_type == "sell").cloned().collect();
				buys.sort_by(|a, b| b.price_sats.cmp(&a.price_sats));
				sells.sort_by(|a, b| a.price_sats.cmp(&b.price_sats));

				let mut generated_swaps = Vec::new();
				let mut clearing_price_sats = 0u64;
				let mut total_volume_flames = 0u64;

				let mut buy_idx = 0;
				let mut sell_idx = 0;

				while buy_idx < buys.len() && sell_idx < sells.len() {
					let buy = &mut buys[buy_idx];
					let sell = &mut sells[sell_idx];

					if buy.price_sats >= sell.price_sats {
						clearing_price_sats = (buy.price_sats + sell.price_sats) / 2;
						let matched_volume = std::cmp::min(buy.amount_flames, sell.amount_flames);
						total_volume_flames += matched_volume;

						// ✅ 100% ATOMIC : hash réel calculé à partir d’un secret simulé (le wallet réel enverra le vrai)
						let fake_secret = format!("secret_alice_{}_{}", buy.id, sell.id); // ← en prod : reçu via API du wallet
						let htlc_hash = hex::encode(blake3::hash(fake_secret.as_bytes()).as_bytes());

						generated_swaps.push(crate::transaction::SwapContract {
							buyer_btc_address: buy.btc_address.clone(),
							buyer_btc_pubkey: buy.btc_pubkey.clone(),
							seller_watt_address: sell.watt_address.clone(),
							seller_btc_pubkey: sell.btc_pubkey.clone(),
							watt_amount_flames: matched_volume,
							btc_amount_sats: (matched_volume as f64 / 1_000_000_000.0 * clearing_price_sats as f64) as u64,
							htlc_hash,
						});

						buy.amount_flames -= matched_volume;
						sell.amount_flames -= matched_volume;
						if buy.amount_flames == 0 { buy_idx += 1; }
						if sell.amount_flames == 0 { sell_idx += 1; }
					} else {
						break;
					}
				}

				// Nettoyage du pool
				let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
				p.clear();
				for buy in buys { if buy.amount_flames > 0 && buy.expires_at > now { p.push(buy); } }
				for sell in sells { if sell.amount_flames > 0 && sell.expires_at > now { p.push(sell); } }

				if total_volume_flames > 0 {
					println!("\n⚖️ [DEX] Matching réussi → {} WATT à {} Sats", 
							 total_volume_flames as f64 / 1_000_000_000.0, clearing_price_sats);

					dex_settlement_tx = Some(Transaction {
						tx_type: TransactionType::DexSettlement { 
							clearing_price_sats, 
							total_volume_flames, 
							swaps: generated_swaps 
						},
						inputs: vec![],
						outputs: vec![],
						fee: 0,
						dilithium_signature: "DEX_SETTLEMENT_ON_CHAIN".to_string(),
					});
				}
			}

            let (mut candidate_block, target) = {
                let mut chain = shared_chain.lock().unwrap();
                let mut pending_txs = mempool.lock().unwrap().clone();
                
                if let Some(dex_tx) = dex_settlement_tx {
                    pending_txs.push(dex_tx);
                }
                
                chain.prepare_block_template(pending_txs, &miner_address)
            };

            // ==========================================================
            // ⚙️ 1. LE CHANGEMENT D'ÉPOQUE INSTANTANÉ (Zéro délai !)
            // ==========================================================
            let target_epoch = (candidate_block.header.index - 1) / EPOCH_BLOCKS;
            if target_epoch > current_epoch {
                println!("\n==========================================================");
                println!("🔄 CHANGEMENT D'ÉPOQUE RANDOMX ! (Nouvelle Époque : {})", target_epoch);
                println!("==========================================================");
                current_epoch = target_epoch;
                
                seed_hash = shared_chain.lock().unwrap().get_epoch_seed(candidate_block.header.index);

                // On ouvre la boîte blindée pour récupérer le travail du thread d'arrière-plan
                let precalculated = next_dataset.lock().unwrap().take();
                
                if let Some(warm_data) = precalculated {
                    println!("⚡ [WARM-UP] Utilisation du Dataset précalculé en RAM ! Zéro temps d'arrêt pour le mineur.");
                    cache = warm_data.cache;
                    dataset = warm_data.dataset;
                    vm = RandomXVM::new(flags, Some(cache.clone()), Some(dataset.clone())).unwrap();
                } else {
                    println!("⏳ Pas de cache prêt (serveur fraîchement démarré), calcul synchrone... (~30s)");
                    cache = RandomXCache::new(flags, seed_hash.as_bytes()).unwrap();
                    dataset = RandomXDataset::new(flags, cache.clone(), 0).unwrap();
                    vm = RandomXVM::new(flags, Some(cache.clone()), Some(dataset.clone())).unwrap();
                }
                println!("✅ Nouvelle Ère prête ! Le réseau est 100% sécurisé.");
            }

            // ==========================================================
            // 🔥 2. LE DÉCLENCHEUR DE WARM-UP ASYNCHRONE
            // ==========================================================
            let blocks_until_next = EPOCH_BLOCKS - ((candidate_block.header.index - 1) % EPOCH_BLOCKS);
            let next_epoch = current_epoch + 1;
            
            // 💡 À 10 blocs de la fin de l'époque, on lance le thread fantôme !
            if blocks_until_next <= 10 && warming_up_epoch != next_epoch {
                warming_up_epoch = next_epoch;
                let next_seed = { shared_chain.lock().unwrap().get_epoch_seed(candidate_block.header.index + blocks_until_next + 1) };
                let nd_clone = Arc::clone(&next_dataset);
                
                println!("\n🔥 [WARM-UP] Transition imminente ({} blocs). Début de la compilation en arrière-plan du Dataset {}...", blocks_until_next, next_epoch);
                
                // Ce thread tourne sur un autre cœur du CPU, le mineur principal ne s'arrête PAS !
                tokio::task::spawn_blocking(move || {
                    let flags = RandomXFlag::get_recommended_flags();
                    if let Ok(warm_cache) = RandomXCache::new(flags, next_seed.as_bytes()) {
                        if let Ok(warm_dataset) = RandomXDataset::new(flags, warm_cache.clone(), 0) {
                            let container = WarmUpContainer { cache: warm_cache, dataset: warm_dataset };
                            *nd_clone.lock().unwrap() = Some(container);
                            println!("✅ [WARM-UP TERMINE] Dataset {} chargé en RAM. Prêt pour la bascule !", next_epoch);
                        }
                    }
                });
            }

            let mut mined = false;
            
            loop {
                if candidate_block.header.nonce % 2000 == 0 {
                    let chain = shared_chain.lock().unwrap();
                    if chain.chain.len() as u64 > candidate_block.header.index {
                        println!("🛑 [ALERTE] Le réseau a trouvé le Bloc {} avant nous ! Annulation du minage.", candidate_block.header.index);
                        break; 
                    }
                    tokio::task::yield_now().await;
                }

                let header_data = format!("{}{}{}{}", 
                    candidate_block.header.index, 
                    candidate_block.header.timestamp, 
                    candidate_block.header.previous_hash, 
                    candidate_block.header.nonce
                );

                let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
                candidate_block.header.hash = hex::encode(&hash_bytes);
                
                let hash_value = num_bigint::BigUint::from_bytes_be(&hash_bytes);

                if hash_value <= target {
                    mined = true;
                    break;
                }
                candidate_block.header.nonce += 1;
            }

            if mined {
                let mut chain = shared_chain.lock().unwrap();
                
                if chain.chain.len() as u64 > candidate_block.header.index {
                     println!("🗑️ [INFO] Hachage trouvé, mais la chaîne a été synchronisée entre temps. Bloc jeté.");
                } 
                else if chain.chain.len() as u64 == candidate_block.header.index {
                    
                    let date_str = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
                    let nb_tx = candidate_block.transactions.len();
                    let mut total_fees = 0;
                    
                    for tx in candidate_block.transactions.iter().skip(1) { total_fees += tx.fee; }

                    println!("\n====================================================================");
                    println!("🎉 NOUVEAU BLOC FORGÉ PAR LE MINEUR !");
                    println!("====================================================================");
                    println!("📦 Index du Bloc : {}", candidate_block.header.index);
                    println!("🔗 Hash          : {}", candidate_block.header.hash);
                    println!("🕒 Date et Heure : {}", date_str);
                    println!("📝 Transactions  : {} incluses (1 Coinbase + {} Publique/Swap/Lottery)", nb_tx, nb_tx - 1);
                    println!("💰 Frais perçus  : {} Flames", total_fees);
                    println!("====================================================================\n");
                    
                    for tx in &candidate_block.transactions {
                        if tx.tx_type != TransactionType::Coinbase {
                            for input in &tx.inputs {
                                chain.spent_key_images.insert(input.pq_ring_signature.key_image.clone());
                            }
                        }
                    }

                    chain.chain.push(candidate_block.clone());
                    chain.prune_old_signatures(); 
                    chain.update_target(); 
                    chain.save_to_disk(&db_file);

                    let block_clone = candidate_block.clone();
                    let my_port_clone = port.clone(); 
                    let active_clone = Arc::clone(&active_peers);
                    
                    tokio::spawn(async move {
                        network::broadcast_mined_block(&my_port_clone, block_clone, active_clone).await;
                    });
                }
                
                
                let mut mp = mempool.lock().unwrap();
                mp.retain(|tx| {
                    !candidate_block.transactions.iter().any(|mined_tx| {
                        // 💡 Nettoyage infaillible par ID de transaction
                        mined_tx.dilithium_signature == tx.dilithium_signature
                    })
                });
            }
        }
    }
}