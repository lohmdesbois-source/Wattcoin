use warp::Filter;
use crate::blockchain::Blockchain;
use crate::transaction::Transaction;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering}; 
use serde::{Serialize, Deserialize};

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
    // 💡 PURISME CYPHERPUNK : On lit le VRAI prix directement depuis le marbre de la blockchain !
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
    let peers_filter = warp::any().map(move || Arc::clone(&known_peers));
    let active_peers_filter = warp::any().map(move || Arc::clone(&active_peers));

    // 💡 LECTURE ON-CHAIN DES SWAPS : On lit l'historique au lieu d'une variable en RAM
    let get_swaps = warp::path("swaps")
        .and(warp::get())
        .and(chain_filter.clone())
        .map(|chain_arc: Arc<Mutex<Blockchain>>| {
            let chain_lock = chain_arc.lock().unwrap();
            let mut all_swaps = Vec::new();
            // On scanne les 100 derniers blocs pour trouver les contrats de Swap gravés On-Chain
            for block in chain_lock.chain.iter().rev().take(100) {
                for tx in &block.transactions {
                    if let crate::transaction::TransactionType::DexSettlement { swaps, .. } = &tx.tx_type {
                        all_swaps.extend(swaps.clone());
                    }
                }
            }
            warp::reply::json(&all_swaps)
        });

    let send_tx = warp::post()
        .and(warp::path("send_tx"))
        .and(warp::body::json())
        .and(mempool_filter.clone())
        .and(chain_filter.clone()) 
        .and(active_peers_filter.clone()) 
        .map(|tx: Transaction, mempool: Arc<Mutex<Vec<Transaction>>>, chain_arc: Arc<Mutex<Blockchain>>, active_peers: crate::network::ActivePeers| {
            
            if tx.fee < 1000 && tx.tx_type != crate::transaction::TransactionType::Coinbase {
                return warp::reply::with_status(warp::reply::json(&"❌ Frais de réseau insuffisants (Min: 1000)"), warp::http::StatusCode::BAD_REQUEST);
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

            if tx.tx_type != crate::transaction::TransactionType::Coinbase {
                let chain_lock = chain_arc.lock().unwrap();
                let pool_lock = mempool.lock().unwrap();

                for input in &tx.inputs {
                    let ki = &input.pq_ring_signature.key_image;
                    if chain_lock.spent_key_images.contains(ki) { return warp::reply::with_status(warp::reply::json(&"❌ Fonds déjà dépensés"), warp::http::StatusCode::BAD_REQUEST); }
                    if pool_lock.iter().any(|m_tx| m_tx.inputs.iter().any(|m_in| &m_in.pq_ring_signature.key_image == ki)) { return warp::reply::with_status(warp::reply::json(&"❌ TX déjà en attente"), warp::http::StatusCode::BAD_REQUEST); }
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
            
            warp::reply::with_status(warp::reply::json(&"✅ TX acceptée par le réseau"), warp::http::StatusCode::OK)
        });
    
    let get_all_txs = warp::get()
        .and(warp::path("all_transactions"))
        .and(chain_filter.clone())
        .map(|chain_arc: Arc<Mutex<Blockchain>>| {
            let chain_lock = chain_arc.lock().unwrap();
            let mut all_txs = Vec::new();
            for block in &chain_lock.chain {
                for tx in &block.transactions { all_txs.push(tx.clone()); }
            }
            warp::reply::json(&all_txs)
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
        .and(warp::body::json())
        .and(dex_pool_filter.clone())
        .and(active_peers_filter.clone()) 
        .map(|order: Order, pool: SharedPool, active_peers: crate::network::ActivePeers| {
            let mut is_new = false;
            {
                let mut p = pool.lock().unwrap();
                if !p.iter().any(|o| o.id == order.id) { p.push(order.clone()); is_new = true; }
            }
            if is_new {
                let order_clone = order.clone();
                tokio::spawn(async move { crate::network::broadcast_order(order_clone, active_peers).await; });
            }
            warp::reply::json(&"Ordre ajouté et propagé")
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
        .and(peers_filter.clone())
        .map(|chain_arc: Arc<Mutex<Blockchain>>, peers: crate::SharedPeers| {
            let chain_lock = chain_arc.lock().unwrap();
            warp::reply::json(&serde_json::json!({
                "blocks": chain_lock.chain.len(), 
                "connected_peers": peers.lock().unwrap().len(),
                "last_price_sats": LAST_PRICE_SATS.load(Ordering::Relaxed), 
                "version": "Wattcoin V2.2.0 (On-Chain DEX)"
            }))
        });
    
    let cors = warp::cors()
        .allow_any_origin()
        .allow_headers(vec!["content-type"])
        .allow_methods(vec!["GET", "POST", "DELETE"]); // 💡 Ajout de DELETE ici

    let routes = send_tx.or(get_all_txs).or(get_decoys).or(get_pool).or(submit_order).or(cancel_order).or(info_route).or(get_swaps)
        .with(cors);
    
    warp::serve(routes).run((host_ip, port)).await;
}