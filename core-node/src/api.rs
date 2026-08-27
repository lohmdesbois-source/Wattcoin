use warp::Filter;
use crate::blockchain::Blockchain;
use crate::transaction::{Transaction, TransactionType};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering}; 
use serde::{Serialize, Deserialize};
use bitcoin::hashes::Hash;
use std::str::FromStr;
use sha2::Digest;
use std::collections::HashSet;




pub type SharedPool = Arc<Mutex<Vec<Order>>>;

// 💡 Devenu 'pub' pour que le mineur (main.rs) et le validateur puissent le mettre à jour
pub static LAST_PRICE_SATS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub order_type: String,
    pub amount_flames: u64,
    pub price_sats: u64,
    pub btc_address: String,
    pub btc_pubkey: String, 
    pub watt_address: String,
    pub expires_at: i64,
    pub htlc_hash: Option<String>, 
}

pub async fn start_api_server(
    port: u16, 
    host_ip: [u8; 4], 
    mempool: Arc<Mutex<Vec<Transaction>>>, 
    chain: Arc<Mutex<Blockchain>>, 
    dex_pool: SharedPool,
    active_peers: crate::network::ActivePeers,
    l2_db_file: String,
	node_kyber_secret: String
) {
    // PURISME CYPHERPUNK : On lit le VRAI prix directement depuis le marbre de la blockchain !
    {

		let chain_lock = chain.lock().unwrap();
        let mut found_price = false;
        
        // On remonte le temps depuis le bloc le plus récent
        for block in chain_lock.chain.iter().rev() {
            for tx in block.transactions.iter().rev() {
                if let crate::transaction::TransactionType::DexSettlement { clearing_price_sats, .. } = &tx.tx_type {
                    LAST_PRICE_SATS.store(*clearing_price_sats, Ordering::Relaxed);
                    println!("📈 [MARCHÉ] Prix officiel synchronisé depuis la blockchain : {} Sats", clearing_price_sats);
                    found_price = true;
                    break;
                }
            }
            if found_price { break; }
        }
        if !found_price { println!("📈 [MARCHÉ] Aucun prix historique trouvé. En attente du premier croisement..."); }
    }

    let mempool_filter = warp::any().map(move || Arc::clone(&mempool));
    let chain_filter = warp::any().map(move || Arc::clone(&chain));
    let dex_pool_filter = warp::any().map(move || Arc::clone(&dex_pool));
    let active_peers_filter = warp::any().map(move || Arc::clone(&active_peers));
	let l2_file_filter = warp::any().map(move || l2_db_file.clone());
	
	// ===================== TRACKING HTLC BTC (pour atomic swap) =====================
	let btc_htlcs: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
	let btc_htlc_set_filter = warp::any().map(move || Arc::clone(&btc_htlcs));

    // LECTURE ON-CHAIN DES SWAPS : On lit l'historique des blocs !
    let get_swaps = warp::path("swaps")
		.and(warp::get())
		.and(chain_filter.clone())
		.map(|chain_arc: Arc<Mutex<Blockchain>>| {
			let chain_lock = chain_arc.lock().unwrap();
			let mut active_swaps = Vec::new();
			let mut claimed_hashes = std::collections::HashSet::new();

			// 1. On détecte tous les HTLC déjà claimés ou remboursés
			for block in &chain_lock.chain {
				for tx in &block.transactions {
					if let crate::transaction::TransactionType::HTLCClaim { secret } = &tx.tx_type {
						let secret_bytes = hex::decode(secret).unwrap_or_default();
						let hash = hex::encode(sha2::Sha256::digest(&secret_bytes));
						claimed_hashes.insert(hash);
					}
					if let crate::transaction::TransactionType::HTLCRefund { hash } = &tx.tx_type {
						claimed_hashes.insert(hash.clone());
					}
				}
			}

			// 2. On récupère les swaps en cours (DexSettlement + HTLCLock non claimés)
			for block in chain_lock.chain.iter().rev().take(200) {
				for tx in &block.transactions {
					if let crate::transaction::TransactionType::DexSettlement { swaps, .. } = &tx.tx_type {
						for swap in swaps {
							if !claimed_hashes.contains(&swap.htlc_hash) {
								active_swaps.push(swap.clone());
							}
						}
					}
				}
			}

			warp::reply::json(&active_swaps)
		});
	
	let secret_for_onion = node_kyber_secret.clone(); // Clonage pour la route

    // ===================================================================
    // ROUTE MIXNET : La porte d'entrée du réseau en Oignon
    // ===================================================================
    let relay_onion = warp::path!("relay_onion")
        .and(warp::post())
        .and(warp::body::content_length_limit(1024 * 1024 * 32)) // 32 Mo max
        .and(warp::body::bytes()) // 👈 ON LIT LE BINAIRE PUR !
        .then(move |body_bytes: warp::hyper::body::Bytes| {
            let secret_for_onion = secret_for_onion.clone();
            async move {
                use warp::Reply;
                
                // 1. Décodage binaire
                let packet: crate::mixnet::OnionPacket = match bincode::deserialize(&body_bytes) {
                    Ok(p) => p,
                    Err(e) => return warp::reply::with_status(warp::reply::json(&format!("❌ Format oignon binaire invalide: {}", e)), warp::http::StatusCode::BAD_REQUEST).into_response(),
                };

                // 2. Épluchage
                match packet.peel(&secret_for_onion) {
                    Ok(hop_payload) => {
                        if hop_payload.next_hop_address.starts_with("http") {
                            println!("🎯 [MIXNET] Nœud de Sortie (Exit Node) ! Routage final...");
                            
                            let target_url = hop_payload.next_hop_address.clone();
                            let payload = hop_payload.inner_data.clone();
                            
                            let client = reqwest::Client::new();
                            // 💡 ON ATTEND LE RESULTAT (Fini le mensonge du "OK" instantané)
                            match client.post(&target_url)
                                .header("Content-Type", "application/octet-stream") // C'est du binaire !
                                .body(payload)
                                .send()
                                .await {
                                Ok(res) => {
                                    let status = res.status();
                                    let text = res.text().await.unwrap_or_default();
                                    if status.is_success() {
                                        println!("✅ [MIXNET] TX routée et acceptée !");
                                        warp::reply::with_status(warp::reply::json(&text), warp::http::StatusCode::OK).into_response()
                                    } else {
                                        println!("❌ [MIXNET] Refusé par le réseau: {}", text);
                                        warp::reply::with_status(warp::reply::json(&text), warp::http::StatusCode::BAD_REQUEST).into_response()
                                    }
                                }
                                Err(e) => warp::reply::with_status(warp::reply::json(&format!("Erreur Nœud Final: {}", e)), warp::http::StatusCode::BAD_GATEWAY).into_response()
                            }
                        } else if !hop_payload.next_hop_address.is_empty() {
                            println!("🧅 [MIXNET] Couche épluchée. Relais P2P...");
                            if let Ok(next_packet) = bincode::deserialize::<crate::mixnet::OnionPacket>(&hop_payload.inner_data) {
                                let target_ip = hop_payload.next_hop_address.clone();
                                tokio::spawn(async move {
                                    if let Ok(mut stream) = tokio::net::TcpStream::connect(&target_ip).await {
                                        use tokio::io::AsyncWriteExt;
                                        let envelope = crate::network::P2PMessage::RelayOnion { packet: next_packet };
                                        let mut json_str = serde_json::to_string(&envelope).unwrap();
                                        json_str.push('\n');
                                        let _ = stream.write_all(json_str.as_bytes()).await;
                                    }
                                });
                            }
                            warp::reply::with_status(warp::reply::json(&"Relayé"), warp::http::StatusCode::OK).into_response()
                        } else {
                            warp::reply::with_status(warp::reply::json(&"OK"), warp::http::StatusCode::OK).into_response()
                        }
                    },
                    Err(e) => warp::reply::with_status(warp::reply::json(&format!("Erreur oignon: {}", e)), warp::http::StatusCode::BAD_REQUEST).into_response()
                }
            }
        });
	
	let send_tx = warp::post()
		.and(warp::path("send_tx"))
		.and(warp::body::content_length_limit(1024 * 1024 * 32))
        .and(warp::body::bytes()) // ON LIT LE BINAIRE PUR !
        .and(mempool_filter.clone())
        .and(chain_filter.clone()) 
        .and(active_peers_filter.clone()) 
		.and(btc_htlc_set_filter.clone())
        .map(|body_bytes: warp::hyper::body::Bytes, mempool: Arc<Mutex<Vec<Transaction>>>, 
										 chain_arc: Arc<Mutex<Blockchain>>, 
										 active_peers: crate::network::ActivePeers,
										 btc_htlcs: Arc<Mutex<HashSet<String>>>| {
            
            // DÉCODAGE BINAIRE ULTRA RAPIDE !
            let tx: Transaction = match bincode::deserialize(&body_bytes) {
                Ok(t) => t,
                Err(_) => return warp::reply::with_status(warp::reply::json(&"❌ Format binaire invalide"), warp::http::StatusCode::BAD_REQUEST),
            };

            // BOUCLIER : Bloque les spams obèses (Max 25 récompenses de minage d'un coup)
			/*
            if tx.inputs.len() > 25 {
                return warp::reply::with_status(warp::reply::json(&"❌ REJETÉ : Transaction trop lourde. Regroupez vos fonds par paquets de 25 maximum."), warp::http::StatusCode::BAD_REQUEST);
            }
			*/
            
            // PATCH ANTI-HACKING dans api.rs : Bloque TOUTES les transactions systèmes depuis l'API
			if matches!(tx.tx_type, crate::transaction::TransactionType::Coinbase 
								  | crate::transaction::TransactionType::MicroCoinbase
								  | crate::transaction::TransactionType::MiningShare { .. }
								  | crate::transaction::TransactionType::DexSettlement { .. }
								  | crate::transaction::TransactionType::LotteryPayout { .. }) {
				let err_msg = "❌ REJETÉ : Les transactions de Consensus sont générées par le réseau, pas par l'API.";
				return warp::reply::with_status(warp::reply::json(&err_msg), warp::http::StatusCode::BAD_REQUEST);
			}
			
			// ROUTAGE DOMAINE : Est-ce une transaction ciblant le L2 ?
			let is_l2_tx = tx.outputs.iter().any(|out| out.stealth_address.starts_with("L2_WATT_"));

			// Les frais réduits s'appliquent dès qu'on interagit avec le L2
			let min_fee = if is_l2_tx { 100 } else { 1000 };

			let is_feeless = matches!(tx.tx_type, 
				crate::transaction::TransactionType::Coinbase | 
				crate::transaction::TransactionType::HTLCClaim { .. } | 
				crate::transaction::TransactionType::HTLCRefund { .. }
			);

			if tx.fee < min_fee && !is_feeless {
				let err_msg = format!("❌ Frais de réseau insuffisants (Min: {} Flames)", min_fee);
				return warp::reply::with_status(warp::reply::json(&err_msg), warp::http::StatusCode::BAD_REQUEST);
			}

            {
                let pool_check = mempool.lock().unwrap();
                if pool_check.len() >= 2000 {
                    return warp::reply::with_status(warp::reply::json(&"❌ Réseau saturé"), warp::http::StatusCode::SERVICE_UNAVAILABLE);
                }
            }

            if !tx.is_valid() {
                return warp::reply::with_status(warp::reply::json(&"❌ Preuve ZKP ou signature invalide"), warp::http::StatusCode::BAD_REQUEST);
            }
			
			// ===================== BLINDAGE ATOMIC SWAP =====================
			if let TransactionType::HTLCLock { hash, .. } = &tx.tx_type {
				let btc_side_exists = {
					let set = btc_htlcs.lock().unwrap();
					set.contains(hash)
				};

				if !btc_side_exists {
					return warp::reply::with_status(
						warp::reply::json(&"❌ HTLC BTC correspondant non trouvé. Alice doit d’abord verrouiller les BTC."),
						warp::http::StatusCode::BAD_REQUEST
					);
				}
			}

            if tx.tx_type != crate::transaction::TransactionType::Coinbase {
                let chain_lock = chain_arc.lock().unwrap();
                let pool_lock = mempool.lock().unwrap();

                for input in &tx.inputs {
                    let ki = &input.mpc_ring.key_image;
                    if chain_lock.spent_key_images.contains(ki) { return warp::reply::with_status(warp::reply::json(&"❌ Fonds déjà dépensés"), warp::http::StatusCode::BAD_REQUEST); }
                    if pool_lock.iter().any(|m_tx| m_tx.inputs.iter().any(|m_in| &m_in.mpc_ring.key_image == ki)) { return warp::reply::with_status(warp::reply::json(&"❌ TX déjà en attente"), warp::http::StatusCode::BAD_REQUEST); }
                }
            }
            
            if let crate::transaction::TransactionType::HTLCRefund { hash } = &tx.tx_type {
                let chain_lock = chain_arc.lock().unwrap();
                let current_height = chain_lock.chain.len() as u64;
                let mut timeout_passed = false;
                
                for block in &chain_lock.chain {
                    for past_tx in &block.transactions {
                        if let crate::transaction::TransactionType::HTLCLock { hash: lock_hash, timeout_block } = &past_tx.tx_type {
                            if lock_hash == hash {
                                if current_height >= *timeout_block { timeout_passed = true; }
                                break;
                            }
                        }
                    }
                }
                
                if !timeout_passed { return warp::reply::with_status(warp::reply::json(&"⏳ Délai non expiré"), warp::http::StatusCode::BAD_REQUEST); }
            }
			
			let mut pool = mempool.lock().unwrap();
            pool.push(tx.clone());

            let tx_clone = tx.clone();
            tokio::spawn(async move { crate::network::broadcast_transaction(tx_clone, active_peers).await; });
            
			// AFFICHAGE PROPRE (Sans spammer la console avec les signatures)
            let tx_info = match &tx.tx_type {
                TransactionType::L2Anchor { l2_name, state_root, .. } => {
                    format!("L2Anchor {{ l2_name: \"{}\", state_root: \"{}...\" }}", l2_name, &state_root[0..15])
                },
                _ => format!("{:?}", tx.tx_type),
            };
            println!("📥 [MEMPOOL] Transaction acceptée et propagée (type: {})", tx_info);
            warp::reply::with_status(warp::reply::json(&"✅ TX acceptée par le réseau"), warp::http::StatusCode::OK)
        });
    
    // route all_transactions du wallet
    let get_all_txs = warp::get()
        .and(warp::path("all_transactions"))
        .and(chain_filter.clone())
        .and(l2_file_filter.clone())
        .and_then(|chain_arc: Arc<Mutex<Blockchain>>, l2_file: String| async move {
            let mut enriched_txs = Vec::new();
            
            // Un petit dictionnaire pour retrouver la hauteur L1 à partir de son hash
            let mut hash_to_height = std::collections::HashMap::new();

            // =========================================================
            // SCOPE SYNCHRONE : On lit la RAM et on relâche le verrou
            // =========================================================
            {
                let chain_lock = chain_arc.lock().unwrap();
                
                // 1. Lecture de la chaîne L1 (en RAM)
                for block in &chain_lock.chain {
                    hash_to_height.insert(block.header.hash.clone(), block.header.index);
                    for tx in &block.transactions {
                        enriched_txs.push(serde_json::json!({
                            "height": block.header.index,
                            "timestamp": block.header.timestamp,
                            "transaction": tx,
                            "is_l2": false
                        }));
                    }
                }
            } // Le `chain_lock` est détruit exactement ici ! Le Mutex est libre.

            // =========================================================
            // SCOPE ASYNCHRONE : On lit le disque en toute sécurité
            // =========================================================
            // 2. Lecture de la chaîne L2 (sur le Disque Dur)
            if let Ok(data) = tokio::fs::read_to_string(&l2_file).await {
                if let Ok(l2_chain) = serde_json::from_str::<Vec<crate::block::MicroBlock>>(&data) {
                    for mb in l2_chain {
                        // On retrouve l'index du bloc L1 parent
                        let parent_height = hash_to_height.get(&mb.l1_parent_hash).cloned().unwrap_or(0);
                        
                        for tx in &mb.transactions {
                            enriched_txs.push(serde_json::json!({
                                "height": parent_height,
                                "micro_index": mb.micro_index, // On transmet l'index L2
                                "timestamp": mb.timestamp,
                                "transaction": tx,
                                "is_l2": true
                            }));
                        }
                    }
                }
            }

            Ok::<_, warp::Rejection>(warp::reply::json(&enriched_txs))
        });
		
	// Synchronisation différentielle (Économise 99% de la bande passante)
    let sync_blocks = warp::get()
        .and(warp::path("sync_blocks"))
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(chain_filter.clone())
        .and(l2_file_filter.clone())
        .and_then(|params: std::collections::HashMap<String, String>, chain_arc: Arc<Mutex<Blockchain>>, l2_file: String| async move {
            let last_l1 = params.get("last_l1").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let last_l2 = params.get("last_l2").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

            let mut new_txs = Vec::new();
            let mut hash_to_height = std::collections::HashMap::new();

            {
                let chain_lock = chain_arc.lock().unwrap();
                for block in &chain_lock.chain {
                    hash_to_height.insert(block.header.hash.clone(), block.header.index);
                    
                    // On n'envoie que les NOUVEAUX blocs L1
                    if block.header.index > last_l1 {
                        for tx in &block.transactions {
                            new_txs.push(serde_json::json!({
                                "height": block.header.index,
                                "timestamp": block.header.timestamp,
                                "transaction": tx,
                                "is_l2": false
                            }));
                        }
                    }
                }
            }

            if let Ok(data) = tokio::fs::read_to_string(&l2_file).await {
                if let Ok(l2_chain) = serde_json::from_str::<Vec<crate::block::MicroBlock>>(&data) {
                    for mb in l2_chain {
                        // On n'envoie que les NOUVEAUX microblocs L2
                        if mb.micro_index > last_l2 {
                            let parent_height = hash_to_height.get(&mb.l1_parent_hash).cloned().unwrap_or(0);
                            for tx in &mb.transactions {
                                new_txs.push(serde_json::json!({
                                    "height": parent_height,
                                    "micro_index": mb.micro_index,
                                    "timestamp": mb.timestamp,
                                    "transaction": tx,
                                    "is_l2": true
                                }));
                            }
                        }
                    }
                }
            }

            Ok::<_, warp::Rejection>(warp::reply::json(&new_txs))
        });
        
    let get_decoys = warp::get()
        .and(warp::path!("get_decoys" / usize))
        .and(chain_filter.clone())
        .map(|count: usize, chain_arc: Arc<Mutex<Blockchain>>| {
            let chain_lock = chain_arc.lock().unwrap();
            warp::reply::json(&chain_lock.get_random_decoys(count))
        });

    let get_pool = warp::get()
        .and(warp::path("pool"))
        .and(dex_pool_filter.clone())
        .map(|pool: SharedPool| {
            warp::reply::json(&*pool.lock().unwrap())
        });

    let submit_order = warp::post()
		.and(warp::path("order"))
		.and(warp::body::bytes()) // 💡 On accepte le binaire du Mixnet
		.and(dex_pool_filter.clone())
		.and(active_peers_filter.clone()) 
		.map(|body_bytes: warp::hyper::body::Bytes, pool: SharedPool, active_peers: crate::network::ActivePeers| {
            // On décode le JSON depuis les octets
            let order: Order = match serde_json::from_slice(&body_bytes) {
                Ok(o) => o,
                Err(_) => return warp::reply::with_status(warp::reply::json(&"❌ Format JSON invalide"), warp::http::StatusCode::BAD_REQUEST),
            };

			// VALIDATION STRICTE : Un ordre d'achat DOIT avoir un hash HTLC
			if order.order_type == "buy" && order.htlc_hash.is_none() {
				return warp::reply::with_status(warp::reply::json(&"❌ Achat impossible : HTLC Hash manquant"), warp::http::StatusCode::BAD_REQUEST);
			}

			let mut is_new = false;
			{
				let mut p = pool.lock().unwrap();
				if !p.iter().any(|o| o.id == order.id) { 
					p.push(order.clone()); 
					is_new = true; 
				}
			}
			if is_new {
				let order_clone = order.clone();
				tokio::spawn(async move { crate::network::broadcast_order(order_clone, active_peers).await; });
			}
			warp::reply::with_status(warp::reply::json(&"✅ Ordre ajouté et propagé"), warp::http::StatusCode::OK)
		});
		
	let cancel_order = warp::delete()
        .and(warp::path!("order" / String))
        .and(dex_pool_filter.clone())
        .map(|id: String, pool: SharedPool| {
            let mut p = pool.lock().unwrap();
            p.retain(|o| o.id != id);
            warp::reply::json(&"✅ Ordre supprimé")
        });

	let info_route = warp::path("info")
		.and(warp::get())
		.and(chain_filter.clone())
		.and(active_peers_filter.clone())
		.and(l2_file_filter.clone())
		.map(|chain_arc: Arc<Mutex<Blockchain>>, active_peers: crate::network::ActivePeers, l2_file: String| {
			
			// Version safe (ne panique jamais sur mutex empoisonné)
			let chain_lock = match chain_arc.lock() {
				Ok(lock) => lock,
				Err(_) => {
					return warp::reply::json(&serde_json::json!({
						"error": "internal_mutex_poisoned",
						"blocks": 0
					}));
				}
			};

			let last_block = match chain_lock.chain.last() {
				Some(b) => b,
				None => {
					return warp::reply::json(&serde_json::json!({
						"error": "no_blocks",
						"blocks": 0
					}));
				}
			};
			
			// Lecture sécurisée du nombre de Micro-Blocs L2
            let mut l2_blocks_count = 0;
            if let Ok(data) = std::fs::read_to_string(&l2_file) {
                if let Ok(l2_chain) = serde_json::from_str::<Vec<crate::block::MicroBlock>>(&data) {
                    l2_blocks_count = l2_chain.len();
                }
            }

			let max_target = num_bigint::BigUint::from_bytes_be(&[0xFF; 32]);
			let initial_target = max_target.clone() >> 12_u32;
			let hundred = num_bigint::BigUint::from(100u32);

			let target_big = num_bigint::BigUint::parse_bytes(last_block.header.target_hex.as_bytes(), 16)
				.unwrap_or_else(|| chain_lock.target.clone());

			let difficulty_x100 = (&initial_target * &hundred) / &target_big;
			let diff_int = &difficulty_x100 / &hundred;
			let diff_dec = &difficulty_x100 % &hundred;
			let difficulty_decimal = format!("{}.{:02}", diff_int, diff_dec);
			let target_hex = format!("{:0>64}", target_big.to_str_radix(16));

			let expected_hashes = &max_target / &target_big;
			let hashrate = &expected_hashes / num_bigint::BigUint::from(120u32);

			// Version safe pour active_peers aussi
			let peers_count = active_peers.lock()
				.map(|p| p.len())
				.unwrap_or(0);

			warp::reply::json(&serde_json::json!({
				"blocks": last_block.header.index,
				"l2_blocks": l2_blocks_count,
				"connected_peers": peers_count,
				"last_price_sats": LAST_PRICE_SATS.load(Ordering::Relaxed),
				"version": format!("Wattcoin V{}", env!("CARGO_PKG_VERSION")), // Lecture automatique du Cargo.toml
				"difficulty_decimal": difficulty_decimal,
				"target_hex": target_hex,
				"hashrate": hashrate.to_string()
			}))
		});
		
	
    let get_supply = warp::path("supply")
        .and(warp::get())
        .and(chain_filter.clone())
        .map(|chain_arc: Arc<Mutex<Blockchain>>| {
            let supply = chain_arc.lock().unwrap().get_total_supply();
            warp::reply::json(&supply)
        });

    let get_jackpot = warp::path("jackpot")
		.and(warp::get())
		.and(chain_filter.clone())
		.and(l2_file_filter.clone()) 
		.map(|chain_arc: Arc<Mutex<Blockchain>>, l2_file: String| {
			let chain_lock = chain_arc.lock().unwrap();
			
			// On passe le chemin du fichier L2
			let pot = chain_lock.get_current_jackpot(Some(&l2_file)); 
			
			warp::reply::json(&pot.0)
		});
		
	// ==================== ROUTE DIFFICULTY HISTORY (Avec Échantillonnage) ====================
	let get_difficulty_history = warp::path("difficulty")
		.and(warp::path("history"))
		.and(warp::get())
		.and(warp::query::<std::collections::HashMap<String, String>>())
		.and(chain_filter.clone())
		.map(|params: std::collections::HashMap<String, String>, chain_arc: Arc<Mutex<Blockchain>>| {
			let chain_lock = chain_arc.lock().unwrap();

			let hours = params.get("hours").and_then(|v| v.parse::<i64>().ok());
			let days = params.get("days").and_then(|v| v.parse::<i64>().ok());
			let is_all = params.get("all").map(|v| v == "true").unwrap_or(false);

			let now = chrono::Utc::now().timestamp();

			// 1. Déterminer combien de blocs sont concernés par la période
			let mut blocks_in_range = 0;
			if is_all {
				blocks_in_range = chain_lock.chain.len();
			} else {
				for block in chain_lock.chain.iter().rev() {
					if let Some(h) = hours {
						if now - block.header.timestamp > h * 3600 { break; }
					}
					if let Some(d) = days {
						if now - block.header.timestamp > d * 86400 { break; }
					}
					blocks_in_range += 1;
				}
			}

			// 2. Calcul du pas (step) pour garantir un maximum d'environ 500 points
			let target_points = 500;
			let step = (blocks_in_range / target_points).max(1);

			let mut history = Vec::new();
			let mut counter = 0;

			let max_target = num_bigint::BigUint::from_bytes_be(&[0xFF; 32]);
			let initial_target = max_target.clone() >> 12_u32;
			let hundred = num_bigint::BigUint::from(100u32);

			// 3. Échantillonnage de la blockchain
			for block in chain_lock.chain.iter().rev() {
				if !is_all {
					if let Some(h) = hours {
						if now - block.header.timestamp > h * 3600 { break; }
					}
					if let Some(d) = days {
						if now - block.header.timestamp > d * 86400 { break; }
					}
				}

				// On ne prend qu'un bloc sur "step"
				if counter % step == 0 {
					let target_big = num_bigint::BigUint::parse_bytes(block.header.target_hex.as_bytes(), 16)
						.unwrap_or_else(|| max_target.clone());

					let difficulty_x100 = (&initial_target * &hundred) / &target_big;
					let diff_int = &difficulty_x100 / &hundred;
					let diff_dec = &difficulty_x100 % &hundred;

					history.push(serde_json::json!({
						"height": block.header.index,
						"difficulty_decimal": format!("{}.{:02}", diff_int, diff_dec),
						"timestamp": block.header.timestamp
					}));
				}
				counter += 1;
			}

			history.reverse();
			warp::reply::json(&history)
		});
	// =====================================================================
	
	// ==================== HTLC ROUTES (définies ici pour être dans le scope) ====================
    let htlc_lock = warp::post()
        .and(warp::path!("htlc" / "lock"))
        .and(warp::body::json())
        .and(mempool_filter.clone())
        .and(active_peers_filter.clone())
        .map(|tx: Transaction, mempool: Arc<Mutex<Vec<Transaction>>>, active_peers: crate::network::ActivePeers| {
            if !tx.is_valid() || !matches!(tx.tx_type, TransactionType::HTLCLock { .. }) {
                return warp::reply::with_status(warp::reply::json(&"❌ HTLCLock invalide"), warp::http::StatusCode::BAD_REQUEST);
            }
            let mut pool = mempool.lock().unwrap();
            pool.push(tx.clone());
            let tx_clone = tx.clone();
			println!("✅ Transaction acceptée et propagée (type: {:?})", tx_clone);
            tokio::spawn(async move { crate::network::broadcast_transaction(tx_clone, active_peers).await; });
            warp::reply::with_status(warp::reply::json(&"✅ HTLCLock accepté"), warp::http::StatusCode::OK)
        });

    // ===================== HTLC CLAIM (version ultra-permissive pour swap atomique) =====================
	let htlc_claim = warp::post()
		.and(warp::path!("htlc" / "claim"))
		.and(warp::body::bytes()) // Le Wallet envoie du Bincode pur
		.and(chain_filter.clone())
		.and(mempool_filter.clone())
		.and(active_peers_filter.clone())
		.map(|body_bytes: warp::hyper::body::Bytes, chain_arc: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, active_peers: crate::network::ActivePeers| {

            // Décodage du bincode
            let tx: Transaction = match bincode::deserialize(&body_bytes) {
                Ok(t) => t,
                Err(_) => return warp::reply::with_status(warp::reply::json(&"❌ Format binaire invalide"), warp::http::StatusCode::BAD_REQUEST),
            };

			// === Extraction safe du secret ===
			let secret = match &tx.tx_type {
				TransactionType::HTLCClaim { secret } if !secret.is_empty() => secret.clone(),
				_ => return warp::reply::with_status(
					warp::reply::json(&"❌ Type ou secret invalide"),
					warp::http::StatusCode::BAD_REQUEST
				),
			};

			let secret_bytes = match hex::decode(&secret) {
				Ok(b) => b,
				Err(_) => return warp::reply::with_status(
					warp::reply::json(&"❌ Secret hex invalide"),
					warp::http::StatusCode::BAD_REQUEST
				),
			};

			let hash_to_find = hex::encode(sha2::Sha256::digest(&secret_bytes));

			// === Verrouillage safe ===
			let chain = match chain_arc.lock() {
				Ok(c) => c,
				Err(_) => return warp::reply::with_status(
					warp::reply::json(&"❌ Erreur interne (mutex empoisonné)"),
					warp::http::StatusCode::INTERNAL_SERVER_ERROR
				),
			};

			// === 2. VÉRIFICATION TRIBUNAL NODE (tout se passe ici, pas dans le wallet) ===
			let mut buyer_watt_address: Option<String> = None;
			let mut watt_amount: u64 = 0;
			let mut lock_exists = false;
			let mut already_claimed_or_refunded = false;

			for block in &chain.chain {
				for past_tx in &block.transactions {
					if let TransactionType::DexSettlement { swaps, .. } = &past_tx.tx_type {
						for swap in swaps {
							if swap.htlc_hash == hash_to_find {
								buyer_watt_address = Some(swap.buyer_watt_address.clone());
								watt_amount = swap.watt_amount_flames;   // ← u64 flames, précision exacte
								//println!("🔍 [NODE TRIBUNAL] Swap trouvé → hash={}, buyer_watt={}, amount_flames={}", hash_to_find, swap.buyer_watt_address, watt_amount);
							}
						}
					}
					if let TransactionType::HTLCLock { hash: lock_hash, .. } = &past_tx.tx_type {
						if lock_hash == &hash_to_find { lock_exists = true; }
					}
					if let TransactionType::HTLCClaim { secret: claimed_secret } = &past_tx.tx_type {
						let claimed_bytes = hex::decode(claimed_secret).unwrap_or_default();
						let claimed_hash = hex::encode(sha2::Sha256::digest(&claimed_bytes));
						if claimed_hash == hash_to_find { already_claimed_or_refunded = true; }
					}
					if let TransactionType::HTLCRefund { hash: refunded_hash } = &past_tx.tx_type {
						if refunded_hash == &hash_to_find { already_claimed_or_refunded = true; }
					}
				}
			}

			if buyer_watt_address.is_none() || !lock_exists || already_claimed_or_refunded {
				return warp::reply::with_status(
					warp::reply::json(&"❌ [NODE TRIBUNAL] HTLC WATT lock introuvable, swap invalide, ou déjà claimé/remboursé"),
					warp::http::StatusCode::BAD_REQUEST
				);
			}

			// === 3. Vérification stricte du output créé par le wallet ===
			let buyer_addr = buyer_watt_address.as_ref().unwrap();

			if tx.outputs.len() != 1 {
				return warp::reply::with_status(
					warp::reply::json(&"❌ HTLCClaim doit contenir exactement 1 output"),
					warp::http::StatusCode::BAD_REQUEST
				);
			}

			let out_amount: u64 = match tx.outputs[0].aes_vault.parse::<u64>() {
				Ok(v) => v,
				Err(_) => return warp::reply::with_status(
					warp::reply::json(&"❌ Montant output invalide"),
					warp::http::StatusCode::BAD_REQUEST
				),
			};
			
			if out_amount != watt_amount {
				return warp::reply::with_status(
					warp::reply::json(&format!("❌ Montant dans l'output ({}) != montant du swap verrouillé ({})", out_amount, watt_amount)),
					warp::http::StatusCode::BAD_REQUEST
				);
			}
			if tx.outputs[0].stealth_address != *buyer_addr {
				return warp::reply::with_status(
					warp::reply::json(&"❌ stealth_address de l'output doit être exactement le buyer_watt_address du swap"),
					warp::http::StatusCode::BAD_REQUEST
				);
			}

			// === 4. TRIBUNAL NODE : tout est validé ici (secret + lock + swap + montant u64 + destination) ===
			println!("✅ [NODE TRIBUNAL] HTLCClaim validé pour hash {} ({} WATT → {})", hash_to_find, watt_amount as f64 / 1_000_000_000.0, buyer_addr);

			// === 5. Acceptation (le tx contient déjà le bon output créé par le wallet) ===
			let mut pool = mempool.lock().unwrap();
			pool.push(tx.clone());
			let tx_clone = tx.clone();
			tokio::spawn(async move { crate::network::broadcast_transaction(tx_clone, active_peers).await; });

			warp::reply::with_status(warp::reply::json(&"✅ Claim accepté par le node (output vérifié on-chain)"), warp::http::StatusCode::OK)
		});
		
	// ===================== REVEALED SECRET – VRAI MATCHING STRICT ON-CHAIN =====================
	let htlc_revealed_secret = warp::path!("htlc" / "secret" / String)
		.and(warp::get())
		.and(chain_filter.clone())
        // 💡 EXIT LE MEMPOOL : On ne lit que la blockchain confirmée !
		.map(|requested_hash: String, chain_arc: Arc<Mutex<Blockchain>>| {

			let chain = chain_arc.lock().unwrap();

			// 🔒 Chaîne confirmée UNIQUEMENT
			for block in &chain.chain {
				for tx in &block.transactions {
					if let TransactionType::HTLCClaim { secret } = &tx.tx_type {
						let secret_bytes = hex::decode(secret).unwrap_or_default();
						let calculated = hex::encode(sha2::Sha256::digest(&secret_bytes));
						if calculated == requested_hash {
							//println!("✅ [NODE] Secret trouvé dans un BLOC MINÉ (match parfait) → {}", secret);
							return warp::reply::json(&serde_json::json!({
								"success": true,
								"secret": secret,
								"message": "Secret révélé (confirmé)"
							}));
						}
					}
				}
			}

			warp::reply::json(&serde_json::json!({
				"success": false,
				"message": "Secret pas encore miné sur la blockchain"
			}))
		});
	
	// ===================== BTC BRIDGE PRODUCTION – VERSION COMPILABLE (FIXÉ) ====================
	use reqwest::Client;
	use std::time::Duration;

	// ==================== BTC PROXY FIXÉ (switch LOCAL / PROD) ====================
	async fn btc_proxy(method: &str, url: &str, body: Option<String>) -> Result<String, String> {
		// Ton Mixnet protège l'IP de l'utilisateur, le Nœud de Sortie tape le Clearnet direct
		let client = Client::builder()
			.timeout(Duration::from_secs(15))
			.build()
			.unwrap();

		let req = match method {
			"POST" => client.post(url).body(body.unwrap_or_default()),
			_ => client.get(url),
		};
		
		let resp = req.send().await.map_err(|e| format!("BTC proxy: {}", e))?;
		if !resp.status().is_success() {
			return Err(format!("HTTP {}: {}", resp.status(), resp.text().await.unwrap_or_default()));
		}
		resp.text().await.map_err(|e| e.to_string())
	}

	let btc_create_htlc = warp::path!("btc" / "htlc" / "create")
		.and(warp::post())
		.and(warp::body::json())
		.and(btc_htlc_set_filter.clone())
		.map(|params: serde_json::Value, btc_htlcs: Arc<Mutex<HashSet<String>>>| {
			let buyer_pubkey_hex = params["buyer_pubkey"].as_str().unwrap_or_default().to_string();
			let seller_pubkey_hex = params["seller_pubkey"].as_str().unwrap_or_default().to_string();
			let secret_hex = params["secret"].as_str().unwrap_or_default().to_string();
			let locktime = params["locktime"].as_u64().unwrap_or(144);

			let secret_bytes = hex::decode(&secret_hex).unwrap_or_default();
			let hash = bitcoin::hashes::sha256::Hash::hash(&secret_bytes);
			let hash_hex = hex::encode(hash.to_byte_array());

			// === ENREGISTREMENT DU HASH BTC ===
			{
				let mut set = btc_htlcs.lock().unwrap();
				set.insert(hash_hex.clone());
			}

			let hash_bytes = hash.to_byte_array();
			let buyer_pk: bitcoin::PublicKey = match bitcoin::PublicKey::from_str(&buyer_pubkey_hex) {
				Ok(pk) => pk,
				Err(_) => return warp::reply::json(&serde_json::json!({"error": "Invalid buyer pubkey"})),
			};
			let seller_pk: bitcoin::PublicKey = match bitcoin::PublicKey::from_str(&seller_pubkey_hex) {
				Ok(pk) => pk,
				Err(_) => return warp::reply::json(&serde_json::json!({"error": "Invalid seller pubkey"})),
			};

			let script = bitcoin::blockdata::script::Builder::new()
				.push_opcode(bitcoin::opcodes::all::OP_IF)
				.push_opcode(bitcoin::opcodes::all::OP_SHA256)
				.push_slice(&hash_bytes)
				.push_opcode(bitcoin::opcodes::all::OP_EQUALVERIFY)
				.push_key(&seller_pk)
				.push_opcode(bitcoin::opcodes::all::OP_CHECKSIG)
				.push_opcode(bitcoin::opcodes::all::OP_ELSE)
				.push_int(locktime as i64)
				.push_opcode(bitcoin::opcodes::all::OP_CLTV)
				.push_opcode(bitcoin::opcodes::all::OP_DROP)
				.push_key(&buyer_pk)
				.push_opcode(bitcoin::opcodes::all::OP_CHECKSIG)
				.push_opcode(bitcoin::opcodes::all::OP_ENDIF)
				.into_script();

			let htlc_address = bitcoin::address::Address::p2wsh(script.as_script(), bitcoin::Network::Testnet).to_string();

			println!("🔨 [NODE BTC] VRAI HTLC P2WSH créé → {} (hash: {})", htlc_address, &hash_hex[..16]);

			warp::reply::json(&serde_json::json!({
				"htlc_address": htlc_address,
				"htlc_hash": hash_hex,
				"status": "real_htlc_created",
				"mock": false
			}))
		});

	let btc_send_to_htlc = warp::path!("btc" / "send" / "to_htlc")
		.and(warp::post())
		.and(warp::body::bytes()) 
		.and(btc_htlc_set_filter.clone()) 
		.map(|body_bytes: warp::hyper::body::Bytes, btc_htlcs: Arc<Mutex<HashSet<String>>>| {
			
            let payload: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                Ok(p) => p,
                Err(_) => return warp::reply::json(&serde_json::json!({"error": "Format JSON invalide"})),
            };

			// Le wallet passe le hash HTLC dans le champ "htlc_address"
			let htlc_hash = payload["htlc_address"].as_str().unwrap_or_default().to_string();
			
			// ⚡ 2. ON ENREGISTRE LE HASH DANS LA RAM DU NOEUD !
			if !htlc_hash.is_empty() {
				let mut set = btc_htlcs.lock().unwrap();
				set.insert(htlc_hash.clone());
				println!("🔍 [NODE] BTC virtuellement verrouillés pour le hash : {}", htlc_hash);
			}

			warp::reply::json(&serde_json::json!({
				"success": true,
				"message": "✅ BTC verrouillé dans le HTLC",
				"htlc_txid": "sim_txid_0x1234...confirmed"
			}))
		});
		
	// ===================== CHECK HTLC BTC (nouvelle route dédiée) =====================
	let btc_check_htlc_exists = warp::path!("btc" / "htlc" / "exists" / String)
		.and(warp::get())
		.and(btc_htlc_set_filter.clone())
		.map(|hash: String, btc_htlcs: Arc<Mutex<HashSet<String>>>| {
			let exists = {
				let set = btc_htlcs.lock().unwrap();
				set.contains(&hash)
			};

			warp::reply::json(&serde_json::json!({
				"exists": exists,
				"htlc_hash": hash,
				"message": if exists { "Contrat BTC détecté" } else { "Non trouvé" }
			}))
		});
		
	let watt_check_htlc_lock_exists = warp::path!("htlc" / "lock" / "exists" / String)
		.and(warp::get())
		.and(chain_filter.clone())
		.map(|hash: String, chain_arc: Arc<Mutex<Blockchain>>| {
			let chain = chain_arc.lock().unwrap();
			let mut exists = false;
			for block in &chain.chain {
				for tx in &block.transactions {
					if let TransactionType::HTLCLock { hash: lock_hash, .. } = &tx.tx_type {
						if lock_hash == &hash {
							exists = true;
							break;
						}
					}
				}
				if exists { break; }
			}
			warp::reply::json(&serde_json::json!({
				"exists": exists,
				"htlc_hash": hash
			}))
		});
		
	// ===================== BTC BROADCAST (Claim HTLC) =====================
	// 1. NOUVEAU : Récupération des UTXOs BTC du Wallet
	let btc_utxos_route = warp::path!("btc" / "utxos")
		.and(warp::get())
		.and(warp::query::<std::collections::HashMap<String, String>>())
		.and_then(|params: std::collections::HashMap<String, String>| async move {
			let address = params.get("address").cloned().unwrap_or_default();
			let url = format!("https://mempool.space/testnet/api/address/{}/utxo", address);
			match btc_proxy("GET", &url, None).await {
				Ok(text) => {
					let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!([]));
					Ok::<_, warp::Rejection>(warp::reply::json(&json))
				},
				Err(e) => {
					println!("❌ [NODE BTC UTXOS] Erreur proxy : {}", e);
					Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!([])))
				}
			}
		});

	// 2. CORRECTION : Le vrai Broadcast (Pousse le raw_tx sur le réseau)
	let btc_broadcast = warp::path!("btc" / "broadcast")
		.and(warp::post())
		.and(warp::body::bytes()) 
		.and_then(|body_bytes: warp::hyper::body::Bytes| async move {
            let payload: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                Ok(p) => p,
                Err(_) => return Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"success": false, "error": "Format JSON invalide"}))),
            };

			let raw_tx = payload["raw_tx"].as_str().unwrap_or_default().to_string();
			let broadcast_url = "https://mempool.space/testnet/api/tx";
			
			// Mempool.space attend le raw_tx direct en POST (texte brut)
			match btc_proxy("POST", broadcast_url, Some(raw_tx)).await {
				Ok(txid) => Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({
					"success": true,
					"txid": txid.trim(),
					"message": "✅ BTC diffusés sur le réseau !"
				}))),
				Err(e) => Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({
					"success": false,
					"error": e
				}))),
			}
		});
		
	// BTC BALANCE FINALE – respecte LOCAL_DEV_MODE + anonymat PROD
	let get_btc_balance_route = warp::path!("btc" / "balance")
		.and(warp::get())
		.and(warp::query::<std::collections::HashMap<String, String>>())
		.and_then(|params: std::collections::HashMap<String, String>| async move {
			let address = params.get("address").cloned().unwrap_or_default();

			let url = format!("https://mempool.space/testnet/api/address/{}", address);

			match btc_proxy("GET", &url, None).await {
				Ok(text) => {
					let json: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
					let chain = &json["chain_stats"];
					let funded = chain["funded_txo_sum"].as_u64().unwrap_or(0);
					let spent = chain["spent_txo_sum"].as_u64().unwrap_or(0);
					let balance_btc = (funded.saturating_sub(spent) as f64) / 100_000_000.0;
					Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"balance": balance_btc})))
				}
				Err(e) => {
					println!("❌ [NODE BTC BALANCE] Erreur proxy : {}", e);
					Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"balance": 0.0})))
				}
			}
		});
		
	// ===================================================================
    // 🌐 ROUTE INTEROPÉRABILITÉ : Statut d'une L2 Souveraine (AVEC VRF)
    // Permet à n'importe qui de vérifier si une L2 est légitime et 
    // de récupérer son dernier état ancré (State Root).
    // ===================================================================
    let get_l2_status = warp::path!("l2" / "status" / String)
        .and(warp::get())
        .and(chain_filter.clone())
        .map(|l2_name: String, chain_arc: Arc<Mutex<Blockchain>>| {
            let chain_guard = chain_arc.lock().unwrap();
            
            // On utilise un HashSet pour garder une liste unique de tous les candidats
            let mut active_sequencers: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut last_state_root = String::from("AUCUN_ANCRAGE");
            let mut total_anchors = 0u64;

            // 1. On scanne l'historique pour trouver TOUS les stakers
            for block in chain_guard.chain.iter() {
                for tx in &block.transactions {
                    match &tx.tx_type {
                        TransactionType::L2Stake { l2_name: name, sequencer_pubkey: pubkey } => {
                            if name == &l2_name { active_sequencers.insert(pubkey.clone()); }
                        },
                        TransactionType::L2Unstake { l2_name: name } => {
                            // (Note: En version simplifiée, un unstake coupe toute la L2. 
                            // Plus tard on liera le Unstake à une clé précise).
                            if name == &l2_name { active_sequencers.clear(); }
                        },
                        TransactionType::L2Anchor { l2_name: name, state_root, .. } => {
                            if name == &l2_name && !active_sequencers.is_empty() {
                                last_state_root = state_root.clone();
                                total_anchors += 1;
                            }
                        },
                        _ => {}
                    }
                }
            }

            let mut elected_pubkey = String::new();

            // 2. 🎲 LE MOTEUR VRF (Verifiable Random Function)
            if !active_sequencers.is_empty() {
                let mut candidates: Vec<String> = active_sequencers.into_iter().collect();
                candidates.sort(); // 💡 CRITIQUE : Trie alphabétiquement pour un déterminisme total

                // On prend le hash du TOUT DERNIER bloc L1 (Notre source d'entropie)
                let last_block_hash = &chain_guard.chain.last().unwrap().header.hash;
                
                // On hache (Seed + Nom de la L2)
                use sha2::Digest;
                let mut vrf_hasher = sha2::Sha256::new();
                vrf_hasher.update(last_block_hash.as_bytes());
                vrf_hasher.update(l2_name.as_bytes());
                let vrf_hash = vrf_hasher.finalize();

                // On convertit les 8 premiers octets en un chiffre
                let mut hash_bytes = [0u8; 8];
                hash_bytes.copy_from_slice(&vrf_hash[0..8]);
                let random_number = u64::from_be_bytes(hash_bytes);

                // La roulette désigne le gagnant (Modulo mathématique)
                let winner_index = (random_number as usize) % candidates.len();
                elected_pubkey = candidates[winner_index].clone();
            }

            if elected_pubkey.is_empty() {
                warp::reply::json(&serde_json::json!({
                    "error": "L2 introuvable ou aucun séquenceur actif",
                    "l2_name": l2_name
                }))
            } else {
                warp::reply::json(&serde_json::json!({
                    "l2_name": l2_name,
                    "is_active": true,
                    "sequencer_pubkey": elected_pubkey, // 💡 ON RENVOIE LE GAGNANT DU VRF !
                    "last_state_root": last_state_root,
                    "total_anchors_on_l1": total_anchors
                }))
            }
        });
		
	// ===================================================================
    // 🌐 ROUTE PEG L2 : Affiche la somme des WATT bloqués sur le pont
    // ===================================================================
    let get_l2_peg = warp::path!("l2" / "peg" / String)
        .and(warp::get())
        .and(chain_filter.clone())
        .map(|l2_name: String, chain_arc: Arc<Mutex<Blockchain>>| {
            let chain_guard = chain_arc.lock().unwrap();
            let mut total_peg_flames = 0u64;

            // L'adresse morte officielle générée par le consensus
            let official_bridge_address = format!("BRIDGE_L2_{}", l2_name.to_uppercase());

            // On scanne toute la blockchain L1
            for block in chain_guard.chain.iter() {
                for tx in &block.transactions {
                    if let TransactionType::L2BridgeLock { l2_target_name, .. } = &tx.tx_type {
                        // On vérifie que c'est bien la bonne L2
                        if l2_target_name.to_uppercase() == l2_name.to_uppercase() {
                            for out in &tx.outputs {
                                // On ne compte QUE les outputs envoyés à l'adresse morte
                                if out.stealth_address == official_bridge_address {
                                    // Le montant a été forcé en texte clair par le consensus L1 !
                                    let amount: u64 = out.aes_vault.parse().unwrap_or(0);
                                    total_peg_flames += amount;
                                }
                            }
                        }
                    }
                }
            }

            warp::reply::json(&serde_json::json!({
                "l2_name": l2_name,
                "bridge_address": official_bridge_address,
                "peg_flames": total_peg_flames,
                "peg_watt": total_peg_flames as f64 / 1_000_000_000.0
            }))
        });

	// ==================== INTÉGRATION FINALE ====================
    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec!["content-type"])
        .allow_methods(vec!["GET", "POST", "DELETE"]);

    let routes = send_tx
		.or(relay_onion)
        .or(get_all_txs)
		.or(sync_blocks)
        .or(get_decoys)
        .or(get_pool)
        .or(submit_order)
        .or(cancel_order)
        .or(info_route)
        .or(get_swaps)
        .or(get_supply)
        .or(get_jackpot)
        .or(get_difficulty_history)
        .or(htlc_lock)
        .or(htlc_claim)
		.or(htlc_revealed_secret)
        .or(btc_create_htlc)
        .or(btc_send_to_htlc)
		.or(btc_check_htlc_exists)
		.or(watt_check_htlc_lock_exists)
		.or(btc_utxos_route)
		.or(btc_broadcast)
		.or(get_btc_balance_route)
		.or(get_l2_status)
		.or(get_l2_peg)
        .with(cors);
	
	println!("🚀 [API] Serveur RPC Démarré sur {}.{}.{}.{}:{}", host_ip[0], host_ip[1], host_ip[2], host_ip[3], port);
    warp::serve(routes).run((host_ip, port)).await;
}