#![recursion_limit = "1024"]

// On n'a plus besoin de déclarer les modules ici, ils sont dans lib.rs
use wattcoin_core::blockchain::{Blockchain, EPOCH_BLOCKS};
use wattcoin_core::transaction::{Transaction, TransactionType};
use wattcoin_core::api::SharedPool;

use std::env;
use std::sync::{Arc, Mutex};
use std::collections::{HashSet, HashMap}; 
use randomx_rs::{RandomXFlag, RandomXCache, RandomXDataset, RandomXVM};


pub type SharedMempool = Arc<Mutex<Vec<Transaction>>>;

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
    
    let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let db_dir = format!("{}/.wattcoin", home_dir);
    if let Err(e) = std::fs::create_dir_all(&db_dir) {
        println!("⚠️ Impossible de créer le dossier .wattcoin : {}", e);
    }

    let role_prefix = if is_relay_mode { "relay" } else { "miner" };
    let l1_db_file = format!("{}/{}_l1_chain_{}.json", db_dir, role_prefix, port);
    let l2_db_file = format!("{}/{}_l2_chain_{}.json", db_dir, role_prefix, port);

    // On utilise maintenant l1_db_file pour charger la chaîne
    let shared_chain = Arc::new(Mutex::new(Blockchain::load_from_disk(&l1_db_file).unwrap_or_else(|_| Blockchain::new())));
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
        //println!("⏳ [MAINNET STARTING BLOCK] Le réseau principal n'a pas encore démarré !");
		println!("⏳ [TESTNET STARTING BLOCK] Le réseau principal n'a pas encore démarré !");
        println!("⏳ Le nœud est en mode veille. Lancement automatique dans {} secondes...", wait_seconds);
        println!("⏳ Laissez ce terminal ouvert. Les moteurs s'allumeront à l'heure H.\n");
        
        // 💡 Le nœud s'endort ici et se réveillera exactement à l'heure du Genesis !
        tokio::time::sleep(tokio::time::Duration::from_secs(wait_seconds as u64)).await;
        
        //println!("🚀 [MAINNET LIVE] C'EST PARTI ! Allumage des moteurs Cypherpunk !");
		println!("🚀 [TESTNET LIVE] C'EST PARTI ! Allumage des moteurs Cypherpunk !");
    }
    // ====================================================================

    // 💡 L'initialisation se fera juste avant le minage
    
    let known_peers: wattcoin_core::SharedPeers = Arc::new(Mutex::new(HashSet::new()));
    if let Some(target) = &peer_target { known_peers.lock().unwrap().insert(target.clone()); }
    let active_peers: wattcoin_core::network::ActivePeers = Arc::new(Mutex::new(HashMap::new()));

    let p2p_chain = Arc::clone(&shared_chain);
    let p2p_mempool = Arc::clone(&mempool);
    let p2p_dex_pool = Arc::clone(&dex_pool);
    let p2p_peers = Arc::clone(&known_peers); 
    let p2p_active = Arc::clone(&active_peers);
    let port_clone = port.clone();
    let bind_ip_p2p = p2p_bind_ip.to_string(); 
	let p2p_l2_db = l2_db_file.clone(); 
    
    // 📡 LE SERVEUR P2P 
    tokio::spawn(async move {
        wattcoin_core::network::start_p2p_server(
            &bind_ip_p2p, &port_clone, p2p_chain, p2p_mempool, p2p_dex_pool, p2p_peers, p2p_active, p2p_l2_db
        ).await;
    });
	
    let api_chain = Arc::clone(&shared_chain);
    let api_mempool = Arc::clone(&mempool);
    let api_peers = Arc::clone(&known_peers); 
    let api_dex_pool = Arc::clone(&dex_pool);
    let api_active_peers = Arc::clone(&active_peers);
    let api_l2_db = l2_db_file.clone(); 
	
    tokio::spawn(async move { 
        wattcoin_core::api::start_api_server(
            api_port, api_bind_ip, api_mempool, api_chain, api_peers, api_dex_pool, api_active_peers, api_l2_db
        ).await; 
    });
	
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
            if wattcoin_core::LOCAL_DEV_MODE {
                println!("🔓 [LOCAL MODE] Connexion TCP directe vers {} (sans Tor)", target_clone);
                if let Ok(socket) = tokio::net::TcpStream::connect(&target_clone).await {
                    let p2p_l2_db_hs = l2_db_file.clone(); 
                    wattcoin_core::network::start_peer_connection(
                        socket, 
                        target_clone.split(':').next().unwrap_or("127.0.0.1").to_string(), 
                        my_port, 
                        p2p_chain_handshake, 
                        p2p_mempool_hs, 
                        p2p_dex_hs, 
                        p2p_peers_hs, 
                        p2p_active_hs,
                        p2p_l2_db_hs 
                    );
                }
            } else {
                // MODE PROD TOR : code original intact
                let p2p_l2_db_tor = l2_db_file.clone(); 
                wattcoin_core::network::connect_to_network(&target_clone, &my_port, &p2p_l2_db_tor, p2p_chain_handshake, p2p_mempool_hs, p2p_dex_hs, p2p_peers_hs, p2p_active_hs).await;
            }
        });
    }

    if is_relay_mode {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            let chain = shared_chain.lock().expect("Mutex empoisonné (panic précédent)");
            chain.save_to_disk(&l1_db_file);
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

						// 💡 VRAI ATOMIC SWAP : On utilise le hash cryptographique scellé par l'acheteur
						let real_htlc_hash = buy.htlc_hash.clone().unwrap_or_else(|| "ERREUR_HASH_MANQUANT".to_string());

						generated_swaps.push(wattcoin_core::transaction::SwapContract {
							buyer_watt_address: buy.watt_address.clone(),      // ← Alice (le claimer WATT)
							buyer_btc_address: buy.btc_address.clone(),
							buyer_btc_pubkey: buy.btc_pubkey.clone(),
							seller_watt_address: sell.watt_address.clone(),    // ← Bob (le locker WATT)
							seller_btc_address: sell.btc_address.clone(),      // ← Bob (où il recevra les BTC au claim)
							seller_btc_pubkey: sell.btc_pubkey.clone(),
							watt_amount_flames: matched_volume,
							btc_amount_sats: (matched_volume as f64 / 1_000_000_000.0 * clearing_price_sats as f64) as u64,
							htlc_hash: real_htlc_hash, // 🔥 SCELLÉ DIRECTEMENT ON-CHAIN !
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
						public_key: "DEX_SETTLEMENT_ON_CHAIN".to_string(), 
						wots_signature: None,
					});
				}
			}

            let (mut candidate_block, target, l2_keys) = {
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
            let share_target = &target * 20u32; // 💡 20x plus facile pour les petits PC !
            let mut last_share_time = 0;
            
            loop {
                if candidate_block.header.nonce % 2000 == 0 {
                    let chain = shared_chain.lock().unwrap();
                    if chain.chain.len() as u64 > candidate_block.header.index {
                        println!("🛑 [ALERTE] Le réseau a trouvé le Bloc {} avant nous ! Annulation du minage.", candidate_block.header.index);
                        break; 
                    }
                    tokio::task::yield_now().await;
                }

                // 🛡️ L2 ANCHORING : Le l2_root est scellé par la Preuve de Travail (PoW) !
                let header_data = format!("{}{}{}{}{}", 
                    candidate_block.header.index, 
                    candidate_block.header.timestamp, 
                    candidate_block.header.previous_hash, 
                    candidate_block.header.nonce,
                    candidate_block.header.l2_root
                );

                let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
                candidate_block.header.hash = hex::encode(&hash_bytes);
                let hash_value = num_bigint::BigUint::from_bytes_be(&hash_bytes);

                if hash_value <= target {
                    mined = true;
                    break;
                } 
                // ⚡ P2POOL NATIF : Si on est proche du but (Share)
                else if hash_value <= share_target {
                    let now = chrono::Utc::now().timestamp();
                    // Limite anti-spam : 1 share max toutes les 5 secondes
                    if now - last_share_time > 5 {
                        last_share_time = now;
                        println!("🤝 [P2POOL] Part de minage trouvée ! Partage avec le réseau...");
                        let share_tx = Transaction {
							tx_type: TransactionType::MiningShare { 
								miner_address: miner_address.clone(), 
								nonce: candidate_block.header.nonce, 
								hash: candidate_block.header.hash.clone(), 
								timestamp: candidate_block.header.timestamp 
							},
							inputs: vec![], outputs: vec![], fee: 0,
							// Une clé unique pour ne pas supprimer les autres parts du Mempool !
							public_key: format!("SHARE_{}", candidate_block.header.nonce), 
							wots_signature: None,
						};
                        let mut pool = mempool.lock().unwrap();
                        pool.push(share_tx.clone());
                        let tx_clone = share_tx.clone();
                        let peers_clone = Arc::clone(&active_peers);
                        tokio::spawn(async move { wattcoin_core::network::broadcast_transaction(tx_clone, peers_clone).await; });
                    }
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
					
					let l1_lottery_tax = total_fees / 100;
					let l1_miner_fees = total_fees - l1_lottery_tax;

                    println!("\n====================================================================");
                    println!("🎉 NOUVEAU BLOC FORGÉ PAR LE MINEUR !");
                    println!("====================================================================");
                    println!("📦 Index du Bloc : {}", candidate_block.header.index);
                    println!("🔗 Hash          : {}", candidate_block.header.hash);
                    println!("🕒 Date et Heure : {}", date_str);
                    println!("📝 Transactions  : {} incluses (1 Coinbase + {} Publique/Swap/Lottery)", nb_tx, nb_tx - 1);
                    println!("💰 Frais perçus  : {} Flames", l1_miner_fees);
                    println!("====================================================================\n");
                    
                    for tx in &candidate_block.transactions {
                        if tx.tx_type != TransactionType::Coinbase {
                            for input in &tx.inputs {
                                chain.spent_key_images.insert(input.mpc_ring.key_image.clone());
                            }
                        }
                    }

                    chain.chain.push(candidate_block.clone()); 
                    chain.update_target(); 
                    chain.save_to_disk(&l1_db_file);
					
					// ====================================================================
                    // ⚡ L'ÉVEIL DU SÉQUENCEUR L2 (PRÉ-CONFIRMATION OFFICIELLE) ⚡
                    // ====================================================================
                    let l1_parent_hash = candidate_block.header.hash.clone();
                    let sequencer_keys = l2_keys.clone();
                    let mempool_seq = Arc::clone(&mempool);
                    let active_peers_seq = Arc::clone(&active_peers);
                    
                    let l2_pubkeys: Vec<String> = sequencer_keys.iter().map(|k| k.public_key.clone()).collect();

                    tokio::spawn(async move {
                        println!("\n⚡ [L2 SEQUENCER] Couronnement réussi ! Je suis le Séquenceur L2 pour les 2 prochaines minutes.");
                        use sha2::Digest; 
                        let mut already_sequenced = std::collections::HashSet::new(); // Mémoire du séquenceur

                        for i in 0..128 {
                            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                            
                            let mut txs_to_sequence = Vec::new();
                            {
                                let mp = mempool_seq.lock().unwrap();
                                for tx in mp.iter() {
                                    // ⚡ Le Séquenceur ne s'occupe QUE des transactions "Pures L2" !
                                    // 💡 FIX : On s'assure qu'il y a des outputs, pour ne pas avaler les MiningShare !
                                    let is_pure_l2 = !tx.outputs.is_empty() && tx.outputs.iter().all(|out| out.stealth_address.starts_with("L2_WATT_"));
                                    
                                    if is_pure_l2 && !already_sequenced.contains(&tx.public_key) {
                                        txs_to_sequence.push(tx.clone());
                                        already_sequenced.insert(tx.public_key.clone());
                                    }
                                }
                            }

                            if txs_to_sequence.is_empty() { continue; }
							
							let true_tx_count = txs_to_sequence.len(); // 💡 On capture la vraie taille ici !
							
                            // ⚡ CRÉATION DE LA MICRO-COINBASE (100 Flames par TX L2)
                            let expected_fees = txs_to_sequence.len() as u64 * 100;
                            let keypair = &sequencer_keys[i];

                            let micro_coinbase = wattcoin_core::transaction::Transaction {
                                tx_type: wattcoin_core::transaction::TransactionType::MicroCoinbase,
                                inputs: vec![],
                                outputs: vec![wattcoin_core::transaction::TransactionOutput {
                                    stealth_address: format!("L2_WATT_{}", keypair.public_key), // Va vers l'adresse L2 du séquenceur
                                    kyber_capsule: format!("MICRO_COINBASE_{}_{}", l1_parent_hash, i),
                                    aes_vault: expected_fees.to_string(),
                                    lattice_commitment: wattcoin_core::lattice::LWECommitment::commit(expected_fees, [0, 0, 0, 0]),
                                }],
                                fee: 0,
                                public_key: "MICRO_COINBASE".to_string(),
                                wots_signature: None,
                            };

                            // On l'insère en tête de bloc
                            txs_to_sequence.insert(0, micro_coinbase);

                            println!("⚡ [L2 SEQUENCER] Signature du MicroBloc {}/128 ({} TXs, Frais encaissés: {} Flames)", 
                                      i, txs_to_sequence.len() - 1, expected_fees);

                            let keypair = &sequencer_keys[i];
                            
                            let mut micro_block = wattcoin_core::block::MicroBlock {
                                l1_parent_hash: l1_parent_hash.clone(),
                                micro_index: i as u64,
                                timestamp: chrono::Utc::now().timestamp(),
                                transactions: txs_to_sequence,
                                sequencer_pubkey: keypair.public_key.clone(),
                                sequencer_reward_address: "FEE_GOES_TO_NEXT_L1_MINER".to_string(), // Inutile maintenant, mais on garde la structure
                                sequencer_sig: wattcoin_core::wots::WotsSignature { chains: vec![] }, 
                                merkle_proof: l2_pubkeys.clone(), 
                            };

                            let mb_data = format!("{}{}{}", micro_block.l1_parent_hash, micro_block.micro_index, micro_block.timestamp);
                            let mut hasher = sha2::Sha512::new();
                            hasher.update(mb_data.as_bytes());
                            let mut hash_arr = [0u8; 64];
                            hash_arr.copy_from_slice(&hasher.finalize());

                            micro_block.sequencer_sig = wattcoin_core::wots::WotsKeyPair::sign(&keypair.secret_key, &hash_arr);

                            // 📡 LE SÉQUENCEUR CRIE SUR LE RÉSEAU P2P !
                            wattcoin_core::network::broadcast_micro_block(micro_block.clone(), Arc::clone(&active_peers_seq)).await;
                            
                            // 💡 NOUVEAU LOG : L'encadré propre du MicroBloc !
                            println!("\n====================================================================");
                            println!("⚡ NOUVEAU MICRO-BLOC L2 SÉQUENCÉ !");
                            println!("====================================================================");
                            println!("📦 Micro-Index   : {}/128", micro_block.micro_index);
                            println!("🔗 Parent L1     : {}", micro_block.l1_parent_hash);
                            println!("🕒 Date et Heure : {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
                            println!("📝 Transactions  : {} incluses (Instantanées)", true_tx_count);
                            println!("💰 Frais perçus  : {} Flames", expected_fees);
                            println!("====================================================================\n");
                        }
                        println!("⚡ [L2 SEQUENCER] Mon règne est terminé. J'attends le prochain bloc L1...");
                    });
                    // ====================================================================

                    let block_clone = candidate_block.clone();
                    let my_port_clone = port.clone(); 
                    let active_clone = Arc::clone(&active_peers);
                    
                    tokio::spawn(async move {
                        wattcoin_core::network::broadcast_mined_block(&my_port_clone, block_clone, active_clone).await;
                    });
                }
                
                
                let mut mp = mempool.lock().unwrap();
                mp.retain(|tx| {
                    !candidate_block.transactions.iter().any(|mined_tx| {
                        // Nettoyage infaillible par ID de transaction
                        mined_tx.public_key == tx.public_key
                    })
                });
            }
        }
    }
}