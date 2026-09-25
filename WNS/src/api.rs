// wattcoin_name_service/src/api.rs

use warp::Filter;
use crate::state::SharedL2State;
use crate::transaction::L2Transaction;
use wots::{Wots, WotsSignature};
use crate::network::{ActiveWnsPeers, WnsP2PMessage, broadcast_message};

pub async fn start_api_server(port: u16, state: SharedL2State, active_peers: ActiveWnsPeers) {
    let state_filter = warp::any().map(move || state.clone());
    let peers_filter = warp::any().map(move || active_peers.clone());

    let get_status = warp::path!("status")
        .and(warp::get())
        .map(|| warp::reply::json(&serde_json::json!({"status": "Séquenceur WNS En Ligne"})));
        
    let get_peg = warp::path!("peg")
        .and(warp::get())
        .and(state_filter.clone())
        .map(|state: SharedL2State| {
            let state_guard = state.lock().unwrap();
            let total_l2_supply: u64 = state_guard.accounts.values().map(|a| a.balance).sum();
            
            warp::reply::json(&serde_json::json!({
                "l2_network": "WNS",
                "total_supply_flames": total_l2_supply,
                "message": "Pour vérifier le Peg, comparez cette valeur avec le solde L1 de l'adresse BRIDGE_L2_WNS"
            }))
        });

    let get_balance = warp::path!("balance" / String)
        .and(warp::get())
        .and(state_filter.clone())
        .map(|address: String, state: SharedL2State| {
            let state_guard = state.lock().unwrap();
            
            let (balance, nonce, auth_key) = if let Some(acc) = state_guard.accounts.get(&address) {
                (acc.balance, acc.nonce, acc.authorized_wots_key.clone())
            } else {
                (0, 0, String::new())
            };
            
            warp::reply::json(&serde_json::json!({
                "address": address,
                "balance": balance,
                "nonce": nonce, // 💡 Le Wallet a besoin de ce nonce pour dériver la bonne clé !
                "authorized_lattice_key": auth_key 
            }))
        });

    let send_tx = warp::path!("send")
        .and(warp::post())
        .and(warp::body::json())
        .and(state_filter.clone())
        .and(peers_filter.clone())
        .map(|tx: L2Transaction, state: SharedL2State, active_peers: ActiveWnsPeers| {
            
            // Évaluation dynamique du poids de la transaction L2
            let tx_weight_bytes = bincode::serialized_size(&tx).unwrap_or(500) as usize;
            let weight_kb = (tx_weight_bytes as f64 / 1024.0).ceil() as u64;
            
            // Le WNS facture au minimum 2000 FLAMEs (pour garantir son bénéfice) 
            // ou un tarif dynamique au poids
            let dynamic_min_fee = std::cmp::max(2000, weight_kb * 100); 

            if tx.fee < dynamic_min_fee {
                return warp::reply::with_status(
                    warp::reply::json(&serde_json::json!({
                        "error": format!("Frais L2 insuffisants. Minimum requis : {} Flames.", dynamic_min_fee)
                    })),
                    warp::http::StatusCode::BAD_REQUEST,
                );
            }
            
            let is_valid_watt = tx.domain_name.ends_with(".watt") && tx.domain_name.len() > 5;
            let is_valid_chain = tx.domain_name.ends_with(".chain") && tx.domain_name.len() > 6;

            if !is_valid_watt && !is_valid_chain {
                return warp::reply::with_status(
                    warp::reply::json(&serde_json::json!({"error": "Nom de domaine invalide (doit finir par .watt ou .chain et contenir au moins 1 caractère)." })),
                    warp::http::StatusCode::BAD_REQUEST,
                );
            }

            let hash = tx.hash_data();
            
            let is_valid = if let Ok(sig) = serde_json::from_str::<WotsSignature>(&tx.signature) {
                hex::encode(&sig.public_key) == tx.sender_pubkey && Wots::verify(&sig, &hash)
            } else {
                false
            };

            if !is_valid {
                return warp::reply::with_status(
                    warp::reply::json(&serde_json::json!({"error": "Signature WOTS+ Invalide"})),
                    warp::http::StatusCode::BAD_REQUEST,
                );
            }

            let mut state_guard = state.lock().unwrap();
            
            if let Some(acc) = state_guard.accounts.get(&tx.account_address) {
                if tx.nonce != acc.nonce + 1 {
                    return warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({"error": "Nonce invalide (Désynchronisation)."})),
                        warp::http::StatusCode::BAD_REQUEST,
                    );
                }
            } else {
                if tx.nonce != 1 {
                    return warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({"error": "Le premier nonce doit être 1."})),
                        warp::http::StatusCode::BAD_REQUEST,
                    );
                }
            }

            state_guard.mempool.push(tx.clone());
            
            println!("📥 [L2 MEMPOOL] Nouvelle TX WNS reçue : Action {:?} sur '{}' (Frais: {})", 
                tx.action, tx.domain_name, tx.fee);

            let msg = WnsP2PMessage::BroadcastTx { tx };
            broadcast_message(&msg, &active_peers, "");

            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"success": true, "message": "Transaction WNS acceptée !"})),
                warp::http::StatusCode::OK,
            )
        });

    let resolve_domain = warp::path!("resolve" / String)
        .and(warp::get())
        .and(state_filter.clone())
        .map(|domain: String, state: SharedL2State| {
            let state_guard = state.lock().unwrap();
            
            if let Some(record_data) = state_guard.domains.get(&domain) {
                let owner = state_guard.domain_owners.get(&domain).cloned().unwrap_or_default();
                
                warp::reply::json(&serde_json::json!({
                    "success": true,
                    "domain": domain,
                    "record_data": record_data, 
                    "owner_pubkey": owner       
                }))
            } else {
                warp::reply::json(&serde_json::json!({
                    "success": false,
                    "error": "Domaine introuvable"
                }))
            }
        });
        
    let get_directory = warp::path!("directory")
        .and(warp::get())
        .and(state_filter.clone())
        .map(|state: SharedL2State| {
            let state_guard = state.lock().unwrap();
            
            warp::reply::json(&serde_json::json!({
                "domains": state_guard.domains,
                "owners": state_guard.domain_owners,
            }))
        });
    
    let cors = warp::cors().allow_any_origin().allow_headers(vec!["content-type"]).allow_methods(vec!["GET", "POST"]);
    let routes = get_status.or(get_balance).or(get_peg).or(send_tx).or(resolve_domain).or(get_directory).with(cors);

    println!("🌐 [L2 API] Serveur RPC WNS Démarré sur http://127.0.0.1:{}", port);
    warp::serve(routes).run(([127, 0, 0, 1], port)).await;
}