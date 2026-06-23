#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};
use rand::{Rng, RngCore, SeedableRng};
use serde::{Serialize, Deserialize};
use std::str::FromStr;
use std::fs; 
use std::path::PathBuf;
use std::time::Duration;
use tauri::{Emitter, Manager};
use sha2::{Sha512, Digest};

// 💡 NOS BEAUX MODULES PROPRES !
mod wots;
mod lattice;
mod merkle_ring; 
mod transaction;

use transaction::{Transaction, TransactionType, TransactionInput, TransactionOutput, SwapContract};
use lattice::{LWECommitment, LATTICE_DIM, LATTICE_Q};

use pqc_kyber::{keypair, encapsulate, decapsulate};

use once_cell::sync::Lazy;
use arti_client::{TorClient, TorClientConfig, StreamPrefs};
use tokio::io::{AsyncWriteExt, AsyncReadExt};
use tokio::sync::Mutex as AsyncMutex;
use std::sync::Mutex as StdMutex;
use tor_rtcompat::PreferredRuntime;

static TOR_CLIENT: Lazy<AsyncMutex<Option<TorClient<PreferredRuntime>>>> = Lazy::new(|| AsyncMutex::new(None));
static TOR_LOCK: Lazy<AsyncMutex<()>> = Lazy::new(|| AsyncMutex::new(()));
// 💡 ASTUCE ANDROID : On stocke le chemin global pour que Tor sache où écrire sur le téléphone
static APP_DATA_DIR: Lazy<StdMutex<Option<PathBuf>>> = Lazy::new(|| StdMutex::new(None));

const ONION_NODE: &str = "jjbeptmy4b2ck5mc5sdjdc7kk6fkrva4laxfu7ufncmvk6qj6duh64yd.onion:8100";
const MATURITY_BLOCKS: u64 = 3; 

// ===================================================================
// 🔥 SWITCH LOCAL / PROD WALLET (identique au node !)
// ===================================================================
const LOCAL_DEV_MODE: bool = false; // ← pour PROD : décommente celle-ci + commente la ligne du dessus
//const LOCAL_DEV_MODE: bool = true; 
// ===================================================================

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

#[derive(Serialize, Deserialize, Clone)]
pub struct WalletKeys {
    mnemonic: String,
    btc_address: String,
    btc_pubkey_hex: String,
    watt_address: String, 
    master_seed_hex: String,
    kyber_secret_hex: String,
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

#[derive(Serialize, Deserialize, Clone)]
pub struct Order {
    pub id: String,
    pub order_type: String,
    pub amount_flames: u64,
    pub price_sats: u64,
    pub btc_address: String,
    pub btc_pubkey: String, 
    pub watt_address: String,
    pub expires_at: i64,
    pub htlc_hash: Option<String>,
}
	
#[derive(Serialize)]
pub struct Balances {
    pub l1: f64,
    pub l2: f64,
}

// 💡 ASTUCE ANDROID : Les chemins renvoient désormais des Result pour ne jamais faire crasher React !
fn get_vault_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let mut path = app.path().app_data_dir().map_err(|e| e.to_string())?;
    if !path.exists() { std::fs::create_dir_all(&path).map_err(|e| e.to_string())?; }
    path.push(".wattcoin_vault");
    Ok(path)
}

fn get_spends_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let mut path = app.path().app_data_dir().map_err(|e| e.to_string())?;
    if !path.exists() { std::fs::create_dir_all(&path).map_err(|e| e.to_string())?; }
    path.push(".wattcoin_spends");
    Ok(path)
}

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
    
    // 💡 ASTUCE ANDROID : On force Arti à utiliser l'espace sécurisé du téléphone
    let app_data_dir = {
        let dir_lock = APP_DATA_DIR.lock().unwrap();
        dir_lock.clone().ok_or("Dossier système non initialisé")?
    };

    let mut state_dir = app_data_dir.clone();
    state_dir.push("arti_state");
    let mut cache_dir = app_data_dir.clone();
    cache_dir.push("arti_cache");

    let _ = std::fs::create_dir_all(&state_dir);
    let _ = std::fs::create_dir_all(&cache_dir);

    // 🔥 LA CORRECTION EST ICI : On utilise le nouveau builder d'Arti
    let mut builder = TorClientConfig::builder();
    builder.storage()
        .state_dir(arti_client::config::CfgPath::new(state_dir.to_str().unwrap().to_string()))
        .cache_dir(arti_client::config::CfgPath::new(cache_dir.to_str().unwrap().to_string()));
        
    let config = builder.build().map_err(|e| format!("Erreur builder Arti: {}", e))?;

    for _ in 1..=3 {
        match TorClient::create_bootstrapped(config.clone()).await {
            Ok(client) => {
                start_arti_socks_proxy(client.clone()).await;
                *lock = Some(client.clone());
                return Ok(client);
            }
            Err(_) => {
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
    Err("Impossible de démarrer Tor".to_string())
}

async fn node_call(method: &str, endpoint: &str, body: Option<String>) -> Result<String, String> {
    if LOCAL_DEV_MODE {
        println!("🔓 [LOCAL WALLET] Appel HTTP direct (pas de Tor) → 127.0.0.1:8100{}", endpoint);
        let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().unwrap();
        let url = format!("http://127.0.0.1:8100{}", endpoint);
        let req = if method == "POST" {
            client.post(&url).header("Content-Type", "application/json").body(body.unwrap_or_default())
        } else {
            client.get(&url)
        };
        let resp = req.send().await.map_err(|e| format!("HTTP local: {}", e))?;
        if !resp.status().is_success() {
            return Err(format!("Node {} → {}", resp.status(), resp.text().await.unwrap_or_default()));
        }
        return Ok(resp.text().await.unwrap_or_default());
    }
    tor_fetch(method, endpoint, body).await
}

async fn tor_fetch(method: &str, endpoint: &str, body: Option<String>) -> Result<String, String> {
    let _guard = tokio::time::timeout(std::time::Duration::from_secs(60), TOR_LOCK.lock())
        .await.map_err(|_| "❌ [TOR] Timeout file d'attente".to_string())?;

    let tor_client = get_tor_client().await?;
    let mut prefs = StreamPrefs::new();
    prefs.connect_to_onion_services(arti_client::config::BoolOrAuto::Explicit(true));

    println!("🕵️ [TOR] Début de la mission vers {}", endpoint);
    
    let mut stream = None;
    for _i in 1..=3 {
        match tokio::time::timeout(std::time::Duration::from_secs(30), tor_client.connect_with_prefs(ONION_NODE, &prefs)).await {
            Ok(Ok(s)) => { 
                stream = Some(s); 
                break; 
            },
            Ok(Err(e)) => println!("⚠️ [TOR] Arti a échoué : {}", e),
            Err(_) => println!("⚠️ [TOR] Timeout 20s !"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    let mut stream = stream.ok_or_else(|| "❌ [TOR] Abandon de la mission.".to_string())?;

    let req = if let Some(ref b) = body {
        format!("{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: WattcoinWallet/1.0\r\nAccept: application/json\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", method, endpoint, ONION_NODE, b.len(), b)
    } else {
        format!("{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: WattcoinWallet/1.0\r\nAccept: application/json\r\nConnection: close\r\n\r\n", method, endpoint, ONION_NODE)
    };

    stream.write_all(req.as_bytes()).await.map_err(|e| format!("Erreur écriture: {}", e))?;
    stream.flush().await.map_err(|e| format!("Erreur flush: {}", e))?;

    let mut response = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf)).await {
            Ok(Ok(0)) => { break; }
            Ok(Ok(n)) => { response.extend_from_slice(&buf[..n]); }
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
            let error_body = body_content.trim();
            println!("❌ [NODE ERROR] 400 reçu → {}", error_body);
            Err(format!("Node a refusé : {}", error_body))
        }
    } else {
        println!("❌ [TOR] Réponse inexploitable ({} octets)", response.len());
        Err("Réponse corrompue".to_string())
    }
}

#[tauri::command]
async fn get_network_info() -> Result<serde_json::Value, String> {
    let res_str = node_call("GET", "/info", None).await?;
    serde_json::from_str(&res_str).map_err(|e| {
        println!("❌ [JSON ERROR INFO] {} | Data: {}", e, res_str);
        e.to_string()
    })
}

#[tauri::command]
async fn get_total_supply() -> Result<u64, String> {
    let res_str = node_call("GET", "/supply", None).await?;
    let supply: u64 = serde_json::from_str(&res_str).unwrap_or(0);
    Ok(supply)
}

#[tauri::command]
async fn get_current_jackpot() -> Result<u64, String> {
    let res_str = node_call("GET", "/jackpot", None).await?;
    let pot: u64 = serde_json::from_str(&res_str).unwrap_or(0);
    Ok(pot)
}

#[tauri::command]
fn create_swap_secret() -> serde_json::Value {
    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    let hash = Sha512::digest(&secret);
    
    serde_json::json!({
        "secret": hex::encode(secret),
        "hash": hex::encode(hash)
    })
}

#[tauri::command]
async fn submit_order(
    order_type: String, amount: f64, price: f64, btc_address: String, 
    btc_pubkey: String, watt_address: String, htlc_hash: Option<String> 
) -> Result<(), String> {
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
        "expires_at": expires_at,
        "htlc_hash": htlc_hash 
    });

    node_call("POST", "/order", Some(order_data.to_string())).await?;
    Ok(())
}

#[tauri::command]
async fn get_dark_pool() -> Result<Vec<Order>, String> {
    let res_str = node_call("GET", "/pool", None).await?;
    let pool = serde_json::from_str::<Vec<Order>>(&res_str).map_err(|e| e.to_string())?;
    Ok(pool)
}

#[tauri::command]
async fn generate_pro_wallet(phrase_option: Option<String>, password: String) -> Result<WalletKeys, String> {
    use bip39::{Mnemonic, Language};
    use bitcoin::Network as BtcNetwork;
    use bitcoin::bip32::{Xpriv, DerivationPath}; 
    use bitcoin::{PrivateKey as BtcPrivateKey, PublicKey as BtcPublicKey, Address as BtcAddress};
    use bitcoin::secp256k1::Secp256k1;
    use sha2::{Sha512, Digest};
	use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let mnemonic = match phrase_option {
        Some(phrase) => {
			// 💡 ASTUCE ANDROID ULTIME : Normalisation Unicode + Chasse aux fantômes
			use unicode_normalization::UnicodeNormalization;

			let phrase_clean = phrase
				.replace('\u{200B}', "")  // Supprime le Zero-Width Space
				.replace('\u{200E}', "")  // Supprime le Left-to-Right Mark
				.replace('\u{200F}', "")  // Supprime le Right-to-Left Mark
				.replace('\u{00A0}', " ") // Transforme les espaces insécables en vrais espaces
				.to_lowercase()
				.nfc()                    // 🛡️ MAGIE : Recompose les accents (e + ´ devient é) !
				.collect::<String>();

			let words: Vec<&str> = phrase_clean.split_whitespace().collect();
			
			// On affiche le nombre de mots reçus dans l'erreur pour aider au debug UI
			if words.len() != 48 { 
				return Err(format!("La phrase doit contenir exactement 48 mots (Reçu : {}).", words.len())); 
			}
			
			let phrase1 = words[0..24].join(" ");
			let phrase2 = words[24..48].join(" ");
			
			let _ = Mnemonic::parse_in(Language::French, &phrase1)
				.map_err(|_| "La première moitié (1-24) est invalide ou contient un mot inconnu.")?;
			let _ = Mnemonic::parse_in(Language::French, &phrase2)
				.map_err(|_| "La deuxième moitié (25-48) est invalide ou contient un mot inconnu.")?;
			
			phrase_clean
		},
        None => {
            let mut ent1 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut ent1);
            let mut ent2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut ent2);
            let m1 = Mnemonic::from_entropy_in(Language::French, &ent1).unwrap();
            let m2 = Mnemonic::from_entropy_in(Language::French, &ent2).unwrap();
            format!("{} {}", m1, m2)
        }
    };

    // On fusionne la phrase ET le mot de passe !
    let salted_entropy = format!("wattcoin_bip39_salt:{}:{}", password, mnemonic);
    let mut current_hash = Sha512::digest(salted_entropy.as_bytes()).to_vec();
    
    // Key Stretching (2048 itérations comme le standard BIP39)
    // Rend le bruteforce impossible, même avec des ASICs.
    for _ in 0..2048 {
        current_hash = Sha512::digest(&current_hash).to_vec();
    }
    
    let master_seed = current_hash; // La Master Seed dépend maintenant ABSOLUMENT du passe.

    // 1. Dérivation Bitcoin
	let secp = Secp256k1::new();
	let root = Xpriv::new_master(BtcNetwork::Testnet, &master_seed).unwrap(); 
	let path = DerivationPath::from_str("m/84'/1'/0'/0/0").unwrap();
	let child = root.derive_priv(&secp, &path).unwrap();
	let btc_priv = BtcPrivateKey::new(child.private_key, BtcNetwork::Testnet);
	let btc_pub = BtcPublicKey::from_private_key(&secp, &btc_priv);
	let compressed_pubkey = bitcoin::CompressedPublicKey::try_from(btc_pub).unwrap();
	let btc_address = BtcAddress::p2wpkh(&compressed_pubkey, BtcNetwork::Testnet).to_string();

	// 2. Dérivation Kyber 100% Déterministe
	// On prend les 32 premiers octets de ta graine maître pour amorcer le générateur
	let mut seed_array = [0u8; 32];
	seed_array.copy_from_slice(&master_seed[0..32]);
	let mut deterministic_rng = rand::rngs::StdRng::from_seed(seed_array);

	// La clé générée sera TOUJOURS la même pour ces 48 mots précis !
	let kyber_keys = keypair(&mut deterministic_rng).map_err(|_| "Erreur génération Kyber")?;

	Ok(WalletKeys {
		mnemonic, 
		btc_address,
		btc_pubkey_hex: btc_pub.to_string(),
		master_seed_hex: hex::encode(&master_seed),
		watt_address: URL_SAFE_NO_PAD.encode(kyber_keys.public),
		kyber_secret_hex: hex::encode(kyber_keys.secret),
	})
}

#[tauri::command] 
fn vault_exists(app: tauri::AppHandle) -> bool { 
    get_vault_path(&app).map(|p| p.exists()).unwrap_or(false) 
}

#[tauri::command]
fn encrypt_vault(app: tauri::AppHandle, password: String, keys_json_string: String) -> Result<(), String> {
    let vault_path = get_vault_path(&app)?;
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
    
    fs::write(vault_path, final_data).map_err(WattError::from)?;
    Ok(())
}

#[tauri::command]
async fn unlock_vault(app: tauri::AppHandle, password: String) -> Result<WalletKeys, String> {
    use pbkdf2::pbkdf2_hmac;
    let vault_path = get_vault_path(&app)?;
    let file_data = fs::read(vault_path).map_err(|e| WattError::Io(e))?;
    if file_data.len() < 28 { return Err(WattError::Vault("Fichier corrompu ou incomplet.".to_string()).into()); }

    let salt = &file_data[0..16];
    let nonce_bytes = &file_data[16..28];
    let ciphertext = &file_data[28..];

    let mut key = [0u8; 32];
    pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), salt, 100_000, &mut key);

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
async fn get_balances(app: tauri::AppHandle, keys: WalletKeys) -> Result<Balances, String> {
    let res_str = node_call("GET", "/all_transactions", None).await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).unwrap_or_default();

    let current_height = get_current_block_height().await.unwrap_or(0);
    let mut l1_flames: u64 = 0;
    let mut l2_flames: u64 = 0;
    
    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();

    let mut spent_capsules = std::collections::HashSet::new();
    if let Ok(spends_path) = get_spends_path(&app) {
        if let Ok(spends) = std::fs::read_to_string(&spends_path) {
            for line in spends.lines() { spent_capsules.insert(line.trim().to_string()); }
        }
    }

    let decrypt_amount = |out: &TransactionOutput| -> Option<u64> {
        if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
            if let Ok(shared_secret) = decapsulate(&capsule_bytes, &sk_bytes) {
                if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                    if vault_bytes.len() > 12 {
                        let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                        let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&shared_secret));
                        if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                            if let Ok(payload_str) = String::from_utf8(plaintext) {
                                let parts: Vec<&str> = payload_str.split('|').collect();
                                if parts.len() == 2 { return parts[0].parse::<u64>().ok(); }
                            }
                        }
                    }
                }
            }
        }
        None
    };

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            if spent_capsules.contains(&out.kyber_capsule) { continue; }
            let mut is_mature = true;
            if out.stealth_address.starts_with("COINBASE_") && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { is_mature = false; }
            if !is_mature { continue; }

            if out.stealth_address == format!("JACKPOT_{}", keys.watt_address) || out.stealth_address == format!("COINBASE_{}", keys.watt_address) {
                if let Ok(amt) = out.aes_vault.parse::<u64>() { l1_flames += amt; }
            } else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("htlc_watt_") {
                if let Some(amt) = decrypt_amount(out) { l1_flames += amt; }
            } else if out.stealth_address.starts_with("L2_WATT_") {
                if let Some(amt) = decrypt_amount(out) { l2_flames += amt; }
            }
        }
    }

    let res_mempool = node_call("GET", "/mempool", None).await.unwrap_or_else(|_| "[]".to_string());
    if let Ok(mempool_txs) = serde_json::from_str::<Vec<Transaction>>(&res_mempool) {
        for tx in mempool_txs {
            for out in tx.outputs.iter() {
                if spent_capsules.contains(&out.kyber_capsule) { continue; }
                if out.stealth_address.starts_with("L2_WATT_") {
                    if let Some(amt) = decrypt_amount(out) { l2_flames += amt; }
                } else if out.stealth_address.starts_with("pq_watt_") {
                    if let Some(amt) = decrypt_amount(out) { l1_flames += amt; }
                }
            }
        }
    }

    Ok(Balances {
        l1: l1_flames as f64 / 1_000_000_000.0,
        l2: l2_flames as f64 / 1_000_000_000.0,
    })
}

async fn get_current_block_height() -> Result<u64, String> {
    let info_str = node_call("GET", "/info", None).await?;
    let info: serde_json::Value = serde_json::from_str(&info_str).map_err(|_| "err".to_string())?;
    Ok(info["blocks"].as_u64().unwrap_or(0))
}

#[tauri::command]
async fn get_history(app: tauri::AppHandle, keys: WalletKeys) -> Result<Vec<HistoryItem>, String> {
    use chrono::{DateTime, Utc, Local};

    let res_str = node_call("GET", "/all_transactions", None).await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str)
        .map_err(|_| "Erreur JSON history".to_string())?;

    let current_height = get_current_block_height().await.unwrap_or(0);
    let mut history = Vec::new();
    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();

    let mut spent_capsules = std::collections::HashSet::new();
    if let Ok(spends_path) = get_spends_path(&app) {
        if let Ok(spends) = std::fs::read_to_string(&spends_path) {
            for line in spends.lines() { spent_capsules.insert(line.trim().to_string()); }
        }
    }

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let timestamp = item["timestamp"].as_i64().unwrap_or(0);

        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) {
            Ok(t) => t,
            Err(_) => continue,
        };
		
		for out in tx.outputs.iter() {
            let is_spent = spent_capsules.contains(&out.kyber_capsule);
            let status_text = if is_spent { "Dépensé" } else { "Disponible" };
			
            let layer_tag = if out.stealth_address.starts_with("L2_WATT_") { "⚡ L2" } else { "💲 L1" };

            let mut is_mature = true;
            if out.stealth_address.starts_with("COINBASE_") && height > 0 && current_height.saturating_sub(height) < MATURITY_BLOCKS {
                is_mature = false;
            }
            if !is_mature { continue; }

            let date_str = if timestamp > 0 {
                let dt: DateTime<Utc> = DateTime::from_timestamp(timestamp, 0).unwrap_or_default();
                dt.with_timezone(&Local).format("%d/%m/%Y %H:%M").to_string()
            } else {
                "En attente".to_string()
            };

            if out.stealth_address == format!("COINBASE_{}", keys.watt_address) 
                || out.stealth_address == format!("JACKPOT_{}", keys.watt_address) 
            {
                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                    let label = if out.stealth_address.starts_with("JACKPOT") { "Jackpot gagné ! 🎰" } else { "Récompense minage ⛏️" };
                    history.push(HistoryItem {
                        id: format!("#{}", height),
                        tx_type: "receive".to_string(),
                        amount: amt as f64 / 1_000_000_000.0,
                        coin: "WATT".to_string(),
                        date: date_str,
                        status: format!("{} {} ({})", layer_tag, label, status_text),
                    });
                }
            } 
            else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("htlc_watt_") || out.stealth_address.starts_with("L2_WATT_") {
				if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
					if let Ok(shared_secret) = decapsulate(&capsule_bytes, &sk_bytes) {
                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                            if vault_bytes.len() > 12 {
                                let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&shared_secret));
                                if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                                    if let Ok(payload_str) = String::from_utf8(plaintext) {
                                        let parts: Vec<&str> = payload_str.split('|').collect();
                                        if parts.len() == 2 {
                                            if let Ok(amt) = parts[0].parse::<u64>() {
                                                
                                                let label = if out.stealth_address.starts_with("htlc_watt_") { 
                                                    "Transfert Swap 🔁" 
                                                } else if matches!(tx.tx_type, TransactionType::MicroCoinbase) {
                                                    "Frais Séquenceur ⚡"
                                                } else { 
                                                    "Transfert" 
                                                };

                                                history.push(HistoryItem {
                                                    id: format!("#{}", height),
                                                    tx_type: "receive".to_string(),
                                                    amount: amt as f64 / 1_000_000_000.0,
                                                    coin: "WATT".to_string(),
                                                    date: date_str,
                                                    status: format!("{} {} ({})", layer_tag, label, status_text),
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
    app: tauri::AppHandle,
    recipient_kyber_hex: String,
    amount: f64,
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    htlc_hash_hex: Option<String>,
    htlc_timeout: Option<u64>,
    spend_from_l2: bool, 
    send_to_l2: bool   
) -> Result<String, String> {
    
    let amount_in_flames = (amount * 1_000_000_000.0) as u64; 
    let fee: u64 = if spend_from_l2 && send_to_l2 { 100 } else { 1000 }; 
    let required_total = amount_in_flames + fee;

    let res_str = node_call("GET", "/all_transactions", None).await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);
    
    let mut spent_capsules = std::collections::HashSet::new();
    if let Ok(spends_path) = get_spends_path(&app) {
        if let Ok(spends) = std::fs::read_to_string(&spends_path) {
            for line in spends.lines() { spent_capsules.insert(line.trim().to_string()); }
        }
    }

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();

    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64; 

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            if spent_capsules.contains(&out.kyber_capsule) { continue; }
            
            let mut is_valid_source = false;
            if spend_from_l2 {
                if out.stealth_address.starts_with("L2_WATT_") { is_valid_source = true; }
            } else {
                if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") {
                    is_valid_source = true;
                }
            }
            if !is_valid_source { continue; }
            if spent_capsules.contains(&out.kyber_capsule) { continue; }
            let mut is_mature = true;
            if out.stealth_address.starts_with("COINBASE_") && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) {
                is_mature = false;
            }
            if !is_mature { continue; }

            let mut is_mine = false;
            let mut val = 0u64;

            if out.stealth_address == format!("COINBASE_{}", sender_kyber_public_hex) 
                || out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) {
                val = out.aes_vault.parse::<u64>().unwrap_or(0);
                is_mine = true;
            } else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("L2_WATT_") {
                if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
                    if let Ok(shared_secret) = decapsulate(&capsule_bytes, &sk_bytes) {
                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                            if vault_bytes.len() > 12 {
                                let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&shared_secret));
                                if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                                    if let Ok(payload_str) = String::from_utf8(plaintext) {
                                        let parts: Vec<&str> = payload_str.split('|').collect();
                                        if parts.len() == 2 {
                                            if let Ok(amt) = parts[0].parse::<u64>() { val = amt; is_mine = true; }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if is_mine && val > 0 {
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), height));
                collected_flames += val;
                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }

    if collected_flames < required_total {
        return Err(format!("❌ Fonds insuffisants ! Besoin de {} WATT.", required_total as f64 / 1_000_000_000.0));
    }

    let change_amount = collected_flames - required_total;
    let mut outputs = Vec::new();
    
    let tx_type = match (htlc_hash_hex, htlc_timeout) {
        (Some(hash), Some(timeout)) => TransactionType::HTLCLock { hash, timeout_block: timeout },
        _ => TransactionType::Standard,
    };

    let clean_recipient = recipient_kyber_hex.replace("wattcoin:", "").replace("L2_WATT_", "").replace("pq_watt_", "").replace("htlc_watt_", "");
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let recipient_bytes = URL_SAFE_NO_PAD.decode(&clean_recipient).map_err(|_| "Adresse WATT invalide".to_string())?;
    
    let (kyber_capsule_1, alice_shared_secret) = encapsulate(&recipient_bytes, &mut rand::thread_rng()).unwrap();

    let mut otp_1 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_1);
    let payload_1 = format!("{}|{}", amount_in_flames, hex::encode(otp_1));
    let aes_key_1 = Key::<Aes256Gcm>::from_slice(&alice_shared_secret);
    let mut nonce_bytes_1 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes_1);
    let encrypted_data_1 = Aes256Gcm::new(aes_key_1).encrypt(Nonce::from_slice(&nonce_bytes_1), payload_1.as_bytes()).map_err(|_| "Erreur AES".to_string())?;
    let mut final_vault_1 = nonce_bytes_1.to_vec(); 
    final_vault_1.extend_from_slice(&encrypted_data_1);

    let mut bf_1 = [0u32; LATTICE_DIM];
    for val in bf_1.iter_mut() { *val = rand::thread_rng().gen_range(0..LATTICE_Q); }
    let commitment_1 = LWECommitment::commit(amount_in_flames, bf_1);

    let stealth_prefix = if send_to_l2 { 
        "L2_WATT_" 
    } else if matches!(tx_type, TransactionType::HTLCLock { .. }) { 
        "htlc_watt_" 
    } else { 
        "pq_watt_" 
    };

    outputs.push(TransactionOutput {
        stealth_address: format!("{}{}", stealth_prefix, hex::encode(&otp_1[0..8])),
        kyber_capsule: hex::encode(&kyber_capsule_1),
        aes_vault: hex::encode(final_vault_1),
        lattice_commitment: commitment_1.clone(),
    });

    if change_amount > 0 {
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
        let (kyber_capsule_2, my_shared_secret) = encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();

        let mut otp_2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_2);
        let payload_2 = format!("{}|{}", change_amount, hex::encode(otp_2));
        let aes_key_2 = Key::<Aes256Gcm>::from_slice(&my_shared_secret);
        let mut nonce_bytes_2 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes_2);
        let encrypted_data_2 = Aes256Gcm::new(aes_key_2).encrypt(Nonce::from_slice(&nonce_bytes_2), payload_2.as_bytes()).unwrap();
        let mut final_vault_2 = nonce_bytes_2.to_vec(); 
        final_vault_2.extend_from_slice(&encrypted_data_2);

        let sum_inputs_t0 = selected_utxos.iter().map(|u| u.2.t_vector[0] as u64).sum::<u64>() % (LATTICE_Q as u64);
        let fee_t0 = (fee * (LATTICE_Q as u64 / 2)) % LATTICE_Q as u64;
        let expected_outputs_sum = (sum_inputs_t0 + (LATTICE_Q as u64) - fee_t0) % (LATTICE_Q as u64);
        let perfect_change_t0 = (expected_outputs_sum + (LATTICE_Q as u64) - commitment_1.t_vector[0] as u64) % (LATTICE_Q as u64);

        let mut t_vector_2 = vec![0u32; LATTICE_DIM];
        t_vector_2[0] = perfect_change_t0 as u32;
        for i in 1..LATTICE_DIM { t_vector_2[i] = rand::thread_rng().gen_range(0..LATTICE_Q); }

        let change_prefix = if spend_from_l2 { "L2_WATT_" } else { "pq_watt_" };

        outputs.push(TransactionOutput {
            stealth_address: format!("{}{}", change_prefix, hex::encode(&otp_2[0..8])),
            kyber_capsule: hex::encode(&kyber_capsule_2),
            aes_vault: hex::encode(final_vault_2),
            lattice_commitment: LWECommitment { a_matrix_seed: [0u8; 32], t_vector: t_vector_2 }
        });
    }

    let wots_keys = crate::wots::WotsKeyPair::generate();
    
    let temp_tx = Transaction {
        tx_type: tx_type.clone(),
        inputs: vec![],
        outputs: outputs.clone(),
        fee,
        wots_signature: None,
        public_key: wots_keys.public_key.clone(),
    };
    let tx_hash = temp_tx.hash_data();

    let mut decoys = vec![wots_keys.public_key.clone()]; 
    for _ in 0..3 {
        let fake_keys = crate::wots::WotsKeyPair::generate();
        decoys.push(fake_keys.public_key);
    }
    
    use rand::seq::SliceRandom;
    decoys.shuffle(&mut rand::thread_rng());
    let real_index = decoys.iter().position(|r| r == &wots_keys.public_key).unwrap();

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        let mpc_sig = crate::merkle_ring::MpcRingSignature::sign(&wots_keys.secret_key, &tx_hash, &decoys, real_index, &utxo.1);
        final_inputs.push(TransactionInput { 
            mpc_ring: mpc_sig, 
            commitment: utxo.2.clone(), 
            source_height: utxo.3 
        });
    }

    let mut tx_pq = Transaction {
        tx_type, inputs: final_inputs, outputs, fee, wots_signature: None, public_key: wots_keys.public_key.clone(),
    };
    tx_pq.wots_signature = Some(crate::wots::WotsKeyPair::sign(&wots_keys.secret_key, &tx_hash));

    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    let _ = node_call("POST", "/send_tx", Some(tx_json)).await?;
    
    use std::io::Write;
    if let Ok(spends_path) = get_spends_path(&app) {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(spends_path) {
            for utxo in &selected_utxos { let _ = writeln!(file, "{}", utxo.1); }
        }
    }

    match &tx_pq.tx_type {
        TransactionType::HTLCLock { .. } => Ok("🔒 HTLC Lock WATT accepté !".to_string()),
        _ => Ok("TX envoyée".to_string()),
    }
}

#[tauri::command]
async fn buy_lottery_ticket(
    app: tauri::AppHandle,
    sender_kyber_secret_hex: String, 
    sender_kyber_public_hex: String
) -> Result<String, String> {
    
    let ticket_price: u64 = 10_000_000_000;
    let fee: u64 = 1000;
    let required_total = ticket_price + fee;

    let res_str = node_call("GET", "/all_transactions", None).await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);

    let mut spent_capsules = std::collections::HashSet::new();
    if let Ok(spends_path) = get_spends_path(&app) {
        if let Ok(spends) = std::fs::read_to_string(&spends_path) {
            for line in spends.lines() { spent_capsules.insert(line.trim().to_string()); }
        }
    }

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();

    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64;

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) {
            Ok(t) => t, Err(_) => continue,
        };

        for out in tx.outputs {
            if spent_capsules.contains(&out.kyber_capsule) { continue; }
            let mut is_mature = true;
            if out.stealth_address.starts_with("COINBASE_") && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) {
                is_mature = false;
            }
            if !is_mature { continue; }

            let mut is_mine = false;
            let mut val = 0u64;

            if out.stealth_address == format!("COINBASE_{}", sender_kyber_public_hex) 
                || out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) {
                val = out.aes_vault.parse::<u64>().unwrap_or(0);
                is_mine = true;
            } else if out.stealth_address.starts_with("pq_watt_") {
                if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
                    if let Ok(shared_secret) = decapsulate(&capsule_bytes, &sk_bytes) {
                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                            if vault_bytes.len() > 12 {
                                let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&shared_secret));
                                if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                                    if let Ok(payload_str) = String::from_utf8(plaintext) {
                                        let parts: Vec<&str> = payload_str.split('|').collect();
                                        if parts.len() == 2 {
                                            if let Ok(amt) = parts[0].parse::<u64>() { val = amt; is_mine = true; }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if is_mine && val > 0 {
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), height));
                collected_flames += val;
                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }

    if collected_flames < required_total { return Err(format!("❌ Fonds insuffisants. Besoin : {:.9} WATT", required_total as f64 / 1_000_000_000.0)); }

    let change_amount = collected_flames - required_total;
    let mut outputs = Vec::new();

    let mut bf_ticket = [0u32; LATTICE_DIM];
    for val in bf_ticket.iter_mut() { *val = rand::thread_rng().gen_range(0..LATTICE_Q); }
    let mut ticket_capsule = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut ticket_capsule);
    
    outputs.push(TransactionOutput {
        stealth_address: "LOTTERY_RESERVE".to_string(),
        kyber_capsule: hex::encode(ticket_capsule),
        aes_vault: ticket_price.to_string(),
        lattice_commitment: LWECommitment::commit(ticket_price, bf_ticket),
    });

    if change_amount > 0 {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
        
        let (kyber_capsule_2, my_shared_secret) = encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();

        let mut otp_2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_2);
        let payload_2 = format!("{}|{}", change_amount, hex::encode(otp_2));
        let aes_key_2 = Key::<Aes256Gcm>::from_slice(&my_shared_secret);
        let mut nonce_bytes_2 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes_2);
        let encrypted_data_2 = Aes256Gcm::new(aes_key_2).encrypt(Nonce::from_slice(&nonce_bytes_2), payload_2.as_bytes()).unwrap();
        let mut final_vault_2 = nonce_bytes_2.to_vec(); 
        final_vault_2.extend_from_slice(&encrypted_data_2);

        let sum_inputs_t0 = selected_utxos.iter().map(|u| u.2.t_vector[0] as u64).sum::<u64>() % (LATTICE_Q as u64);
        let fee_t0 = (fee * (LATTICE_Q as u64 / 2)) % LATTICE_Q as u64;
        let expected_outputs_sum = (sum_inputs_t0 + (LATTICE_Q as u64) - fee_t0) % (LATTICE_Q as u64);
        let perfect_change_t0 = (expected_outputs_sum + (LATTICE_Q as u64) - outputs[0].lattice_commitment.t_vector[0] as u64) % (LATTICE_Q as u64);

        let mut t_vector_2 = vec![0u32; LATTICE_DIM];
        t_vector_2[0] = perfect_change_t0 as u32;
        for i in 1..LATTICE_DIM { t_vector_2[i] = rand::thread_rng().gen_range(0..LATTICE_Q); }

        outputs.push(TransactionOutput {
            stealth_address: format!("pq_watt_{}", hex::encode(&otp_2[0..8])), 
            kyber_capsule: hex::encode(&kyber_capsule_2),
            aes_vault: hex::encode(final_vault_2), 
            lattice_commitment: LWECommitment { a_matrix_seed: [0u8; 32], t_vector: t_vector_2 }
        });
    }

    let info_str = node_call("GET", "/info", None).await?;
    let info: serde_json::Value = serde_json::from_str(&info_str).map_err(|_| "Erreur INFO".to_string())?;
    let current_blocks = info["blocks"].as_u64().unwrap_or(0);
    let target_block = current_blocks + (10 - (current_blocks % 10));

    let wots_keys = crate::wots::WotsKeyPair::generate();
    let temp_tx = Transaction {
        tx_type: TransactionType::HTLCLottery { target_block, player_pubkey: sender_kyber_public_hex.clone() },
        inputs: vec![],
        outputs: outputs.clone(),
        fee,
        wots_signature: None,
        public_key: wots_keys.public_key.clone(),
    };
    let tx_hash = temp_tx.hash_data();

    let mut decoys = vec![wots_keys.public_key.clone()]; 
    for _ in 0..3 {
        let fake_keys = crate::wots::WotsKeyPair::generate();
        decoys.push(fake_keys.public_key);
    }
    
    use rand::seq::SliceRandom;
    decoys.shuffle(&mut rand::thread_rng());
    let real_index = decoys.iter().position(|r| r == &wots_keys.public_key).unwrap();

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        let mpc_sig = crate::merkle_ring::MpcRingSignature::sign(&wots_keys.secret_key, &tx_hash, &decoys, real_index, &utxo.1);
        final_inputs.push(TransactionInput { 
            mpc_ring: mpc_sig, 
            commitment: utxo.2.clone(), 
            source_height: utxo.3 
        });
    }

    let mut tx_pq = Transaction {
        tx_type: TransactionType::HTLCLottery { target_block, player_pubkey: sender_kyber_public_hex.clone() }, 
        inputs: final_inputs, outputs, fee, wots_signature: None, public_key: wots_keys.public_key.clone()
    };
    tx_pq.wots_signature = Some(crate::wots::WotsKeyPair::sign(&wots_keys.secret_key, &tx_hash));

    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_json)).await?;

    use std::io::Write;
    if let Ok(spends_path) = get_spends_path(&app) {
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(spends_path) {
            for utxo in &selected_utxos { let _ = writeln!(file, "{}", utxo.1); }
        }
    }
    Ok(format!("🎟️ TICKET VALIDÉ ! Le tirage aura lieu au bloc {}.", target_block))
}

#[tauri::command]
async fn refund_wattcoin_swap(hash: String, _watt_address: String, _amount: f64) -> Result<String, String> {
    let refund_tx = Transaction {
        tx_type: TransactionType::HTLCRefund { hash: hash.clone() },
        inputs: vec![],
        outputs: vec![],
        fee: 1000,
        wots_signature: None,
        public_key: hash,
    };
    let tx_json = serde_json::to_string(&refund_tx).map_err(|e| e.to_string())?;
    let _ = node_call("POST", "/send_tx", Some(tx_json)).await?;
    Ok("🔙 REMBOURSEMENT WATT DEMANDÉ !".to_string())
}

#[tauri::command]
async fn get_active_swaps(btc_address: String, watt_address: String) -> Result<Vec<SwapContract>, String> {
    let res_str = match node_call("GET", "/swaps", None).await {
        Ok(s) => s,
        Err(e) => {
            println!("❌ [DEBUG] Erreur /swaps : {}", e);
            return Ok(vec![]); 
        }
    };

    let all_swaps: Vec<SwapContract> = serde_json::from_str(&res_str).unwrap_or_default();
    let my_swaps: Vec<SwapContract> = all_swaps.into_iter()
        .filter(|s| s.buyer_btc_address == btc_address || s.seller_watt_address == watt_address)
        .collect();

    if my_swaps.is_empty() {
        if let Ok(cached) = std::fs::read_to_string("/tmp/my_swaps_cache.json") {
            if let Ok(parsed) = serde_json::from_str::<Vec<SwapContract>>(&cached) {
                return Ok(parsed);
            }
        }
    }

    Ok(my_swaps)
}

#[tauri::command]
async fn check_btc_contract_exists(htlc_hash: &str) -> Result<bool, String> {
    let res_str = node_call("GET", &format!("/btc/htlc/exists/{}", htlc_hash), None).await
        .unwrap_or_else(|_| r#"{"exists": false}"#.to_string()); 
    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    let exists = json["exists"].as_bool().unwrap_or(false); 
    Ok(exists)
}

#[tauri::command]
async fn claim_wattcoin_swap(secret: String, _hash: String, amount_flames: u64, watt_address: String) -> Result<String, String> {

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
	let my_pk_bytes = URL_SAFE_NO_PAD.decode(&watt_address).map_err(|_| "Adresse invalide".to_string())?;
    let (kyber_capsule, shared_secret) = encapsulate(&my_pk_bytes, &mut rand::thread_rng()).map_err(|_| "Clé corrompue".to_string())?;

    let mut otp = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp);
    let payload = format!("{}|{}", amount_flames, hex::encode(otp));
    let aes_key = Key::<Aes256Gcm>::from_slice(&shared_secret);
    let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let encrypted = Aes256Gcm::new(aes_key).encrypt(Nonce::from_slice(&nonce_bytes), payload.as_bytes()).unwrap();

    let mut final_vault = nonce_bytes.to_vec();
    final_vault.extend_from_slice(&encrypted);

    let claim_output = TransactionOutput {
        stealth_address: watt_address.clone(),   
        kyber_capsule: hex::encode(&kyber_capsule),
        aes_vault: hex::encode(final_vault),
        lattice_commitment: LWECommitment::commit(amount_flames, [0u32; LATTICE_DIM]),
    };

    let secret_bytes = hex::decode(&secret).unwrap_or_default();
    let claim_tx = Transaction {
        tx_type: TransactionType::HTLCClaim { secret },
        inputs: vec![],
        outputs: vec![claim_output],
        fee: 1000,
        wots_signature: None,
        public_key: hex::encode(sha2::Sha256::digest(&secret_bytes)),
    };

    let tx_json = serde_json::to_string(&claim_tx).map_err(|e| e.to_string())?;
    node_call("POST", "/htlc/claim", Some(tx_json)).await?;
    Ok("✅ Claim envoyé au node.".to_string())
}

#[tauri::command]
async fn check_watt_lock_exists(hash: String) -> Result<bool, String> {
    let res_str = node_call("GET", &format!("/htlc/lock/exists/{}", hash), None).await
        .unwrap_or_else(|_| r#"{"exists": false}"#.to_string());
    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    let exists = json["exists"].as_bool().unwrap_or(false);
    Ok(exists)
}

#[tauri::command]
async fn cancel_order(order_id: String) -> Result<String, String> {
    node_call("DELETE", &format!("/order/{}", order_id), None).await?;
    Ok("Ordre annulé avec succès".to_string())
}

#[tauri::command]
fn destroy_vault(app: tauri::AppHandle) -> Result<String, String> {
    let vault_path = get_vault_path(&app)?;
    if vault_path.exists() {
        fs::remove_file(vault_path).map_err(|_| "⚠️ Impossible de supprimer le coffre.".to_string())?;
        if let Ok(spends_path) = get_spends_path(&app) {
            if spends_path.exists() { let _ = fs::remove_file(spends_path); }
        }
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
async fn create_btc_htlc(buyer_pubkey_hex: String, seller_pubkey_hex: String, secret_hex: String, locktime: u64) -> Result<String, String> {
    let payload = serde_json::json!({
        "buyer_pubkey": buyer_pubkey_hex,
        "seller_pubkey": seller_pubkey_hex,
        "secret": secret_hex,
        "locktime": locktime
    });
    node_call("POST", "/btc/htlc/create", Some(serde_json::to_string(&payload).unwrap())).await
}

#[tauri::command]
async fn get_btc_balance(master_seed_hex: String, btc_address: Option<String>) -> Result<f64, String> {
    use bitcoin::bip32::{Xpriv, DerivationPath};
    use bitcoin::{Network, Address, PrivateKey};
    use bitcoin::secp256k1::Secp256k1;

    let address = btc_address.unwrap_or_else(|| {
        let seed = hex::decode(&master_seed_hex).unwrap_or_default();
        let secp = Secp256k1::new();
        let root = Xpriv::new_master(Network::Testnet, &seed).expect("Seed invalide");
        let path = DerivationPath::from_str("m/84'/1'/0'/0/0").expect("Path invalide");
        let child = root.derive_priv(&secp, &path).expect("Dérivation échouée");
        let privkey = PrivateKey::new(child.private_key, Network::Testnet);
        let pubkey = privkey.public_key(&secp);
        let compressed = bitcoin::CompressedPublicKey::try_from(pubkey).unwrap();
        Address::p2wpkh(&compressed, Network::Testnet).to_string()
    });

    let res_str = node_call("GET", &format!("/btc/balance?address={}", address), None).await
        .unwrap_or_else(|_| r#"{"balance": 0.0}"#.to_string());

    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    Ok(json["balance"].as_f64().unwrap_or(0.0))
}

#[tauri::command]
async fn send_btc_to_htlc(htlc_address: String, amount_btc: f64, raw_tx: Option<String>) -> Result<String, String> {
    let payload = serde_json::json!({
        "htlc_address": htlc_address,
        "amount_btc": amount_btc,
        "raw_tx": raw_tx.unwrap_or_default()
    });
    node_call("POST", "/btc/send/to_htlc", Some(serde_json::to_string(&payload).unwrap())).await
        .map(|_| "✅ BTC verrouillé dans le HTLC".to_string())
        .map_err(|e| format!("Erreur node : {}", e))
}

#[tauri::command]
async fn register_real_swap_hash(pending_placeholder: String, real_htlc_hash: String) -> Result<String, String> {
    let payload = serde_json::json!({
        "pending_placeholder": pending_placeholder,
        "real_htlc_hash": real_htlc_hash
    });
    let _ = node_call("POST", "/swaps/update_hash", Some(serde_json::to_string(&payload).unwrap())).await;
    Ok("✅ Hash réel enregistré dans le SwapContract".to_string())
}

#[tauri::command]
async fn send_btc_direct(recipient_address: String, amount_btc: f64) -> Result<String, String> {
    let payload = serde_json::json!({ "recipient": recipient_address, "amount_btc": amount_btc });
    node_call("POST", "/btc/send/direct", Some(serde_json::to_string(&payload).unwrap())).await
        .map(|_| "✅ BTC envoyé".to_string())
}

#[tauri::command]
async fn auto_claim_btc_swap(htlc_hash: String, _htlc_address: String) -> Result<String, String> {
    let res_str = node_call("GET", &format!("/htlc/secret/{}", htlc_hash), None).await?;
    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    
    if json["success"].as_bool().unwrap_or(false) {
        let secret = json["secret"].as_str().unwrap_or_default().to_string();
        let raw_witness_tx = format!(
            "02000000000101{}0000000000000000000000000000000000000000000000000000000000000000ffffffff{}00000000", 
            htlc_hash, secret
        );
        let payload = serde_json::json!({ "raw_tx": raw_witness_tx });
        match node_call("POST", "/btc/broadcast", Some(serde_json::to_string(&payload).unwrap())).await {
            Ok(_) => Ok(format!("🎉 CLAIM BTC RÉUSSI ! (Secret: {}...)", &secret[0..10])),
            Err(e) => Err(format!("Erreur broadcast BTC : {}", e))
        }
    } else {
        Err("Secret non révélé".to_string())
    }
}

#[tauri::command]
async fn get_revealed_secret(htlc_hash: String) -> Result<String, String> {
    let res_str = node_call("GET", &format!("/htlc/secret/{}", htlc_hash), None).await
        .unwrap_or_else(|_| r#"{"success":false}"#.to_string());
    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    if json["success"].as_bool().unwrap_or(false) {
        Ok(json["secret"].as_str().unwrap_or_default().to_string())
    } else {
        Err(json["message"].as_str().unwrap_or("Secret pas encore révélé par Alice").to_string())
    }
}

#[tauri::command]
fn get_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 1. On crée le builder de base SANS le "mut"
    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init());

    // 2. 💡 ASTUCE RUST : Le "Shadowing". 
    // Uniquement sur mobile, on écrase l'ancienne variable "builder" par une nouvelle
    // qui contient le plugin caméra !
    #[cfg(mobile)]
    let builder = builder.plugin(tauri_plugin_barcode_scanner::init());

    // 3. On continue normalement
    builder.setup(|app| {
        let app_handle = app.handle().clone();
            
            // 💡 ASTUCE ANDROID : Initialisation du chemin global pour Arti/Tor
            if let Ok(path) = app_handle.path().app_data_dir() {
                if !path.exists() { let _ = std::fs::create_dir_all(&path); }
                *APP_DATA_DIR.lock().unwrap() = Some(path);
            }

            tauri::async_runtime::spawn(async move {
                let mut last_blocks = 0;
                loop {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    if let Ok(res_str) = node_call("GET", "/info", None).await {
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
            create_swap_secret, submit_order, get_dark_pool, get_balances, get_btc_balance, cancel_order,
            send_wattcoin, create_btc_htlc, send_btc_to_htlc, check_btc_contract_exists, claim_wattcoin_swap, check_watt_lock_exists, refund_wattcoin_swap,
            destroy_vault, get_active_swaps, auto_claim_btc_swap, get_revealed_secret, register_real_swap_hash, send_btc_direct, get_history, 
            save_miner_script, get_total_supply, get_current_jackpot, buy_lottery_ticket,
            get_version
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}