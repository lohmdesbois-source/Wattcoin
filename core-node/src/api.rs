use warp::Filter;
use crate::blockchain::Blockchain;
use crate::transaction::{Transaction, TransactionType};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering}; 
use serde::{Serialize, Deserialize};
use bitcoin::hashes::Hash;
use std::str::FromStr;
use sha2::Digest;

pub type SharedPool = Arc<Mutex<Vec<Order>>>;

// 'pub' pour que le mineur (main.rs) et le validateur puissent le mettre à jour
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
    node_kyber_secret: String,
    node_kyber_pub: String
) {
    // PURISME CYPHERPUNK : On lit le VRAI prix directement depuis le marbre de la blockchain !
    {
		let chain_lock = chain.lock().unwrap();
        let mut found_price = false;
        
        for i in (0..=chain_lock.current_height).rev() {
            if let Some(block) = chain_lock.get_block_by_height(i) {
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
        }
        if !found_price { println!("📈 [MARCHÉ] Aucun prix historique trouvé. En attente du premier croisement..."); }
    }

    let mempool_filter = warp::any().map(move || Arc::clone(&mempool));
    let chain_filter = warp::any().map(move || Arc::clone(&chain));
    let dex_pool_filter = warp::any().map(move || Arc::clone(&dex_pool));
    let active_peers_filter = warp::any().map(move || Arc::clone(&active_peers));

    // ===================================================================
    // Route pour que les Wallets connaissent la tarification !
    // ===================================================================
    let get_fee_schedule = warp::path("fee_schedule")
        .and(warp::get())
        .map(|| {
            warp::reply::json(&serde_json::json!({
                "l1_min_fee_flames": 1000,
                "l1_fee_per_kb_flames": 20,
                "l2_min_fee_flames": 100,
                "l2_fee_per_kb_flames": 2, // Le L2 facture au poids maintenant !
                "description": "Les transactions paient au poids. Le L2 est 10x moins cher que le L1."
            }))
        });
		
	// ===================================================================
    // LE THERMOMÈTRE DU RÉSEAU : Estimation des frais en temps réel
    // ===================================================================
    let get_fee_estimate = warp::path("fee_estimate")
        .and(warp::get())
        .and(mempool_filter.clone())
        .map(|mempool: Arc<Mutex<Vec<Transaction>>>| {
            let pool = mempool.lock().unwrap();
            
            let mut fee_rates = Vec::new();
            
            // 1. On extrait la rentabilité de chaque transaction
            for tx in pool.iter() {
                let is_feeless = matches!(tx.tx_type, 
                    crate::transaction::TransactionType::Coinbase | 
                    crate::transaction::TransactionType::MicroCoinbase |
                    crate::transaction::TransactionType::MiningShare { .. } | 
                    crate::transaction::TransactionType::DexSettlement { .. } |
                    crate::transaction::TransactionType::LotteryPayout { .. } | 
                    crate::transaction::TransactionType::HTLCClaim { .. } | 
                    crate::transaction::TransactionType::HTLCRefund { .. }
                );
                
                if !is_feeless {
                    let tx_weight_bytes = bincode::serialized_size(tx).unwrap_or(1) as usize;
                    let weight_kb = (tx_weight_bytes as f64 / 1024.0).ceil() as u64;
                    let weight_kb = if weight_kb == 0 { 1 } else { weight_kb };
                    
                    let rate = tx.fee / weight_kb;
                    fee_rates.push((rate, tx_weight_bytes));
                }
            }
            
            // 2. On trie du plus cher au moins cher (Libre Marché)
            fee_rates.sort_by(|a, b| b.0.cmp(&a.0));
            
            let mut fast_rate = 20;   // Tarif de base L1
            let mut medium_rate = 20;
            let mut slow_rate = 20;
            let mut cumulative_size = 0;
            
            // 3. On définit les paliers dans le bloc de 32 Mo
            for (rate, size) in fee_rates {
                cumulative_size += size;
                
                if cumulative_size <= 10 * 1024 * 1024 {
                    fast_rate = rate; // Le prix pour être dans les 10 premiers Mo
                }
                if cumulative_size <= 25 * 1024 * 1024 {
                    medium_rate = rate; // Le prix pour être dans les 25 premiers Mo
                }
                if cumulative_size <= 32 * 1024 * 1024 {
                    slow_rate = rate; // Le prix pour passer de justesse
                }
            }
            
            // Bouclier : on ne peut jamais descendre sous le tarif de base
            fast_rate = std::cmp::max(fast_rate, 20);
            medium_rate = std::cmp::max(medium_rate, 20);
            slow_rate = std::cmp::max(slow_rate, 20);
            
            warp::reply::json(&serde_json::json!({
                "fast_flames_per_kb": fast_rate,
                "medium_flames_per_kb": medium_rate,
                "slow_flames_per_kb": slow_rate,
                "mempool_size_bytes": cumulative_size,
                "is_congested": cumulative_size > 32 * 1024 * 1024
            }))
        });

    let get_swaps = warp::path("swaps")
		.and(warp::get())
		.and(chain_filter.clone())
		.map(|chain_arc: Arc<Mutex<Blockchain>>| {
			let chain_lock = chain_arc.lock().unwrap();
			let mut active_swaps = Vec::new();
			let mut claimed_hashes = std::collections::HashSet::new();

			for i in 0..=chain_lock.current_height {
                if let Some(block) = chain_lock.get_block_by_height(i) {
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
			}

            let start = chain_lock.current_height.saturating_sub(200);
			for i in (start..=chain_lock.current_height).rev() {
                if let Some(block) = chain_lock.get_block_by_height(i) {
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
			}

			warp::reply::json(&active_swaps)
		});
	
	let secret_for_onion = node_kyber_secret.clone(); 
	// On clone le port pour pouvoir l'utiliser dans la closure
	let port_for_ssrf = port; 

	let relay_onion = warp::path!("relay_onion")
		.and(warp::post())
		.and(warp::body::content_length_limit(1024 * 1024 * 32)) 
		.and(warp::body::bytes()) 
		.then(move |body_bytes: warp::hyper::body::Bytes| {
			let secret_for_onion = secret_for_onion.clone();
			async move {
				use warp::Reply;
                
                let packet: crate::mixnet::OnionPacket = match bincode::deserialize(&body_bytes) {
					Ok(p) => p,
					Err(e) => return warp::reply::with_status(warp::reply::json(&format!("❌ Format invalide : {}", e)), warp::http::StatusCode::BAD_REQUEST).into_response(),
				};

				match packet.peel(&secret_for_onion) {
					Ok(hop_payload) => {
						if hop_payload.next_hop_address.starts_with("http") {
							let target_url = hop_payload.next_hop_address.clone();
							
							// On autorise le nœud à se parler à LUI-MÊME sur SON port API !
							let is_self_api = target_url.starts_with(&format!("http://127.0.0.1:{}/", port_for_ssrf)) 
										   || target_url.starts_with(&format!("http://localhost:{}/", port_for_ssrf));

							// SSRF : On bloque les requêtes locales, SAUF si c'est pour notre propre API !
							if !is_self_api && (
								target_url.contains("127.0.0.1") 
								|| target_url.contains("localhost") 
								|| target_url.contains("169.254.") 
								|| target_url.contains("10.")      
								|| target_url.contains("192.168.") 
							) {
								println!("🚨 [SÉCURITÉ] Tentative de SSRF bloquée vers : {}", target_url);
								return warp::reply::with_status(warp::reply::json(&"❌ [SÉCURITÉ] SSRF Interdit"), warp::http::StatusCode::FORBIDDEN).into_response();
							}

                            println!("🎯 [MIXNET] Nœud de Sortie (Exit Node) ! Routage final...");
                            let payload = hop_payload.inner_data.clone();
                            
                            let client = reqwest::Client::new();
                            match client.post(&target_url)
                                .header("Content-Type", "application/octet-stream") 
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
                                        
                                        // FRAMING BINAIRE
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
        .and(warp::body::bytes()) 
        .and(mempool_filter.clone())
        .and(chain_filter.clone()) 
        .and(active_peers_filter.clone()) 
        .map(|body_bytes: warp::hyper::body::Bytes, mempool: Arc<Mutex<Vec<Transaction>>>, 
										 chain_arc: Arc<Mutex<Blockchain>>, 
										 active_peers: crate::network::ActivePeers| {
            
            let tx: Transaction = match bincode::deserialize(&body_bytes) {
                Ok(t) => t,
                Err(e) => {
                    println!("❌ [API] Échec du décodage binaire sur /send_tx : {}", e);
                    return warp::reply::with_status(warp::reply::json(&"❌ Format binaire invalide"), warp::http::StatusCode::BAD_REQUEST);
                }
            };
            
			if matches!(tx.tx_type, crate::transaction::TransactionType::Coinbase 
								  | crate::transaction::TransactionType::MicroCoinbase
								  | crate::transaction::TransactionType::MiningShare { .. }
								  | crate::transaction::TransactionType::DexSettlement { .. }
								  | crate::transaction::TransactionType::LotteryPayout { .. }) {
				let err_msg = "❌ REJETÉ : Les transactions de Consensus sont générées par le réseau, pas par l'API.";
				return warp::reply::with_status(warp::reply::json(&err_msg), warp::http::StatusCode::BAD_REQUEST);
			}
			
            // ====================================================================
			// LE TRIBUNAL ÉCONOMIQUE : Calcul dynamique des frais (Poids / Ko)
			// ====================================================================
			let is_pure_l2 = !tx.outputs.is_empty() && tx.outputs.iter().all(|out| out.stealth_address.starts_with("L2_WATT_"));
            let is_l1_interop = matches!(tx.tx_type, 
                crate::transaction::TransactionType::L2Anchor { .. } |
                crate::transaction::TransactionType::L2BridgeLock { .. } |
                crate::transaction::TransactionType::L2Stake { .. } |
                crate::transaction::TransactionType::L2Unstake { .. }
            );

			let is_feeless = matches!(tx.tx_type, 
				crate::transaction::TransactionType::Coinbase | 
				crate::transaction::TransactionType::HTLCClaim { .. } | 
				crate::transaction::TransactionType::HTLCRefund { .. }
			);

            let tx_weight_bytes = bincode::serialized_size(&tx).unwrap_or(0);
            let weight_kb = (tx_weight_bytes as f64 / 1024.0).ceil() as u64;

            // Calcul du prix : L2 Interne = 2 Flames/Ko (min 100). L1 ou Interop = 20 Flames/Ko (min 1000).
            let min_fee = if is_pure_l2 && !is_l1_interop {
                std::cmp::max(100, weight_kb * 2) 
            } else {
                std::cmp::max(1000, weight_kb * 20)
            };

			if tx.fee < min_fee && !is_feeless {
				let err_msg = format!("❌ Frais de réseau insuffisants (Poids: {} Ko. Min requis: {} Flames)", tx_weight_bytes / 1024, min_fee);
                println!("{}", err_msg);
				return warp::reply::with_status(warp::reply::json(&err_msg), warp::http::StatusCode::BAD_REQUEST);
			}

            {
                let pool_check = mempool.lock().unwrap();
                if pool_check.len() >= 2000 {
                    return warp::reply::with_status(warp::reply::json(&"❌ Réseau saturé"), warp::http::StatusCode::SERVICE_UNAVAILABLE);
                }
            }

            if !tx.is_valid() {
                return warp::reply::with_status(warp::reply::json(&"❌ Math ou signature invalide"), warp::http::StatusCode::BAD_REQUEST);
            }

            if tx.tx_type != crate::transaction::TransactionType::Coinbase {
                let chain_lock = chain_arc.lock().unwrap();
                let pool_lock = mempool.lock().unwrap();
				
				// Une TX strictement identique déjà en mempool = succès (retry réseau), pas une erreur
				let tx_hash = tx.hash_data();
				if pool_lock.iter().any(|m| m.hash_data() == tx_hash) {
					return warp::reply::with_status(warp::reply::json(&"✅ TX déjà en mempool"), warp::http::StatusCode::OK);
				}

                for input in &tx.inputs {
                    if chain_lock.spent_key_images.contains(&input.utxo_id) { 
                        return warp::reply::with_status(warp::reply::json(&"❌ UTXO déjà dépensé on-chain"), warp::http::StatusCode::BAD_REQUEST); 
                    }
                    if pool_lock.iter().any(|m_tx| m_tx.inputs.iter().any(|i| i.utxo_id == input.utxo_id)) { 
                        return warp::reply::with_status(warp::reply::json(&"❌ UTXO déjà en cours de dépense dans la mempool"), warp::http::StatusCode::BAD_REQUEST); 
                    }
                }
				
				// VERROU API WOTS+
				if let Some(sig) = &tx.wots_signature {
					let ki = hex::encode(&sig.public_key);
					
					// 1. Vérification on-chain
					if chain_lock.spent_key_images.contains(&ki) {
						return warp::reply::with_status(warp::reply::json(&"❌ Clé WOTS+ déjà cramée on-chain"), warp::http::StatusCode::BAD_REQUEST); 
					}
					
					// 2. Vérification mempool simplifiée
					let mut in_mempool = false;
					for m_tx in pool_lock.iter() {
						if let Some(m_sig) = &m_tx.wots_signature {
							if hex::encode(&m_sig.public_key) == ki {
								in_mempool = true;
								break;
							}
						}
					}
					
					if in_mempool {
						return warp::reply::with_status(warp::reply::json(&"❌ Clé WOTS+ déjà utilisée dans la mempool"), warp::http::StatusCode::BAD_REQUEST); 
					}
				}
            }
            
            if matches!(tx.tx_type, TransactionType::HTLCClaim { .. } | TransactionType::HTLCRefund { .. }) {
				let chain_lock = chain_arc.lock().unwrap();
				let next_height = chain_lock.current_height + 1;
				if let Err(e) = chain_lock.check_htlc_tx(&tx, next_height, &mut std::collections::HashSet::new()) {
					return warp::reply::with_status(warp::reply::json(&e), warp::http::StatusCode::BAD_REQUEST);
				}
			}
			
			let mut pool = mempool.lock().unwrap();
            pool.push(tx.clone());

            let tx_clone = tx.clone();
            tokio::spawn(async move { crate::network::broadcast_transaction(tx_clone, active_peers).await; });
            
            let tx_info = match &tx.tx_type {
                TransactionType::L2Anchor { l2_name, state_root, .. } => {
                    format!("L2Anchor {{ l2_name: \"{}\", state_root: \"{}...\" }}", l2_name, &state_root[0..15])
                },
                _ => format!("{:?}", tx.tx_type),
            };
            println!("📥 [MEMPOOL] Transaction acceptée et propagée (type: {}, Frais: {} Flames)", tx_info, tx.fee);
            warp::reply::with_status(warp::reply::json(&"✅ TX acceptée par le réseau"), warp::http::StatusCode::OK)
        });
    
    let get_all_txs = warp::get()
        .and(warp::path("all_transactions"))
        .and(chain_filter.clone())
        .map(|chain_arc: Arc<Mutex<Blockchain>>| {
            let mut enriched_txs = Vec::new();
            let mut hash_to_height = std::collections::HashMap::new();

            {
                let chain_lock = chain_arc.lock().unwrap();
                
                for i in 0..=chain_lock.current_height {
                    if let Some(block) = chain_lock.get_block_by_height(i) {
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
                }

                if let Ok(l2_tree) = chain_lock.db.open_tree("l2_blocks") {
                    for item in l2_tree.iter() {
                        if let Ok((_, value)) = item {
                            if let Ok(mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                                // RÉSOLUTION INFAILLIBLE DU PARENT L1 VIA L'INDEX RAM
                                let parent_height = hash_to_height.get(&mb.l1_parent_hash).copied().unwrap_or(0);

                                for tx in &mb.transactions {
                                    enriched_txs.push(serde_json::json!({
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
            } 

            warp::reply::json(&enriched_txs)
        });
		
    let sync_blocks = warp::get()
        .and(warp::path("sync_blocks"))
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(chain_filter.clone())
        .map(|params: std::collections::HashMap<String, String>, chain_arc: Arc<Mutex<Blockchain>>| {
            let last_l1 = params.get("last_l1").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let last_l2 = params.get("last_l2").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);

            let mut new_txs = Vec::new();
            let mut hash_to_height = std::collections::HashMap::new();

            {
                let chain_lock = chain_arc.lock().unwrap();
                for i in 0..=chain_lock.current_height {
                    if let Some(block) = chain_lock.get_block_by_height(i) {
                        hash_to_height.insert(block.header.hash.clone(), block.header.index);
                        
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

                if let Ok(l2_tree) = chain_lock.db.open_tree("l2_blocks") {
                    for item in l2_tree.iter() {
                        if let Ok((_, value)) = item {
                            if let Ok(mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                                if mb.micro_index > last_l2 {
                                    
                                    // RÉSOLUTION INFAILLIBLE DU PARENT L1 VIA L'INDEX RAM
                                    let parent_height = hash_to_height.get(&mb.l1_parent_hash).copied().unwrap_or(0);

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
                }
            }

            warp::reply::json(&new_txs)
        });
        
    let get_pool = warp::get()
        .and(warp::path("pool"))
        .and(dex_pool_filter.clone())
        .map(|pool: SharedPool| {
            warp::reply::json(&*pool.lock().unwrap())
        });

    let submit_order = warp::post()
		.and(warp::path("order"))
		.and(warp::body::bytes()) 
		.and(dex_pool_filter.clone())
		.and(active_peers_filter.clone()) 
		.map(|body_bytes: warp::hyper::body::Bytes, pool: SharedPool, active_peers: crate::network::ActivePeers| {
            let order: Order = match serde_json::from_slice(&body_bytes) {
                Ok(o) => o,
                Err(_) => return warp::reply::with_status(warp::reply::json(&"❌ Format JSON invalide"), warp::http::StatusCode::BAD_REQUEST),
            };

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
        .map(|token_hex: String, pool: SharedPool| {
            // ANTI IDOR DEX : Le client envoie son jeton secret. On le hache pour retrouver l'ID public !
            let token_bytes = hex::decode(&token_hex).unwrap_or_default();
            let expected_id = hex::encode(sha2::Sha256::digest(&token_bytes));
            
            let mut p = pool.lock().unwrap();
            let initial_len = p.len();
            p.retain(|o| o.id != expected_id);
            
            if p.len() < initial_len {
                warp::reply::json(&"✅ Ordre supprimé")
            } else {
                warp::reply::json(&"❌ Échec : Ordre introuvable ou jeton d'annulation invalide")
            }
        });

	let info_route = warp::path("info")
		.and(warp::get())
		.and(chain_filter.clone())
		.and(active_peers_filter.clone())
		.map(|chain_arc: Arc<Mutex<Blockchain>>, active_peers: crate::network::ActivePeers| {
			
			let chain_lock = match chain_arc.lock() {
				Ok(lock) => lock,
				Err(_) => {
					return warp::reply::json(&serde_json::json!({
						"error": "internal_mutex_poisoned",
						"blocks": 0
					}));
				}
			};

			let last_block = chain_lock.get_last_block();
			
            let mut l2_blocks_count = 0;
            if let Ok(l2_tree) = chain_lock.db.open_tree("l2_blocks") {
                l2_blocks_count = l2_tree.len();
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

			let peers_count = active_peers.lock()
				.map(|p| p.len())
				.unwrap_or(0);

			warp::reply::json(&serde_json::json!({
				"blocks": last_block.header.index,
				"l2_blocks": l2_blocks_count,
				"connected_peers": peers_count,
				"last_price_sats": LAST_PRICE_SATS.load(Ordering::Relaxed),
				"version": format!("Wattcoin V{}", env!("CARGO_PKG_VERSION")), 
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
		.map(|chain_arc: Arc<Mutex<Blockchain>>| {
			let chain_lock = chain_arc.lock().unwrap();
			let pot = chain_lock.get_current_jackpot(); 
			warp::reply::json(&pot.0)
		});
		
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

			let mut blocks_in_range = 0;
			if is_all {
				blocks_in_range = (chain_lock.current_height + 1) as usize;
			} else {
				for i in (0..=chain_lock.current_height).rev() {
                    if let Some(block) = chain_lock.get_block_by_height(i) {
                        if let Some(h) = hours {
                            if now - block.header.timestamp > h * 3600 { break; }
                        }
                        if let Some(d) = days {
                            if now - block.header.timestamp > d * 86400 { break; }
                        }
                        blocks_in_range += 1;
                    }
				}
			}

			let target_points = 500;
			let step = (blocks_in_range / target_points).max(1);

			let mut history = Vec::new();
			let mut counter = 0;

			let max_target = num_bigint::BigUint::from_bytes_be(&[0xFF; 32]);
			let initial_target = max_target.clone() >> 12_u32;
			let hundred = num_bigint::BigUint::from(100u32);

			for i in (0..=chain_lock.current_height).rev() {
                if let Some(block) = chain_lock.get_block_by_height(i) {
                    if !is_all {
                        if let Some(h) = hours {
                            if now - block.header.timestamp > h * 3600 { break; }
                        }
                        if let Some(d) = days {
                            if now - block.header.timestamp > d * 86400 { break; }
                        }
                    }

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
			}

			history.reverse();
			warp::reply::json(&history)
		});

	let htlc_claim = warp::post()
		.and(warp::path!("htlc" / "claim"))
		.and(warp::body::bytes()) 
		.and(chain_filter.clone())
		.and(mempool_filter.clone())
		.and(active_peers_filter.clone())
		.map(|body_bytes: warp::hyper::body::Bytes, chain_arc: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, active_peers: crate::network::ActivePeers| {

            let tx: Transaction = match bincode::deserialize(&body_bytes) {
				Ok(t) => t,
				Err(_) => return warp::reply::with_status(warp::reply::json(&"❌ Format binaire invalide"), warp::http::StatusCode::BAD_REQUEST),
			};

			// Rejet immédiat si la structure de la transaction est invalide
			if !tx.is_valid() {
				return warp::reply::with_status(warp::reply::json(&"❌ HTLCClaim invalide (structure interne rejetée)"), warp::http::StatusCode::BAD_REQUEST);
			}

			if !matches!(tx.tx_type, TransactionType::HTLCClaim { .. }) {
				return warp::reply::with_status(warp::reply::json(&"❌ Type invalide"), warp::http::StatusCode::BAD_REQUEST);
			}
			{
				let chain = chain_arc.lock().unwrap();
				if let Err(e) = chain.check_htlc_tx(&tx, chain.current_height + 1, &mut std::collections::HashSet::new()) {
					return warp::reply::with_status(warp::reply::json(&e), warp::http::StatusCode::BAD_REQUEST);
				}
			}

			println!("✅ [NODE TRIBUNAL] HTLCClaim validé.");

			let mut pool = mempool.lock().unwrap();
			pool.push(tx.clone());
			let tx_clone = tx.clone();
			tokio::spawn(async move { crate::network::broadcast_transaction(tx_clone, active_peers).await; });

			warp::reply::with_status(warp::reply::json(&"✅ Claim accepté par le node (output vérifié on-chain)"), warp::http::StatusCode::OK)
		});
		
	let htlc_revealed_secret = warp::path!("htlc" / "secret" / String)
		.and(warp::get())
		.and(chain_filter.clone())
		.map(|requested_hash: String, chain_arc: Arc<Mutex<Blockchain>>| {

			let chain = chain_arc.lock().unwrap();

			for i in 0..=chain.current_height {
                if let Some(block) = chain.get_block_by_height(i) {
                    for tx in &block.transactions {
                        if let TransactionType::HTLCClaim { secret } = &tx.tx_type {
                            let secret_bytes = hex::decode(secret).unwrap_or_default();
                            let calculated = hex::encode(sha2::Sha256::digest(&secret_bytes));
                            if calculated == requested_hash {
                                return warp::reply::json(&serde_json::json!({
                                    "success": true,
                                    "secret": secret,
                                    "message": "Secret révélé (confirmé)"
                                }));
                            }
                        }
                    }
                }
			}

			warp::reply::json(&serde_json::json!({
				"success": false,
				"message": "Secret pas encore miné sur la blockchain"
			}))
		});
	
	use reqwest::Client;
	use std::time::Duration;

	async fn btc_proxy(method: &str, endpoint: &str, body: Option<String>) -> Result<String, String> {
		let client = Client::builder()
			.timeout(Duration::from_secs(30)) // Le nœud a 30s par tentative (le wallet attend 120s, on est large)
			// ON UTILISE UNE IDENTITÉ HONNÊTE ET OFFICIELLE !
			//.user_agent("WattcoinCore/3.0 (Rust HTTP Client)")
			.build()
			.unwrap();

		let providers = [
			"https://mempool.space/testnet/api",
			"https://blockstream.info/testnet/api"
		];

		let mut last_error = String::new();

		for provider in providers {
			let url = format!("{}{}", provider, endpoint);
			let req = match method {
				"POST" => client.post(&url).body(body.clone().unwrap_or_default()),
				_ => client.get(&url),
			};
			
			match req.send().await {
				Ok(resp) if resp.status().is_success() => {
					return resp.text().await.map_err(|e| e.to_string());
				}
				Ok(resp) => {
					let status = resp.status(); // On sauvegarde le statut AVANT de détruire resp
					let text = resp.text().await.unwrap_or_default(); // Ceci détruit resp
					
					last_error = format!("HTTP {}: {}", status, text);
					println!("⚠️ [NODE BTC] Rejet de {} : {}", provider, last_error);
					
					if status == 400 { return Err(last_error); }
				}
				Err(e) => {
					last_error = e.to_string();
					println!("⚠️ [NODE BTC] Timeout/Erreur sur {} : Bascule sur le secours...", provider);
				}
			}
		}

		Err(format!("Tous les explorateurs BTC sont HS. Dernière erreur : {}", last_error))
	}

	let btc_create_htlc = warp::path!("btc" / "htlc" / "create")
		.and(warp::post())
		.and(warp::body::json())
		.and(chain_filter.clone()) 
		.map(|params: serde_json::Value, chain_arc: Arc<Mutex<Blockchain>>| {
			let buyer_pubkey_hex = params["buyer_pubkey"].as_str().unwrap_or_default().to_string();
			let seller_pubkey_hex = params["seller_pubkey"].as_str().unwrap_or_default().to_string();
			let secret_hex = params["secret"].as_str().unwrap_or_default().to_string();
			let locktime = params["locktime"].as_u64().unwrap_or(144);

			let secret_bytes = hex::decode(&secret_hex).unwrap_or_default();
			let hash = bitcoin::hashes::sha256::Hash::hash(&secret_bytes);
			let hash_hex = hex::encode(hash.to_byte_array());
			// ÉCRITURE SLED (C'est la seule chose dont on a besoin maintenant)
			let _ = chain_arc.lock().unwrap().lock_btc_htlc(&hash_hex);

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
		.and(chain_filter.clone()) 
		.map(|body_bytes: warp::hyper::body::Bytes, chain_arc: Arc<Mutex<Blockchain>>| {
			
            let payload: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                Ok(p) => p,
                Err(_) => return warp::reply::json(&serde_json::json!({"error": "Format JSON invalide"})),
            };

			let htlc_hash = payload["htlc_address"].as_str().unwrap_or_default().to_string();
			if !htlc_hash.is_empty() {
                // ÉCRITURE SLED UNIQUEMENT
				let _ = chain_arc.lock().unwrap().lock_btc_htlc(&htlc_hash);
				println!("🔍 [NODE] BTC verrouillés et enregistré pour le hash : {}", htlc_hash);
			}

			warp::reply::json(&serde_json::json!({
				"success": true,
				"message": "✅ BTC verrouillé dans le HTLC",
				"htlc_txid": "Broadcasted via Mixnet"
			}))
		});
		
	let btc_check_htlc_exists = warp::path!("btc" / "htlc" / "exists" / String)
		.and(warp::get())
		.and(chain_filter.clone()) 
		.map(|hash: String, chain_arc: Arc<Mutex<Blockchain>>| {
			// LECTURE SLED
			let exists = chain_arc.lock().unwrap().is_btc_htlc_locked(&hash);

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
			for i in 0..=chain.current_height {
                if let Some(block) = chain.get_block_by_height(i) {
                    for tx in &block.transactions {
                        if let TransactionType::HTLCLock { hash: lock_hash, .. } = &tx.tx_type {
                            if lock_hash == &hash {
                                exists = true;
                                break;
                            }
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
		
	let btc_utxos_route = warp::path!("btc" / "utxos")
		.and(warp::get())
		.and(warp::query::<std::collections::HashMap<String, String>>())
		.and_then(|params: std::collections::HashMap<String, String>| async move {
			let address = params.get("address").cloned().unwrap_or_default();
			let endpoint = format!("/address/{}/utxo", address); 
			match btc_proxy("GET", &endpoint, None).await {
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

	let btc_broadcast = warp::path!("btc" / "broadcast")
		.and(warp::post())
		.and(warp::body::bytes()) 
		.and_then(|body_bytes: warp::hyper::body::Bytes| async move {
            let payload: serde_json::Value = match serde_json::from_slice(&body_bytes) {
                Ok(p) => p,
                Err(_) => return Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!({"success": false, "error": "Format JSON invalide"}))),
            };

			let raw_tx = payload["raw_tx"].as_str().unwrap_or_default().to_string();
			let endpoint = "/tx"; 
			
			match btc_proxy("POST", endpoint, Some(raw_tx)).await {
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
		
	let get_btc_balance_route = warp::path!("btc" / "balance")
		.and(warp::get())
		.and(warp::query::<std::collections::HashMap<String, String>>())
		.and_then(|params: std::collections::HashMap<String, String>| async move {
			let address = params.get("address").cloned().unwrap_or_default();
			let endpoint = format!("/address/{}", address); 

			match btc_proxy("GET", &endpoint, None).await {
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
		
	let get_btc_txs_route = warp::path!("btc" / "txs")
		.and(warp::get())
		.and(warp::query::<std::collections::HashMap<String, String>>())
		.and_then(|params: std::collections::HashMap<String, String>| async move {
			let address = params.get("address").cloned().unwrap_or_default();
			let endpoint = format!("/address/{}/txs", address); 
			
			match btc_proxy("GET", &endpoint, None).await {
				Ok(text) => {
					let json: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::json!([]));
					Ok::<_, warp::Rejection>(warp::reply::json(&json))
				},
				Err(e) => {
					println!("❌ [NODE BTC TXS] Erreur proxy : {}", e);
					Ok::<_, warp::Rejection>(warp::reply::json(&serde_json::json!([])))
				}
			}
		});
		
    let get_l2_status = warp::path!("l2" / "status" / String)
        .and(warp::get())
        .and(chain_filter.clone())
        .map(|l2_name: String, chain_arc: Arc<Mutex<Blockchain>>| {
            let chain_guard = chain_arc.lock().unwrap();
            
            let mut active_sequencers: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut last_state_root = String::from("AUCUN_ANCRAGE");
            let mut total_anchors = 0u64;

            for i in 0..=chain_guard.current_height {
                if let Some(block) = chain_guard.get_block_by_height(i) {
                    for tx in &block.transactions {
                        match &tx.tx_type {
                            TransactionType::L2Stake { l2_name: name, sequencer_pubkey: pubkey } => {
                                if name == &l2_name { active_sequencers.insert(pubkey.clone()); }
                            },
                            TransactionType::L2Unstake { l2_name: name } => {
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
            }

            let mut elected_pubkey = String::new();

            if !active_sequencers.is_empty() {
                let mut candidates: Vec<String> = active_sequencers.into_iter().collect();
                candidates.sort(); 

                let last_block_hash = &chain_guard.get_last_block().header.hash;
                
                use sha2::Digest;
                let mut lowest_score = [0xFFu8; 32];
                
                for candidate in &candidates {
                    let mut vrf_hasher = sha2::Sha256::new();
                    vrf_hasher.update(last_block_hash.as_bytes());
                    vrf_hasher.update(l2_name.as_bytes());
                    vrf_hasher.update(candidate.as_bytes()); 
                    
                    let mut vrf_hash = [0u8; 32];
                    vrf_hash.copy_from_slice(&vrf_hasher.finalize());
                    
                    if vrf_hash < lowest_score {
                        lowest_score = vrf_hash;
                        elected_pubkey = candidate.clone();
                    }
                }
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
                    "sequencer_pubkey": elected_pubkey, 
                    "last_state_root": last_state_root,
                    "total_anchors_on_l1": total_anchors
                }))
            }
        });
		
    let get_l2_peg = warp::path!("l2" / "peg" / String)
        .and(warp::get())
        .and(chain_filter.clone())
        .map(|l2_name: String, chain_arc: Arc<Mutex<Blockchain>>| {
            let chain_guard = chain_arc.lock().unwrap();
            let mut total_peg_flames = 0u64;

            let official_bridge_address = format!("BRIDGE_L2_{}", l2_name.to_uppercase());

            for i in 0..=chain_guard.current_height {
                if let Some(block) = chain_guard.get_block_by_height(i) {
                    for tx in &block.transactions {
                        if let TransactionType::L2BridgeLock { l2_target_name, .. } = &tx.tx_type {
                            if l2_target_name.to_uppercase() == l2_name.to_uppercase() {
                                for out in &tx.outputs {
                                    if out.stealth_address == official_bridge_address {
                                        let amount: u64 = out.aes_vault.parse().unwrap_or(0);
                                        total_peg_flames += amount;
                                    }
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
		
	// Expose la clé Kyber pour le chiffrement des Wallets
    let pubkey_clone = node_kyber_pub.clone();
    let get_pubkey = warp::path("pubkey")
        .and(warp::get())
        .map(move || {
            warp::reply::json(&serde_json::json!({
                "pubkey": pubkey_clone
            }))
        });
		
	// ===================================================================
    // EXPLORATEUR : PAGINATION ET RECHERCHE DANS LA DB SLED
    // ===================================================================
    let explorer_api = warp::path("explorer")
        .and(warp::get())
        .and(warp::query::<std::collections::HashMap<String, String>>())
        .and(chain_filter.clone())
        .map(|params: std::collections::HashMap<String, String>, chain_arc: Arc<Mutex<Blockchain>>| {
            let chain = chain_arc.lock().unwrap();
            let page = params.get("page").and_then(|v| v.parse::<u64>().ok()).unwrap_or(1).max(1);
            let limit = 10;
            let layer = params.get("layer").cloned().unwrap_or_else(|| "l1".to_string());
            let search = params.get("q").cloned().unwrap_or_default().to_lowercase();

            let mut results = Vec::new();
            let mut total_pages = 1;

            if layer == "l1" {
                if !search.is_empty() {
                    // RECHERCHE DANS LE L1
                    for i in (0..=chain.current_height).rev() {
                        if let Some(block) = chain.get_block_by_height(i) {
                            let mut match_found = block.header.index.to_string() == search || block.header.hash.to_lowercase() == search;
                            if !match_found {
                                for tx in &block.transactions {
                                    if tx.public_key.to_lowercase().contains(&search) { match_found = true; break; }
                                    for out in &tx.outputs {
                                        if out.stealth_address.to_lowercase().contains(&search) || out.kyber_capsule.to_lowercase().contains(&search) { match_found = true; break; }
                                    }
                                }
                            }
                            if match_found {
                                results.push(serde_json::json!({ "height": block.header.index, "timestamp": block.header.timestamp, "transactions": block.transactions, "is_l2": false }));
                                if results.len() >= limit as usize { break; } // Limite la recherche à 10 résultats max
                            }
                        }
                    }
                } else {
                    // PAGINATION CLASSIQUE L1
                    total_pages = (chain.current_height + limit) / limit;
                    let start_idx = chain.current_height.saturating_sub((page - 1) * limit);
                    
                    for i in 0..limit {
                        if start_idx < i { break; }
                        if let Some(block) = chain.get_block_by_height(start_idx - i) {
                            results.push(serde_json::json!({ "height": block.header.index, "timestamp": block.header.timestamp, "transactions": block.transactions, "is_l2": false }));
                        }
                    }
                }
            } else {
                // PAGINATION ET RECHERCHE L2 (Sled Tree)
                if let Ok(l2_tree) = chain.db.open_tree("l2_blocks") {
                    let total_items = l2_tree.len() as u64;
                    total_pages = (total_items + limit - 1) / limit;
                    let skip = (page - 1) * limit;
                    let mut count = 0;

                    for item in l2_tree.iter().rev() {
                        if let Ok((_, value)) = item {
                            if let Ok(mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                                if !search.is_empty() {
                                    let mut match_found = mb.micro_index.to_string() == search || mb.l1_parent_hash.to_lowercase() == search;
                                    if !match_found {
                                        for tx in &mb.transactions {
                                            if tx.public_key.to_lowercase().contains(&search) { match_found = true; break; }
                                            for out in &tx.outputs {
                                                if out.stealth_address.to_lowercase().contains(&search) || out.kyber_capsule.to_lowercase().contains(&search) { match_found = true; break; }
                                            }
                                        }
                                    }
                                    if match_found {
                                        results.push(serde_json::json!({ "micro_index": mb.micro_index, "timestamp": mb.timestamp, "transactions": mb.transactions, "is_l2": true }));
                                        if results.len() >= limit as usize { break; }
                                    }
                                } else {
                                    if count >= skip && count < skip + limit {
                                        results.push(serde_json::json!({ "micro_index": mb.micro_index, "timestamp": mb.timestamp, "transactions": mb.transactions, "is_l2": true }));
                                    }
                                    count += 1;
                                    if count >= skip + limit { break; }
                                }
                            }
                        }
                    }
                }
            }

            warp::reply::json(&serde_json::json!({
                "blocks": results,
                "total_pages": total_pages.max(1),
                "current_page": page
            }))
        });

    // SÉCURITÉ CORS : Interdit aux pages web de forger des requêtes en arrière-plan
    // Le Wallet desktop n'utilise pas de navigateur, il n'est donc pas bloqué.
    let cors = warp::cors()
		//*
        .allow_origins(vec![
            "http://127.0.0.1:8100", 
            "http://localhost:8100",
            "https://wallet.wattcoin.network"
        ])
		//*/
		//.allow_any_origin()
        .allow_headers(vec!["content-type"])
        .allow_methods(vec!["GET", "POST", "DELETE"]);

    let routes = send_tx
		.or(relay_onion)
        .or(get_all_txs)
		.or(sync_blocks)
        .or(get_pool)
        .or(submit_order)
        .or(cancel_order)
        .or(info_route)
        .or(get_swaps)
        .or(get_supply)
        .or(get_jackpot)
        .or(get_difficulty_history)
        .or(htlc_claim)
		.or(htlc_revealed_secret)
        .or(btc_create_htlc)
        .or(btc_send_to_htlc)
		.or(btc_check_htlc_exists)
		.or(watt_check_htlc_lock_exists)
		.or(btc_utxos_route)
		.or(btc_broadcast)
		.or(get_btc_balance_route)
		.or(get_btc_txs_route)
		.or(get_l2_status)
		.or(get_l2_peg)
        .or(get_fee_schedule) // La nouvelle route dynamique pour les frais !
		.or(get_fee_estimate) // Route pour l'explorer
		.or(get_pubkey)
		.or(explorer_api)
        .with(cors);
	
	println!("🚀 [API] Serveur RPC Démarré sur {}.{}.{}.{}:{}", host_ip[0], host_ip[1], host_ip[2], host_ip[3], port);
    warp::serve(routes).run((host_ip, port)).await;
}