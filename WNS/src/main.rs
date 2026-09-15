use dotenv::dotenv;
use reqwest::Client;
use serde_json::Value;
use sha2::{Digest, Sha256}; // SHA-256 pour WOTS+
use std::env;
use std::fs;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::collections::HashMap;

use wattcoin_core::transaction::{Transaction, TransactionType, TransactionOutput};


use wattcoin_name_service::api::start_api_server;
use wattcoin_name_service::state::L2State;
use wattcoin_name_service::network::{start_wns_p2p_server, ActiveWnsPeers, WnsP2PMessage};
use wattcoin_name_service::transaction::WnsBlock;

#[derive(serde::Serialize, serde::Deserialize)]
struct SequencerKeys {
    public_key: String,
    secret_key_hex: String, // Stocké en Hexa (32 * 34 = 1088 bytes)
}

// Outil interne pour décoder la clé WOTS+ sauvegardée
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
	let l2_p2p_port = env::var("L2_P2P_PORT").unwrap_or_else(|_| "8201".to_string()).parse::<u16>().unwrap_or(8201);
    let l2_api_port = env::var("L2_API_PORT").unwrap_or_else(|_| "8200".to_string()).parse::<u16>().unwrap_or(8200);
	let l2_seed_node = env::var("L2_SEED_NODE").unwrap_or_else(|_| "".to_string());

    println!("🚀 Démarrage du Séquenceur L2 [{}]...", l2_name);

    let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let db_path = format!("{}/.wattcoin/wns_db", home_dir);
    
    let state = Arc::new(Mutex::new(
        L2State::load_from_disk(&db_path).unwrap_or_else(|| L2State::new(&db_path))
    ));
	
    let active_wns_peers: ActiveWnsPeers = Arc::new(Mutex::new(HashMap::new()));
    let state_p2p = Arc::clone(&state);
    let active_p2p = Arc::clone(&active_wns_peers);
    
    tokio::spawn(async move {
        start_wns_p2p_server(l2_p2p_port, state_p2p, active_p2p).await;
    });
	
    if !l2_seed_node.is_empty() {
        let state_client = Arc::clone(&state);
        let active_client = Arc::clone(&active_wns_peers);
        let seed_ip_clone = l2_seed_node.clone();
        
        tokio::spawn(async move {
            println!("🔄 Tentative de connexion au Séquenceur racine : {}", seed_ip_clone);
            if let Ok(socket) = tokio::net::TcpStream::connect(&seed_ip_clone).await {
                println!("✅ Connecté au Séquenceur WNS Racine ({}) !", seed_ip_clone);
                wattcoin_name_service::network::start_peer_connection(
                    socket, seed_ip_clone, state_client, active_client
                );
            } else {
                println!("⚠️ Impossible de joindre le nœud racine WNS.");
            }
        });
    }

    let state_clone = Arc::clone(&state);
    let api_peers = Arc::clone(&active_wns_peers); 
    
    tokio::spawn(async move {
        start_api_server(l2_api_port, state_clone, api_peers).await; 
    });

    // 💡  VRAI WOTS+
    let hot_wallet = match fs::read_to_string("sequencer_keys.json").and_then(|data| serde_json::from_str::<SequencerKeys>(&data).map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))) {
        Ok(keys) => keys,
        Err(_) => {
            println!("🔧 Anciennes clés ou fichier introuvable. Génération du Hot Wallet Séquenceur (WOTS+)...");
            let mut seed = [0u8; 32];
            rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut seed);
            
            let keys_wots = wots::Wots::generate_keypair(&seed, 0);
            
            let mut sk_hex = String::new();
            for chunk in keys_wots.0 { sk_hex.push_str(&hex::encode(chunk)); }
            
            // La clé publique est le 2ème élément du tuple (keys_wots.1)
            let keys = SequencerKeys { public_key: hex::encode(&keys_wots.1), secret_key_hex: sk_hex };
            fs::write("sequencer_keys.json", serde_json::to_string(&keys).unwrap()).unwrap();
            keys
        }
    };

    let pubkey = hot_wallet.public_key.clone();
    
    println!("=====================================================");
    println!("🔑 MA CLÉ PUBLIQUE (HOT WALLET) : \n{}", pubkey);
    println!("👉 ACTION REQUISE : Copiez cette clé et allez 'Staker' sur le L1 avec le nom '{}' !", l2_name);
    println!("=====================================================\n");

    let client = Client::new();
	
    let scanner_state = Arc::clone(&state);
    let scanner_client = Client::new();
    let scanner_l1_url = l1_node_url.clone();
    
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await; 
            
            let last_l1 = { scanner_state.lock().unwrap().last_l1_block };
            let sync_url = format!("{}/sync_blocks?last_l1={}&last_l2=0", scanner_l1_url, last_l1);
            
            if let Ok(res) = scanner_client.get(&sync_url).send().await {
                if let Ok(new_txs) = res.json::<Vec<Value>>().await {
                    let mut state_guard = scanner_state.lock().unwrap();
                    let mut highest_block = last_l1;
                    let mut minted_something = false;

                    for item in new_txs {
                        let height = item["height"].as_u64().unwrap_or(0);
                        if height > highest_block { highest_block = height; }
                        
                        let tx = &item["transaction"];
                        let tx_type = &tx["tx_type"];
                        
                        if tx_type["L2BridgeLock"].is_object() {
                            let lock_data = &tx_type["L2BridgeLock"];
                            let target_name = lock_data["l2_target_name"].as_str().unwrap_or("");
                            
							if target_name == "WNS" {
								let receiver_raw = lock_data["l2_receiver_pubkey"].as_str().unwrap_or("");
								
								let parts: Vec<&str> = receiver_raw.split('|').collect();
								let account_address = parts[0].to_string();
								let first_wots_key = if parts.len() > 1 { parts[1].to_string() } else { account_address.clone() };
								
								if let Some(outputs) = tx["outputs"].as_array() {
									if !outputs.is_empty() {
										let amount_str = outputs[0]["aes_vault"].as_str().unwrap_or("0");
										let amount: u64 = amount_str.parse().unwrap_or(0);
										
										if amount > 0 {
											let account = state_guard.accounts.entry(account_address.clone()).or_insert(wattcoin_name_service::state::L2Account {
												balance: 0,
                                                nonce: 0, // 💡 Initialisation du Nonce à Zéro
												authorized_wots_key: first_wots_key, 
											});
											account.balance += amount;
											
											println!("💸 [BRIDGE MINT] Dépôt L1 détecté ! {} Flames crédités...", amount);
											minted_something = true;
										}
									}
								}
							}
                        }
                    }

                    if highest_block > last_l1 {
                        state_guard.last_l1_block = highest_block;
                        if minted_something {
                            state_guard.save_to_disk();
                        }
                    }
                }
            }
        }
    });
	
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

		let (state_root, block_idx, valid_txs, fees, withdrawals_l2) = {
            let mut state_guard = state.lock().unwrap();
            
            if state_guard.mempool.is_empty() {
                (String::new(), state_guard.block_index, vec![], 0, vec![])
            } else {
                let (idx, txs, f, w) = state_guard.process_mempool(&pubkey); 
                let root = state_guard.compute_state_root();
                state_guard.save_to_disk();
                (root, idx, txs, f, w)
            }
        };

        let tx_count = valid_txs.len(); 
        if tx_count == 0 {
            continue; 
        }

        println!("=====================================================");
        println!("⛏️  NOUVEAU MICRO-BLOC WNS FORGÉ ! (Index: #{})", block_idx);
        println!("📝 Enregistrements/Mises à jour : {}", tx_count);
        println!("💰 Frais récoltés               : {} Flames", fees); 
        println!("🌳 Racine du Registre (Root)    : {}", &state_root[..32]); 
        println!("⚓  Envoi de l'ancrage au L1 en cours...");
        println!("=====================================================\n");

        let mut hasher = Sha256::new(); // 💡 WOTS+ veut du SHA-256 !
        hasher.update(state_root.as_bytes());
        let mut hash_array = [0u8; 32];
        hash_array.copy_from_slice(&hasher.finalize());
        
		// 💡 VRAIE SIGNATURE WOTS+ PAR LE SÉQUENCEUR
        let secret_matrix = decode_wots_sk(&hot_wallet.secret_key_hex);
        let public_key_bytes = hex::decode(&pubkey).unwrap_or_default();
        
        // On signe avec WOTS+ (Wots::sign attend la matrice secrète, l'index, le hash 32 octets et la clé publique)
        let wots_sig = wots::Wots::sign(&secret_matrix, block_idx, &hash_array, &public_key_bytes);
        
        // On sérialise la structure complète de la signature en JSON pour le champ `signature` (qui est une String)
        let signature_hex = serde_json::to_string(&wots_sig).unwrap();
		
        let mut l1_withdrawals = Vec::new();
        for (l1_addr, amt) in withdrawals_l2 {
            l1_withdrawals.push(TransactionOutput {
                stealth_address: l1_addr,
                kyber_capsule: format!("UNPEG_WNS_{}", block_idx),
                aes_vault: amt.to_string(),
                lattice_commitment: wattcoin_core::lattice::LWECommitment::commit(amt, &[0u64; wattcoin_core::lattice::LATTICE_DIM]),
            });
        }

        let anchor_tx = Transaction {
            tx_type: TransactionType::L2Anchor {
                l2_name: l2_name.clone(),
                state_root: state_root.clone(),
                sequencer_signature: signature_hex.clone(), // Stocké en Hexadécimal pur ou binaire bincode
                withdrawals: l1_withdrawals, 
            },
            inputs: vec![],
            outputs: vec![],
            fee: 1000,
            wots_signature: None,
            public_key: pubkey.clone(),
        };

        let tx_bytes = bincode::serialize(&anchor_tx).expect("Erreur sérialisation L2Anchor");
        let _ = client.post(&format!("{}/send_tx", l1_node_url))
            .header("Content-Type", "application/octet-stream")
            .body(tx_bytes)
            .send().await;
		
		let new_block = WnsBlock {
			index: block_idx,
			l1_parent_hash: "L1_HASH_ICI".to_string(),
			state_root: state_root.clone(),
			sequencer_pubkey: pubkey.clone(),
			transactions: valid_txs,
			signature: signature_hex.clone(),
		};

		let msg = WnsP2PMessage::BroadcastBlock { block: new_block };
		wattcoin_name_service::network::broadcast_message(&msg, &active_wns_peers, "");
    }
}