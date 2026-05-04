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
        .and(active_peers_filter.clone()) 
        .map(|tx: Transaction, mempool: Arc<Mutex<Vec<Transaction>>>, active_peers: crate::network::ActivePeers| {
            if tx.is_valid() {
                let mut pool = mempool.lock().unwrap();
                // 💡 FIX : On compare le 1er output
                if !pool.iter().any(|t| t.outputs[0].kyber_capsule == tx.outputs[0].kyber_capsule) {
                    pool.push(tx.clone());
                    println!("📥 [API] Nouvelle TX ajoutée au Mempool !");

                    let tx_clone = tx.clone();
                    tokio::spawn(async move {
                        crate::network::broadcast_transaction(tx_clone, active_peers).await;
                    });
                    
                    warp::reply::with_status(warp::reply::json(&"✅ TX acceptée"), warp::http::StatusCode::OK)
                } else {
                    warp::reply::with_status(warp::reply::json(&"⚠️ Déjà dans le mempool"), warp::http::StatusCode::BAD_REQUEST)
                }
            } else {
                warp::reply::with_status(warp::reply::json(&"❌ Cryptographie invalide !"), warp::http::StatusCode::BAD_REQUEST)
            }
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
    let resolve_batch = warp::post()
        .and(warp::path("resolve"))
        .and(dex_pool_filter.clone())
        .and(swaps_filter_for_resolve) // 💡 AJOUT : On passe la mémoire des swaps à la route
        .map(|pool: SharedPool, swaps_memory: SharedSwaps| { 
            let mut orders = pool.lock().unwrap();
            if orders.is_empty() {
                return warp::reply::json(&BatchResult { success: false, message: "Piscine vide.".to_string(), clearing_price_sats: 0, total_volume_flames: 0, swaps: vec![] });
            }

            let mut buys: Vec<Order> = orders.iter().filter(|o| o.order_type == "buy").cloned().collect();
            let mut sells: Vec<Order> = orders.iter().filter(|o| o.order_type == "sell").cloned().collect();

            buys.sort_by(|a, b| b.price_sats.cmp(&a.price_sats));
            sells.sort_by(|a, b| a.price_sats.cmp(&b.price_sats));

            let mut clearing_price_sats = 0;
            let mut total_volume_flames = 0;
            let mut generated_swaps = Vec::new();
            let mut i = 0; let mut j = 0;

            while i < buys.len() && j < sells.len() {
                if buys[i].price_sats >= sells[j].price_sats {
                    
                    clearing_price_sats = (buys[i].price_sats + sells[j].price_sats) / 2;
                    let trade_amount_flames = buys[i].amount_flames.min(sells[j].amount_flames);
                    total_volume_flames += trade_amount_flames;

                    let mut secret_bytes = [0u8; 32];
                    rand::thread_rng().fill_bytes(&mut secret_bytes);
                    let htlc_hash = hex::encode(blake3::hash(&secret_bytes).as_bytes());

                    let w_amt = trade_amount_flames as f64 / 1_000_000_000.0;
                    let btc_amount_sats = (w_amt * clearing_price_sats as f64) as u64;

                    
                    generated_swaps.push(SwapContract {
                        buyer_btc_address: buys[i].btc_address.clone(),
                        buyer_btc_pubkey: buys[i].btc_pubkey.clone(),    // 💡 AJOUT
                        seller_watt_address: sells[j].watt_address.clone(),
                        seller_btc_pubkey: sells[j].btc_pubkey.clone(),  // 💡 AJOUT
                        watt_amount_flames: trade_amount_flames,
                        btc_amount_sats,
                        htlc_secret: hex::encode(secret_bytes),
                        htlc_hash,
                    });

                    buys[i].amount_flames -= trade_amount_flames;
                    sells[j].amount_flames -= trade_amount_flames;
                    if buys[i].amount_flames == 0 { i += 1; }
                    if sells[j].amount_flames == 0 { j += 1; }
                } else { 
                    break;
                }
            }

            orders.clear();

            if total_volume_flames > 0 {
                println!("⚖️ [DEX] Ordres croisés ! Volume: {} Flames", total_volume_flames);
                swaps_memory.lock().unwrap().extend(generated_swaps.clone());
                
                // 💡 MISE À JOUR DU COURS DU WATT !
                LAST_PRICE_SATS.store(clearing_price_sats, Ordering::Relaxed);
                
                warp::reply::json(&BatchResult { success: true, message: "Ordres croisés !".to_string(), clearing_price_sats, total_volume_flames, swaps: generated_swaps })
            } else {
                warp::reply::json(&BatchResult { success: false, message: "Aucun croisement possible.".to_string(), clearing_price_sats: 0, total_volume_flames: 0, swaps: vec![] })
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

    // 💡 AJOUT : On n'oublie pas d'ajouter get_swaps dans les routes finales
    let routes = send_tx.or(get_all_txs).or(get_decoys).or(get_pool).or(submit_order).or(resolve_batch).or(info_route).or(get_swaps)
        .with(cors);
    
    warp::serve(routes).run((host_ip, port)).await;
}