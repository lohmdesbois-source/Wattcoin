use dotenv::dotenv;
use reqwest::Client;
use serde_json::Value;
use sha2::{Digest, Sha512};
use std::env;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wattcoin_core::transaction::{Transaction, TransactionType};
use wattcoin_core::wots::WotsKeyPair;

use wattcoin_darkpool_sdk::api::start_api_server;
use wattcoin_darkpool_sdk::state::DarkpoolState; // 💡 On importe le nouveau state

#[tokio::main]
async fn main() {
    dotenv().ok();
    let l1_node_url = env::var("L1_NODE_URL").unwrap_or_else(|_| "http://127.0.0.1:8100".to_string());
    let l2_name = env::var("L2_NAME").expect("❌ ERREUR : La variable L2_NAME est requise dans le .env !");
    let block_time = env::var("BLOCK_TIME_SECONDS").unwrap_or_else(|_| "15".to_string()).parse::<u64>().unwrap_or(15);
    let l2_api_port = env::var("L2_API_PORT").unwrap_or_else(|_| "8200".to_string()).parse::<u16>().unwrap_or(8200);

    println!("🚀 Démarrage du Séquenceur L2 (Mode Darkpool) [{}]...", l2_name);

    let db_path = "darkpool_state.json";
    let state = Arc::new(Mutex::new(
        DarkpoolState::load_from_disk(db_path).unwrap_or_else(|| DarkpoolState::new())
    ));

    let state_clone = Arc::clone(&state);
    tokio::spawn(async move {
        start_api_server(l2_api_port, state_clone).await;
    });

    let hot_wallet = if let Ok(data) = fs::read_to_string("sequencer_keys.json") {
        serde_json::from_str::<WotsKeyPair>(&data).unwrap()
    } else {
        println!("🔧 Génération du Hot Wallet...");
        let keys = WotsKeyPair::generate();
        fs::write("sequencer_keys.json", serde_json::to_string(&keys).unwrap()).unwrap();
        keys
    };

    let pubkey = hot_wallet.public_key.clone();
    
    println!("=====================================================");
    println!("🔑 MA CLÉ PUBLIQUE (HOT WALLET) : \n{}", pubkey);
    println!("👉 ACTION REQUISE : Allez 'Staker' sur le L1 avec le nom '{}' !", l2_name);
    println!("=====================================================\n");

    let client = Client::new();

    loop {
        println!("⏳ Vérification des droits Séquenceur sur le L1 (Attente {}s)...", block_time);
        tokio::time::sleep(Duration::from_secs(block_time)).await;

        let status_url = format!("{}/l2/status/{}", l1_node_url, l2_name);
        if let Ok(res) = client.get(&status_url).send().await {
            if let Ok(json) = res.json::<Value>().await {
                let is_active = json["is_active"].as_bool().unwrap_or(false);
                let onchain_pubkey = json["sequencer_pubkey"].as_str().unwrap_or("");

                if !is_active || onchain_pubkey != pubkey {
                    println!("🛑 ARRÊT : La L2 est désactivée OU le VRF a élu un autre Séquenceur !");
                    continue; 
                }
            } else { continue; }
        } else { continue; }

        // B. Exécution (Vérification ZKP) et calcul du State Root
        let (state_root, block_idx, tx_count, fees) = {
            let mut state_guard = state.lock().unwrap();
            let (idx, count, f) = state_guard.process_mempool(); // Plus besoin d'adresse séquenceur ici !
            let root = state_guard.compute_state_root();
            
            state_guard.save_to_disk(db_path);
            
            (root, idx, count, f)
        };

        println!("=====================================================");
        println!("🥷  NOUVEAU MICRO-BLOC DARKPOOL FORGÉ ! (Index: #{})", block_idx);
        println!("📝 Transactions intraçables incluses : {}", tx_count);
        println!("💰 Frais brulés (Network)            : {} jetons", fees);
        // Sécurité : On n'affiche les 32 premiers caractères que si la chaîne est assez longue !
        let display_root = if state_root.len() >= 32 { &state_root[..32] } else { &state_root };
        println!("🌳 State Root (Racine des KeyImages) : {}...", display_root);
        println!("⚓  Ancrage ZKP sur le L1 Wattcoin en cours...");
        println!("=====================================================\n");

        // C. Signature (Le reste de ton code ne change pas)
        let mut hasher = Sha512::new();
        hasher.update(state_root.as_bytes());
        let mut hash_array = [0u8; 64];
        hash_array.copy_from_slice(&hasher.finalize());
        let wots_sig = WotsKeyPair::sign(&hot_wallet.secret_key, &hot_wallet.public_seed, &hash_array);

        let anchor_tx = Transaction {
            tx_type: TransactionType::L2Anchor {
                l2_name: l2_name.clone(),
                state_root: state_root.clone(),
                sequencer_signature: serde_json::to_string(&wots_sig).unwrap(),
            },
            inputs: vec![],
            outputs: vec![],
            fee: 1000,
            wots_signature: None,
            public_key: pubkey.clone(),
        };

        let tx_json = serde_json::to_string(&anchor_tx).unwrap();
        let _ = client.post(&format!("{}/send_tx", l1_node_url))
            .header("Content-Type", "application/json")
            .body(tx_json)
            .send().await;
    }
}