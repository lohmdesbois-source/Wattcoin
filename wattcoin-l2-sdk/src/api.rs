use warp::Filter;
use crate::state::SharedL2State;
use crate::transaction::L2Transaction;
use wattcoin_core::wots::{WotsKeyPair, WotsSignature};

pub async fn start_api_server(port: u16, state: SharedL2State) {
    let state_filter = warp::any().map(move || state.clone());

    // ====================================================================
    // 🌐 GET /status : Vérifier l'état du Séquenceur
    // ====================================================================
    let get_status = warp::path!("status")
        .and(warp::get())
        .map(|| warp::reply::json(&serde_json::json!({"status": "Séquenceur L2 En Ligne"})));

    // ====================================================================
    // 💰 GET /balance/{adresse} : Consulter son solde L2
    // ====================================================================
    let get_balance = warp::path!("balance" / String)
        .and(warp::get())
        .and(state_filter.clone())
        .map(|address: String, state: SharedL2State| {
            let state_guard = state.lock().unwrap();
            let balance = state_guard.balances.get(&address).unwrap_or(&0);
            
            warp::reply::json(&serde_json::json!({
                "address": address,
                "balance": balance
            }))
        });

    // ====================================================================
    // 💸 POST /send : Envoyer une transaction L2
    // ====================================================================
    let send_tx = warp::path!("send")
        .and(warp::post())
        .and(warp::body::json())
        .and(state_filter.clone())
        .map(|tx: L2Transaction, state: SharedL2State| {
            
            // 1. Vérification Cryptographique WOTS+
            let hash = tx.hash_data();
            if let Ok(sig) = serde_json::from_str::<WotsSignature>(&tx.signature) {
                if !WotsKeyPair::verify(&tx.sender_pubkey, &sig, &hash) {
                    return warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({"error": "Signature Invalide"})),
                        warp::http::StatusCode::BAD_REQUEST,
                    );
                }
            } else {
                return warp::reply::with_status(
                    warp::reply::json(&serde_json::json!({"error": "Signature corrompue"})),
                    warp::http::StatusCode::BAD_REQUEST,
                );
            }

            // 2. Ajout au Mempool de la L2
            let mut state_guard = state.lock().unwrap();
            state_guard.mempool.push(tx.clone());

            println!("📥 [L2 MEMPOOL] Nouvelle TX reçue : {} vers {} (Montant: {}, Frais: {})", 
                &tx.sender_pubkey[..10], &tx.receiver_address[..10], tx.amount, tx.fee);

            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"success": true, "message": "Transaction L2 acceptée !"})),
                warp::http::StatusCode::OK,
            )
        });

    let cors = warp::cors().allow_any_origin().allow_headers(vec!["content-type"]).allow_methods(vec!["GET", "POST"]);
    let routes = get_status.or(get_balance).or(send_tx).with(cors);

    println!("🌐 [L2 API] Serveur RPC Démarré sur http://127.0.0.1:{}", port);
    warp::serve(routes).run(([127, 0, 0, 1], port)).await;
}