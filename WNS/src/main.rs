use dotenv::dotenv;
use reqwest::Client;
use serde_json::Value;
use sha2::Digest;
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

// Le coffre ne garde que la GRAINE : R et toutes les feuilles s'en déduisent
#[derive(serde::Serialize, serde::Deserialize)]
struct SequencerKeys {
    seed_hex: String,
}

// Payload d'un carburant caché "montant|otp|R" (chiffré Kyber + AES pour le WNS) → otp hex.
// On ne croit pas le payload : can_spend recalcule ensuite l'engagement on-chain.
fn open_gas_payload(out: &TransactionOutput, kyber_sk: &[u8]) -> Option<String> {
    use aes_gcm::{Aes256Gcm, KeyInit, aead::Aead};
    let capsule = hex::decode(&out.kyber_capsule).ok()?;
    let shared = pqc_kyber::decapsulate(&capsule, kyber_sk).ok()?;
    let vault = hex::decode(&out.aes_vault).ok()?;
    if vault.len() <= 12 { return None; }
    let cipher = Aes256Gcm::new(&shared.into());
    #[allow(deprecated)]
    let nonce = aes_gcm::Nonce::from_slice(&vault[0..12]);
    let text = String::from_utf8(cipher.decrypt(nonce, &vault[12..]).ok()?).ok()?;
    let mut parts = text.split('|');
    let amount: u64 = parts.next()?.parse().ok()?;
    let otp = parts.next()?.to_string();
    (amount == out.amount).then_some(otp)
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
    println!("🧅 IDENTITÉ MIXNET DU NŒUD WNS (clé Kyber) :");
    println!("{}", node_kyber_pub);
    println!("(L'adresse de CARBURANT complète <clé>.<racine> est affichée plus bas.)");
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
        println!("🔧 Première exécution : Génération de l'identité hypertree du Séquenceur...");
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        let keys = SequencerKeys { seed_hex: hex::encode(seed) };
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

    // Identité hypertree : R = clé à staker ; une feuille neuve par signature (journal dans la base du WNS)
    let mut seed = [0u8; 32];
    seed.copy_from_slice(&hex::decode(&hot_wallet.seed_hex).expect("❌ Graine du séquenceur illisible"));
    let leaves_journal = state.lock().unwrap().db.clone().expect("La DB Sled n'est pas initialisée")
        .open_tree("sequencer_leaves").expect("Impossible d'ouvrir le journal des feuilles");
    let mut signer = wattcoin_name_service::sequencer::SequencerSigner::new(&seed, leaves_journal);
    let pubkey = signer.root_hex(); 
    
    println!("=====================================================");
    println!("🔑 RACINE HYPERTREE DU SÉQUENCEUR (à staker) : \n{}", pubkey);
    println!("⛽ ADRESSE DE CARBURANT (payez-la avec « Envoyer » depuis un wallet) : \n{}.{}", node_kyber_pub, pubkey);
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

        let (state_root, block_idx, valid_txs, fees, _withdrawals_l2) = {
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

        // ==============================================================
        // CARBURANT L1 : UTXO payés à "<kyber_wns>.<R>" (cachés pq_watt_ ou publics),
        // dépensés avec une preuve de propriété hypertree, comme n'importe quel wallet.
        // (Retraits L2 → L1 et auto-siphon : désactivés tant que le désancrage n'est pas conçu.)
        // ==============================================================
        const GAS_TARGET: u64 = 10_000; // largement assez pour un ancrage (le reste revient en monnaie)
        let gas_address = format!("{}.{}", node_kyber_pub, pubkey);
        let wns_kyber_sk = hex::decode(&node_kyber_secret).unwrap_or_default();
        let mut gas: Vec<(wattcoin_core::transaction::TransactionInput, String)> = Vec::new(); // (input, utxo_id)
        let mut total_l1_collected = 0u64;

        if let Ok(res) = client.get(&format!("{}/all_transactions", l1_node_url)).send().await {
            if let Ok(all_txs) = res.json::<Vec<serde_json::Value>>().await {
                for item in all_txs {
                    if item["is_l2"].as_bool().unwrap_or(false) { continue; }
                    let height = item["height"].as_u64().unwrap_or(0);
                    let Ok(tx) = serde_json::from_value::<Transaction>(item["transaction"].clone()) else { continue };
                    for out in tx.outputs {
                        if out.amount == 0 || spent_utxos_tree.contains_key(&out.kyber_capsule).unwrap_or(false) { continue; }
                        // Caché : on ouvre le payload "montant|otp|R" ; public : R en fin d'adresse
                        let otp_hex = if out.stealth_address.starts_with("pq_watt_") {
                            match open_gas_payload(&out, &wns_kyber_sk) { Some(otp) => otp, None => continue }
                        } else { String::new() };
                        // Dernier mot à la règle du consensus, à l'identique
                        if !wattcoin_core::transaction::can_spend(&out, &pubkey, &otp_hex) { continue; }
                        total_l1_collected += out.amount;
                        gas.push((wattcoin_core::transaction::TransactionInput {
                            utxo_id: out.kyber_capsule.clone(),
                            amount: out.amount,
                            source_height: height,
                            owner_otp: otp_hex,
                            owner: 0,
                        }, out.kyber_capsule.clone()));
                        if total_l1_collected >= GAS_TARGET { break; }
                    }
                    if total_l1_collected >= GAS_TARGET { break; }
                }
            }
        }

        if gas.is_empty() {
            println!("🛑 [ÉCHEC ANCRAGE] Aucun carburant L1. Envoyez des WATT (bouton Envoyer) à :\n{}", gas_address);
            let mut state_guard = state.lock().unwrap();
            state_guard.block_index -= 1; // Rollback simple
            continue;
        }

        // Signature hypertree de l'état : UNE feuille neuve, liée au nom du L2 (vérifiée par le L1 et les pairs)
        let anchor_msg = wattcoin_core::transaction::anchor_message(&l2_name, &state_root);
        let anchor_sig = match signer.sign(&anchor_msg) {
            Ok(s) => s,
            Err(e) => {
                println!("{}", e);
                let mut state_guard = state.lock().unwrap();
                state_guard.block_index -= 1;
                continue;
            }
        };
        let signature_hex = serde_json::to_string(&anchor_sig).unwrap();

        // Transaction d'ancrage : carburant → frais + monnaie rendue sur "<kyber_wns>.<R>"
        let (inputs, newly_spent_utxos): (Vec<_>, Vec<String>) = gas.into_iter().unzip();
        let mut anchor_tx = Transaction {
            tx_type: TransactionType::L2Anchor {
                l2_name: l2_name.clone(),
                state_root: state_root.clone(),
                sequencer_signature: signature_hex.clone(),
                withdrawals: Vec::new(),
            },
            inputs,
            outputs: vec![TransactionOutput {
                stealth_address: gas_address.clone(),
                kyber_capsule: format!("CHANGE_WNS_{}_{}", block_idx, hex::encode(rand::random::<[u8; 8]>())), // identifiant unique
                aes_vault: String::new(),
                amount: 0,
            }],
            fee: 0,
            wots_signature: None,
            public_key: String::new(),
            owners: vec![wattcoin_core::transaction::OwnerProof { root: pubkey.clone(), signature: None }],
        };

        // Frais au poids RÉEL (+ marge pour la preuve de propriété, pas encore signée)
        let weight_kb = (bincode::serialized_size(&anchor_tx).unwrap_or(0) + 8_000 + 1023) / 1024;
        let l1_fee_sats = std::cmp::max(1000, weight_kb * 20);
        if total_l1_collected <= l1_fee_sats {
            println!("🛑 [ÉCHEC ANCRAGE] Carburant insuffisant ({} ≤ {} Flames). Envoyez des WATT à :\n{}", total_l1_collected, l1_fee_sats, gas_address);
            let mut state_guard = state.lock().unwrap();
            state_guard.block_index -= 1;
            continue;
        }
        let change_l1 = total_l1_collected - l1_fee_sats;
        anchor_tx.fee = l1_fee_sats;
        anchor_tx.outputs[0].amount = change_l1;
        anchor_tx.outputs[0].aes_vault = change_l1.to_string();

        // Preuve de propriété du carburant : une AUTRE feuille neuve, sur le hash COMPLET (ancrage inclus)
        let tx_msg = anchor_tx.hash_data();
        match signer.sign(&tx_msg) {
            Ok(sig) => anchor_tx.owners[0].signature = Some(sig),
            Err(e) => {
                println!("{}", e);
                let mut state_guard = state.lock().unwrap();
                state_guard.block_index -= 1;
                continue;
            }
        }

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
                            l2_name: l2_name.clone(),
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