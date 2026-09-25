use dotenv::dotenv;
use reqwest::Client;
use serde_json::Value;
use sha2::{Digest, Sha256}; 
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
    let l2_p2p_port = env::var("L2_P2P_PORT").unwrap_or_else(|_| "8201".to_string()).parse::<u16>().unwrap_or(8201);
    let l2_api_port = env::var("L2_API_PORT").unwrap_or_else(|_| "8200".to_string()).parse::<u16>().unwrap_or(8200);

    println!("🚀 Démarrage du Séquenceur L2 [{}]...", l2_name);

    let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let base_dir = format!("{}/.wattcoin", home_dir);
    let db_path = format!("{}/wns_db", base_dir); 
    std::fs::create_dir_all(&db_path).unwrap(); 
    
    // ==============================================================
    // 1. SÉCURITÉ MIXNET : Identité Kyber du Nœud WNS
    // ==============================================================
    let kyber_sec_path = format!("{}/wns_kyber.secret", base_dir);
    let kyber_pub_path = format!("{}/wns_kyber.pub", base_dir);
    
    let node_kyber_secret = if std::path::Path::new(&kyber_sec_path).exists() {
        std::fs::read_to_string(&kyber_sec_path).unwrap().trim().to_string()
    } else {
        println!("🔑 Première exécution WNS : Génération de l'identité quantique du Nœud Relais...");
        let mut rng = rand::thread_rng();
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD}; 
        let keys = pqc_kyber::keypair(&mut rng).expect("Erreur génération Kyber");
        let sec_hex = hex::encode(keys.secret); 
        let pub_hex = URL_SAFE_NO_PAD.encode(keys.public); 
        
        std::fs::write(&kyber_sec_path, &sec_hex).unwrap();
        std::fs::write(&kyber_pub_path, &pub_hex.clone()).unwrap();
        
        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(mut perms) = std::fs::metadata(&kyber_sec_path).map(|m| m.permissions()) {
                perms.set_mode(0o600); 
                let _ = std::fs::set_permissions(&kyber_sec_path, perms);
            }
        }
        
        sec_hex
    };
    
    let node_kyber_pub = std::fs::read_to_string(&kyber_pub_path).unwrap().trim().to_string();
    println!("============================================================");
    println!("🧅 IDENTITÉ MIXNET ET RÉSERVOIR D'ESSENCE WNS !");
    println!("Envoyez des WATT (Virement normal) sur cette adresse Kyber pour financer les ancrages :");
    println!("{}", node_kyber_pub);
    println!("============================================================\n");

    let state = Arc::new(Mutex::new(
        L2State::load_from_disk(&db_path).unwrap_or_else(|| L2State::new(&db_path))
    ));
    
    let active_wns_peers: ActiveWnsPeers = Arc::new(Mutex::new(HashMap::new()));
    
    let state_p2p = Arc::clone(&state);
    let active_p2p = Arc::clone(&active_wns_peers);
    let p2p_kyber_secret = node_kyber_secret.clone();
    tokio::spawn(async move {
        start_wns_p2p_server(l2_p2p_port, state_p2p, active_p2p, p2p_kyber_secret).await;
    });

    let state_api = Arc::clone(&state);
    let api_peers = Arc::clone(&active_wns_peers); 
    tokio::spawn(async move {
        start_api_server(l2_api_port, state_api, api_peers).await; 
    });

    // ==============================================================
    // 2. SÉCURITÉ WOTS+ : Identité Séquenceur Chiffrée (AES-256-GCM)
    // ==============================================================
    let sequencer_password = env::var("SEQUENCER_PASSWORD").unwrap_or_else(|_| {
        println!("🔒 [SÉCURITÉ] Aucun SEQUENCER_PASSWORD détecté dans le .env.");
        rpassword::prompt_password("Tapez le mot de passe pour déchiffrer/créer le Séquenceur (la frappe est invisible) : ")
            .expect("Erreur lors de la lecture du mot de passe au clavier")
    });
    
    let sequencer_vault_path = format!("{}/wns_sequencer.vault", base_dir);

    use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
    use rand::RngCore;

    let derive_key = |pwd: &str, salt: &[u8]| -> [u8; 32] {
        let mut current_hash = sha2::Sha256::digest(format!("{}:{:?}", pwd, salt).as_bytes()).to_vec();
        for _ in 0..10_000 {
            current_hash = sha2::Sha256::digest(&current_hash).to_vec();
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&current_hash[0..32]);
        key
    };

    let hot_wallet = if std::path::Path::new(&sequencer_vault_path).exists() {
        println!("🔓 Déchiffrement du Hot Wallet Séquenceur en cours...");
        
        let file_data = fs::read(&sequencer_vault_path).expect("Impossible de lire le vault.");
        if file_data.len() < 28 { panic!("Fichier vault corrompu."); }

        let salt = &file_data[0..16];
        let nonce_bytes = &file_data[16..28];
        let ciphertext = &file_data[28..];

        let key_bytes = derive_key(&sequencer_password, salt);
        let cipher = Aes256Gcm::new(&key_bytes.into());
        
        #[allow(deprecated)]
        let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);

        let plaintext = cipher.decrypt(nonce, ciphertext)
            .expect("❌ Mot de passe du Séquenceur incorrect ou coffre altéré.");
        
        let json_string = String::from_utf8(plaintext).expect("Erreur UTF-8");
        serde_json::from_str::<SequencerKeys>(&json_string).expect("JSON invalide")
    } else {
        println!("🔧 Première exécution : Génération du Hot Wallet Séquenceur (WOTS+)...");
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        
        let keys_wots = wots::Wots::generate_keypair(&seed, 0);
        let mut sk_hex = String::new();
        for chunk in keys_wots.0 { sk_hex.push_str(&hex::encode(chunk)); }
        
        let keys = SequencerKeys { public_key: hex::encode(&keys_wots.1), secret_key_hex: sk_hex };
        let keys_json = serde_json::to_string(&keys).unwrap();

        println!("🔒 Chiffrement AES-256-GCM de l'identité WOTS+...");
        let mut salt = [0u8; 16]; rand::thread_rng().fill_bytes(&mut salt);
        
        let key_bytes = derive_key(&sequencer_password, &salt);
        let cipher = Aes256Gcm::new(&key_bytes.into());
        
        let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
        
        #[allow(deprecated)]
        let nonce = aes_gcm::Nonce::from_slice(&nonce_bytes);
        
        let ciphertext = cipher.encrypt(nonce, keys_json.as_bytes()).unwrap();
        
        let mut final_data = Vec::new();
        final_data.extend_from_slice(&salt); 
        final_data.extend_from_slice(&nonce_bytes); 
        final_data.extend_from_slice(&ciphertext);
        
        fs::write(&sequencer_vault_path, final_data).unwrap();

        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(mut perms) = std::fs::metadata(&sequencer_vault_path).map(|m| m.permissions()) {
                perms.set_mode(0o600); 
                let _ = std::fs::set_permissions(&sequencer_vault_path, perms);
            }
        }
        keys
    };

    let pubkey = hot_wallet.public_key.clone(); 
    
    println!("=====================================================");
    println!("🔑 CLÉ PUBLIQUE D'AUTORITÉ WOTS+ : \n{}", pubkey);
    println!("👉 ACTION REQUISE : Copiez cette clé et allez 'Staker' 100 WATT sur le L1 avec le nom '{}' !", l2_name);
    println!("=====================================================\n");

    let client = Client::new();
    
    let scanner_state = Arc::clone(&state);
    let scanner_client = Client::new();
    let scanner_l1_url = l1_node_url.clone();
    
    // ==============================================================
    // 3. SCANNER L1 (Bridge Lock -> WNS Deposit)
    // ==============================================================
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
                                                nonce: 0,
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

    // 💡 MÉMOIRE PERSISTANTE (SLED) DES UTXOS DÉJÀ DÉPENSÉS (CRASH-SAFE)
    let db_handle = {
        let state_guard = state.lock().unwrap();
        state_guard.db.clone().expect("La DB Sled n'est pas initialisée")
    };
    let spent_utxos_tree = db_handle.open_tree("spent_l1_utxos").expect("Impossible d'ouvrir l'arbre des UTXOs");

    // ==============================================================
    // 4. BOUCLE PRINCIPALE : L2 ANCHOR & AUTO-SIPHON
    // ==============================================================
    loop {
        println!("⏳ Séquenceur WNS en veille pour {} secondes (Attente du prochain tour)...", block_time);
        tokio::time::sleep(Duration::from_secs(block_time)).await;

        let status_url = format!("{}/l2/status/{}", l1_node_url, l2_name);
        if let Ok(res) = client.get(&status_url).send().await {
            if let Ok(json) = res.json::<Value>().await {
                let is_active = json["is_active"].as_bool().unwrap_or(false);
                let onchain_pubkey = json["sequencer_pubkey"].as_str().unwrap_or("");

                if !is_active {
                    println!("⏸️ Le réseau L2 '{}' est inactif (Aucun Staker détecté sur le L1).", l2_name);
                    continue; 
                }

                if onchain_pubkey != pubkey {
                    println!("🎲 Le Tribunal VRF a élu un autre séquenceur ({}...).\nOu vous n'avez pas stake pour pouvoir être élu, on passe notre tour.", &onchain_pubkey[0..15]);
                    continue; 
                }
                
                println!("👑 SUCCÈS ! Le VRF a élu NOTRE Nœud comme Séquenceur pour ce tour !");

            } else { 
                println!("❌ Erreur de lecture JSON du statut L1.");
                continue; 
            }
        } else { 
            println!("📡 Attente de la connexion avec le Nœud L1...");
            continue; 
        }

        let (state_root, block_idx, valid_txs, fees, mut withdrawals_l2) = {
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
            println!("📭 Mempool L2 vide. Aucun bloc à forger, on économise le gaz !");
            continue; 
        }

        println!("=====================================================");
        println!("⛏️  NOUVEAU MICRO-BLOC WNS FORGÉ ! (Index: #{})", block_idx);
        println!("📝 Enregistrements/Mises à jour : {}", tx_count);
        println!("💰 Frais récoltés (Sur le L2)   : {} Flames", fees); 
        println!("🌳 Racine du Registre (Root)    : {}", &state_root[..32]); 
        println!("=====================================================\n");

        let estimated_weight_kb = 100;
        let l1_fee_sats = std::cmp::max(1000, estimated_weight_kb * 20); 

        // AUTO-SIPHON : Le Séquenceur rapatrie ses gains du L2 vers le L1
        let mut my_l2_balance = 0;
        {
            let state_guard = state.lock().unwrap();
            if let Some(acc) = state_guard.accounts.get(&pubkey) {
                my_l2_balance = acc.balance;
            }
        }
        
        let reserve_l2 = 1_000_000_000; // Il laisse toujours 1 WATT sur le L2
        if my_l2_balance > reserve_l2 + l1_fee_sats {
            let amount_to_withdraw = my_l2_balance - reserve_l2;
            withdrawals_l2.push((pubkey.clone(), amount_to_withdraw));
            
            let mut state_guard = state.lock().unwrap();
            if let Some(acc) = state_guard.accounts.get_mut(&pubkey) {
                acc.balance -= amount_to_withdraw;
            }
            println!("🔄 [AUTO-SIPHON] Le séquenceur rapatrie {} Flames du L2 vers le L1.", amount_to_withdraw);
        }

        // PAIEMENT L1 (RÉSERVOIR D'ESSENCE)
        let utxos_url = format!("{}/all_transactions", l1_node_url);
        let mut selected_inputs = Vec::new();
        let mut total_l1_collected = 0u64;
        let mut newly_spent_utxos = Vec::new(); // 💡 Déclaré ici au bon niveau de scope !

        if let Ok(res) = client.get(&utxos_url).send().await {
            if let Ok(all_txs) = res.json::<Vec<serde_json::Value>>().await {
                
                let wns_kyber_sk = hex::decode(&node_kyber_secret).unwrap_or_default();

                for item in all_txs {
                    let height = item["height"].as_u64().unwrap_or(0);
                    let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
                    if is_l2 { continue; } 

                    let tx_json = item["transaction"].clone();
                    if let Ok(tx) = serde_json::from_value::<Transaction>(tx_json) {
                        for out in tx.outputs {
                            
                            // 💡 ON INTERROGE SLED : Est-ce que ce billet L1 a déjà été consommé ?
                            let utxo_id = out.kyber_capsule.clone();
                            if spent_utxos_tree.contains_key(&utxo_id).unwrap_or(false) {
                                continue; 
                            }

                            let mut is_my_gas = false;
                            let mut amt_collected = 0u64;

                            if out.stealth_address == pubkey {
                                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                                    amt_collected = amt;
                                    is_my_gas = true;
                                }
                            } else if out.stealth_address.starts_with("pq_watt_") {
                                if let Ok(capsule) = hex::decode(&out.kyber_capsule) {
                                    if let Ok(shared_secret) = pqc_kyber::decapsulate(&capsule, &wns_kyber_sk) {
                                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                                            if vault_bytes.len() > 12 {
                                                use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
                                                let cipher = Aes256Gcm::new(&shared_secret.into());
                                                let mut n_arr = [0u8; 12];
                                                n_arr.copy_from_slice(&vault_bytes[0..12]);
                                                
                                                #[allow(deprecated)]
                                                let nonce = aes_gcm::Nonce::from_slice(&n_arr);
                                                
                                                if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                                                    if let Ok(payload_str) = String::from_utf8(plaintext) {
                                                        let parts: Vec<&str> = payload_str.split('|').collect();
                                                        if parts.len() >= 2 {
                                                            if let Ok(amt) = parts[0].parse::<u64>() {
                                                                amt_collected = amt;
                                                                is_my_gas = true;
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            if is_my_gas && amt_collected > 0 {
                                selected_inputs.push(wattcoin_core::transaction::TransactionInput {
                                    commitment: out.lattice_commitment.clone(),
                                    source_height: height,
                                });
                                total_l1_collected += amt_collected;
                                newly_spent_utxos.push(utxo_id); // 💡 On l'enregistre pour Sled
                                break; 
                            }
                        }
                    }
                    if total_l1_collected >= l1_fee_sats { break; }
                }
            }
        }

        if total_l1_collected < l1_fee_sats {
            println!("🛑 [ÉCHEC ANCRAGE] Le Réservoir L1 est vide ! (Requis: {} Flames).", l1_fee_sats);
            let mut state_guard = state.lock().unwrap();
            state_guard.block_index -= 1; // Rollback simple
            continue;
        }

        let change_l1 = total_l1_collected - l1_fee_sats;

        let mut hasher = Sha256::new();
        hasher.update(state_root.as_bytes());
        let mut hash_array = [0u8; 32];
        hash_array.copy_from_slice(&hasher.finalize());
        
        let secret_matrix = decode_wots_sk(&hot_wallet.secret_key_hex);
        let public_key_bytes = hex::decode(&pubkey).unwrap_or_default();
        
        let wots_sig = wots::Wots::sign(&secret_matrix, block_idx, &hash_array, &public_key_bytes);
        let signature_hex = serde_json::to_string(&wots_sig).unwrap();
        
        let mut final_outputs = Vec::new();
        
        for (l1_addr, amt) in withdrawals_l2 {
            final_outputs.push(TransactionOutput {
                stealth_address: l1_addr,
                kyber_capsule: format!("UNPEG_WNS_{}", block_idx),
                aes_vault: amt.to_string(),
                lattice_commitment: wattcoin_core::lattice::LWECommitment::commit(amt, &[0u64; wattcoin_core::lattice::LATTICE_DIM]),
            });
        }

        if change_l1 > 0 {
            final_outputs.push(TransactionOutput {
                stealth_address: pubkey.clone(),
                kyber_capsule: format!("CHANGE_WNS_{}", block_idx),
                aes_vault: change_l1.to_string(),
                lattice_commitment: wattcoin_core::lattice::LWECommitment::commit(change_l1, &[0u64; wattcoin_core::lattice::LATTICE_DIM]),
            });
        }

        let mut anchor_tx = Transaction {
            tx_type: TransactionType::L2Anchor {
                l2_name: l2_name.clone(),
                state_root: state_root.clone(),
                sequencer_signature: signature_hex.clone(), 
                withdrawals: Vec::new(), 
            },
            inputs: selected_inputs,
            outputs: final_outputs,
            fee: l1_fee_sats,
            wots_signature: None,
            public_key: pubkey.clone(),
        };

        let tx_hash_64 = anchor_tx.hash_data();
        let mut tx_hash_32 = [0u8; 32];
        tx_hash_32.copy_from_slice(&tx_hash_64[0..32]);
        anchor_tx.wots_signature = Some(wots::Wots::sign(&secret_matrix, block_idx, &tx_hash_32, &public_key_bytes));

        println!("⚓ Envoi de l'ancrage au L1 en cours (Frais payés : {} Flames)...", l1_fee_sats);

        let tx_bytes = bincode::serialize(&anchor_tx).expect("Erreur sérialisation L2Anchor");
        match client.post(&format!("{}/send_tx", l1_node_url))
            .header("Content-Type", "application/octet-stream")
            .body(tx_bytes)
            .send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        
                        // 💡 L'ANCRAGE A RÉUSSI : On grave les UTXOs dans Sled
                        for used_utxo in newly_spent_utxos {
                            let _ = spent_utxos_tree.insert(&used_utxo, &[]);
                        }
                        let _ = spent_utxos_tree.flush(); // Protection Crash-Safe

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
                        println!("✅ Ancrage accepté par le réseau L1 !");
                    } else {
                        println!("❌ Rejeté par le réseau L1 : {}", resp.text().await.unwrap_or_default());
                        let mut state_guard = state.lock().unwrap();
                        state_guard.block_index -= 1;
                    }
                },
                Err(e) => {
                    println!("❌ Impossible de joindre le réseau L1 : {}", e);
                }
            }
    }
}