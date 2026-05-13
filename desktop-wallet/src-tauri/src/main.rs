#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};
use rand::{Rng, RngCore}; 
use serde::{Serialize, Deserialize};
use std::str::FromStr;
use std::fs; 
use std::path::Path;
use std::collections::HashSet;
use std::time::Duration;
use tauri::Emitter;

use pqcrypto_traits::kem::{Ciphertext, SharedSecret, PublicKey as _, SecretKey as _};
use pqcrypto_traits::sign::{SignedMessage, PublicKey as _, SecretKey as _};

use once_cell::sync::Lazy;
use arti_client::{TorClient, TorClientConfig, StreamPrefs};
use tokio::io::{AsyncWriteExt, AsyncReadExt};
use tokio::sync::Mutex as AsyncMutex;
use tor_rtcompat::PreferredRuntime;

const ONION_NODE: &str = "jjbeptmy4b2ck5mc5sdjdc7kk6fkrva4laxfu7ufncmvk6qj6duh64yd.onion:8100";
const VAULT_FILE: &str = ".wattcoin_vault";
const LATTICE_Q: u32 = 8380417; 
const LATTICE_DIM: usize = 4;

#[derive(Debug)]
pub enum WattError {
    Crypto(String),
    Network(String),
    Io(std::io::Error),
    Vault(String),
    Json(serde_json::Error),
}

impl From<std::io::Error> for WattError {
    fn from(err: std::io::Error) -> Self { WattError::Io(err) }
}

impl From<serde_json::Error> for WattError {
    fn from(err: serde_json::Error) -> Self { WattError::Json(err) }
}

impl From<WattError> for String {
    fn from(err: WattError) -> String {
        match err {
            WattError::Crypto(msg) => format!("🔒 Erreur Cryptographique : {}", msg),
            WattError::Network(msg) => format!("🧅 Erreur Réseau Tor : {}", msg),
            WattError::Io(err) => format!("💾 Erreur Disque/Fichier : {}", err),
            WattError::Vault(msg) => format!("🏦 Erreur Coffre-Fort : {}", msg),
            WattError::Json(err) => format!("🧩 Erreur Données : {}", err),
        }
    }
}

static TOR_CLIENT: Lazy<AsyncMutex<Option<TorClient<PreferredRuntime>>>> = Lazy::new(|| AsyncMutex::new(None));
static TOR_LOCK: Lazy<AsyncMutex<()>> = Lazy::new(|| AsyncMutex::new(()));

async fn start_arti_socks_proxy(tor_client: arti_client::TorClient<PreferredRuntime>) {
    if let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:9150").await {
        println!("🥷 [PROXY] Micro-serveur SOCKS5 ouvert sur le port 9150 !");
        tokio::spawn(async move {
            loop {
                if let Ok((mut stream, _)) = listener.accept().await {
                    let tc = tor_client.clone();
                    tokio::spawn(async move {
                        let mut buf = [0u8; 256];
                        if stream.read_exact(&mut buf[0..2]).await.is_err() { return; }
                        if buf[0] != 0x05 { println!("❌ [PROXY] Erreur: Ce n'est pas du SOCKS5"); return; }
                        let num_methods = buf[1] as usize;
                        if stream.read_exact(&mut buf[0..num_methods]).await.is_err() { return; }
                        if stream.write_all(&[0x05, 0x00]).await.is_err() { return; } 
                        
                        if stream.read_exact(&mut buf[0..4]).await.is_err() { return; }
                        if buf[0] != 0x05 || buf[1] != 0x01 { println!("❌ [PROXY] Erreur: Commande non supportée"); return; } 
                        
                        let host = match buf[3] {
                            0x01 => {
                                if stream.read_exact(&mut buf[0..4]).await.is_err() { return; }
                                format!("{}.{}.{}.{}", buf[0], buf[1], buf[2], buf[3])
                            }
                            0x03 => {
                                let mut len_buf = [0u8; 1];
                                if stream.read_exact(&mut len_buf).await.is_err() { return; }
                                let mut domain_buf = vec![0u8; len_buf[0] as usize];
                                if stream.read_exact(&mut domain_buf).await.is_err() { return; }
                                String::from_utf8_lossy(&domain_buf).into_owned()
                            }
                            _ => { println!("❌ [PROXY] Erreur: Type d'adresse IPv6 non supporté"); return; }
                        };

                        let mut port_buf = [0u8; 2];
                        if stream.read_exact(&mut port_buf).await.is_err() { return; }
                        let port = u16::from_be_bytes(port_buf);

                        println!("🥷 [PROXY] BDK demande une connexion vers {}:{}...", host, port);

                        let mut prefs = arti_client::StreamPrefs::new();
                        prefs.connect_to_onion_services(arti_client::config::BoolOrAuto::Explicit(true));
                        
                        match tc.connect_with_prefs((host.clone(), port), &prefs).await {
                            Ok(mut tor_stream) => {
                                println!("✅ [PROXY] Tunnel Tor établi avec succès pour {} !", host);
                                if stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0,0,0,0, 0,0]).await.is_err() { return; }
                                let _ = tokio::io::copy_bidirectional(&mut stream, &mut tor_stream).await;
                            }
                            Err(e) => {
                                println!("❌ [PROXY] Arti n'a pas pu joindre {} : {}", host, e);
                                let _ = stream.write_all(&[0x05, 0x01, 0x00, 0x01, 0,0,0,0, 0,0]).await;
                            }
                        }
                    });
                }
            }
        });
    } else {
        println!("⚠️ [TOR] Impossible d'ouvrir le port 9150");
    }
}

async fn get_tor_client() -> Result<TorClient<PreferredRuntime>, String> {
    let mut lock = TOR_CLIENT.lock().await;
    if let Some(client) = &*lock { return Ok(client.clone()); }
    
    println!("🧅 [TOR] Initialisation du blindage Arti...");
    let config = TorClientConfig::default();
    let client = TorClient::create_bootstrapped(config).await.map_err(|e| format!("Erreur Bootstrap: {}", e))?;
    
    start_arti_socks_proxy(client.clone()).await;
    
    *lock = Some(client.clone());
    Ok(client)
}

async fn tor_fetch(method: &str, endpoint: &str, body: Option<String>) -> Result<String, String> {
    let _guard = tokio::time::timeout(std::time::Duration::from_secs(60), TOR_LOCK.lock())
        .await.map_err(|_| "❌ [TOR] Timeout file d'attente".to_string())?;

    let tor_client = get_tor_client().await?;
    let mut prefs = StreamPrefs::new();
    prefs.connect_to_onion_services(arti_client::config::BoolOrAuto::Explicit(true));

    println!("\n==============================================");
    println!("🕵️ [TOR] Début de la mission vers {}", endpoint);
    
    let mut stream = None;
    for i in 1..=3 {
        println!("⏳ [TOR] Percée du tunnel (Tentative {}/3)...", i);
        match tokio::time::timeout(std::time::Duration::from_secs(20), tor_client.connect_with_prefs(ONION_NODE, &prefs)).await {
            Ok(Ok(s)) => { 
                println!("✅ [TOR] Tunnel établi !");
                stream = Some(s); 
                break; 
            },
            Ok(Err(e)) => println!("⚠️ [TOR] Arti a échoué : {}", e),
            Err(_) => println!("⚠️ [TOR] Timeout 20s !"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    let mut stream = stream.ok_or_else(|| "❌ [TOR] Abandon de la mission.".to_string())?;

    println!("📤 [TOR] Envoi de la requête HTTP...");
    let req = if let Some(ref b) = body {
        format!("{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: WattcoinWallet/1.0\r\nAccept: application/json\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", method, endpoint, ONION_NODE, b.len(), b)
    } else {
        format!("{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: WattcoinWallet/1.0\r\nAccept: application/json\r\nConnection: close\r\n\r\n", method, endpoint, ONION_NODE)
    };

    stream.write_all(req.as_bytes()).await.map_err(|e| format!("Erreur écriture: {}", e))?;
    stream.flush().await.map_err(|e| format!("Erreur flush: {}", e))?;

    println!("📥 [TOR] Attente de la réponse...");
    let mut response = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf)).await {
            Ok(Ok(0)) => {
                println!("✅ [TOR] Le Serveur Relais a terminé l'envoi.");
                break;
            }
            Ok(Ok(n)) => {
                response.extend_from_slice(&buf[..n]);
            }
            Ok(Err(e)) => return Err(format!("Erreur lecture: {}", e)),
            Err(_) => {
                println!("⚠️ [TOR] Timeout 5s ! Le serveur refuse de couper la ligne, on garde les {} octets reçus.", response.len());
                break;
            }
        }
    }

    let resp_str = String::from_utf8_lossy(&response);
    if let Some(idx) = resp_str.find("\r\n\r\n") {
        let headers = &resp_str[..idx];
        let mut body_content = resp_str[idx+4..].to_string(); 
        
        if headers.to_lowercase().contains("transfer-encoding: chunked") {
            println!("🧩 [TOR] Découpage Chunked détecté. Reconstruction en cours...");
            let mut decoded = String::new();
            let mut curr = body_content.as_str();
            while let Some(i) = curr.find("\r\n") {
                let hex_str = curr[..i].trim();
                if let Ok(len) = usize::from_str_radix(hex_str, 16) {
                    if len == 0 { break; } 
                    curr = &curr[i+2..];
                    if curr.len() >= len {
                        decoded.push_str(&curr[..len]);
                        curr = &curr[len..];
                        if curr.starts_with("\r\n") { curr = &curr[2..]; }
                    } else { 
                        decoded.push_str(curr); 
                        break; 
                    }
                } else { break; }
            }
            body_content = decoded;
        }

        let final_body = body_content.trim().to_string();
        if headers.contains("200 OK") {
            println!("🎯 [TOR] Extraction réussie ({} octets)", final_body.len());
            Ok(final_body)
        } else {
            Err(format!("Erreur HTTP: {}", headers))
        }
    } else {
        println!("❌ [TOR] Réponse inexploitable ({} octets)", response.len());
        Err("Réponse corrompue".to_string())
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct WalletKeys {
    mnemonic: String,
    btc_address: String,
    btc_pubkey_hex: String,
    watt_address: String, 
    master_seed_hex: String,
    kyber_secret_hex: String,
    dilithium_public_hex: String,
    dilithium_secret_hex: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PQLatticeRingSignature {
    pub key_image: String,
    pub c0: String,
    pub z_responses: Vec<Vec<u32>>,
    pub p_keys: Vec<Vec<u32>>, 
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LWECommitment {
    pub a_matrix_seed: [u8; 32], 
    pub t_vector: Vec<u32>,      
}

impl LWECommitment {
    pub fn commit(amount: u64, blinding_factor: [u32; LATTICE_DIM]) -> Self {
        use rand::{Rng, SeedableRng};
        let mut rng = rand::thread_rng();
        let mut a_matrix_seed = [0u8; 32];
        rng.fill_bytes(&mut a_matrix_seed);
        
        let mut a_matrix = vec![vec![0u32; LATTICE_DIM]; LATTICE_DIM];
        let mut seed_rng = rand::rngs::StdRng::from_seed(a_matrix_seed);
        for row in a_matrix.iter_mut() {
            for val in row.iter_mut() { *val = seed_rng.gen_range(0..LATTICE_Q); }
        }

        let mut t_vector = vec![0u32; LATTICE_DIM];
        for i in 0..LATTICE_DIM {
            let mut sum: u64 = 0;
            for j in 0..LATTICE_DIM {
                sum += (a_matrix[i][j] as u64 * blinding_factor[j] as u64) % LATTICE_Q as u64;
            }
            let error_term = rng.gen_range(0..5); 
            let message_term = if i == 0 { (amount * (LATTICE_Q as u64 / 2)) % LATTICE_Q as u64 } else { 0 };
            t_vector[i] = ((sum + error_term as u64 + message_term) % LATTICE_Q as u64) as u32;
        }

        LWECommitment { a_matrix_seed, t_vector }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionOutput {
    pub stealth_address: String,
    pub kyber_capsule: String,
    pub aes_vault: String,
    pub lattice_commitment: LWECommitment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInput {
    pub pq_ring_inputs: Vec<String>,
    pub pq_ring_signature: PQLatticeRingSignature,
    pub commitment: LWECommitment,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionType {
    Coinbase,
    Standard,
    HTLCLock { hash: String, timeout_block: u64 },
    HTLCClaim { secret: String },
    HTLCRefund { hash: String },
    DexSettlement { clearing_price_sats: u64, total_volume_flames: u64, swaps: Vec<SwapContract> },
}

#[derive(Serialize)]
pub struct HistoryItem {
    pub id: String,
    pub tx_type: String,
    pub amount: f64,
    pub coin: String,
    pub date: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub tx_type: TransactionType,
    pub inputs: Vec<TransactionInput>, 
    pub outputs: Vec<TransactionOutput>, 
    pub fee: u64, 
    pub dilithium_signature: String, 
}

#[derive(Serialize, Deserialize, Clone)]
struct Order { pub id: String, pub order_type: String, pub amount_flames: u64, pub price_sats: u64, pub btc_address: String, pub btc_pubkey: String, pub watt_address: String, pub expires_at: i64 }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapContract { 
    pub buyer_btc_address: String, 
    pub buyer_btc_pubkey: String, 
    pub seller_watt_address: String, 
    pub seller_btc_pubkey: String, 
    pub watt_amount_flames: u64, 
    pub btc_amount_sats: u64, 
    pub htlc_secret: String, 
    pub htlc_hash: String 
}

#[tauri::command]
async fn get_network_info() -> Result<serde_json::Value, String> {
    let res_str = tor_fetch("GET", "/info", None).await?;
    serde_json::from_str(&res_str).map_err(|e| {
        println!("❌ [JSON ERROR INFO] {} | Data: {}", e, res_str);
        e.to_string()
    })
}

#[tauri::command]
async fn submit_order(order_type: String, amount: f64, price: f64, btc_address: String, btc_pubkey: String, watt_address: String) -> Result<(), String> {
    let mut rand_bytes = [0u8; 4]; rand::thread_rng().fill_bytes(&mut rand_bytes);
    let amount_flames = (amount * 1_000_000_000.0) as u64; 
    let price_sats = (price * 100_000_000.0) as u64; 
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let expires_at = now + 7200; 

    let order_data = serde_json::json!({
        "id": hex::encode(rand_bytes),
        "order_type": order_type,
        "amount_flames": amount_flames, 
        "price_sats": price_sats,        
        "btc_address": btc_address,
        "btc_pubkey": btc_pubkey,
        "watt_address": watt_address,
        "expires_at": expires_at 
    });

    tor_fetch("POST", "/order", Some(order_data.to_string())).await?;
    Ok(())
}

#[tauri::command]
async fn get_dark_pool() -> Result<Vec<Order>, String> {
    let res_str = tor_fetch("GET", "/pool", None).await?;
    let pool = serde_json::from_str::<Vec<Order>>(&res_str).map_err(|e| e.to_string())?;
    Ok(pool)
}

#[tauri::command]
async fn generate_pro_wallet(phrase_option: Option<String>) -> Result<WalletKeys, String> {
    use bip39::{Mnemonic, Language};
    use bitcoin::Network as BtcNetwork;
    use bitcoin::bip32::{Xpriv, DerivationPath}; 
    use bitcoin::{PrivateKey as BtcPrivateKey, PublicKey as BtcPublicKey, Address as BtcAddress};
    use bitcoin::secp256k1::Secp256k1;
    use pqcrypto_kyber::kyber768;
    use pqcrypto_dilithium::dilithium3;

    let mnemonic = match phrase_option {
        Some(phrase) => Mnemonic::parse_in(Language::French, &phrase).map_err(|_| "Phrase invalide")?,
        None => {
            let mut entropy = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut entropy);
            Mnemonic::from_entropy_in(Language::French, &entropy).unwrap()
        }
    };
    let seed = mnemonic.to_seed("");

    let secp = Secp256k1::new();
    let root = Xpriv::new_master(BtcNetwork::Testnet, &seed).unwrap();
    let path = DerivationPath::from_str("m/84'/1'/0'/0/0").unwrap();
    let child = root.derive_priv(&secp, &path).unwrap();
    let btc_priv = BtcPrivateKey::new(child.private_key, BtcNetwork::Testnet);
    let btc_pub = BtcPublicKey::from_private_key(&secp, &btc_priv);
    let compressed_pubkey = bitcoin::CompressedPublicKey::try_from(btc_pub).unwrap();
    let btc_address = BtcAddress::p2wpkh(&compressed_pubkey, BtcNetwork::Testnet).to_string();

    let (kyber_pk, kyber_sk) = kyber768::keypair();
    let (dilithium_pk, dilithium_sk) = dilithium3::keypair();

    Ok(WalletKeys {
        mnemonic: mnemonic.to_string(),
        btc_address,
        btc_pubkey_hex: btc_pub.to_string(),
        master_seed_hex: hex::encode(seed),
        watt_address: hex::encode(kyber_pk.as_bytes()),
        kyber_secret_hex: hex::encode(kyber_sk.as_bytes()),
        dilithium_public_hex: hex::encode(dilithium_pk.as_bytes()),
        dilithium_secret_hex: hex::encode(dilithium_sk.as_bytes()),
    })
}

#[tauri::command] 
fn vault_exists() -> bool { Path::new(VAULT_FILE).exists() }

#[tauri::command]
fn encrypt_vault(password: String, keys_json_string: String) -> Result<(), String> {
    let mut salt = [0u8; 16]; rand::thread_rng().fill_bytes(&mut salt);
    let mut key = [0u8; 32]; pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), &salt, 100_000, &mut key);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    
    let ciphertext = cipher.encrypt(nonce, keys_json_string.as_bytes())
        .map_err(|_| WattError::Crypto("Échec du chiffrement AES-256-GCM".to_string()))?;
    
    let mut final_data = Vec::new();
    final_data.extend_from_slice(&salt); 
    final_data.extend_from_slice(&nonce_bytes); 
    final_data.extend_from_slice(&ciphertext);
    
    fs::write(VAULT_FILE, final_data).map_err(WattError::from)?; 
    Ok(())
}

#[tauri::command]
async fn unlock_vault(password: String) -> Result<WalletKeys, String> {
    use pbkdf2::pbkdf2_hmac;
    use sha2::Sha256;

    let file_data = fs::read(VAULT_FILE).map_err(|e| WattError::Io(e))?;
    if file_data.len() < 28 { return Err(WattError::Vault("Fichier corrompu ou incomplet.".to_string()).into()); }

    let salt = &file_data[0..16];
    let nonce_bytes = &file_data[16..28];
    let ciphertext = &file_data[28..];

    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, 100_000, &mut key);

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext)
        .map_err(|_| WattError::Vault("Mot de passe incorrect ou coffre altéré.".to_string()))?;
    
    let json_string = String::from_utf8(plaintext)
        .map_err(|_| WattError::Crypto("Erreur de décodage UTF-8 post-déchiffrement.".to_string()))?;
        
    let keys: WalletKeys = serde_json::from_str(&json_string).map_err(|e| WattError::Json(e))?;
    Ok(keys)
}

#[tauri::command]
async fn get_watt_balance(keys: WalletKeys) -> Result<f64, String> {
    let res_str = tor_fetch("GET", "/all_transactions", None).await?;
    let all_txs: Vec<Transaction> = serde_json::from_str(&res_str).map_err(|e| {
        println!("❌ [JSON ERROR BALANCE] {}", e);
        "Erreur JSON".to_string()
    })?;

    let mut balance_flames: u64 = 0;
    use pqcrypto_kyber::kyber768;

    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
    let kyber_sk = kyber768::SecretKey::from_bytes(&sk_bytes).unwrap();

    let mut spent_capsules = HashSet::new();
    if let Ok(spends) = fs::read_to_string(".wattcoin_spends") {
        for line in spends.lines() { spent_capsules.insert(line.trim().to_string()); }
    }

    for tx in all_txs {
        let is_lock_tx = matches!(tx.tx_type, TransactionType::HTLCLock { .. });

        for (index, out) in tx.outputs.iter().enumerate() {
            if spent_capsules.contains(&out.kyber_capsule) { continue; }
            if is_lock_tx && index == 0 { continue; }

            if out.stealth_address == format!("COINBASE_{}", keys.watt_address) {
                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                    balance_flames += amt;
                }
            } else if out.stealth_address.starts_with("pq_watt_") {
                if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
                    if let Ok(ciphertext) = kyber768::Ciphertext::from_bytes(&capsule_bytes) {
                        let shared_secret = kyber768::decapsulate(&ciphertext, &kyber_sk);
                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                            if vault_bytes.len() > 12 {
                                let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                                let aes_key = Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes());
                                let cipher = Aes256Gcm::new(aes_key);
                                
                                if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                                    if let Ok(payload_str) = String::from_utf8(plaintext) {
                                        let parts: Vec<&str> = payload_str.split('|').collect();
                                        if parts.len() == 2 {
                                            if let Ok(amt) = parts[0].parse::<u64>() {
                                                balance_flames += amt;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(balance_flames as f64 / 1_000_000_000.0)
}

#[tauri::command]
async fn get_history(keys: WalletKeys) -> Result<Vec<HistoryItem>, String> {
    let res_str = tor_fetch("GET", "/all_transactions", None).await?;
    let all_txs: Vec<Transaction> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;

    let mut history = Vec::new();
    use pqcrypto_kyber::kyber768;
    use aes_gcm::{Aes256Gcm, Key, Nonce, aead::Aead};

    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
    let kyber_sk = kyber768::SecretKey::from_bytes(&sk_bytes).unwrap();

    let mut spent_capsules = std::collections::HashSet::new();
    if let Ok(spends) = fs::read_to_string(".wattcoin_spends") {
        for line in spends.lines() { spent_capsules.insert(line.trim().to_string()); }
    }

    for (i, tx) in all_txs.iter().enumerate() {
        let is_lock = matches!(tx.tx_type, TransactionType::HTLCLock { .. });

        for (out_idx, out) in tx.outputs.iter().enumerate() {
            if is_lock && out_idx == 0 { continue; } 

            let is_spent = spent_capsules.contains(&out.kyber_capsule);
            let status_text = if is_spent { "Dépensé" } else { "Disponible" };

            if out.stealth_address == format!("COINBASE_{}", keys.watt_address) {
                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                    history.push(HistoryItem {
                        id: format!("Bloc N°{}", i ), 
                        tx_type: "receive".to_string(),
                        amount: amt as f64 / 1_000_000_000.0,
                        coin: "WATT".to_string(),
                        date: "Récemment".to_string(),
                        status: format!("Minage ({})", status_text),
                    });
                }
            } 
            else if out.stealth_address.starts_with("pq_watt_") {
                if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
                    if let Ok(ciphertext) = kyber768::Ciphertext::from_bytes(&capsule_bytes) {
                        let shared_secret = kyber768::decapsulate(&ciphertext, &kyber_sk);
                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                            if vault_bytes.len() > 12 {
                                let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                                let aes_key = Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes());
                                let cipher = Aes256Gcm::new(aes_key);
                                
                                if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                                    if let Ok(payload_str) = String::from_utf8(plaintext) {
                                        let parts: Vec<&str> = payload_str.split('|').collect();
                                        if parts.len() == 2 {
                                            if let Ok(amt) = parts[0].parse::<u64>() {
                                                history.push(HistoryItem {
                                                    id: format!("{}...", &out.kyber_capsule[0..12]),
                                                    tx_type: "receive".to_string(),
                                                    amount: amt as f64 / 1_000_000_000.0,
                                                    coin: "WATT".to_string(),
                                                    date: "Récemment".to_string(),
                                                    status: format!("Transfert ({})", status_text),
                                                });
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    history.reverse();
    Ok(history)
}

#[tauri::command]
async fn send_wattcoin(
    recipient_kyber_hex: String, 
    amount: f64, 
    sender_dilithium_secret_hex: String,
    sender_dilithium_public_hex: String,
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    htlc_hash_hex: Option<String>, 
    htlc_timeout: Option<u64>
) -> Result<String, String> {
    use pqcrypto_kyber::kyber768;
    use pqcrypto_dilithium::dilithium3;
    use rand::seq::SliceRandom;

    let amount_in_flames = (amount * 1_000_000_000.0) as u64; 
    let fee: u64 = 1000;
    let required_total = amount_in_flames + fee;

    let res_str = tor_fetch("GET", "/all_transactions", None).await?;
    let all_txs: Vec<Transaction> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    
    let mut spent_capsules = HashSet::new();
    if let Ok(spends) = fs::read_to_string(".wattcoin_spends") {
        for line in spends.lines() { spent_capsules.insert(line.trim().to_string()); }
    }

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    let kyber_sk = kyber768::SecretKey::from_bytes(&sk_bytes).map_err(|_| "Clé Kyber corrompue".to_string())?;
    
    let mut selected_utxos: Vec<(u64, String, LWECommitment)> = Vec::new();
    let mut current_input_sum = 0u64;

    for tx in all_txs {
        for out in tx.outputs {
            if spent_capsules.contains(&out.kyber_capsule) { continue; }

            let mut is_mine = false;
            let mut val = 0;

            if out.stealth_address == format!("COINBASE_{}", sender_kyber_public_hex) {
                val = out.aes_vault.parse::<u64>().unwrap_or(0);
                is_mine = true;
            } else if out.stealth_address.starts_with("pq_watt_") {
                if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
                    if let Ok(ciphertext) = kyber768::Ciphertext::from_bytes(&capsule_bytes) {
                        let shared_secret = kyber768::decapsulate(&ciphertext, &kyber_sk);
                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                            if vault_bytes.len() > 12 {
                                let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                                let aes_key = Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes());
                                let cipher = Aes256Gcm::new(aes_key);
                                if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                                    if let Ok(payload_str) = String::from_utf8(plaintext) {
                                        let parts: Vec<&str> = payload_str.split('|').collect();
                                        if parts.len() == 2 {
                                            if let Ok(amt) = parts[0].parse::<u64>() {
                                                val = amt;
                                                is_mine = true;
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if is_mine && val > 0 {
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone()));
                current_input_sum += val;
                if current_input_sum >= required_total { break; }
            }
        }
        if current_input_sum >= required_total { break; }
    }

    if current_input_sum < required_total {
        return Err(format!("❌ Fonds insuffisants ! Vous essayez d'envoyer {} WATT (frais inclus) mais vous n'avez que {} WATT libres.", required_total as f64 / 1_000_000_000.0, current_input_sum as f64 / 1_000_000_000.0));
    }

    let change_amount = current_input_sum - required_total;
    let mut outputs = Vec::new();

    let recipient_bytes = hex::decode(&recipient_kyber_hex).map_err(|_| "Adresse invalide".to_string())?;
    let bob_pk = kyber768::PublicKey::from_bytes(&recipient_bytes).map_err(|_| "Clé Kyber corrompue".to_string())?;
    let (alice_shared_secret, kyber_capsule_1) = kyber768::encapsulate(&bob_pk);

    let mut otp_1 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_1);
    let payload_1 = format!("{}|{}", amount_in_flames, hex::encode(otp_1));
    let aes_key_1 = Key::<Aes256Gcm>::from_slice(alice_shared_secret.as_bytes());
    let mut nonce_bytes_1 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes_1);
    let encrypted_data_1 = Aes256Gcm::new(aes_key_1).encrypt(Nonce::from_slice(&nonce_bytes_1), payload_1.as_bytes()).map_err(|_| "Erreur AES".to_string())?;
    let mut final_vault_1 = nonce_bytes_1.to_vec(); final_vault_1.extend_from_slice(&encrypted_data_1);

    let mut bf_1 = [0u32; LATTICE_DIM];
    for val in bf_1.iter_mut() { *val = rand::thread_rng().gen_range(0..LATTICE_Q); }
    let commitment_1 = LWECommitment::commit(amount_in_flames, bf_1);

    outputs.push(TransactionOutput {
        stealth_address: format!("pq_watt_{}", hex::encode(&otp_1[0..8])),
        kyber_capsule: hex::encode(kyber_capsule_1.as_bytes()),
        aes_vault: hex::encode(final_vault_1),
        lattice_commitment: commitment_1.clone(),
    });

    if change_amount > 0 {
        let my_pk_bytes = hex::decode(&sender_kyber_public_hex).unwrap();
        let my_pk = kyber768::PublicKey::from_bytes(&my_pk_bytes).unwrap();
        let (my_shared_secret, kyber_capsule_2) = kyber768::encapsulate(&my_pk);

        let mut otp_2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_2);
        let payload_2 = format!("{}|{}", change_amount, hex::encode(otp_2));
        let aes_key_2 = Key::<Aes256Gcm>::from_slice(my_shared_secret.as_bytes());
        let mut nonce_bytes_2 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes_2);
        let encrypted_data_2 = Aes256Gcm::new(aes_key_2).encrypt(Nonce::from_slice(&nonce_bytes_2), payload_2.as_bytes()).unwrap();
        let mut final_vault_2 = nonce_bytes_2.to_vec(); final_vault_2.extend_from_slice(&encrypted_data_2);

        let sum_inputs_t0 = selected_utxos.iter().map(|u| u.2.t_vector[0] as u64).sum::<u64>() % (LATTICE_Q as u64);
        let fee_t0 = (fee * (LATTICE_Q as u64 / 2)) % LATTICE_Q as u64;
        let expected_outputs_sum = (sum_inputs_t0 + (LATTICE_Q as u64) - fee_t0) % (LATTICE_Q as u64);
        let perfect_change_t0 = (expected_outputs_sum + (LATTICE_Q as u64) - commitment_1.t_vector[0] as u64) % (LATTICE_Q as u64);

        let mut t_vector_2 = vec![0u32; LATTICE_DIM];
        t_vector_2[0] = perfect_change_t0 as u32;
        for i in 1..LATTICE_DIM { t_vector_2[i] = rand::thread_rng().gen_range(0..LATTICE_Q); }

        outputs.push(TransactionOutput {
            stealth_address: format!("pq_watt_{}", hex::encode(&otp_2[0..8])),
            kyber_capsule: hex::encode(kyber_capsule_2.as_bytes()),
            aes_vault: hex::encode(final_vault_2),
            lattice_commitment: LWECommitment { a_matrix_seed: [0u8; 32], t_vector: t_vector_2 },
        });
    } else {
        let sum_inputs_t0 = selected_utxos.iter().map(|u| u.2.t_vector[0] as u64).sum::<u64>() % (LATTICE_Q as u64);
        let fee_t0 = (fee * (LATTICE_Q as u64 / 2)) % LATTICE_Q as u64;
        outputs[0].lattice_commitment.t_vector[0] = ((sum_inputs_t0 + (LATTICE_Q as u64) - fee_t0) % (LATTICE_Q as u64)) as u32;
    }

    let tx_data_to_sign = format!("{:?}{}", outputs, fee);
    let mut final_inputs = Vec::new();

    let sk_bytes = hex::decode(&sender_dilithium_secret_hex).unwrap();
    let dilithium_secret = dilithium3::SecretKey::from_bytes(&sk_bytes).unwrap();
    let dilithium_signature = dilithium3::sign(tx_data_to_sign.as_bytes(), &dilithium_secret);

    let decoy_res = tor_fetch("GET", "/get_decoys/10", None).await.unwrap_or_default();
    let real_decoys: Vec<String> = serde_json::from_str(&decoy_res).unwrap_or_default();

    for utxo in &selected_utxos {
        let mut pq_ring: Vec<String> = real_decoys.clone();
        while pq_ring.len() < 10 {
            let (fake_pk, _) = dilithium3::keypair();
            pq_ring.push(hex::encode(fake_pk.as_bytes()));
        }
        pq_ring.push(sender_dilithium_public_hex.clone());
        pq_ring.shuffle(&mut rand::thread_rng());

        let my_real_index = pq_ring.iter().position(|r| r == &sender_dilithium_public_hex).unwrap();
        let n = pq_ring.len();
        let mut z_responses = vec![vec![0u32; LATTICE_DIM]; n];
        let mut p_keys = vec![vec![0u32; LATTICE_DIM]; n]; 
        let mut challenges_c = vec![vec![0u8; 32]; n];
        
        let mut s_vector = [0u32; LATTICE_DIM];
        let mut my_p = vec![0u32; LATTICE_DIM];
        for j in 0..LATTICE_DIM {
            let offset = j * 4;
            s_vector[j] = u32::from_le_bytes(sk_bytes[offset..offset+4].try_into().unwrap_or([0; 4])) % LATTICE_Q;
            let base_g = (j as u32 + 1) * 1337;
            my_p[j] = ((s_vector[j] as u64 * base_g as u64) % LATTICE_Q as u64) as u32; 
        }
        p_keys[my_real_index] = my_p;

        let mut alpha = vec![0u32; LATTICE_DIM];
        for j in 0..LATTICE_DIM { alpha[j] = rand::thread_rng().gen_range(0..LATTICE_Q); }

        let mut current_index = my_real_index;
        
        let mut hasher = blake3::Hasher::new();
        hasher.update(tx_data_to_sign.as_bytes());
        hasher.update(pq_ring[my_real_index].as_bytes()); 
        for j in 0..LATTICE_DIM {
            let base_g = (j as u32 + 1) * 1337; 
            let r_val = (alpha[j] as u64 * base_g as u64) % LATTICE_Q as u64;
            hasher.update(&(r_val as u32).to_le_bytes());
        }
        let mut next_c = hasher.finalize().as_bytes().to_vec();
        challenges_c[(current_index + 1) % n] = next_c.clone();

        for _ in 1..n {
            current_index = (current_index + 1) % n;
            let pk_hex = &pq_ring[current_index];
            
            for j in 0..LATTICE_DIM {
                p_keys[current_index][j] = rand::thread_rng().gen_range(0..LATTICE_Q);
                z_responses[current_index][j] = rand::thread_rng().gen_range(0..LATTICE_Q);
            }

            let c_i = u32::from_le_bytes(next_c[0..4].try_into().unwrap_or([0; 4])) % LATTICE_Q;

            let mut r_i = vec![0u32; LATTICE_DIM];
            for j in 0..LATTICE_DIM {
                let base_g = (j as u32 + 1) * 1337;
                let part1 = (z_responses[current_index][j] as u64 * base_g as u64) % LATTICE_Q as u64;
                let part2 = (c_i as u64 * p_keys[current_index][j] as u64) % LATTICE_Q as u64;
                r_i[j] = ((part1 + part2) % LATTICE_Q as u64) as u32;
            }

            let mut hasher_sim = blake3::Hasher::new();
            hasher_sim.update(tx_data_to_sign.as_bytes());
            hasher_sim.update(pk_hex.as_bytes()); 
            for val in r_i { hasher_sim.update(&val.to_le_bytes()); }
            next_c = hasher_sim.finalize().as_bytes().to_vec();
            challenges_c[(current_index + 1) % n] = next_c.clone();
        }

        let my_c = u32::from_le_bytes(challenges_c[my_real_index][0..4].try_into().unwrap_or([0; 4])) % LATTICE_Q;
        
        for j in 0..LATTICE_DIM {
            let cs_product = (my_c as u64 * s_vector[j] as u64) % LATTICE_Q as u64;
            if alpha[j] as u64 >= cs_product {
                z_responses[my_real_index][j] = (alpha[j] as u64 - cs_product) as u32;
            } else {
                z_responses[my_real_index][j] = (LATTICE_Q as u64 + alpha[j] as u64 - cs_product) as u32;
            }
        }

        let unique_seed = format!("{}{}", hex::encode(&sk_bytes), utxo.1);
        let key_image = hex::encode(blake3::hash(unique_seed.as_bytes()).as_bytes());

        let lattice_signature = PQLatticeRingSignature {
            key_image,
            c0: hex::encode(&challenges_c[0]),
            z_responses,
            p_keys, 
        };

        final_inputs.push(TransactionInput {
            pq_ring_inputs: pq_ring,
            pq_ring_signature: lattice_signature,
            commitment: utxo.2.clone(),
        });
    }

    let tx_type = match (htlc_hash_hex, htlc_timeout) {
        (Some(hash), Some(timeout)) => {
            TransactionType::HTLCLock { hash, timeout_block: timeout }
        },
        _ => TransactionType::Standard,
    };

    let tx_pq = Transaction {
        tx_type, 
        inputs: final_inputs,
        outputs, 
        fee,
        dilithium_signature: hex::encode(dilithium_signature.as_bytes()),
    };

    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    let _ = tor_fetch("POST", "/send_tx", Some(tx_json)).await?;

    use std::fs::OpenOptions;
    use std::io::Write;
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(".wattcoin_spends") {
        for utxo in &selected_utxos {
            let _ = writeln!(file, "{}", utxo.1);
        }
    }
    
    if tx_pq.tx_type == TransactionType::Standard {
        Ok(format!("☢️ TX ZKP ENVOYÉE !\nInputs : {}\nOutputs : {}", selected_utxos.len(), tx_pq.outputs.len()))
    } else {
        Ok("🔒 CONTRAT HTLC DÉPLOYÉ ! Les fonds sont verrouillés sur le réseau Wattcoin.".to_string())
    }
}

// 💡 NOUVELLE COMMANDE POUR LE REMBOURSEMENT HTLC
#[tauri::command]
async fn refund_wattcoin_swap(
    hash: String, watt_address: String, amount: f64
) -> Result<String, String> {
    
    let res_str = tor_fetch("GET", "/all_transactions", None).await?;
    let all_txs: Vec<Transaction> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;

    let mut locked_utxo = None;
    let mut locked_commitment = None;

    for tx in all_txs {
        if let TransactionType::HTLCLock { hash: lock_hash, .. } = &tx.tx_type {
            if lock_hash == &hash && !tx.outputs.is_empty() {
                locked_utxo = Some(tx.outputs[0].kyber_capsule.clone());
                locked_commitment = Some(tx.outputs[0].lattice_commitment.clone());
                break;
            }
        }
    }

    let (utxo_id, commitment) = match (locked_utxo, locked_commitment) {
        (Some(u), Some(c)) => (u, c),
        _ => return Err("Impossible de trouver les fonds verrouillés !".to_string()),
    };

    let key_image = hex::encode(blake3::hash(format!("REFUND_{}_{}", hash, utxo_id).as_bytes()).as_bytes());
    let dummy_signature = PQLatticeRingSignature { key_image, c0: String::new(), z_responses: vec![], p_keys: vec![] };
    let refund_input = TransactionInput { pq_ring_inputs: vec![], pq_ring_signature: dummy_signature, commitment };

    use pqcrypto_kyber::kyber768;
    let recipient_bytes = hex::decode(&watt_address).unwrap();
    let pk = kyber768::PublicKey::from_bytes(&recipient_bytes).unwrap();
    let (shared_secret, kyber_capsule) = kyber768::encapsulate(&pk);

    let total_locked_flames = (amount * 1_000_000_000.0) as u64; 
    let fee: u64 = 1000;
    if total_locked_flames <= fee { return Err("Montant trop faible pour payer les frais.".to_string()); }
    let amount_to_receive = total_locked_flames - fee;

    let mut otp = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp);
    let payload = format!("{}|{}", amount_to_receive, hex::encode(otp));
    
    let aes_key = Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes());
    let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let encrypted_data = Aes256Gcm::new(aes_key).encrypt(Nonce::from_slice(&nonce_bytes), payload.as_bytes()).unwrap();
    let mut final_vault = nonce_bytes.to_vec(); final_vault.extend_from_slice(&encrypted_data);

    let mut bf_claim = [0u32; LATTICE_DIM];
    for val in bf_claim.iter_mut() { *val = rand::thread_rng().gen_range(0..LATTICE_Q); }
    let out_commitment = LWECommitment::commit(amount_to_receive, bf_claim);

    let refund_tx = Transaction {
        tx_type: TransactionType::HTLCRefund { hash: hash.clone() },
        inputs: vec![refund_input],
        outputs: vec![ TransactionOutput {
            stealth_address: format!("pq_watt_{}", hex::encode(&otp[0..8])),
            kyber_capsule: hex::encode(kyber_capsule.as_bytes()),
            aes_vault: hex::encode(final_vault), lattice_commitment: out_commitment,
        }],
        fee, dilithium_signature: hash.clone(), 
    };

    let tx_json = serde_json::to_string(&refund_tx).map_err(|e| e.to_string())?;
    let _ = tor_fetch("POST", "/send_tx", Some(tx_json)).await?;

    Ok(format!("🔙 DEMANDE DE REMBOURSEMENT ENVOYÉE ! (Frais réseau payés : {} Flames).", fee))
}

#[tauri::command]
async fn get_active_swaps(btc_address: String, watt_address: String) -> Result<Vec<SwapContract>, String> {
    let res_str = tor_fetch("GET", "/swaps", None).await?;
    let all_swaps: Vec<SwapContract> = serde_json::from_str(&res_str).unwrap_or_default();
    
    let my_swaps: Vec<SwapContract> = all_swaps.into_iter()
        .filter(|s| s.buyer_btc_address == btc_address || s.seller_watt_address == watt_address)
        .collect();
        
    Ok(my_swaps)
}

#[tauri::command]
async fn claim_wattcoin_swap(
    secret: String, hash: String, watt_address: String, amount: f64
) -> Result<String, String> {
    let secret_bytes = hex::decode(&secret).unwrap_or_default();
    let calculated_hash = hex::encode(blake3::hash(&secret_bytes).as_bytes());
    if calculated_hash != hash { return Err("❌ Le secret révélé par le DEX est un faux !".to_string()); }

    let res_str = tor_fetch("GET", "/all_transactions", None).await?;
    let all_txs: Vec<Transaction> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;

    let mut locked_utxo = None;
    let mut locked_commitment = None;

    for tx in all_txs {
        if let TransactionType::HTLCLock { hash: lock_hash, .. } = &tx.tx_type {
            if lock_hash == &hash && !tx.outputs.is_empty() {
                locked_utxo = Some(tx.outputs[0].kyber_capsule.clone());
                locked_commitment = Some(tx.outputs[0].lattice_commitment.clone());
                break;
            }
        }
    }

    let (utxo_id, commitment) = match (locked_utxo, locked_commitment) {
        (Some(u), Some(c)) => (u, c),
        _ => return Err("⏳ Le vendeur n'a pas encore verrouillé ses WATT !".to_string()),
    };

    let key_image = hex::encode(blake3::hash(format!("CLAIM_{}_{}", secret, utxo_id).as_bytes()).as_bytes());
    let dummy_signature = PQLatticeRingSignature { key_image, c0: String::new(), z_responses: vec![], p_keys: vec![] };
    let claim_input = TransactionInput { pq_ring_inputs: vec![], pq_ring_signature: dummy_signature, commitment };

    use pqcrypto_kyber::kyber768;

    let recipient_bytes = hex::decode(&watt_address).unwrap();
    let pk = kyber768::PublicKey::from_bytes(&recipient_bytes).unwrap();
    let (shared_secret, kyber_capsule) = kyber768::encapsulate(&pk);

    let total_locked_flames = (amount * 1_000_000_000.0) as u64; 
    let fee: u64 = 1000;
    if total_locked_flames <= fee { return Err("❌ Montant bloqué trop faible pour payer les frais réseau.".to_string()); }
    let amount_to_receive = total_locked_flames - fee;

    let mut otp = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp);
    let payload = format!("{}|{}", amount_to_receive, hex::encode(otp));
    
    let aes_key = Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes());
    let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let encrypted_data = Aes256Gcm::new(aes_key).encrypt(Nonce::from_slice(&nonce_bytes), payload.as_bytes()).unwrap();
    let mut final_vault = nonce_bytes.to_vec(); final_vault.extend_from_slice(&encrypted_data);

    let mut bf_claim = [0u32; LATTICE_DIM];
    for val in bf_claim.iter_mut() { *val = rand::thread_rng().gen_range(0..LATTICE_Q); }
    let out_commitment = LWECommitment::commit(amount_to_receive, bf_claim);

    let claim_tx = Transaction {
        tx_type: TransactionType::HTLCClaim { secret: secret.clone() },
        inputs: vec![claim_input],
        outputs: vec![
            TransactionOutput {
                stealth_address: format!("pq_watt_{}", hex::encode(&otp[0..8])),
                kyber_capsule: hex::encode(kyber_capsule.as_bytes()),
                aes_vault: hex::encode(final_vault),
                lattice_commitment: out_commitment,
            }
        ],
        fee, 
        dilithium_signature: hash.clone(), 
    };

    let tx_json = serde_json::to_string(&claim_tx).map_err(|e| e.to_string())?;
    let _ = tor_fetch("POST", "/send_tx", Some(tx_json)).await?;

    Ok(format!("🎉 ATOMIC SWAP RÉUSSI ! Le secret a débloqué les fonds (Frais réseau payés : {} Flames).", fee))
}

#[tauri::command]
async fn cancel_order(order_id: String) -> Result<String, String> {
    tor_fetch("DELETE", &format!("/order/{}", order_id), None).await?;
    Ok("Ordre annulé avec succès".to_string())
}

#[tauri::command]
fn destroy_vault() -> Result<String, String> {
    if Path::new(VAULT_FILE).exists() {
        fs::remove_file(VAULT_FILE).map_err(|_| "⚠️ Impossible de supprimer le coffre.".to_string())?;
        Ok("🗑️ Coffre-fort nucléarisé avec succès. Adieu !".to_string())
    } else {
        Ok("Le coffre était déjà vide.".to_string())
    }
}

#[tauri::command]
fn save_miner_script(os: String, address: String) -> Result<String, String> {
    let home = if cfg!(windows) { std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".to_string()) } else { std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string()) };
    let base_dir = std::path::PathBuf::from(&home);
    
    let mut target_dir = base_dir.join("Downloads");
    if !target_dir.exists() { target_dir = base_dir.join("Téléchargements"); }
    if !target_dir.exists() { target_dir = base_dir.join("Desktop"); }
    if !target_dir.exists() { target_dir = base_dir.join("Bureau"); }
    if !target_dir.exists() { target_dir = base_dir; }

    let filename = if os == "linux" { "start_miner.sh" } else { "start_miner.bat" };
    let file_path = target_dir.join(filename);
    let short_addr = if address.len() > 15 { &address[0..15] } else { &address };

    let content = if os == "linux" {
        format!("#!/bin/bash\n\n# Lancement du Nœud Wattcoin\necho \"🔥 Démarrage du Nœud pour {}...\"\n./wattcoin_core 8001 {} jjbeptmy4b2ck5mc5sdjdc7kk6fkrva4laxfu7ufncmvk6qj6duh64yd.onion:8000 --live\n", short_addr, address)
    } else {
        format!("@echo off\n:: Lancement du Nœud Wattcoin\necho 🔥 Demarrage du Noeud pour {}...\nwattcoin_core.exe 8001 {} jjbeptmy4b2ck5mc5sdjdc7kk6fkrva4laxfu7ufncmvk6qj6duh64yd.onion:8000 --live\npause\n", short_addr, address)
    };

    std::fs::write(&file_path, content).map_err(|e| format!("Erreur d'écriture : {}", e))?;

    #[cfg(target_family = "unix")]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(mut perms) = std::fs::metadata(&file_path).map(|m| m.permissions()) {
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&file_path, perms);
        }
    }

    Ok(format!("Script généré avec succès dans :\n{}", file_path.display()))
}

#[tauri::command]
async fn create_btc_htlc(buyer_pubkey_hex: String, seller_pubkey_hex: String, secret_hex: String, locktime: u32) -> Result<String, String> {
    use bitcoin::hashes::{sha256, Hash};
    use bitcoin::blockdata::script::Builder;
    use bitcoin::opcodes::all::*;
    use bitcoin::{Address, Network};

    let buyer_pubkey = bitcoin::PublicKey::from_str(&buyer_pubkey_hex).map_err(|_| "Clé Alice invalide".to_string())?;
    let seller_pubkey = bitcoin::PublicKey::from_str(&seller_pubkey_hex).map_err(|_| "Clé Bob invalide".to_string())?;
    let secret_bytes = hex::decode(&secret_hex).map_err(|_| "Secret invalide".to_string())?;
    let btc_hash = sha256::Hash::hash(&secret_bytes);

    let htlc_script = Builder::new()
        .push_opcode(OP_IF)
            .push_opcode(OP_SHA256)
            .push_slice(&btc_hash.to_byte_array())
            .push_opcode(OP_EQUALVERIFY)
            .push_key(&seller_pubkey) 
        .push_opcode(OP_ELSE)
            .push_int(locktime as i64)
            .push_opcode(OP_CSV)
            .push_opcode(OP_DROP)
            .push_key(&buyer_pubkey)  
        .push_opcode(OP_ENDIF)
        .push_opcode(OP_CHECKSIG)
        .into_script();

    let address = Address::p2wsh(&htlc_script, Network::Testnet);
    Ok(address.to_string())
}

#[tauri::command]
async fn get_btc_balance(master_seed_hex: String) -> Result<f64, String> {
    let _tor = get_tor_client().await?; 

    let task = tokio::task::spawn_blocking(move || -> Result<f64, String> {
        use bdk::bitcoin::Network as BdkNetwork;
        use bdk::bitcoin::bip32::ExtendedPrivKey as BdkXpriv;
        use bdk::blockchain::esplora::EsploraBlockchainConfig;
        use bdk::blockchain::{EsploraBlockchain, ConfigurableBlockchain}; 
        use bdk::{Wallet, SyncOptions};
        use bdk::database::MemoryDatabase;

        let seed = hex::decode(&master_seed_hex).map_err(|_| "Erreur Seed".to_string())?;
        let xprv = BdkXpriv::new_master(BdkNetwork::Testnet, &seed).map_err(|e| e.to_string())?;
        let desc = format!("wpkh({}/84'/1'/0'/0/*)", xprv);
        let change_desc = format!("wpkh({}/84'/1'/0'/1/*)", xprv);

        let wallet = Wallet::new(&desc, Some(&change_desc), BdkNetwork::Testnet, MemoryDatabase::default())
            .map_err(|e| e.to_string())?;

        let endpoints = [
            "http://explorerzydxu5ecjrkwceayqybizmpjjznk5izmitf2modhcusuqlid.onion/testnet/api",
            "http://mempoolhqx4isw62xs7abwphsq7ldayuidyx2v2oethdxxj6vhok4niad.onion/testnet/api"
        ];

        let mut synced = false;
        for endpoint in endpoints {
            let config = EsploraBlockchainConfig {
                base_url: endpoint.to_string(), proxy: Some("socks5h://127.0.0.1:9150".to_string()), 
                concurrency: Some(4), stop_gap: 20, timeout: Some(120),
            };

            if let Ok(blockchain) = EsploraBlockchain::from_config(&config) {
                if wallet.sync(&blockchain, SyncOptions::default()).is_ok() {
                    synced = true; break;
                }
            }
        }

        if !synced { return Err("❌ Services cachés Bitcoin injoignables.".to_string()); }

        let balance = wallet.get_balance().map_err(|e| e.to_string())?;
        Ok((balance.confirmed + balance.untrusted_pending) as f64 / 100_000_000.0)
    });

    match tokio::time::timeout(std::time::Duration::from_secs(300), task).await {
        Ok(Ok(Ok(bal))) => Ok(bal),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(e)) => Err(format!("Thread crash: {}", e)),
        Err(_) => Err("Timeout API Bitcoin via Tor".to_string()),
    }
}

#[tauri::command]
async fn send_btc_to_htlc(master_seed_hex: String, htlc_address: String, amount_btc: f64) -> Result<String, String> {
    let _tor = get_tor_client().await?;
    
    let task = tokio::task::spawn_blocking(move || -> Result<String, String> {
        use bdk::bitcoin::Network as BdkNetwork;
        use bdk::bitcoin::bip32::ExtendedPrivKey as BdkXpriv;
        use bdk::bitcoin::Address as BdkAddress;
        use bdk::blockchain::esplora::EsploraBlockchainConfig;
        use bdk::blockchain::{EsploraBlockchain, ConfigurableBlockchain};
        use bdk::{Wallet, SyncOptions, SignOptions, FeeRate};
        use bdk::database::MemoryDatabase;

        let seed = hex::decode(&master_seed_hex).map_err(|_| "Erreur Seed".to_string())?;
        let xprv = BdkXpriv::new_master(BdkNetwork::Testnet, &seed).map_err(|e| e.to_string())?;
        let desc = format!("wpkh({}/84'/1'/0'/0/*)", xprv);
        let change_desc = format!("wpkh({}/84'/1'/0'/1/*)", xprv);

        let wallet = Wallet::new(&desc, Some(&change_desc), BdkNetwork::Testnet, MemoryDatabase::default())
            .map_err(|e| format!("Erreur Init Wallet: {}", e))?;

        let endpoints = [
            "http://explorerzydxu5ecjrkwceayqybizmpjjznk5izmitf2modhcusuqlid.onion/testnet/api",
            "http://mempoolhqx4isw62xs7abwphsq7ldayuidyx2v2oethdxxj6vhok4niad.onion/testnet/api"
        ];

        let mut active_blockchain = None;
        for endpoint in endpoints {
            let config = EsploraBlockchainConfig {
                base_url: endpoint.to_string(), proxy: Some("socks5h://127.0.0.1:9150".to_string()),
                concurrency: Some(4), stop_gap: 20, timeout: Some(120),
            };

            if let Ok(blockchain) = EsploraBlockchain::from_config(&config) {
                if wallet.sync(&blockchain, SyncOptions::default()).is_ok() {
                    active_blockchain = Some(blockchain); break;
                }
            }
        }

        let blockchain = active_blockchain.ok_or_else(|| "❌ Services cachés BTC injoignables.".to_string())?;

        let target_address = BdkAddress::from_str(&htlc_address).map_err(|_| "Adresse HTLC invalide".to_string())?;
        let amount_sats = (amount_btc * 100_000_000.0) as u64;

        let (mut psbt, _details) = {
            let mut builder = wallet.build_tx();
            builder.add_recipient(target_address.payload.script_pubkey(), amount_sats);
            builder.fee_rate(FeeRate::from_sat_per_vb(2.0)); 
            builder.finish().map_err(|e| format!("Erreur TX Builder: {}", e))?
        };

        let finalized = wallet.sign(&mut psbt, SignOptions::default()).map_err(|e| e.to_string())?;
        if !finalized { return Err("❌ BDK n'a pas pu signer.".to_string()); }

        let raw_tx = psbt.extract_tx();
        bdk::blockchain::Blockchain::broadcast(&blockchain, &raw_tx).map_err(|e| format!("Erreur Broadcast: {}", e))?;

        Ok(format!("✅ Contrat BTC déployé via Tor !\nTXID: {}", raw_tx.txid()))
    });

    match tokio::time::timeout(std::time::Duration::from_secs(300), task).await {
        Ok(Ok(Ok(res))) => Ok(res),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(e)) => Err(format!("Thread crash: {}", e)),
        Err(_) => Err("Timeout Tor".to_string()),
    }
}

#[tauri::command]
async fn send_btc_direct(master_seed_hex: String, recipient_address: String, amount_btc: f64) -> Result<String, String> {
    let _tor = get_tor_client().await?; 
    
    let task = tokio::task::spawn_blocking(move || -> Result<String, String> {
        use bdk::bitcoin::Network as BdkNetwork;
        use bdk::bitcoin::bip32::ExtendedPrivKey as BdkXpriv;
        use bdk::bitcoin::Address as BdkAddress;
        use bdk::blockchain::esplora::EsploraBlockchainConfig;
        use bdk::blockchain::{EsploraBlockchain, ConfigurableBlockchain};
        use bdk::{Wallet, SyncOptions, SignOptions, FeeRate};
        use bdk::database::MemoryDatabase;
        use std::str::FromStr;

        let seed = hex::decode(&master_seed_hex).map_err(|_| "Erreur Seed".to_string())?;
        let xprv = BdkXpriv::new_master(BdkNetwork::Testnet, &seed).map_err(|e| e.to_string())?;
        let desc = format!("wpkh({}/84'/1'/0'/0/*)", xprv);
        let change_desc = format!("wpkh({}/84'/1'/0'/1/*)", xprv);

        let wallet = Wallet::new(&desc, Some(&change_desc), BdkNetwork::Testnet, MemoryDatabase::default())
            .map_err(|e| format!("Erreur Init Wallet: {}", e))?;

        let endpoints = [
            "http://explorerzydxu5ecjrkwceayqybizmpjjznk5izmitf2modhcusuqlid.onion/testnet/api",
            "http://mempoolhqx4isw62xs7abwphsq7ldayuidyx2v2oethdxxj6vhok4niad.onion/testnet/api"
        ];

        let mut active_blockchain = None;
        for endpoint in endpoints {
            let config = EsploraBlockchainConfig {
                base_url: endpoint.to_string(), proxy: Some("socks5h://127.0.0.1:9150".to_string()),
                concurrency: Some(4), stop_gap: 20, timeout: Some(120),
            };

            if let Ok(blockchain) = EsploraBlockchain::from_config(&config) {
                if wallet.sync(&blockchain, SyncOptions::default()).is_ok() {
                    active_blockchain = Some(blockchain); break;
                }
            }
        }

        let blockchain = active_blockchain.ok_or_else(|| "❌ Services cachés BTC injoignables.".to_string())?;
        let target_address = BdkAddress::from_str(&recipient_address).map_err(|_| "Adresse invalide".to_string())?;
        let amount_sats = (amount_btc * 100_000_000.0) as u64;

        let balance = wallet.get_balance().map_err(|e| e.to_string())?;
        if (balance.confirmed + balance.untrusted_pending) < amount_sats + 1000 {
            return Err("Fonds insuffisants !".to_string());
        }

        let (mut psbt, _details) = {
            let mut builder = wallet.build_tx();
            builder.add_recipient(target_address.payload.script_pubkey(), amount_sats);
            builder.fee_rate(FeeRate::from_sat_per_vb(2.0)); 
            builder.finish().map_err(|e| format!("Erreur Builder: {}", e))?
        };

        let finalized = wallet.sign(&mut psbt, SignOptions::default()).map_err(|e| e.to_string())?;
        if !finalized { return Err("Échec signature".to_string()); }

        let raw_tx = psbt.extract_tx();
        bdk::blockchain::Blockchain::broadcast(&blockchain, &raw_tx).map_err(|e| format!("Erreur Broadcast: {}", e))?;

        Ok(format!("✅ BTC envoyés via Tor !\nTXID: {}", raw_tx.txid()))
    });

    match tokio::time::timeout(std::time::Duration::from_secs(300), task).await {
        Ok(Ok(Ok(res))) => Ok(res),
        Ok(Ok(Err(e))) => Err(e),
        Ok(Err(e)) => Err(format!("Thread crash: {}", e)),
        Err(_) => Err("Timeout Tor".to_string()),
    }
}

#[tauri::command]
async fn claim_btc_swap(master_seed_hex: String, htlc_address: String, secret_hex: String, buyer_pubkey_hex: String, seller_pubkey_hex: String) -> Result<String, String> {
    use bdk::bitcoin::Network;
    use bdk::bitcoin::bip32::{ExtendedPrivKey, DerivationPath}; 
    use bdk::bitcoin::{Address, PrivateKey, PublicKey, OutPoint, Txid, Sequence, Transaction, TxIn, TxOut, Witness, ScriptBuf};
    use bdk::bitcoin::blockdata::script::Builder;
    use bdk::bitcoin::opcodes::all::*;
    use bdk::bitcoin::hashes::{sha256, Hash};
    use bdk::bitcoin::secp256k1::{Secp256k1, Message};
    use bdk::bitcoin::sighash::{SighashCache, EcdsaSighashType}; 
    use bdk::bitcoin::absolute::LockTime; 
    use std::str::FromStr;

    let _tor = get_tor_client().await?; 

    let seed = hex::decode(&master_seed_hex).map_err(|_| "Erreur Seed".to_string())?;
    let xprv = ExtendedPrivKey::new_master(Network::Testnet, &seed).map_err(|e| e.to_string())?;
    let secp = Secp256k1::new();
    
    let path = DerivationPath::from_str("m/84'/1'/0'/0/0").unwrap();
    let child = xprv.derive_priv(&secp, &path).map_err(|e| e.to_string())?;
    let bob_priv_key = PrivateKey::new(child.private_key, Network::Testnet);
    
    let buyer_pubkey = PublicKey::from_str(&buyer_pubkey_hex).unwrap();
    let seller_pubkey = PublicKey::from_str(&seller_pubkey_hex).unwrap();
    let bob_address = Address::p2wpkh(&seller_pubkey, Network::Testnet).unwrap();

    let secret_bytes = hex::decode(&secret_hex).map_err(|_| "Secret invalide".to_string())?;
    let btc_hash = sha256::Hash::hash(&secret_bytes); 
    
    let htlc_script = Builder::new()
        .push_opcode(OP_IF)
            .push_opcode(OP_SHA256)
            .push_slice(&btc_hash.to_byte_array()) 
            .push_opcode(OP_EQUALVERIFY)
            .push_key(&seller_pubkey) 
        .push_opcode(OP_ELSE)
            .push_int(144)
            .push_opcode(OP_CSV)
            .push_opcode(OP_DROP)
            .push_key(&buyer_pubkey)
        .push_opcode(OP_ENDIF)
        .push_opcode(OP_CHECKSIG)
        .into_script();

    let p2wsh_address = Address::p2wsh(&htlc_script, Network::Testnet);
    if p2wsh_address.to_string() != htlc_address { return Err("Erreur critique: Script reconstruit invalide.".to_string()); }

    let proxy = reqwest::Proxy::all("socks5h://127.0.0.1:9150").map_err(|_| "Impossible de lier le proxy".to_string())?;
    let client = reqwest::Client::builder().proxy(proxy).build().map_err(|_| "Erreur HTTP".to_string())?;

    let endpoints = [
        "http://explorerzydxu5ecjrkwceayqybizmpjjznk5izmitf2modhcusuqlid.onion/testnet/api",
        "http://mempoolhqx4isw62xs7abwphsq7ldayuidyx2v2oethdxxj6vhok4niad.onion/testnet/api"
    ];

    let mut utxos = None;
    let mut active_endpoint = String::new();

    for endpoint in endpoints {
        let url = format!("{}/address/{}/utxo", endpoint, htlc_address);
        if let Ok(res) = client.get(&url).send().await {
            if let Ok(json) = res.json::<serde_json::Value>().await {
                utxos = Some(json);
                active_endpoint = endpoint.to_string();
                break;
            }
        }
    }

    let utxos = utxos.ok_or_else(|| "❌ Services cachés Bitcoin injoignables.".to_string())?;
    if utxos.as_array().unwrap_or(&vec![]).is_empty() { return Err("❌ Aucun Bitcoin trouvé dans le contrat !".to_string()); }
    
    let txid_str = utxos[0]["txid"].as_str().unwrap();
    let txid = Txid::from_str(txid_str).map_err(|e| e.to_string())?;
    let vout = utxos[0]["vout"].as_u64().unwrap() as u32;
    let value_sats = utxos[0]["value"].as_u64().unwrap();

    let fee_sats = 600; 
    if value_sats <= fee_sats { return Err("Montant trop faible pour payer les frais !".to_string()); }
    let amount_to_receive = value_sats - fee_sats;

    let txin = TxIn {
        previous_output: OutPoint { txid, vout },
        script_sig: ScriptBuf::new(), 
        sequence: Sequence::MAX, 
        witness: Witness::new(), 
    };

    let txout = TxOut {
        value: amount_to_receive,
        script_pubkey: bob_address.script_pubkey(),
    };

    let mut tx = Transaction {
        version: 2, lock_time: LockTime::ZERO, input: vec![txin], output: vec![txout],
    };

    let mut sighash_cache = SighashCache::new(&mut tx);
    let sighash = sighash_cache.segwit_signature_hash(0, &htlc_script, value_sats, EcdsaSighashType::All).map_err(|e| e.to_string())?;

    let message = Message::from_slice(&sighash.to_byte_array()).unwrap();
    let signature = secp.sign_ecdsa(&message, &bob_priv_key.inner);
    
    let mut sig_with_hashtype = signature.serialize_der().to_vec();
    sig_with_hashtype.push(EcdsaSighashType::All as u8);

    let mut witness = Witness::new();
    witness.push(sig_with_hashtype);  
    witness.push(secret_bytes);       
    witness.push(vec![1]);            
    witness.push(htlc_script.into_bytes()); 

    tx.input[0].witness = witness;

    let raw_tx_bytes = bdk::bitcoin::consensus::encode::serialize(&tx);
    let raw_tx_hex = hex::encode(raw_tx_bytes);

    let broadcast_url = format!("{}/tx", active_endpoint);
    let broadcast_res = client.post(&broadcast_url)
        .body(raw_tx_hex)
        .send().await.map_err(|_| "Erreur réseau (Broadcast via Tor)".to_string())?;

    if broadcast_res.status().is_success() {
         Ok(format!("🎉 VRAI SWAP RÉUSSI VIA TOR ! Tx diffusée : {}\nVous avez reçu {} sats !", tx.txid(), amount_to_receive))
    } else {
         let err_text = broadcast_res.text().await.unwrap_or_default();
         Err(format!("Rejeté par Bitcoin : {}", err_text))
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_handle = app.handle().clone();
            
            tauri::async_runtime::spawn(async move {
                let mut last_blocks = 0;
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if let Ok(res_str) = tor_fetch("GET", "/info", None).await {
                        if let Ok(info) = serde_json::from_str::<serde_json::Value>(&res_str) {
                            if let Some(blocks) = info["blocks"].as_u64() {
                                if blocks > last_blocks {
                                    last_blocks = blocks;
                                    let _ = app_handle.emit("network-update", ());
                                }
                            }
                        }
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_network_info, generate_pro_wallet, encrypt_vault, unlock_vault, vault_exists,
            submit_order, get_dark_pool, get_watt_balance, get_btc_balance, cancel_order,
            send_wattcoin, create_btc_htlc, send_btc_to_htlc, claim_wattcoin_swap, refund_wattcoin_swap,
            destroy_vault, get_active_swaps, claim_btc_swap, send_btc_direct, get_history, 
            save_miner_script
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}