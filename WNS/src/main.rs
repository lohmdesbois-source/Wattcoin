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

// Nombre de maillons du L2 déjà GRAVÉS sur le L1 (None : L1 injoignable)
async fn l1_anchor_height(client: &Client, l1_url: &str, l2_name: &str) -> Option<u64> {
    let res = client.get(&format!("{}/l2/peg/{}", l1_url, l2_name)).send().await.ok()?;
    res.json::<Value>().await.ok()?["l2_height"].as_u64()
}

// Envoie l'ancrage au L1. Err(Some(raison)) = REFUSÉ par le L1 ; Err(None) = L1 injoignable
async fn send_anchor(client: &Client, l1_url: &str, tx_bytes: Vec<u8>) -> Result<(), Option<String>> {
    match client.post(&format!("{}/send_tx", l1_url))
        .header("Content-Type", "application/octet-stream")
        .body(tx_bytes)
        .send().await {
        Ok(resp) if resp.status().is_success() => Ok(()),
        Ok(resp) => Err(Some(resp.text().await.unwrap_or_default())),
        Err(_) => Err(None),
    }
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

    // Carburant L1 : UTXO payés à "<kyber_wns>.<R>" (cachés pq_watt_ ou publics), dépensés avec une preuve hypertree.
    // C'est AUSSI là que reviennent les frais L2 du séquenceur (dernier retrait UNPEG_ de chaque ancrage).
    const GAS_TARGET: u64 = 10_000; // largement assez pour un ancrage (le reste revient en monnaie)
    let gas_address = format!("{}.{}", node_kyber_pub, pubkey);
    let wns_kyber_sk = hex::decode(&node_kyber_secret).unwrap_or_default();

    // Ancrage envoyé, pas encore gravé : (état d'avant le bloc, UTXO de carburant engagés, tours d'attente, bloc à diffuser, TX d'ancrage)
    // Tant que le L1 ne l'a pas REFUSÉ, il peut encore être miné : on ne revient JAMAIS en arrière sur un simple délai.
    let mut pending: Option<(L2State, Vec<String>, u32, WnsBlock, Vec<u8>)> = None;
    const RESEND_EVERY_ROUNDS: u32 = 8; // ~2 min à 15 s : on renvoie la MÊME TX (mempool L1 perdue, nœud redémarré…)

    // ==============================================================
    // 4. BOUCLE PRINCIPALE : UN BLOC L2 = UN ANCRAGE (maillon, total 1:1, retraits)
    // ==============================================================
    loop {
        println!("⏳ Séquenceur WNS en veille pour {} secondes (Attente du prochain tour)...", block_time);
        tokio::time::sleep(Duration::from_secs(block_time)).await;

        // 0. UN ancrage à la fois : on attend que le L1 ait GRAVÉ le précédent.
        //    Seul un REFUS du L1 (et maillon toujours pas gravé) autorise le retour en arrière.
        if let Some((snapshot, utxos, rounds, block, tx_bytes)) = pending.take() {
            let mine = state.lock().unwrap().block_index;
            let Some(h) = l1_anchor_height(&client, &l1_node_url, &l2_name).await else {
                println!("📡 L1 injoignable : l'ancrage #{} reste en attente.", mine);
                pending = Some((snapshot, utxos, rounds, block, tx_bytes));
                continue;
            };
            if h >= mine {
                println!("✅ Ancrage #{} GRAVÉ sur le L1 : diffusion du bloc aux pairs WNS.", mine);
                wattcoin_name_service::network::broadcast_message(&WnsP2PMessage::BroadcastBlock { block }, &active_wns_peers, "");
            } else if (rounds + 1) % RESEND_EVERY_ROUNDS != 0 {
                println!("⏳ Ancrage #{} en attente de gravure sur le L1 (tour {})...", mine, rounds + 1);
                pending = Some((snapshot, utxos, rounds + 1, block, tx_bytes));
                continue;
            } else {
                // Toujours pas gravé : on RENVOIE la même TX (le L1 l'a peut-être perdue)
                match send_anchor(&client, &l1_node_url, tx_bytes.clone()).await {
                    Ok(()) => {
                        println!("🔁 Ancrage #{} renvoyé au L1 (toujours valide), on attend sa gravure...", mine);
                        pending = Some((snapshot, utxos, rounds + 1, block, tx_bytes));
                    }
                    Err(None) => {
                        println!("📡 L1 injoignable : l'ancrage #{} reste en attente.", mine);
                        pending = Some((snapshot, utxos, rounds + 1, block, tx_bytes));
                    }
                    Err(Some(e)) => {
                        // Refusé… parce que miné entre-temps ? On revérifie AVANT de revenir en arrière.
                        match l1_anchor_height(&client, &l1_node_url, &l2_name).await {
                            Some(h) if h >= mine => {
                                println!("✅ Ancrage #{} GRAVÉ sur le L1 : diffusion du bloc aux pairs WNS.", mine);
                                wattcoin_name_service::network::broadcast_message(&WnsP2PMessage::BroadcastBlock { block }, &active_wns_peers, "");
                            }
                            Some(_) => {
                                println!("⚠️ Ancrage #{} refusé par le L1 ({}) : retour à l'état d'avant, ses TX L2 repartent en mempool.", mine, e);
                                state.lock().unwrap().rollback_to(snapshot);
                                for u in &utxos { let _ = spent_utxos_tree.remove(u.as_bytes()); }
                                let _ = spent_utxos_tree.flush();
                            }
                            None => {
                                println!("📡 L1 injoignable : l'ancrage #{} reste en attente.", mine);
                                pending = Some((snapshot, utxos, rounds + 1, block, tx_bytes));
                            }
                        }
                    }
                }
                continue;
            }
        }

        let status = match client.get(&format!("{}/l2/status/{}", l1_node_url, l2_name)).send().await {
            Ok(res) => res.json::<Value>().await.ok(),
            Err(_) => None,
        };
        let Some(status) = status else {
            println!("📡 Attente de la connexion avec le Nœud L1...");
            continue;
        };
        if !status["is_active"].as_bool().unwrap_or(false) {
            println!("⏸️ Le réseau L2 '{}' est inactif (Aucun Staker détecté sur le L1).", l2_name);
            continue;
        }
        let onchain_pubkey = status["sequencer_pubkey"].as_str().unwrap_or("");
        if onchain_pubkey != pubkey {
            println!("🎲 Le Tribunal VRF a élu un autre séquenceur ({}...).\nOu vous n'avez pas stake pour pouvoir être élu, on passe notre tour.", onchain_pubkey.get(0..15).unwrap_or(onchain_pubkey));
            continue;
        }
        println!("👑 SUCCÈS ! Le VRF a élu NOTRE Nœud comme Séquenceur pour ce tour !");
        // Ce que le L1 a DÉJÀ gravé pour ce L2 (sert au rattrapage après séquençage)
        let l1_anchors = status["total_anchors_on_l1"].as_u64().unwrap_or(0);
        let l1_root = status["last_state_root"].as_str().unwrap_or("").to_string();

        // 1. Séquençage. L'état d'avant est gardé : si le L1 refuse, on y revient À L'IDENTIQUE.
        let (snapshot, valid_txs, fees, withdrawals, l2_height, prev_root, state_root, l1_height, l2_total) = {
            let mut s = state.lock().unwrap();
            let snapshot = s.clone();
            let (txs, fees, withdrawals) = s.process_mempool();
            if txs.is_empty() {
                println!("📭 Mempool L2 vide (ou rien d'applicable). Aucun bloc à forger, on économise le gaz !");
                continue;
            }
            let prev_root = s.last_root.clone();
            let state_root = s.compute_state_root();
            s.block_index += 1;
            s.last_root = state_root.clone();
            s.save_to_disk();
            (snapshot, txs, fees, withdrawals, s.block_index, prev_root, state_root, s.last_l1_block, s.l2_total())
        };

        println!("=====================================================");
        println!("⛏️  NOUVEAU BLOC WNS FORGÉ ! (Maillon #{})", l2_height);
        println!("📝 Transactions               : {}", valid_txs.len());
        println!("🌉 Retraits vers le L1        : {}", withdrawals.len());
        println!("💰 Frais (rapatriés sur L1)   : {} Flames", fees); 
        println!("⚖️  Total L2 (1:1)             : {} Flames (dépôts couverts jusqu'au bloc L1 {})", l2_total, l1_height);
        println!("🌳 Racine du Registre (Root)  : {}", &state_root[..32]); 
        println!("=====================================================\n");

        // 1 bis. Rattrapage : ce maillon est-il DÉJÀ gravé sur le L1 ? (ancrage miné après un retour en arrière,
        //        WNS redémarré pendant l'attente…) Même racine = même état : on l'adopte, sans rien renvoyer.
        if l1_anchors >= l2_height {
            if l1_anchors == l2_height && l1_root == state_root {
                println!("🔁 Maillon #{} déjà gravé sur le L1 avec la même racine : état resynchronisé.", l2_height);
            } else {
                println!("⛔ DIVERGENCE : le L1 a déjà {} maillon(s), dernière racine {}… ≠ la nôtre. Bloc annulé.",
                    l1_anchors, l1_root.get(0..16).unwrap_or(&l1_root));
                state.lock().unwrap().rollback_to(snapshot);
            }
            continue;
        }

        // 2. Retraits L2 → L1 (les `unpeg` premières sorties) : ceux des utilisateurs, puis les frais du
        //    séquenceur vers SA propre adresse de carburant (il se rembourse par le pont, comme tout le monde)
        let mut unpeg = withdrawals;
        if fees > 0 { unpeg.push((gas_address.clone(), fees)); }
        let unpeg_outputs: Vec<TransactionOutput> = unpeg.iter().enumerate().map(|(i, (addr, amount))| TransactionOutput {
            stealth_address: addr.clone(),
            kyber_capsule: wattcoin_core::transaction::unpeg_capsule(&l2_name, l2_height, i),
            aes_vault: amount.to_string(),
            amount: *amount,
        }).collect();

        // 3. Carburant L1 (hors retraits UNPEG_ encore en maturation : le L1 ne les laisserait pas passer)
        let mut gas: Vec<(wattcoin_core::transaction::TransactionInput, String)> = Vec::new(); // (input, utxo_id)
        let mut total_l1_collected = 0u64;

        if let Ok(res) = client.get(&format!("{}/all_transactions", l1_node_url)).send().await {
            if let Ok(all_txs) = res.json::<Vec<serde_json::Value>>().await {
                let tip = all_txs.iter().filter_map(|i| i["height"].as_u64()).max().unwrap_or(0);
                // UTXO déjà dépensés ON-CHAIN : la chaîne fait foi, pas notre mémoire locale
                let spent_on_chain: std::collections::HashSet<String> = all_txs.iter()
                    .filter_map(|i| i["transaction"]["inputs"].as_array())
                    .flatten()
                    .filter_map(|inp| inp["utxo_id"].as_str().map(String::from))
                    .collect();
                for item in all_txs {
                    if item["is_l2"].as_bool().unwrap_or(false) { continue; }
                    let height = item["height"].as_u64().unwrap_or(0);
                    let Ok(tx) = serde_json::from_value::<Transaction>(item["transaction"].clone()) else { continue };
                    for out in tx.outputs {
                        if out.amount == 0 || spent_on_chain.contains(&out.kyber_capsule)
                            || spent_utxos_tree.contains_key(&out.kyber_capsule).unwrap_or(false) { continue; }
                        if out.kyber_capsule.starts_with("UNPEG_") && height + wattcoin_core::blockchain::UNPEG_DELAY_BLOCKS > tip + 1 { continue; }
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
            println!("🛑 [ÉCHEC ANCRAGE] Aucun carburant L1 mûr. Envoyez des WATT (bouton Envoyer) à :\n{}", gas_address);
            state.lock().unwrap().rollback_to(snapshot);
            continue;
        }

        // 4. Signature hypertree du maillon : UNE feuille neuve (vérifiée par le L1 et les pairs)
        let anchor_msg = wattcoin_core::transaction::anchor_message(&l2_name, l2_height, &prev_root, &state_root, l1_height, l2_total, &unpeg_outputs);
        let anchor_sig = match signer.sign(&anchor_msg) {
            Ok(s) => s,
            Err(e) => {
                println!("{}", e);
                state.lock().unwrap().rollback_to(snapshot);
                continue;
            }
        };
        let signature_hex = serde_json::to_string(&anchor_sig).unwrap();

        // 5. Transaction d'ancrage : [retraits UNPEG_…] + monnaie du carburant sur "<kyber_wns>.<R>"
        let (inputs, newly_spent_utxos): (Vec<_>, Vec<String>) = gas.into_iter().unzip();
        let mut outputs = unpeg_outputs.clone();
        outputs.push(TransactionOutput {
            stealth_address: gas_address.clone(),
            kyber_capsule: format!("CHANGE_WNS_{}_{}", l2_height, hex::encode(rand::random::<[u8; 8]>())), // identifiant unique
            aes_vault: String::new(),
            amount: 0,
        });
        let mut anchor_tx = Transaction {
            tx_type: TransactionType::L2Anchor {
                l2_name: l2_name.clone(),
                l2_height,
                prev_root: prev_root.clone(),
                state_root: state_root.clone(),
                l1_height,
                l2_total,
                sequencer_signature: signature_hex.clone(),
                unpeg: unpeg_outputs.len() as u16,
            },
            inputs,
            outputs,
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
            state.lock().unwrap().rollback_to(snapshot);
            continue;
        }
        let change_l1 = total_l1_collected - l1_fee_sats;
        let ci = anchor_tx.outputs.len() - 1; // la monnaie est la DERNIÈRE sortie
        anchor_tx.fee = l1_fee_sats;
        anchor_tx.outputs[ci].amount = change_l1;
        anchor_tx.outputs[ci].aes_vault = change_l1.to_string();

        // Preuve de propriété du carburant : une AUTRE feuille neuve, sur le hash COMPLET (ancrage inclus)
        let tx_msg = anchor_tx.hash_data();
        match signer.sign(&tx_msg) {
            Ok(sig) => anchor_tx.owners[0].signature = Some(sig),
            Err(e) => {
                println!("{}", e);
                state.lock().unwrap().rollback_to(snapshot);
                continue;
            }
        }

        let new_block = WnsBlock {
            index: l2_height,
            l2_name: l2_name.clone(),
            l1_parent_hash: String::new(),
            prev_root,
            state_root: state_root.clone(),
            l1_height,
            l2_total,
            withdrawals: unpeg_outputs,
            sequencer_pubkey: pubkey.clone(),
            transactions: valid_txs,
            signature: signature_hex,
        };

        println!("⚓ Envoi de l'ancrage #{} au L1 (Frais payés : {} Flames)...", l2_height, l1_fee_sats);
        let mark_spent = |utxos: &[String]| {
            for u in utxos { let _ = spent_utxos_tree.insert(u.as_bytes(), &[]); }
            let _ = spent_utxos_tree.flush(); // Protection Crash-Safe
        };

        let tx_bytes = bincode::serialize(&anchor_tx).expect("Erreur sérialisation L2Anchor");
        match send_anchor(&client, &l1_node_url, tx_bytes.clone()).await {
            Ok(()) => {
                mark_spent(&newly_spent_utxos);
                println!("📮 Ancrage #{} accepté en mempool L1. En attente de gravure...", l2_height);
                pending = Some((snapshot, newly_spent_utxos, 0, new_block, tx_bytes));
            }
            Err(Some(e)) => {
                println!("❌ Ancrage #{} rejeté par le L1 : {}", l2_height, e);
                state.lock().unwrap().rollback_to(snapshot); // retour À L'IDENTIQUE, les TX repartent en mempool
            }
            Err(None) => {
                // Peut-être reçu, peut-être pas : on attend sa gravure (et on le renverra)
                println!("❌ Impossible de joindre le réseau L1 : l'ancrage #{} est gardé en attente.", l2_height);
                mark_spent(&newly_spent_utxos);
                pending = Some((snapshot, newly_spent_utxos, 0, new_block, tx_bytes));
            }
        }
    }
}