use warp::Filter;
use crate::state::SharedDarkpoolState;
use wattcoin_core::transaction::Transaction;

pub async fn start_api_server(port: u16, state: SharedDarkpoolState) {
    let state_filter = warp::any().map(move || state.clone());

    let get_status = warp::path!("status")
        .and(warp::get())
        .map(|| warp::reply::json(&serde_json::json!({"status": "Séquenceur Darkpool En Ligne"})));

    let send_tx = warp::path!("send_tx")
        .and(warp::post())
        .and(warp::body::json())
        .and(state_filter.clone())
        .map(|tx: Transaction, state: SharedDarkpoolState| {
            
            // 1. Le Séquenceur utilise le Tribunal Quantique du L1 pour valider l'anonymat !
            if !tx.is_valid() {
                return warp::reply::with_status(
                    warp::reply::json(&serde_json::json!({"error": "Preuve Lattice ou Ring Signature Invalide"})),
                    warp::http::StatusCode::BAD_REQUEST,
                );
            }

            let mut state_guard = state.lock().unwrap();
            
            // 2. Anti-double dépense immédiat dans le Mempool
            for input in &tx.inputs {
                if state_guard.spent_key_images.contains(&input.mpc_ring.key_image) {
                    return warp::reply::with_status(
                        warp::reply::json(&serde_json::json!({"error": "Double Dépense détectée !"})),
                        warp::http::StatusCode::CONFLICT,
                    );
                }
            }

            // 3. Ajout au Mempool
            state_guard.mempool.push(tx);
            println!("🥷 [DARKPOOL MEMPOOL] Nouvelle Transaction Intraçable reçue et vérifiée !");

            warp::reply::with_status(
                warp::reply::json(&serde_json::json!({"success": true, "message": "Transaction Darkpool acceptée !"})),
                warp::http::StatusCode::OK,
            )
        });

    let cors = warp::cors().allow_any_origin().allow_headers(vec!["content-type"]).allow_methods(vec!["GET", "POST"]);
    let routes = get_status.or(send_tx).with(cors);

    println!("🥷 [L2 API] Serveur RPC Darkpool Démarré sur http://127.0.0.1:{}", port);
    warp::serve(routes).run(([127, 0, 0, 1], port)).await;
}