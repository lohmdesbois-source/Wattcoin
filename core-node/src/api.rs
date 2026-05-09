use warp::Filter;
use crate::blockchain::Blockchain;
use crate::transaction::Transaction;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering}; // 💡 NOUVEAU
use serde::{Serialize, Deserialize};
use rand::RngCore;

pub type SharedPool = Arc<Mutex<Vec<Order>>>;
pub type SharedSwaps = Arc<Mutex<Vec<SwapContract>>>;

// 💡 NOUVEAU : Mémoire globale du prix du WATT
static LAST_PRICE_SATS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Order {
    pub id: String,
    pub order_type: String,
    pub amount_flames: u64,
    pub price_sats: u64,
    pub btc_address: String,
    pub btc_pubkey: String, // 💡 Vraie Clé Publique Bitcoin
    pub watt_address: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SwapContract {
    pub buyer_btc_address: String,
    pub buyer_btc_pubkey: String,   // 💡 Clé d'Alice
    pub seller_watt_address: String,
    pub seller_btc_pubkey: String,  // 💡 Clé de Bob
    pub watt_amount_flames: u64,
    pub btc_amount_sats: u64,
    pub htlc_secret: String,
    pub htlc_hash: String,
}

#[derive(Serialize, Deserialize)]
pub struct BatchResult {
    pub success: bool,
    pub message: String,
    pub clearing_price_sats: u64,
    pub total_volume_flames: u64,
    pub swaps: Vec<SwapContract>,
}

pub async fn start_api_server(
    port: u16, 
    host_ip: [u8; 4], 
    mempool: Arc<Mutex<Vec<Transaction>>>, 
    chain: Arc<Mutex<Blockchain>>, 
    known_peers: crate::SharedPeers, 
    dex_pool: SharedPool,
    active_peers: crate::network::ActivePeers
) {
    // 💡 NOUVEAU : On recharge le dernier prix connu depuis le disque dur !
    if let Ok(price_str) = std::fs::read_to_string(".watt_market_price") {
        if let Ok(saved_price) = price_str.trim().parse::<u64>() {
            LAST_PRICE_SATS.store(saved_price, Ordering::Relaxed);
            println!("📈 [MARCHÉ] Dernier prix du WATT rechargé : {} Sats", saved_price);
        }
    }

    let mempool_filter = warp::any().map(move || Arc::clone(&mempool));
    let chain_filter = warp::any().map(move || Arc::clone(&chain));
    let dex_pool_filter = warp::any().map(move || Arc::clone(&dex_pool));
    let peers_filter = warp::any().map(move || Arc::clone(&known_peers));
    let active_peers_filter = warp::any().map(move || Arc::clone(&active_peers));

    // 💡 AJOUT : Création de la mémoire des Swaps
    let active_swaps: SharedSwaps = Arc::new(Mutex::new(Vec::new()));
    
    // On crée un clone de l'Arc pour la route resolve
    let active_swaps_resolve = Arc::clone(&active_swaps);
    let swaps_filter_for_resolve = warp::any().map(move || Arc::clone(&active_swaps_resolve));
    
    // On crée un AUTRE clone de l'Arc pour la route get
    let active_swaps_get = Arc::clone(&active_swaps);
    let swaps_filter_for_get = warp::any().map(move || Arc::clone(&active_swaps_get));

    let get_swaps = warp::path("swaps")
        .and(warp::get())
        .and(swaps_filter_for_get)
        .map(|swaps: SharedSwaps| {
            warp::reply::json(&*swaps.lock().unwrap())
        });

    // 1. ROUTE : RECEVOIR UNE TRANSACTION WATTCOIN
    let send_tx = warp::post()
        .and(warp::path("send_tx"))
        .and(warp::body::json())
        .and(mempool_filter.clone())
        .and(chain_filter.clone()) // 💡 AJOUT : On donne l'accès à la Blockchain pour vérifier l'historique !
        .and(active_peers_filter.clone()) 
		.map(|tx: Transaction, mempool: Arc<Mutex<Vec<Transaction>>>, chain_arc: Arc<Mutex<Blockchain>>, active_peers: crate::network::ActivePeers| {
			
			// 1. Protection Anti-Poussière (Min Fee : 1000 Flames)
			if tx.fee < 1000 && tx.tx_type != crate::transaction::TransactionType::Coinbase {
				return warp::reply::with_status(warp::reply::json(&"❌ Frais de réseau insuffisants (Min: 1000)"), warp::http::StatusCode::BAD_REQUEST);
			}

			// 2. Limite de taille du Mempool
			{
				let pool_check = mempool.lock().unwrap();
				if pool_check.len() >= 2000 {
					return warp::reply::with_status(warp::reply::json(&"❌ Réseau saturé, réessayez plus tard"), warp::http::StatusCode::SERVICE_UNAVAILABLE);
				}
			}

			// 3. Vérification Cryptographique Pure
			if !tx.is_valid() {
				return warp::reply::with_status(warp::reply::json(&"❌ Preuve ZKP ou signature invalide"), warp::http::StatusCode::BAD_REQUEST);
			}


            // 2. 🛡️ LE PARE-FEU ANTI DOUBLE-DÉPENSE
            if tx.tx_type != crate::transaction::TransactionType::Coinbase {
                let chain_lock = chain_arc.lock().unwrap();
                let pool_lock = mempool.lock().unwrap();

                for input in &tx.inputs {
                    let ki = &input.pq_ring_signature.key_image;
                    
                    // A. Est-ce que cette pièce a DÉJÀ été minée et dépensée dans le passé ?
                    if chain_lock.spent_key_images.contains(ki) {
                        println!("🛑 [PARE-FEU] Double-dépense rejetée (Déjà dans la blockchain) : {}", ki);
                        return warp::reply::with_status(warp::reply::json(&"❌ Fonds déjà dépensés !"), warp::http::StatusCode::BAD_REQUEST);
                    }
                    
                    // B. Est-ce qu'un pirate essaie de spammer le mempool avec la même pièce simultanément ?
                    if pool_lock.iter().any(|m_tx| m_tx.inputs.iter().any(|m_in| &m_in.pq_ring_signature.key_image == ki)) {
                        println!("🛑 [PARE-FEU] Double-dépense rejetée (Déjà dans le mempool) : {}", ki);
                        return warp::reply::with_status(warp::reply::json(&"❌ Transaction déjà en attente !"), warp::http::StatusCode::BAD_REQUEST);
                    }
                }
            }
			
			// 🛡️ PARE-FEU HTLC REFUND (Vérification du Timelock)
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
                
                if !timeout_passed { 
                    println!("🛑 [PARE-FEU] Tentative de Refund prématurée ! (Contrat: {})", hash);
                    return warp::reply::with_status(warp::reply::json(&"⏳ Le délai (Timelock) n'est pas encore expiré !"), warp::http::StatusCode::BAD_REQUEST); 
                }
            }

            // 3. Si on arrive ici, la transaction est mathématiquement saine ET unique !
            let mut pool = mempool.lock().unwrap();
            pool.push(tx.clone());
            println!("📥 [API] Nouvelle TX ZKP ajoutée au Mempool !");

            // 4. On hurle la transaction sur le réseau Tor (Gossip Protocol)
            let tx_clone = tx.clone();
            tokio::spawn(async move {
                crate::network::broadcast_transaction(tx_clone, active_peers).await;
            });
            
            warp::reply::with_status(warp::reply::json(&"✅ TX acceptée par le réseau"), warp::http::StatusCode::OK)
        });
    
    // 2. ROUTE : HISTORIQUE POUR LE WALLET
    let get_all_txs = warp::get()
        .and(warp::path("all_transactions"))
        .and(chain_filter.clone())
        .map(|chain_arc: Arc<Mutex<Blockchain>>| {
            let chain_lock = chain_arc.lock().unwrap();
            let mut all_txs = Vec::new();
            for block in &chain_lock.chain {
                for tx in &block.transactions {
                    all_txs.push(tx.clone());
                }
            }
            warp::reply::json(&all_txs)
        });
        
    // 2.5 ROUTE : OBTENIR DES LEURRES POUR L'ANONYMAT (RING SIGNATURES)
    let get_decoys = warp::get()
        .and(warp::path!("get_decoys" / usize))
        .and(chain_filter.clone())
        .map(|count: usize, chain_arc: Arc<Mutex<Blockchain>>| {
            let chain_lock = chain_arc.lock().unwrap();
            let decoys = chain_lock.get_random_decoys(count);
            warp::reply::json(&decoys)
        });

    // 3. ROUTE : LIRE LE CARNET D'ORDRES
    let get_pool = warp::get()
        .and(warp::path("pool"))
        .and(dex_pool_filter.clone())
        .map(|pool: SharedPool| {
            let orders = pool.lock().unwrap().clone();
            warp::reply::json(&orders)
        });

    // 4. ROUTE : SOUMETTRE UN ORDRE DEX
    let submit_order = warp::post()
        .and(warp::path("order"))
        .and(warp::body::json())
        .and(dex_pool_filter.clone())
        .and(active_peers_filter.clone()) 
        .map(|order: Order, pool: SharedPool, active_peers: crate::network::ActivePeers| {
            println!("📡 [API DEX] Ordre reçu du Wallet : {} {} WATT", order.order_type, order.amount_flames);
            
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
                tokio::spawn(async move {
                    crate::network::broadcast_order(order_clone, active_peers).await;
                });
            }
            warp::reply::json(&"Ordre ajouté et propagé")
        });

    // 5. 💥 ROUTE : LE MOTEUR DE MATCHING
    let resolve_route = warp::path("resolve")
        .and(warp::post())
        .and(dex_pool_filter.clone())            // 💡 FIX (nom de variable)
        .and(swaps_filter_for_resolve.clone())   // 💡 FIX (nom de variable)
        .map(|pool: SharedPool, swaps_memory: SharedSwaps| {
            let mut p = pool.lock().unwrap();
            
            // 💡 FIX : "p" est déjà la liste, on utilise p.iter() (plus de p.orders)
            let mut buys: Vec<_> = p.iter().filter(|o| o.order_type == "buy").cloned().collect();
            let mut sells: Vec<_> = p.iter().filter(|o| o.order_type == "sell").cloned().collect();

            buys.sort_by(|a, b| b.price_sats.cmp(&a.price_sats));
            sells.sort_by(|a, b| a.price_sats.cmp(&b.price_sats));

            let mut generated_swaps = Vec::new();
            let mut clearing_price_sats = 0;
            let mut total_volume_flames = 0;

            let mut buy_idx = 0;
            let mut sell_idx = 0;

            while buy_idx < buys.len() && sell_idx < sells.len() {
                let buy = &mut buys[buy_idx];
                let sell = &mut sells[sell_idx];

                if buy.price_sats >= sell.price_sats {
                    clearing_price_sats = (buy.price_sats + sell.price_sats) / 2; 

                    let matched_volume = std::cmp::min(buy.amount_flames, sell.amount_flames);
                    total_volume_flames += matched_volume;

                    let mut secret_bytes = [0u8; 32];
                    rand::thread_rng().fill_bytes(&mut secret_bytes);
                    let htlc_secret = hex::encode(secret_bytes);
                    let htlc_hash = hex::encode(blake3::hash(&secret_bytes).as_bytes());

                    let btc_amount = (matched_volume as f64 / 1_000_000_000.0 * clearing_price_sats as f64) as u64;

                    generated_swaps.push(SwapContract {
                        buyer_btc_address: buy.btc_address.clone(),
                        buyer_btc_pubkey: buy.btc_pubkey.clone(),
                        seller_watt_address: sell.watt_address.clone(),
                        seller_btc_pubkey: sell.btc_pubkey.clone(),
                        watt_amount_flames: matched_volume,
                        btc_amount_sats: btc_amount,
                        htlc_secret,
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

            // 💡 FIX : On utilise p.clear() au lieu de p.orders.clear()
            p.clear();

            if total_volume_flames > 0 {
                println!("⚖️ [DEX] Ordres croisés ! Volume: {} Flames, Prix unitaire: {} Sats", total_volume_flames, clearing_price_sats);
                swaps_memory.lock().unwrap().extend(generated_swaps.clone());
                
                // 💡 FIX : On utilise LAST_PRICE_SATS directement
                LAST_PRICE_SATS.store(clearing_price_sats, std::sync::atomic::Ordering::Relaxed);
                
                let _ = std::fs::write(".watt_market_price", clearing_price_sats.to_string());
                
                // 💡 FIX : On utilise BatchResult directement
                warp::reply::json(&BatchResult { 
                    success: true, 
                    message: "Ordres croisés !".to_string(), 
                    clearing_price_sats, 
                    total_volume_flames, 
                    swaps: generated_swaps 
                })
            } else {
                warp::reply::json(&BatchResult { 
                    success: false, 
                    message: "Pas de croisement possible".to_string(), 
                    clearing_price_sats: 0, 
                    total_volume_flames: 0, 
                    swaps: vec![] 
                })
            }
        });
    
    let info_route = warp::path("info")
        .and(warp::get())
        .and(chain_filter.clone())
        .and(peers_filter.clone())
        .map(|chain_arc: Arc<Mutex<Blockchain>>, peers: crate::SharedPeers| {
            let chain_lock = chain_arc.lock().unwrap();
            warp::reply::json(&serde_json::json!({
                "blocks": chain_lock.chain.len(), 
                "connected_peers": peers.lock().unwrap().len(),
                "last_price_sats": LAST_PRICE_SATS.load(Ordering::Relaxed), // 💡 PRIX ENVOYÉ AU WALLET
                "version": "Wattcoin V2.2.0"
            }))
        });
    
    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec!["content-type"])
        .allow_methods(vec!["GET", "POST"]);

    // 💡 FIX : On utilise resolve_route au lieu de resolve_batch
    let routes = send_tx.or(get_all_txs).or(get_decoys).or(get_pool).or(submit_order).or(resolve_route).or(info_route).or(get_swaps)
        .with(cors);
    
    warp::serve(routes).run((host_ip, port)).await;
}