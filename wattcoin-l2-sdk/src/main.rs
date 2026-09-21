use dotenv::dotenv;
use reqwest::Client;
use serde_json::Value;
use sha2::Digest;
use std::env;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use wattcoin_core::transaction::{Transaction, TransactionType};


// On importe nos nouveaux modules !
use wattcoin_l2_sdk::api::start_api_server;
use wattcoin_l2_sdk::state::L2State;

#[derive(serde::Serialize, serde::Deserialize)]
struct SequencerKeys {
    public_key: String,
    secret_key_hex: String,
}



fn decode_wots_sk(hex_str: &str) -> Vec<[u8; 32]> {
    let bytes = hex::decode(hex_str).unwrap();
    let mut sk = Vec::new();
    for chunk in bytes.chunks_exact(32) {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(chunk);
        sk.push(arr);
    }
    sk
}



#[tokio::main]
async fn main() {
    dotenv().ok();
    let l1_node_url = env::var("L1_NODE_URL").unwrap_or_else(|_| "http://127.0.0.1:8100".to_string());
    let l2_name = env::var("L2_NAME").expect("❌ ERREUR : La variable L2_NAME est requise dans le .env !");
    let block_time = env::var("BLOCK_TIME_SECONDS").unwrap_or_else(|_| "15".to_string()).parse::<u64>().unwrap_or(15);
    let l2_api_port = env::var("L2_API_PORT").unwrap_or_else(|_| "8200".to_string()).parse::<u16>().unwrap_or(8200);
	let premine_pubkey = env::var("PREMINE_PUBKEY").ok();
    let premine_amount = env::var("PREMINE_AMOUNT").unwrap_or_else(|_| "0".to_string()).parse::<u64>().unwrap_or(0);
    // AJOUT DE LA LECTURE DU REWARD :
    let l2_block_reward = env::var("L2_BLOCK_REWARD").unwrap_or_else(|_| "50".to_string()).parse::<u64>().unwrap_or(50);

    println!("🚀 Démarrage du Séquenceur L2 [{}]...", l2_name);

    // 1. Initialisation de la Base de Données Partagée (Avec chargement disque !)
    let db_path = "l2_state.json";
    let state = Arc::new(Mutex::new(
        L2State::load_from_disk(db_path)
            .unwrap_or_else(|| L2State::new(premine_pubkey, premine_amount, l2_block_reward))
    ));

    // 2. Démarrage de l'API L2 (En arrière-plan)
    let state_clone = Arc::clone(&state);
    tokio::spawn(async move {
        start_api_server(l2_api_port, state_clone).await;
    });

    // 3. Gestion du Hot Wallet
    let hot_wallet = match fs::read_to_string("sequencer_keys.json").and_then(|data| serde_json::from_str::<SequencerKeys>(&data).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))) {
		Ok(keys) => keys,
		Err(_) => {
			println!("🔧 Génération du Hot Wallet Séquenceur (WOTS+)...");
			let mut seed = [0u8; 32];
			rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut seed);
			let keys_wots = wots::Wots::generate_keypair(&seed, 0);
			let mut sk_hex = String::new();
			for chunk in keys_wots.0 { sk_hex.push_str(&hex::encode(chunk)); }
			
			let keys = SequencerKeys { public_key: hex::encode(&keys_wots.1), secret_key_hex: sk_hex };
			fs::write("sequencer_keys.json", serde_json::to_string(&keys).unwrap()).unwrap();
			keys
		}
	};
	let pubkey = hot_wallet.public_key.clone();
    
    // ==========================================================
    // AJOUT : L'AFFICHAGE INDISPENSABLE DE LA CLÉ !
    // ==========================================================
    println!("=====================================================");
    println!("🔑 MA CLÉ PUBLIQUE (HOT WALLET) : \n{}", pubkey);
    println!("👉 ACTION REQUISE : Copiez cette clé et allez 'Staker' sur le L1 avec le nom '{}' !", l2_name);
    println!("=====================================================\n");

    let client = Client::new();

    // 4. Boucle d'Ancrage Principale
    loop {
        // PETITE ASTUCE UX : On dit à l'utilisateur ce qu'on fait avant de dormir 15 secondes
        println!("⏳ Vérification des droits Séquenceur sur le L1 (Attente {}s)...", block_time);
        tokio::time::sleep(Duration::from_secs(block_time)).await;

        // A. Vérification des droits sur le L1 (Le Tribunal)
        let status_url = format!("{}/l2/status/{}", l1_node_url, l2_name);
        if let Ok(res) = client.get(&status_url).send().await {
            if let Ok(json) = res.json::<Value>().await {
                let is_active = json["is_active"].as_bool().unwrap_or(false);
                let onchain_pubkey = json["sequencer_pubkey"].as_str().unwrap_or("");

                if !is_active || onchain_pubkey != pubkey {
                    println!("🛑 ARRÊT : La L2 est désactivée OU le VRF a élu un autre Séquenceur !");
                    continue; // On ne quitte pas le programme, on attend notre tour !
                }
            } else { continue; }
        } else { continue; }

        // B. Exécution des Transactions et calcul du State Root
        let (state_root, block_idx, tx_count, fees) = {
            let mut state_guard = state.lock().unwrap();
            let (idx, count, f) = state_guard.process_mempool(&pubkey); 
            let root = state_guard.compute_state_root();
            
            // ON SAUVEGARDE L'ÉTAT SUR LE DISQUE DUR !
            state_guard.save_to_disk(db_path);
            
            (root, idx, count, f)
        };
		
		// On passe notre tour s'il n'y a rien à faire !
        if tx_count == 0 {
            continue; 
        }

        println!("=====================================================");
        println!("⛏️  NOUVEAU MICRO-BLOC L2 FORGÉ ! (Index: #{})", block_idx);
        println!("🎁 Coinbase (Création)      : {} coins", l2_block_reward);
        println!("📝 Transactions L2 incluses : {}", tx_count);
        println!("💰 Frais récoltés           : {} jetons", fees);
        println!("🌳 State Root (Racine)      : {}", &state_root[..32]); // Affichage raccourci pour la propreté
        println!("⚓  Envoi de l'ancrage au L1 en cours...");
        println!("=====================================================\n");

        // C. Signature WOTS+
		let mut hasher = sha2::Sha256::new();
		hasher.update(state_root.as_bytes());
		let mut hash_array = [0u8; 32];
		hash_array.copy_from_slice(&hasher.finalize());
		
		let secret_matrix = decode_wots_sk(&hot_wallet.secret_key_hex);
		let public_key_bytes = hex::decode(&pubkey).unwrap_or_default();
		let wots_sig = wots::Wots::sign(&secret_matrix, block_idx, &hash_array, &public_key_bytes);
		let signature_hex = serde_json::to_string(&wots_sig).unwrap();

		// D. Envoi au L1
		let mut anchor_tx = Transaction {
			tx_type: TransactionType::L2Anchor {
				l2_name: l2_name.clone(),
				state_root: state_root.clone(),
				sequencer_signature: signature_hex,
				withdrawals: vec![],
			},
			inputs: vec![],
			outputs: vec![],
			fee: 0, // Sera remplacé
			wots_signature: None, // 👈 Remplace `lattice_signature: None`
			public_key: pubkey.clone(),
		};

		// Frais dynamiques
		let tx_weight_bytes = bincode::serialized_size(&anchor_tx).unwrap_or(0) as usize;
		let weight_kb = (tx_weight_bytes as f64 / 1024.0).ceil() as u64;
		anchor_tx.fee = std::cmp::max(1000, weight_kb * 20);

        // On envoie le bloc d'ancrage en BINAIRE PUR (Bincode) !
		let tx_bytes = bincode::serialize(&anchor_tx).expect("Erreur de sérialisation binaire");
		let _ = client.post(&format!("{}/send_tx", l1_node_url))
			.header("Content-Type", "application/octet-stream")
			.body(tx_bytes)
			.send().await;
    }
}