#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};
use rand::{Rng, RngCore, SeedableRng};
use serde::{Serialize, Deserialize};
use std::str::FromStr;
use std::fs; 
use std::path::PathBuf;
use std::collections::HashMap;
use sha2::Digest;
use unicode_normalization::UnicodeNormalization;
use once_cell::sync::Lazy;
use tokio::sync::Mutex as AsyncMutex;
use std::sync::Mutex as StdMutex;
use pqc_kyber::{keypair, encapsulate, decapsulate};


// 1. IMPORT DES OUTILS L1 (Depuis le Node Core)
pub use wattcoin_core::lattice::{self, LWECommitment, LATTICE_DIM};
pub use wattcoin_core::transaction::{Transaction, TransactionType, TransactionInput, TransactionOutput, SwapContract};
pub use wattcoin_core::mixnet::{OnionPacket, HopPayload};
// On importe la logique officielle du Nœud L1 !
pub use wattcoin_core::network::{WNS_RESOLVERS, NETWORK_SEEDS, WNS_CACHE, sync_wns_directory};

// 2. IMPORT DES OUTILS L2 WNS (Directement depuis le Séquenceur WNS !)
pub use wattcoin_name_service::transaction::{L2Transaction, WnsAction};

// NOS MODULES PROPRES !
pub mod app;


pub static CURRENT_WALLET: Lazy<StdMutex<String>> = Lazy::new(|| StdMutex::new("Principal".to_string()));
static CACHED_CHAIN: Lazy<AsyncMutex<(String, u64, u64, String)>> = Lazy::new(|| AsyncMutex::new((String::new(), 0, 0, String::new())));
// LE SYSTÈME DE SUIVI EN DIRECT
pub static SYNC_STATUS: Lazy<StdMutex<String>> = Lazy::new(|| StdMutex::new(String::new()));



const MATURITY_BLOCKS: u64 = 12; 
const FLAME: u64 = 1_000_000_000;

// ===================================================================
// SWITCH LOCAL / PROD WALLET (identique au node !)
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
    pub mnemonic: String,
    pub btc_address: String,
    pub btc_pubkey_hex: String,
    pub watt_address: String, 
	#[serde(default)] // Empêche le crash sur les vieux portefeuilles
    pub watt_short_address: String, // L'adresse P2PKH légère !
    pub master_seed_hex: String,
    pub kyber_secret_hex: String,
}

#[derive(Serialize, Clone)]
pub struct HistoryItem {
    pub id: String,
    pub tx_type: String,
    pub amount: f64,
    pub coin: String,
    pub date: String,
    pub status: String,
    pub layer: String, // "L1" ou "L2"
    pub raw_timestamp: i64, // Chronologie dans l'historique
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

#[derive(Deserialize)]
struct EsploraUtxo {
    txid: String,
    vout: u32,
    value: u64,
}

#[derive(Serialize)]
pub struct DataItem {
    pub id: String,
    pub layer: String,
    pub data_type: String, // "MSG" ou "POE"
    pub content: String,
    pub date: String,
    pub timestamp: i64,
}

// Évite de recalculer le déchiffrement Kyber/AES des vieux blocs
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct WalletCache {
    pub last_scanned_height: u64,
	pub last_scanned_micro_index: u64,
    pub my_decrypted_payloads: std::collections::HashMap<String, String>, 
	pub known_spent_key_images: std::collections::HashSet<String>,
	pub known_used_lattice_pubkeys: std::collections::HashSet<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FeeSchedule {
    pub l1_min_fee_flames: u64,
    pub l1_fee_per_kb_flames: u64,
    pub l2_min_fee_flames: u64,
    pub l2_fee_per_kb_flames: u64,
    pub description: String,
}







// ===================================================================
// MEMPOOL LOCALE (RAM) AVEC TIME-TO-LIVE
// ===================================================================
pub static PENDING_SPENDS: Lazy<StdMutex<HashMap<String, i64>>> = Lazy::new(|| StdMutex::new(HashMap::new()));

pub fn mark_tx_as_pending_in_ram(tx: &Transaction) {
    let mut pending = PENDING_SPENDS.lock().unwrap();
    let now = chrono::Utc::now().timestamp();
    
    for input in &tx.inputs {
        let mut hasher = sha2::Sha512::new();
        for val in &input.commitment.t_vector {
            hasher.update(val.to_le_bytes());
        }
        pending.insert(hex::encode(hasher.finalize()), now);
    }
    
    if let Some(sig) = &tx.wots_signature {
        pending.insert(hex::encode(&sig.public_key), now);
    }
}

pub async fn get_fee_schedule() -> Result<FeeSchedule, String> {
    let res_str = node_call("GET", "/fee_schedule", None).await?;
    serde_json::from_str(&res_str).map_err(|e| format!("Erreur parsing fee_schedule: {}", e))
}

// Modifie ta fonction calculate_dynamic_fee (vers la ligne 153 de lib_8.rs)
pub fn calculate_dynamic_fee(num_inputs: usize, num_outputs: usize, is_pure_l2: bool, schedule: &FeeSchedule) -> u64 {
    let input_size = 8_200.0; 
    let output_size = 8_250.0;
    let wots_signature_size = 90_000.0; // La taille de la signature post-quantique (~90 Ko)
    let base_tx_overhead = 500.0;
    
    // Le poids total inclut une signature par transaction.
    let exact_bytes = (num_inputs as f64 * input_size) + (num_outputs as f64 * output_size) + wots_signature_size + base_tx_overhead;
    let weight_kb = (exact_bytes / 1024.0).ceil() as u64;

    if is_pure_l2 {
        std::cmp::max(schedule.l2_min_fee_flames, weight_kb * schedule.l2_fee_per_kb_flames)
    } else {
        std::cmp::max(schedule.l1_min_fee_flames, weight_kb * schedule.l1_fee_per_kb_flames)
    }
}

pub fn set_status(msg: &str) {
    if let Ok(mut status) = SYNC_STATUS.lock() {
        *status = msg.to_string();
    }
}

pub fn get_status() -> String {
    if let Ok(status) = SYNC_STATUS.lock() {
        status.clone()
    } else {
        String::new()
    }
}

pub fn get_base_dir() -> Option<PathBuf> {
    #[cfg(target_os = "android")]
    {
        // Chemin de secours universel sur Android
        Some(PathBuf::from("/data/user/0/com.ohm.wattcoin/files"))
    }
    #[cfg(not(target_os = "android"))]
    {
        // Sur PC, on utilise le dossier de données standard du système
        dirs::data_dir()
    }
}

fn get_vault_path() -> Result<PathBuf, String> {
    let mut path = crate::get_base_dir().ok_or("Impossible de trouver le dossier système")?;
    path.push("wattcoin_wallet");
    if !path.exists() { std::fs::create_dir_all(&path).map_err(|e| e.to_string())?; }
    let name = CURRENT_WALLET.lock().unwrap().clone();
    path.push(format!("{}.vault", name));
    Ok(path)
}

pub fn set_active_wallet(name: &str) {
    *CURRENT_WALLET.lock().unwrap() = name.to_string();
}

pub fn list_wallets() -> Vec<String> {
    let mut wallets = Vec::new();
    if let Some(mut path) = crate::get_base_dir() {
        path.push("wattcoin_wallet");
        
        // MIGRATION AUTOMATIQUE : On renomme l'ancien coffre unique s'il existe
        let old_vault = path.join(".wattcoin_vault");
        let old_spends = path.join(".wattcoin_spends");
        let old_lattice = path.join(".wattcoin_lattice_index");
        
        if old_vault.exists() {
            let _ = std::fs::rename(&old_vault, path.join("Principal.vault"));
            if old_spends.exists() { let _ = std::fs::rename(&old_spends, path.join("Principal.spends")); }
            if old_lattice.exists() { let _ = std::fs::rename(&old_lattice, path.join("Principal.lattice")); }
        }
        
        // Lecture de tous les coffres disponibles
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                let file_name = entry.file_name().into_string().unwrap_or_default();
                if file_name.ends_with(".vault") {
                    wallets.push(file_name.replace(".vault", ""));
                }
            }
        }
    }
    wallets
}

pub fn get_swap_secrets_path() -> Result<PathBuf, String> {
    let mut path = crate::get_base_dir().ok_or("Impossible de trouver le dossier système")?;
    path.push("wattcoin_wallet");
    if !path.exists() { std::fs::create_dir_all(&path).map_err(|e| e.to_string())?; }
    
    // On lie le fichier au nom du wallet actif !
    let name = CURRENT_WALLET.lock().unwrap().clone();
    path.push(format!("{}_swap_secrets.json", name));
    
    Ok(path)
}

// Le client global : on désactive le recyclage des connexions TCP !
static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap()
});

async fn node_call(method: &str, endpoint: &str, body: Option<Vec<u8>>) -> Result<String, String> {
    
    // 1. On synchronise l'annuaire WNS s'il est vide
    {
        let cache = WNS_CACHE.lock().await;
        if cache.is_empty() {
            drop(cache); // On relâche le verrou pour ne pas bloquer
            sync_wns_directory(LOCAL_DEV_MODE).await;
        }
    }

    crate::set_status("⏳ Routage en oignon via WNS...");

    // 2. On cherche notre Nœud Racine ("seed.watt") dans l'annuaire RAM
    let cache = WNS_CACHE.lock().await;
    
    for &seed_domain in NETWORK_SEEDS {
        if let Some((node_url, node_pubkey)) = cache.get(seed_domain) {
            
            // L'ASTUCE POUR LE LOCAL_DEV_MODE
            // Même si le WNS nous donne l'IP de prod, si on est en local, on force le routage vers localhost !
			let target_ip = if LOCAL_DEV_MODE {
				"127.0.0.1:8100".to_string()
			} else {
				// On récupère l'IP du WNS (ex: 80.78.26.243), et on utilise ton NGINX sur /api !
				let node_p2p = node_url.clone();
				let ip_part = if let Some(idx) = node_p2p.rfind(':') { &node_p2p[..idx] } else { &node_p2p };
				format!("{}/api", ip_part) // Le Wallet tapera sur http://80.78.26.243/api
			};

			let original_public_url = format!("http://{}{}", target_ip, endpoint);

			// GESTION DU TYPE (JSON vs BINAIRE) SELON LA ROUTE
			let (final_url, final_body, final_ct) = if method == "POST" && body.is_some() {
				
				// L'URL cible à l'intérieur de l'oignon DOIT rester 127.0.0.1:8100 (C'est ce que le Nœud comprend en le déballant)
				let internal_target_url = format!("http://127.0.0.1:8100{}", endpoint);
				
				// On passe du binaire à l'oignon
				let packet = wrap_in_onion(&internal_target_url, &body.clone().unwrap(), node_pubkey)?;
				let onion_bytes = bincode::serialize(&packet).map_err(|_| "Erreur bincode Onion".to_string())?;
				
				// On l'envoie à l'extérieur via NGINX
				(format!("http://{}/relay_onion", target_ip), Some(onion_bytes), "application/octet-stream")
			} else {
                let ct = if endpoint == "/send_tx" { "application/octet-stream" } else { "application/json" };
				(original_public_url, body.clone(), ct)
			};

            // Envoi HTTP
            let req = match method {
                "POST" => HTTP_CLIENT.post(&final_url).header("Content-Type", final_ct).body(final_body.unwrap_or_default()),
                "DELETE" => HTTP_CLIENT.delete(&final_url), 
                _ => HTTP_CLIENT.get(&final_url),           
            };

            // BORROW CHECKER RUST ICI :
            match req.send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        return Ok(resp.text().await.unwrap_or_default());
                    } else {
                        let status = resp.status(); 
                        let error_msg = resp.text().await.unwrap_or_default(); 
                        println!("⚠️ [RESEAU] Le nœud {} a rejeté la requête (HTTP {}) : {}", seed_domain, status, error_msg);
                        return Err(format!("❌ Rejeté par le Nœud : {}", error_msg)); 
                    }
                },
                Err(e) => { println!("⚠️ [RESEAU] Erreur de connexion brute avec {} : {}", seed_domain, e); }
            }
        }
    }

    Err("❌ Impossible de router la transaction : Vérifiez que le Séquenceur WNS tourne et contient seed.watt.".to_string())
}

pub fn wrap_in_onion(
    target_url: &str, 
    payload: &[u8], // On prend des bytes purs !
    node_pubkey_hex: &str
) -> Result<OnionPacket, String> {
    let pk_bytes = hex::decode(node_pubkey_hex).map_err(|_| "Clé publique du nœud invalide")?;
    
    let mut rng = rand::thread_rng();
    let (capsule, shared_secret) = pqc_kyber::encapsulate(&pk_bytes, &mut rng).map_err(|_| "Erreur Kyber")?;
    
    let hop = HopPayload {
        next_hop_address: target_url.to_string(),
        inner_data: payload.to_vec(),
    };
    // Le coeur de l'oignon en binaire
    let hop_bytes = bincode::serialize(&hop).map_err(|_| "Erreur bincode Hop")?;
    
    let aes_key = Key::<Aes256Gcm>::from_slice(&shared_secret);
    let cipher = Aes256Gcm::new(aes_key);
    
    let mut nonce_bytes = [0u8; 12];
    rng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    
    let ciphertext = cipher.encrypt(nonce, hop_bytes.as_slice()).map_err(|_| "Erreur de chiffrement AES")?;
        
    let mut encrypted_payload = nonce_bytes.to_vec();
    encrypted_payload.extend(ciphertext);
    
    Ok(OnionPacket {
        kyber_capsule: capsule.to_vec(), // Hexa supprimé !
        encrypted_payload, // Hexa supprimé !
    })
}

pub async fn get_network_info() -> Result<serde_json::Value, String> {
    let res_str = node_call("GET", "/info", None).await?;
    serde_json::from_str(&res_str).map_err(|e| {
        println!("❌ [JSON ERROR INFO] {} | Data: {}", e, res_str);
        e.to_string()
    })
}


pub async fn get_total_supply() -> Result<u64, String> {
    let res_str = node_call("GET", "/supply", None).await?;
    let supply: u64 = serde_json::from_str(&res_str).unwrap_or(0);
    Ok(supply)
}


pub async fn get_current_jackpot() -> Result<u64, String> {
    let res_str = node_call("GET", "/jackpot", None).await?;
    
    // On gère les deux formats pour éviter le crash JSON
    let pot: u64 = if let Ok(tuple) = serde_json::from_str::<(u64, serde_json::Value)>(&res_str) {
        tuple.0 // Si le node renvoie un tableau [10, []]
    } else if let Ok(val) = serde_json::from_str::<u64>(&res_str) {
        val // Si le node renvoie directement le chiffre
    } else {
        0
    };
    
    Ok(pot)
}


pub async fn submit_order(
    order_type: String, amount: f64, price: f64, btc_address: String, 
    btc_pubkey: String, watt_address: String, htlc_hash: Option<String> 
) -> Result<(), String> {
    let mut rand_bytes = [0u8; 4]; rand::thread_rng().fill_bytes(&mut rand_bytes);
    let amount_flames = (amount * 1_000_000_000.0) as u64; 
	let price_sats = price as u64;
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

    node_call("POST", "/order", Some(order_data.to_string().into_bytes())).await?;
    Ok(())
}


pub async fn get_dark_pool() -> Result<Vec<Order>, String> {
    let res_str = node_call("GET", "/pool", None).await?;
    let pool = serde_json::from_str::<Vec<Order>>(&res_str).map_err(|e| e.to_string())?;
    Ok(pool)
}


pub async fn generate_pro_wallet(phrase_option: Option<String>, password: String) -> Result<WalletKeys, String> {
    use bip39::{Mnemonic, Language};
    use bitcoin::Network as BtcNetwork;
    use bitcoin::bip32::{Xpriv, DerivationPath}; 
    use bitcoin::{PrivateKey as BtcPrivateKey, PublicKey as BtcPublicKey, Address as BtcAddress};
    use bitcoin::secp256k1::Secp256k1;
    use sha2::{Sha512, Digest};
	use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};

    let mnemonic = match phrase_option {
		Some(phrase) => {
			use unicode_normalization::UnicodeNormalization;

			let phrase_clean = phrase
				.replace('\u{200B}', "")  
				.replace('\u{200E}', "")  
				.replace('\u{200F}', "")  
				.replace('\u{00A0}', " ") 
				.to_lowercase()
				.nfc() // TOUJOURS STOCKÉ EN NFC POUR L'UI
				.collect::<String>();

			let words: Vec<String> = phrase_clean
				.split_whitespace()
				.map(|w| w.to_string()) 
				.collect();
			
			if words.len() != 48 { 
				return Err(format!("La phrase doit contenir exactement 48 mots (Reçu : {}).", words.len())); 
			}
			
			let phrase1 = words[0..24].join(" ");
			let phrase2 = words[24..48].join(" ");
			
			// MAGIE CRYPTO : bip39 exige le format NFKD pour parser
			let _ = Mnemonic::parse_in(Language::French, &phrase1.nfkd().collect::<String>())
				.map_err(|_| "La première moitié (1-24) est invalide ou contient un mot inconnu.")?;
			let _ = Mnemonic::parse_in(Language::French, &phrase2.nfkd().collect::<String>())
				.map_err(|_| "La deuxième moitié (25-48) est invalide ou contient un mot inconnu.")?;
			
			words.join(" ")
		},
		None => {
			let mut ent1 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut ent1);
			let mut ent2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut ent2);
			let m1 = Mnemonic::from_entropy_in(Language::French, &ent1).unwrap();
			let m2 = Mnemonic::from_entropy_in(Language::French, &ent2).unwrap();
			
			// MAGIE VISUELLE : On force le NFC dès la création pour que le coffre soit propre !
			format!("{} {}", m1, m2).nfc().collect::<String>()
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
	
	// HACHAGE P2PKH (L'Adresse Courte pour le minage !)
    let kyber_pub_hash = sha2::Sha256::digest(&kyber_keys.public);
    let watt_short_address = format!("Wq{}", bs58::encode(kyber_pub_hash).into_string());

    Ok(WalletKeys {
        mnemonic, 
        btc_address,
        btc_pubkey_hex: btc_pub.to_string(),
        master_seed_hex: hex::encode(&master_seed),
        watt_address: URL_SAFE_NO_PAD.encode(&kyber_keys.public), // ON UTILISE KYBER !
		watt_short_address, // Adresse courte
        kyber_secret_hex: hex::encode(kyber_keys.secret),
    })
}


pub fn vault_exists() -> bool { 
    get_vault_path().map(|p| p.exists()).unwrap_or(false) 
}


pub fn encrypt_vault(password: String, keys_json_string: String) -> Result<(), String> {
    let vault_path = get_vault_path()?;
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


pub async fn unlock_vault(password: String) -> Result<WalletKeys, String> {
    use pbkdf2::pbkdf2_hmac;
    let vault_path = get_vault_path()?;
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
    
    let json_string = String::from_utf8(plaintext).map_err(|_| WattError::Crypto("Erreur UTF-8".to_string()))?;
    let mut keys: WalletKeys = serde_json::from_str(&json_string).map_err(|e| WattError::Json(e))?;
    
    // MIGRATION AUTOMATIQUE : Si l'adresse courte est vide (vieux wallet), on la calcule à la volée !
    if keys.watt_short_address.is_empty() {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        if let Ok(kyber_pub) = URL_SAFE_NO_PAD.decode(&keys.watt_address) {
            let kyber_pub_hash = sha2::Sha256::digest(&kyber_pub);
            keys.watt_short_address = format!("Wq{}", bs58::encode(kyber_pub_hash).into_string());
        }
    }
    Ok(keys)
}


// Scanne uniquement les nouveautés de la blockchain en 0.01 seconde
pub fn update_spent_cache_fast(enriched: &[serde_json::Value], cache: &mut WalletCache, cache_updated: &mut bool) {
    let force_full_scan = (cache.known_spent_key_images.is_empty() || cache.known_used_lattice_pubkeys.is_empty()) && cache.last_scanned_height > 0;
    
    let mut pending = PENDING_SPENDS.lock().unwrap();
    let now = chrono::Utc::now().timestamp();
    
    // PURGE DES FANTÔMES : On libère les fonds bloqués depuis plus de 2 heures !
    pending.retain(|_, timestamp| now - *timestamp < 7200);

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0);

        let is_new = if is_l2 { micro_index > cache.last_scanned_micro_index } else { height > cache.last_scanned_height };
        
        if is_new || height == 0 || force_full_scan {
            if let Ok(tx) = serde_json::from_value::<Transaction>(item["transaction"].clone()) {
                
                for input in tx.inputs {
                    let mut hasher = sha2::Sha512::new();
                    for val in &input.commitment.t_vector {
                        hasher.update(val.to_le_bytes());
                    }
                    let commit_hash = hex::encode(hasher.finalize());
                    
                    // TRANSFERT RAM -> DISQUE : Le billet est confirmé !
                    if cache.known_spent_key_images.insert(commit_hash.clone()) {
                        pending.remove(&commit_hash); // On le retire de la RAM
                        *cache_updated = true;
                    }
                }
                
                // MÊME CHOSE POUR LA CLÉ WOTS+
                if let Some(sig) = &tx.wots_signature {
                    let pubkey_hash = hex::encode(&sig.public_key);
                    if cache.known_spent_key_images.insert(pubkey_hash.clone()) {
                        pending.remove(&pubkey_hash); // On la retire de la RAM
                        *cache_updated = true;
                    }
                }
            }
        }
    }
}

pub async fn get_balances(keys: WalletKeys) -> Result<Balances, String> {
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).unwrap_or_default();

    let current_height = get_current_block_height().await.unwrap_or(0);
    let mut l1_flames: u64 = 0;
    let mut l2_flames: u64 = 0;
    
    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
    crate::set_status("🔐 Déchiffrement quantique de vos fonds...");

    let mut cache = load_cache();
    let mut cache_updated = false;
    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
    let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect();

    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    let mut decrypt_amount = |out: &TransactionOutput, height: u64, is_l2: bool, micro_index: u64| -> Option<u64> {
        if let Some(p_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
            let parts: Vec<&str> = p_str.split('|').collect();
            if parts.len() >= 2 { return parts[0].parse::<u64>().ok(); }
        }
        None
    };

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0);

        if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
        if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }

        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut commit_hasher = sha2::Sha512::new();
            for val in &out.lattice_commitment.t_vector {
                commit_hasher.update(val.to_le_bytes());
            }
            let expected_key_image = hex::encode(commit_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image) { continue; }
            
            let mut is_mature = true;
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.kyber_capsule.starts_with("MICRO_COINBASE_");
            
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { 
                is_mature = false; 
            }
            if !is_mature { continue; }

            // LE NETTOYAGE CYPHPERPUNK :
            if out.stealth_address == format!("COINBASE_{}", keys.watt_short_address)
                || out.stealth_address == format!("JACKPOT_{}", keys.watt_short_address)
                || out.stealth_address == keys.watt_address 
            {
                if let Ok(amt) = out.aes_vault.parse::<u64>() { l1_flames += amt; }
            } else if out.stealth_address.starts_with("pq_watt_") {
                if let Some(amt) = decrypt_amount(out, height, is_l2, micro_index) { l1_flames += amt; }
            } else if out.stealth_address.starts_with("L2_WATT_") {
                if let Some(amt) = decrypt_amount(out, height, is_l2, micro_index) { l2_flames += amt; }
            }
        }
    }

    if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
    if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
    if cache_updated { save_cache(&cache); }

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


pub async fn get_history(keys: WalletKeys) -> Result<Vec<HistoryItem>, String> {
    use chrono::{DateTime, Utc, Local};

    // Remplacement de l'appel réseau
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str)
        .map_err(|_| "Erreur JSON history".to_string())?;

    let current_height = get_current_block_height().await.unwrap_or(0);
    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
	let mut cache = load_cache();
    let mut cache_updated = false;
	
	crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
	let spent_keys_snapshot = cache.known_spent_key_images.clone();
	let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect();
    
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    // On utilise directement un Vec, zéro groupement !
    let mut final_history: Vec<HistoryItem> = Vec::new();

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let timestamp = item["timestamp"].as_i64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0); // On force en u64 directement

        if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
        if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }

        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) {
            Ok(t) => t,
            Err(_) => continue,
        };
		
		for out in tx.outputs.iter() {
			// Calcul déterministe : Ce billet a-t-il été dépensé (même depuis un autre appareil) ?
			let mut commit_hasher = sha2::Sha512::new();
			for val in &out.lattice_commitment.t_vector {
				commit_hasher.update(val.to_le_bytes());
			}
			let expected_key_image = hex::encode(commit_hasher.finalize());

            let is_spent = spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image);
            let status_text = if is_spent { "Dépensé" } else { "Disponible" };

            let mut is_mature = true;
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.kyber_capsule.starts_with("MICRO_COINBASE_");
            
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { 
                is_mature = false; 
            }
            if !is_mature { continue; }

            let date_str = if timestamp > 0 {
                let dt: DateTime<Utc> = DateTime::from_timestamp(timestamp, 0).unwrap_or_default();
                dt.with_timezone(&Local).format("%d/%m/%Y %H:%M").to_string()
            } else {
                "En attente".to_string()
            };

            let mut amt_to_add = 0f64;
            let mut label = String::new();

            // Détection des montants en clair
            if out.stealth_address == format!("COINBASE_{}", keys.watt_address) 
                || out.stealth_address == format!("COINBASE_{}", keys.watt_short_address)
                || out.stealth_address == format!("JACKPOT_{}", keys.watt_address) 
				|| out.stealth_address == format!("JACKPOT_{}", keys.watt_short_address)
                || out.stealth_address == keys.watt_address 
            {
                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                    amt_to_add = amt as f64 / 1_000_000_000.0;
                    
                    // Séparation claire du Finder, des Parts, et du Jackpot
                    if out.stealth_address.starts_with("JACKPOT") {
                        label = "Jackpot gagné ! 🎰".to_string();
                    } else if out.stealth_address == keys.watt_address {
                        label = "Swap Atomique Réclamé ⚡".to_string(); 
                    } else if out.kyber_capsule.starts_with("SHARE_") {
                        label = "Part de minage (P2Pool) ⛏".to_string();
                    } else {
                        label = "Récompense bloc + Frais ⛏".to_string();
                    }
                }
            } 
            // 2. Détection des montants chiffrés
            else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("L2_WATT_") {
                
                // Appel propre à notre nouveau déchiffreur :
                if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
                    let parts: Vec<&str> = payload_str.split('|').collect();
                    if parts.len() >= 2 {
                        if let Ok(amt) = parts[0].parse::<u64>() {
                            amt_to_add = amt as f64 / 1_000_000_000.0;
                            if matches!(tx.tx_type, TransactionType::MicroCoinbase) {
                                label = "Frais Séquenceur ⚡".to_string();
                            } else if out.stealth_address.starts_with("L2_WATT_") && !is_l2 {
                                label = "Dépôt (Bridge L1 ➡ L2) 🌉".to_string(); 
                            } else { 
                                label = "Transfert".to_string(); 
                            }
                        }
                    }
                }
            }

            // Si cet UTXO nous appartient, on l'ajoute individuellement !
            if amt_to_add > 0.0 {
                let current_layer = if out.stealth_address.starts_with("L2_WATT_") { "L2".to_string() } else { "L1".to_string() };
                let display_id = if is_l2 && micro_index > 0 { format!("MicroBloc #{}", micro_index) } else { format!("Bloc #{}", height) };
                let status_full = format!("{} ({})", label, status_text);

                final_history.push(HistoryItem {
                    id: display_id,
                    tx_type: "receive".to_string(),
                    amount: amt_to_add,
                    coin: "WATT".to_string(),
                    date: date_str,
                    status: status_full,
                    layer: current_layer, 
                    raw_timestamp: timestamp,
                });
            }
        }
    }
	
	if current_max_l1 > cache.last_scanned_height {
        cache.last_scanned_height = current_max_l1;
        cache_updated = true;
    }
    if current_max_l2 > cache.last_scanned_micro_index {
        cache.last_scanned_micro_index = current_max_l2;
        cache_updated = true;
    }
    if cache_updated { save_cache(&cache); }

    // On aspire l'historique BTC et on l'ajoute à la liste !
    if let Ok(mut btc_history) = get_btc_history(&keys.btc_address).await {
        final_history.append(&mut btc_history);
    }

    // Tri chronologique infaillible (Le Vec se trie directement)
    final_history.sort_by(|a, b| b.raw_timestamp.cmp(&a.raw_timestamp));

    Ok(final_history)
}

pub async fn get_messages(keys: WalletKeys) -> Result<Vec<DataItem>, String> {
    use chrono::{DateTime, Utc, Local};

    // 1. Remplacement réseau
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;

    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
    let mut data_items = Vec::new();
    let mut cache = load_cache();
    let mut cache_updated = false;
    
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let timestamp = item["timestamp"].as_i64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0); 

        if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
        if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }

        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("L2_WATT_") {
                
                if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
                    let parts: Vec<&str> = payload_str.split('|').collect();
                    
                    if parts.len() >= 3 && parts[0] == "0" {
                        let data_str = parts[2..].join("|");
                        
                        let mut data_type = String::new();
                        let mut content = String::new();

                        if data_str.starts_with("MSG:") {
                            data_type = "MSG".to_string();
                            content = data_str[4..].to_string();
                        } else if data_str.starts_with("POE:") {
                            data_type = "POE".to_string();
                            content = data_str[4..].to_string();
                        }

                        if !data_type.is_empty() {
                            let current_layer = if out.stealth_address.starts_with("L2_WATT_") { "L2".to_string() } else { "L1".to_string() };
                            let display_id = if is_l2 && micro_index > 0 { format!("MicroBloc #{}", micro_index) } else { format!("Bloc #{}", height) };
                            let date_str = if timestamp > 0 {
                                let dt: DateTime<Utc> = DateTime::from_timestamp(timestamp, 0).unwrap_or_default();
                                dt.with_timezone(&Local).format("%d/%m/%Y %H:%M").to_string()
                            } else { "En attente".to_string() };

                            data_items.push(DataItem {
                                id: display_id,
                                layer: current_layer,
                                data_type,
                                content,
                                date: date_str,
                                timestamp,
                            });
                        }
                    }
                }
            }
        }
    }
    
    if current_max_l1 > cache.last_scanned_height {
        cache.last_scanned_height = current_max_l1;
        cache_updated = true;
    }
    if current_max_l2 > cache.last_scanned_micro_index {
        cache.last_scanned_micro_index = current_max_l2;
        cache_updated = true;
    }
    if cache_updated { save_cache(&cache); }

    // Tri par date décroissante
    data_items.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(data_items)
}

/// Génère les facteurs d'aveuglement (Blinding Factors) pour les nouveaux outputs.
/// Règle d'or : Sum(Inputs) = Sum(Outputs) mod 2^64
pub fn generate_balanced_blinding_factors(
    input_bfs: &[Vec<u64>], 
    num_outputs: usize
) -> Vec<Vec<u64>> {
    assert!(num_outputs > 0, "Une transaction doit avoir au moins un output");

    // 1. On calcule la somme des masques de tous les UTXOs que l'on dépense
    let mut sum_in = vec![0u64; LATTICE_DIM];
    for bf in input_bfs {
        for i in 0..LATTICE_DIM {
            sum_in[i] = sum_in[i].wrapping_add(bf[i]);
        }
    }

    let mut out_bfs = vec![vec![0u64; LATTICE_DIM]; num_outputs];
    let mut sum_out_temp = vec![0u64; LATTICE_DIM];
    let mut rng = rand::thread_rng();

    // 2. Pour tous les outputs SAUF LE DERNIER, on génère de l'aléatoire pur
    for out_idx in 0..(num_outputs - 1) {
        for i in 0..LATTICE_DIM {
            let r: u64 = rng.r#gen(); // De l'aléatoire sur 64 bits
            out_bfs[out_idx][i] = r;
            sum_out_temp[i] = sum_out_temp[i].wrapping_add(r);
        }
    }

    // 3. LA MAGIE : Le tout dernier output encaisse la différence stricte
    // Ainsi, sum(out_bfs) sera EXACTEMENT ÉGAL à sum_in
    for i in 0..LATTICE_DIM {
        out_bfs[num_outputs - 1][i] = sum_in[i].wrapping_sub(sum_out_temp[i]);
    }

    out_bfs
}

pub async fn send_wattcoin(
    recipient_kyber_hex: String,
    amount: f64,
    tip_watt: f64, 
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    master_seed_hex: String,       
    htlc_hash_hex: Option<String>,
    htlc_timeout: Option<u64>,
    spend_from_l2: bool, 
    send_to_l2: bool   
) -> Result<String, String> { 

    let clean_recipient = recipient_kyber_hex.replace("wattcoin:", "").replace("L2_WATT_", "").replace("pq_watt_", "").replace("htlc_watt_", "");
    if clean_recipient.starts_with("Wq") {
        return Err("❌ Erreur : L'adresse courte (Wq...) est réservée au minage et à l'identification. Vous devez utiliser la longue adresse de réception Kyber pour envoyer des fonds.".to_string());
    }

    let schedule = get_fee_schedule().await?; 
    let is_pure_l2 = spend_from_l2 && send_to_l2;

    let amount_in_flames = (amount * 1_000_000_000.0) as u64; 
    let tip_flames = (tip_watt * 1_000_000_000.0) as u64; 
    
    let max_send = 50_000 * FLAME;
    if amount_in_flames > max_send {
        return Err("❌ Transaction trop volumineuse ! Limite de sécurité : 50 000 WATT par envoi. Veuillez faire plusieurs virements.".to_string());
    }
    
    let mut num_inputs = 0;
    let mut fee = calculate_dynamic_fee(1, 2, is_pure_l2, &schedule) + tip_flames; 
    let mut required_total = amount_in_flames + fee;

    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let decoded_pub = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap_or_default();
    let my_short_address = format!("Wq{}", bs58::encode(sha2::Sha256::digest(&decoded_pub)).into_string());

    let mut cache = load_cache();
    let mut cache_updated = false;
    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
    let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect();

    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;
    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64; 
    let mut input_blinding_factors = Vec::new(); 

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0);

        if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
        if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }

        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut commit_hasher = sha2::Sha512::new();
            for val in &out.lattice_commitment.t_vector {
                commit_hasher.update(val.to_le_bytes());
            }
            let expected_key_image = hex::encode(commit_hasher.finalize());

            // FILTRAGE STRICT
            if spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image) { continue; }
            
            let is_valid_source = if spend_from_l2 {
				out.stealth_address.starts_with("L2_WATT_")
			} else {
				// On autorise sender_kyber_public_hex (Les fonds du DEX !)
				out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.stealth_address == sender_kyber_public_hex
			};
            if !is_valid_source { continue; }
            
            let mut is_mature = true;
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.kyber_capsule.starts_with("MICRO_COINBASE_");
            
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { is_mature = false; }
            if !is_mature { continue; }

            let mut is_mine = false;
            let mut val = 0u64;
            let mut my_bf = vec![0u64; LATTICE_DIM];

            if out.stealth_address == format!("COINBASE_{}", my_short_address) 
                || out.stealth_address == format!("JACKPOT_{}", my_short_address) 
                || out.stealth_address == sender_kyber_public_hex // Fallback claim
            {
                val = out.aes_vault.parse::<u64>().unwrap_or(0); is_mine = true;
            } 
            else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("L2_WATT_") {
                if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
                    let parts: Vec<&str> = payload_str.split('|').collect();
                    if parts.len() >= 2 {
                        if let Ok(amt) = parts[0].parse::<u64>() { 
                            val = amt; is_mine = true; 
                            if parts.len() == 3 {
                                if let Ok(parsed_bf) = serde_json::from_str::<Vec<u64>>(parts[2]) { my_bf = parsed_bf; }
                            }
                        }
                    }
                }
            }

            if is_mine && val > 0 {
                let actual_source_height = if is_system_reward { height } else { 0 };
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), actual_source_height));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                num_inputs += 1; 

                fee = calculate_dynamic_fee(num_inputs, 2, is_pure_l2, &schedule) + tip_flames; 
                required_total = amount_in_flames + fee;

                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
    
    if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
    if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
    if cache_updated { save_cache(&cache); }

    if collected_flames < required_total {
        return Err(format!("❌ Fonds insuffisants ! Besoin de {} WATT.", required_total as f64 / 1_000_000_000.0));
    }

    let selected_utxos_clone = selected_utxos.clone();
    let tx_pq_result = tokio::task::spawn_blocking(move || {
        let change_amount = collected_flames - required_total;
        let total_outputs_count = 1 + if change_amount > 0 { 1 } else { 0 };
        let balanced_bfs = generate_balanced_blinding_factors(&input_blinding_factors, total_outputs_count);
        let mut outputs = Vec::new();
        let mut bf_index = 0;
        
        let tx_type = match (htlc_hash_hex, htlc_timeout) {
            (Some(hash), Some(timeout)) => TransactionType::HTLCLock { hash, timeout_block: timeout },
            _ => TransactionType::Standard,
        };

        let recipient_bytes = URL_SAFE_NO_PAD.decode(&clean_recipient).map_err(|_| "Adresse WATT invalide".to_string())?;
        let stealth_prefix = if send_to_l2 { "L2_WATT_" } else if matches!(tx_type, TransactionType::HTLCLock { .. }) { "htlc_watt_" } else { "pq_watt_" };

        let current_bf = &balanced_bfs[bf_index];
        let (kyber_capsule, shared_secret) = pqc_kyber::encapsulate(&recipient_bytes, &mut rand::thread_rng()).map_err(|_| "❌ Erreur : La clé Kyber du destinataire est invalide.".to_string())?;
        let mut otp = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp);
        
        let bf_json = serde_json::to_string(current_bf).unwrap();
        let payload = format!("{}|{}|{}", amount_in_flames, hex::encode(otp), bf_json);
        
        let aes_key = Key::<Aes256Gcm>::from_slice(&shared_secret);
        let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let encrypted_data = Aes256Gcm::new(aes_key).encrypt(Nonce::from_slice(&nonce_bytes), payload.as_bytes()).map_err(|_| "Erreur AES".to_string())?;
        let mut final_vault = nonce_bytes.to_vec(); final_vault.extend_from_slice(&encrypted_data);

        let commitment = LWECommitment::commit(amount_in_flames, current_bf);

        outputs.push(TransactionOutput {
            stealth_address: format!("{}{}", stealth_prefix, hex::encode(&otp[0..8])),
            kyber_capsule: hex::encode(&kyber_capsule),
            aes_vault: hex::encode(final_vault),
            lattice_commitment: commitment,
        });
        bf_index += 1;

        if change_amount > 0 {
            let change_bf = &balanced_bfs[bf_index]; 
            let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
            let change_prefix = if spend_from_l2 { "L2_WATT_" } else { "pq_watt_" };

            let (kyber_capsule_change, my_shared_secret) = pqc_kyber::encapsulate(&my_pk_bytes, &mut rand::thread_rng()).map_err(|_| "❌ Erreur de chiffrement interne (Change).".to_string())?;
            let mut otp_c = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_c);
            let bf_json_c = serde_json::to_string(change_bf).unwrap();
            let payload_c = format!("{}|{}|{}", change_amount, hex::encode(otp_c), bf_json_c);
            let aes_key_c = Key::<Aes256Gcm>::from_slice(&my_shared_secret);
            let mut nonce_bytes_c = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes_c);
            let encrypted_data_c = Aes256Gcm::new(aes_key_c).encrypt(Nonce::from_slice(&nonce_bytes_c), payload_c.as_bytes()).unwrap();
            let mut final_vault_c = nonce_bytes_c.to_vec(); final_vault_c.extend_from_slice(&encrypted_data_c);
            let commitment_c = LWECommitment::commit(change_amount, change_bf);

            outputs.push(TransactionOutput {
                stealth_address: format!("{}{}", change_prefix, hex::encode(&otp_c[0..8])),
                kyber_capsule: hex::encode(&kyber_capsule_change),
                aes_vault: hex::encode(final_vault_c),
                lattice_commitment: commitment_c
            });
        }

        let mut seed_bytes = [0u8; 32];
        let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
        seed_bytes.copy_from_slice(&decoded_seed[0..32]);

        let mut current_index = 0u64;
        let wots_keys = loop {
            let keys = wots::Wots::generate_keypair(&seed_bytes, current_index);
            let pk_hex = hex::encode(&keys.1);
            // 👈 FILTRAGE AUSSI SUR LA CLÉ WOTS+ !
            if !spent_keys_snapshot.contains(&pk_hex) && !pending_snapshot.contains(&pk_hex) {
                break keys;
            }
            current_index += 1;
        };
        let pubkey_hex = hex::encode(&wots_keys.1);

        let mut final_inputs = Vec::new();
        for utxo in &selected_utxos_clone {
            final_inputs.push(TransactionInput { commitment: utxo.2.clone(), source_height: utxo.3 });
        }

        let mut tx_pq = Transaction { 
            tx_type, 
            inputs: final_inputs, 
            outputs, 
            fee, 
            wots_signature: None, 
            public_key: pubkey_hex.clone() 
        };
        
        let tx_hash_64 = tx_pq.hash_data();
        let mut tx_hash_32 = [0u8; 32];
        tx_hash_32.copy_from_slice(&tx_hash_64[0..32]);

        tx_pq.wots_signature = Some(wots::Wots::sign(&wots_keys.0, current_index, &tx_hash_32, &wots_keys.1));

        Ok::<Transaction, String>(tx_pq)
    }).await.map_err(|e| format!("Erreur du thread CPU : {}", e))?;

    let tx_pq = tx_pq_result?;
    let tx_bytes = bincode::serialize(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_bytes)).await?;

    crate::mark_tx_as_pending_in_ram(&tx_pq); // 👈 MÉMOIRE IMMÉDIATE

    Ok("✅ Succès".to_string())
}


pub async fn send_data(
    recipient_kyber_hex: String, 
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    master_seed_hex: String,      
    data_type: String, 
    content: String,
    use_l2: bool
) -> Result<String, String> {
    
    send_data_internal(recipient_kyber_hex, sender_kyber_secret_hex, sender_kyber_public_hex, master_seed_hex, data_type, content, use_l2).await
}

// 2. Le Moteur Public (Testable par Cargo !)
pub async fn send_data_internal(
    recipient_kyber_hex: String, 
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    master_seed_hex: String,        
    data_type: String, 
    content: String,
    use_l2: bool
) -> Result<String, String> {   

    let clean_recipient = recipient_kyber_hex.replace("wattcoin:", "").replace("L2_WATT_", "").replace("pq_watt_", "");
    if clean_recipient.starts_with("Wq") {
        return Err("❌ Erreur : L'adresse courte (Wq...) ne peut pas recevoir de données/fonds. Utilisez l'adresse Kyber.".to_string());
    }
    
    let schedule = get_fee_schedule().await?; 
    let mut num_inputs = 0;
    let mut fee = calculate_dynamic_fee(1, 2, false, &schedule);
    let mut required_total = fee;

    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);
	
	let mut cache = load_cache();
	let mut cache_updated = false;
	crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
	let spent_keys_snapshot = cache.known_spent_key_images.clone();
    let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect();

	let mut current_max_l1 = cache.last_scanned_height;
	let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
	let decoded_pub = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap_or_default();
	let my_short_address = format!("Wq{}", bs58::encode(sha2::Sha256::digest(&decoded_pub)).into_string());
    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64; 
    let mut input_blinding_factors = Vec::new(); 

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
		let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
		let micro_index = item["micro_index"].as_u64().unwrap_or(0);

		if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
		if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
			let mut commit_hasher = sha2::Sha512::new();
			for val in &out.lattice_commitment.t_vector {
				commit_hasher.update(val.to_le_bytes());
			}
			let expected_key_image = hex::encode(commit_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image) { continue; }
            
			let is_valid_source = if use_l2 { out.stealth_address.starts_with("L2_WATT_") } else { out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.stealth_address == sender_kyber_public_hex };
            if !is_valid_source { continue; }
            
            let mut is_mature = true;
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.kyber_capsule.starts_with("MICRO_COINBASE_");
            
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { 
                is_mature = false; 
            }
            if !is_mature { continue; }

            let mut is_mine = false;
            let mut val = 0u64;
            let mut my_bf = vec![0u64; LATTICE_DIM];

            if out.stealth_address == format!("COINBASE_{}", sender_kyber_public_hex) 
                || out.stealth_address == format!("COINBASE_{}", my_short_address) 
				|| out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) 
				|| out.stealth_address == format!("JACKPOT_{}", my_short_address)
                || out.stealth_address == sender_kyber_public_hex 
            {
                val = out.aes_vault.parse::<u64>().unwrap_or(0); is_mine = true;
            } else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("L2_WATT_") {
				if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
					let parts: Vec<&str> = payload_str.split('|').collect();
					if parts.len() >= 2 {
						if let Ok(amt) = parts[0].parse::<u64>() { 
							val = amt; is_mine = true; 
							if parts.len() == 3 {
								if let Ok(parsed_bf) = serde_json::from_str::<Vec<u64>>(parts[2]) { my_bf = parsed_bf; }
							}
						}
					}
				}
			}

             if is_mine && val > 0 {
                let actual_source_height = if is_system_reward { height } else { 0 };
                
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), actual_source_height));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                num_inputs += 1;
                
                fee = calculate_dynamic_fee(num_inputs, 2, false, &schedule);
                required_total = fee;

                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
	
	if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
	if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
	if cache_updated { save_cache(&cache); }

    if collected_flames < required_total { return Err("❌ Fonds insuffisants pour payer les frais réseau.".to_string()); }

    let change_amount = collected_flames - fee;
    let total_outputs_count = 1 + if change_amount > 0 { 1 } else { 0 }; 
    let balanced_bfs = generate_balanced_blinding_factors(&input_blinding_factors, total_outputs_count);
    
    let mut outputs = Vec::new();
    let mut bf_index = 0;
	
    let recipient_bytes = URL_SAFE_NO_PAD.decode(&clean_recipient).map_err(|_| "Adresse WATT invalide".to_string())?;
    let stealth_prefix = if use_l2 { "L2_WATT_" } else { "pq_watt_" };

    let data_bf = &balanced_bfs[bf_index];
    let (kyber_capsule, shared_secret) = pqc_kyber::encapsulate(&recipient_bytes, &mut rand::thread_rng())
        .map_err(|_| "❌ Erreur : La clé Kyber du destinataire est invalide ou corrompue.".to_string())?;
    let mut otp = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp);
    
    let payload = format!("0|{}|{}:{}", hex::encode(otp), data_type, content);
    
    let aes_key = Key::<Aes256Gcm>::from_slice(&shared_secret);
    let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let encrypted_data = Aes256Gcm::new(aes_key).encrypt(Nonce::from_slice(&nonce_bytes), payload.as_bytes()).map_err(|_| "Erreur AES".to_string())?;
    let mut final_vault = nonce_bytes.to_vec(); final_vault.extend_from_slice(&encrypted_data);

    outputs.push(TransactionOutput {
        stealth_address: format!("{}{}", stealth_prefix, hex::encode(&otp[0..8])),
        kyber_capsule: hex::encode(&kyber_capsule),
        aes_vault: hex::encode(final_vault),
        lattice_commitment: LWECommitment::commit(0, data_bf), 
    });
    bf_index += 1;

    if change_amount > 0 {
		let change_bf = &balanced_bfs[bf_index];
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
        let (kyber_capsule_change, my_shared_secret) = pqc_kyber::encapsulate(&my_pk_bytes, &mut rand::thread_rng())
            .map_err(|_| "❌ Erreur de chiffrement interne (Change).".to_string())?;
        let mut otp2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp2);
        
        let bf_json = serde_json::to_string(change_bf).unwrap();
        let payload2 = format!("{}|{}|{}", change_amount, hex::encode(otp2), bf_json);
        
        let aes_key2 = Key::<Aes256Gcm>::from_slice(&my_shared_secret);
        let mut nonce_bytes2 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes2);
        let encrypted_data2 = Aes256Gcm::new(aes_key2).encrypt(Nonce::from_slice(&nonce_bytes2), payload2.as_bytes()).unwrap();
        let mut final_vault2 = nonce_bytes2.to_vec(); final_vault2.extend_from_slice(&encrypted_data2);

        outputs.push(TransactionOutput {
            stealth_address: format!("{}{}", stealth_prefix, hex::encode(&otp2[0..8])),
            kyber_capsule: hex::encode(&kyber_capsule_change),
            aes_vault: hex::encode(final_vault2),
            lattice_commitment: LWECommitment::commit(change_amount, change_bf)
        });
    }

    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);

    let mut current_index = 0u64;
    let wots_keys = loop {
        let keys = wots::Wots::generate_keypair(&seed_bytes, current_index);
        let pk_hex = hex::encode(&keys.1);
        if !spent_keys_snapshot.contains(&pk_hex) && !pending_snapshot.contains(&pk_hex) {
            break keys;
        }
        current_index += 1;
    };
    let pubkey_hex = hex::encode(&wots_keys.1);

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        final_inputs.push(TransactionInput { commitment: utxo.2.clone(), source_height: utxo.3 });
    }

    let mut tx_pq = Transaction { 
        tx_type: TransactionType::Standard, 
        inputs: final_inputs, 
        outputs, 
        fee, 
        wots_signature: None, 
        public_key: pubkey_hex.clone() 
    };
    
    let tx_hash_64 = tx_pq.hash_data();
    let mut tx_hash_32 = [0u8; 32];
    tx_hash_32.copy_from_slice(&tx_hash_64[0..32]);

    tx_pq.wots_signature = Some(wots::Wots::sign(&wots_keys.0, current_index, &tx_hash_32, &wots_keys.1));

    let tx_bytes = bincode::serialize(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_bytes)).await?;

    crate::mark_tx_as_pending_in_ram(&tx_pq);

    Ok("✅ Succès".to_string())
}


pub async fn buy_lottery_ticket(
    sender_kyber_secret_hex: String, 
    sender_kyber_public_hex: String,
    master_seed_hex: String,
    ticket_price_flames: u64	
) -> Result<String, String> {   
    
    let schedule = get_fee_schedule().await?; // Tarif dynamique
    let mut num_inputs = 0;
    let mut fee = calculate_dynamic_fee(1, 2, false, &schedule);
    let mut required_total = ticket_price_flames + fee;

    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);
	
	let mut cache = load_cache();
	let mut cache_updated = false;
	crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
	let spent_keys_snapshot = cache.known_spent_key_images.clone();
    let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect(); // RAM

	let mut current_max_l1 = cache.last_scanned_height;
	let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
	let decoded_pub = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap_or_default();
	let my_short_address = format!("Wq{}", bs58::encode(sha2::Sha256::digest(&decoded_pub)).into_string());

    let mut selected_utxos = Vec::new();
    let mut input_blinding_factors = Vec::new();
    let mut collected_flames = 0u64;

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
		let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
		let micro_index = item["micro_index"].as_u64().unwrap_or(0);

		if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
		if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut commit_hasher = sha2::Sha512::new();
			for val in &out.lattice_commitment.t_vector { commit_hasher.update(val.to_le_bytes()); }
			let expected_key_image = hex::encode(commit_hasher.finalize());

            // FILTRAGE RAM & DISQUE
            if spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image) { continue; }
            
            let mut is_mature = true;
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.stealth_address == sender_kyber_public_hex;
            
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { is_mature = false; }
            if !is_mature { continue; }

            let mut is_mine = false;
            let mut val = 0u64;
            let mut my_bf = vec![0u64; LATTICE_DIM];

            if out.stealth_address == format!("COINBASE_{}", sender_kyber_public_hex) 
                || out.stealth_address == format!("COINBASE_{}", my_short_address) 
				|| out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) 
				|| out.stealth_address == format!("JACKPOT_{}", my_short_address)
                || out.stealth_address == sender_kyber_public_hex 
            {
                val = out.aes_vault.parse::<u64>().unwrap_or(0);
                is_mine = true;
            } else if out.stealth_address.starts_with("pq_watt_") {
				if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
					let parts: Vec<&str> = payload_str.split('|').collect();
					if parts.len() >= 2 {
						if let Ok(amt) = parts[0].parse::<u64>() { 
							val = amt; is_mine = true; 
							if parts.len() == 3 {
								if let Ok(parsed_bf) = serde_json::from_str::<Vec<u64>>(parts[2]) { my_bf = parsed_bf; }
							}
						}
					}
				}
			}

            if is_mine && val > 0 {
                let actual_source_height = if is_system_reward { height } else { 0 };
                
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), actual_source_height));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                num_inputs += 1;
                
                fee = calculate_dynamic_fee(num_inputs, 2, false, &schedule);
                required_total = ticket_price_flames + fee;

                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
	
	if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
	if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
	if cache_updated { save_cache(&cache); }

    if collected_flames < required_total { return Err(format!("❌ Fonds insuffisants. Besoin : {:.9} WATT", required_total as f64 / 1_000_000_000.0)); }

    let change_amount = collected_flames - required_total;
    let total_outputs_count = 1 + if change_amount > 0 { 1 } else { 0 };

    let balanced_bfs = generate_balanced_blinding_factors(&input_blinding_factors, total_outputs_count);
    let mut bf_index = 0;

    let mut outputs = Vec::new();

    let ticket_bf = &balanced_bfs[bf_index];
    let mut ticket_capsule = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut ticket_capsule);
    
    outputs.push(TransactionOutput {
        stealth_address: "LOTTERY_RESERVE".to_string(),
        kyber_capsule: hex::encode(ticket_capsule),
        aes_vault: ticket_price_flames.to_string(),
        lattice_commitment: LWECommitment::commit(ticket_price_flames, ticket_bf),
    });
    bf_index += 1;

    if change_amount > 0 {
        let change_bf = &balanced_bfs[bf_index];
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
        let (kyber_capsule_2, my_shared_secret) = encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();
        let mut otp_2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_2);
        let bf_json = serde_json::to_string(change_bf).unwrap();
        let payload_2 = format!("{}|{}|{}", change_amount, hex::encode(otp_2), bf_json);
        let aes_key_2 = Key::<Aes256Gcm>::from_slice(&my_shared_secret);
        let mut nonce_bytes_2 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes_2);
        let encrypted_data_2 = Aes256Gcm::new(aes_key_2).encrypt(Nonce::from_slice(&nonce_bytes_2), payload_2.as_bytes()).unwrap();
        let mut final_vault_2 = nonce_bytes_2.to_vec(); 
        final_vault_2.extend_from_slice(&encrypted_data_2);

        outputs.push(TransactionOutput {
            stealth_address: format!("pq_watt_{}", hex::encode(&otp_2[0..8])), 
            kyber_capsule: hex::encode(&kyber_capsule_2),
            aes_vault: hex::encode(final_vault_2), 
            lattice_commitment: LWECommitment::commit(change_amount, change_bf)
        });
    }

    let info_str = node_call("GET", "/info", None).await?;
    let info: serde_json::Value = serde_json::from_str(&info_str).map_err(|_| "Erreur INFO".to_string())?;
    let current_blocks = info["blocks"].as_u64().unwrap_or(0);
    let target_block = current_blocks + (10 - (current_blocks % 10));

    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);

    let mut current_index = 0u64;
    let wots_keys = loop {
        let keys = wots::Wots::generate_keypair(&seed_bytes, current_index);
        let pk_hex = hex::encode(&keys.1);
        // FILTRAGE DE LA CLÉ WOTS+
        if !spent_keys_snapshot.contains(&pk_hex) && !pending_snapshot.contains(&pk_hex) {
            break keys;
        }
        current_index += 1;
    };
    let pubkey_hex = hex::encode(&wots_keys.1);

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        final_inputs.push(TransactionInput { commitment: utxo.2.clone(), source_height: utxo.3 });
    }

    let mut tx_pq = Transaction { 
        tx_type: TransactionType::HTLCLottery { target_block, player_pubkey: sender_kyber_public_hex.clone() }, 
        inputs: final_inputs, 
        outputs, 
        fee, 
        wots_signature: None, 
        public_key: pubkey_hex.clone() 
    };
    
    let tx_hash_64 = tx_pq.hash_data();
    let mut tx_hash_32 = [0u8; 32];
    tx_hash_32.copy_from_slice(&tx_hash_64[0..32]);

    tx_pq.wots_signature = Some(wots::Wots::sign(&wots_keys.0, current_index, &tx_hash_32, &wots_keys.1));

    let tx_bytes = bincode::serialize(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_bytes)).await?;

    crate::mark_tx_as_pending_in_ram(&tx_pq); // MÉMOIRE IMMÉDIATE

    Ok("✅ Ticket de loterie acheté !".to_string())
}


pub async fn refund_wattcoin_swap(hash: String, _watt_address: String, _amount: f64) -> Result<String, String> {
    let refund_tx = Transaction {
        tx_type: TransactionType::HTLCRefund { hash: hash.clone() },
        inputs: vec![],
        outputs: vec![],
        fee: 0, // 0 frais 
        wots_signature: None,
        public_key: hash,
    };
    let tx_bytes = bincode::serialize(&refund_tx).map_err(|e| e.to_string())?;
    let _ = node_call("POST", "/send_tx", Some(tx_bytes)).await?;
    Ok("🔙 REMBOURSEMENT WATT DEMANDÉ !".to_string())
}


// Permet au Watchtower de nettoyer son cache une fois le travail fini
pub fn remove_swap_from_cache(hash: &str) {
    if let Ok(mut path) = get_swap_secrets_path() {
        path.set_file_name("active_swaps_cache.json");
        if let Ok(cached) = std::fs::read_to_string(&path) {
            if let Ok(mut parsed) = serde_json::from_str::<Vec<SwapContract>>(&cached) {
                parsed.retain(|s| s.htlc_hash != hash);
                let _ = std::fs::write(&path, serde_json::to_string(&parsed).unwrap_or_default());
            }
        }
    }
}

// Le Wallet garde sa propre mémoire des contrats !
pub async fn get_active_swaps(btc_address: String, watt_address: String) -> Result<Vec<SwapContract>, String> {
    let res_str = match node_call("GET", "/swaps", None).await {
        Ok(s) => s,
        Err(e) => {
            println!("❌ [DEBUG] Erreur /swaps : {}", e);
            "".to_string() // On retourne vide pour utiliser le cache
        }
    };

    let mut my_swaps = Vec::new();
    if !res_str.is_empty() {
        let all_swaps: Vec<SwapContract> = serde_json::from_str(&res_str).unwrap_or_default();
        my_swaps = all_swaps.into_iter()
            .filter(|s| s.buyer_btc_address == btc_address || s.seller_watt_address == watt_address)
            .collect();
    }

    // FIX ANTI-AMNÉSIE : On sauvegarde dans le VRAI dossier sécurisé de l'OS (pas /tmp/)
    let mut cache_path = get_swap_secrets_path().unwrap_or_else(|_| PathBuf::from("swaps.json"));
    cache_path.set_file_name("active_swaps_cache.json");

    let mut final_swaps = std::collections::HashMap::new();

    // 1. On charge la mémoire locale
    if let Ok(cached) = std::fs::read_to_string(&cache_path) {
        if let Ok(parsed) = serde_json::from_str::<Vec<SwapContract>>(&cached) {
            for s in parsed { final_swaps.insert(s.htlc_hash.clone(), s); }
        }
    }

    // 2. On fusionne avec les nouveautés du Nœud
    for s in my_swaps {
        final_swaps.insert(s.htlc_hash.clone(), s);
    }

    let merged_list: Vec<SwapContract> = final_swaps.into_values().collect();

    // 3. On sauvegarde la mémoire fusionnée
    if !merged_list.is_empty() {
        let _ = std::fs::write(&cache_path, serde_json::to_string(&merged_list).unwrap_or_default());
    }

    Ok(merged_list)
}


pub async fn check_btc_contract_exists(htlc_hash: &str) -> Result<bool, String> {
    let res_str = node_call("GET", &format!("/btc/htlc/exists/{}", htlc_hash), None).await
        .unwrap_or_else(|_| r#"{"exists": false}"#.to_string()); 
    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    let exists = json["exists"].as_bool().unwrap_or(false); 
    Ok(exists)
}


pub async fn claim_wattcoin_swap(secret: String, _hash: String, amount_flames: u64, watt_address: String) -> Result<String, String> {
    // PAS DE KYBER/AES ICI ! Le Tribunal L1 doit pouvoir lire le montant exact.
    let claim_output = TransactionOutput {
        stealth_address: watt_address.clone(),   // L'adresse publique brute
        kyber_capsule: "HTLC_CLAIM".to_string(), // Un marqueur propre
        aes_vault: amount_flames.to_string(),    // Le montant en texte clair !
        lattice_commitment: LWECommitment::commit(amount_flames, &[0u64; LATTICE_DIM]),
    };

    let secret_bytes = hex::decode(&secret).unwrap_or_default();
    let claim_tx = Transaction {
        tx_type: TransactionType::HTLCClaim { secret },
        inputs: vec![],
        outputs: vec![claim_output],
        fee: 0, // 0 frais 
        wots_signature: None,
        public_key: hex::encode(sha2::Sha256::digest(&secret_bytes)),
    };

    let tx_bytes = bincode::serialize(&claim_tx).map_err(|e| e.to_string())?;
    node_call("POST", "/htlc/claim", Some(tx_bytes)).await?;
    Ok("✅ Claim envoyé au node.".to_string())
}


pub async fn check_watt_lock_exists(hash: String) -> Result<bool, String> {
    let res_str = node_call("GET", &format!("/htlc/lock/exists/{}", hash), None).await
        .unwrap_or_else(|_| r#"{"exists": false}"#.to_string());
    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    let exists = json["exists"].as_bool().unwrap_or(false);
    Ok(exists)
}


pub async fn cancel_order(order_id: String) -> Result<String, String> {
    node_call("DELETE", &format!("/order/{}", order_id), None).await?;
    Ok("Ordre annulé avec succès".to_string())
}


pub fn delete_wallet(name: &str) -> Result<String, String> {
    if let Some(mut path) = crate::get_base_dir() {
        path.push("wattcoin_wallet");
        
        let vault_path = path.join(format!("{}.vault", name));
        let spends_path = path.join(format!("{}.spends", name));
        let lattice_path = path.join(format!("{}.lattice", name));
        let cache_path = path.join(format!("{}.cache", name));
        let chain_path = path.join(format!("{}_chain.json", name));
        let swap_path = path.join(format!("{}_swap_secrets.json", name));
        let db_path = path.join(format!("{}.db", name)); // 👈 NOUVEAU : SLED DB
        
        if vault_path.exists() { let _ = std::fs::remove_file(vault_path); }
        if spends_path.exists() { let _ = std::fs::remove_file(spends_path); }
        if lattice_path.exists() { let _ = std::fs::remove_file(lattice_path); }
        if cache_path.exists() { let _ = std::fs::remove_file(cache_path); }
        if chain_path.exists() { let _ = std::fs::remove_file(chain_path); }
        if swap_path.exists() { let _ = std::fs::remove_file(swap_path); }
        // 👈 Sled génère un dossier, on supprime donc tout le dossier et son contenu :
        if db_path.exists() { let _ = std::fs::remove_dir_all(db_path); } 
        
        Ok(format!("Le portefeuille '{}' a été supprimé.", name))
    } else {
        Err("Impossible d'accéder au dossier système.".to_string())
    }
}


pub fn save_miner_script(os: String, address: String) -> Result<String, String> {
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
        format!("#!/bin/bash\n\n# Lancement du Nœud Wattcoin\necho \"🔥 Démarrage du Nœud pour {}...\"\n./wattcoin_core 8001 {} 80.78.26.243:8000 --live\n", short_addr, address)
    } else {
        format!("@echo off\n:: Lancement du Nœud Wattcoin\necho 🔥 Demarrage du Noeud pour {}...\nwattcoin_core.exe 8001 {} 80.78.26.243:8000 --live\npause\n", short_addr, address)
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


pub async fn get_btc_balance(master_seed_hex: String, btc_address: Option<String>) -> Result<f64, String> {
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

    let res_str = node_call("GET", &format!("/btc/balance?address={}", address), None).await?;

    let json: serde_json::Value = serde_json::from_str(&res_str).map_err(|e| e.to_string())?;
    Ok(json["balance"].as_f64().unwrap_or(0.0))
}

pub async fn get_btc_history(btc_address: &str) -> Result<Vec<HistoryItem>, String> {
    let res_str = node_call("GET", &format!("/btc/txs?address={}", btc_address), None).await?;
    let txs: Vec<serde_json::Value> = serde_json::from_str(&res_str).unwrap_or_default();
    
    let mut history = Vec::new();
    
    for tx in txs {
        let txid = tx["txid"].as_str().unwrap_or_default().to_string();
        let status = tx["status"].clone();
        let confirmed = status["confirmed"].as_bool().unwrap_or(false);
        // Si la TX n'est pas encore minée, on prend l'heure actuelle pour la mettre tout en haut
        let timestamp = status["block_time"].as_i64().unwrap_or_else(|| chrono::Utc::now().timestamp());
        
        let date_str = if confirmed {
            use chrono::{DateTime, Utc, Local};
            let dt: DateTime<Utc> = DateTime::from_timestamp(timestamp, 0).unwrap_or_default();
            dt.with_timezone(&Local).format("%d/%m/%Y %H:%M").to_string()
        } else {
            "En attente (Mempool)".to_string()
        };

        // Calcul du solde entrant (Ce qu'on dépense)
        let mut total_in = 0i64;
        if let Some(vin) = tx["vin"].as_array() {
            for input in vin {
                if let Some(prevout) = input.get("prevout") {
                    if prevout["scriptpubkey_address"].as_str() == Some(btc_address) {
                        total_in += prevout["value"].as_i64().unwrap_or(0);
                    }
                }
            }
        }

        // Calcul du solde sortant (Ce qu'on reçoit)
        let mut total_out = 0i64;
        if let Some(vout) = tx["vout"].as_array() {
            for output in vout {
                if output["scriptpubkey_address"].as_str() == Some(btc_address) {
                    total_out += output["value"].as_i64().unwrap_or(0);
                }
            }
        }

        let diff = total_out - total_in;
        if diff == 0 { continue; } // Cette transaction ne nous concerne pas financièrement
        
        let (tx_type, amount_sats) = if diff > 0 {
            ("receive".to_string(), diff as u64)
        } else {
            ("send".to_string(), (-diff) as u64)
        };
        
        let amount_btc = amount_sats as f64 / 100_000_000.0;
        let display_status = if confirmed { "Confirmé" } else { "En attente" };
        let prefix = if diff > 0 { "Reçu" } else { "Envoyé" };

        history.push(HistoryItem {
            id: format!("{}...", &txid[0..15]),
            tx_type,
            amount: amount_btc,
            coin: "BTC".to_string(),
            date: date_str,
            status: format!("{} ({})", prefix, display_status),
            layer: "BTC".to_string(), // 👈 Le nouvel onglet !
            raw_timestamp: timestamp,
        });
    }
    
    Ok(history)
}


pub async fn send_btc_to_htlc(
    swap: SwapContract,
    master_seed_hex: String
) -> Result<String, String> {
    use bitcoin::blockdata::script::Builder;
    use bitcoin::opcodes::all::*;
    use bitcoin::{Network, Address, PublicKey};
    use std::str::FromStr;

    let buyer_pk = PublicKey::from_str(&swap.buyer_btc_pubkey).map_err(|_| "Buyer PK invalide")?;
    let seller_pk = PublicKey::from_str(&swap.seller_btc_pubkey).map_err(|_| "Seller PK invalide")?;
    let hash_bytes = hex::decode(&swap.htlc_hash).map_err(|_| "Hash invalide")?;
    let locktime = 144i64; 

    // Conversion PushBytes pour bitcoin 0.32[cite: 12]
    let push_hash = <&bitcoin::script::PushBytes>::try_from(hash_bytes.as_slice())
        .map_err(|_| "Erreur de conversion PushBytes")?;

    let witness_script = Builder::new()
        .push_opcode(OP_IF)
        .push_opcode(OP_SHA256)
        .push_slice(push_hash)
        .push_opcode(OP_EQUALVERIFY)
        .push_key(&seller_pk)
        .push_opcode(OP_CHECKSIG)
        .push_opcode(OP_ELSE)
        .push_int(locktime)
        .push_opcode(OP_CLTV)
        .push_opcode(OP_DROP)
        .push_key(&buyer_pk)
        .push_opcode(OP_CHECKSIG)
        .push_opcode(OP_ENDIF)
        .into_script();

    let htlc_addr = Address::p2wsh(&witness_script, Network::Testnet).to_string();
    // L'acheteur ajoute 500 sats au contrat pour couvrir les frais de Claim du vendeur !
	let amount_btc = (swap.btc_amount_sats as f64 + 500.0) / 100_000_000.0;

    let tx_result = send_btc_direct(htlc_addr, amount_btc, master_seed_hex).await?;

    let payload = serde_json::json!({
        "htlc_address": swap.htlc_hash,
        "amount_btc": amount_btc,
    });
    let _ = node_call("POST", "/btc/send/to_htlc", Some(serde_json::to_string(&payload).unwrap().into_bytes())).await;

    Ok(tx_result)
}

pub async fn auto_claim_btc_swap(
    swap: SwapContract,
    secret: String,
    master_seed_hex: String
) -> Result<String, String> {
    use bitcoin::{Network, Address, Amount, OutPoint, Sequence, TxIn, TxOut, Witness, Txid};
    use bitcoin::transaction::{Transaction as BtcTransaction, Version};
    use bitcoin::absolute::LockTime;
    use bitcoin::sighash::{SighashCache, EcdsaSighashType};
    use bitcoin::blockdata::script::Builder;
    use bitcoin::opcodes::all::*;
    use std::str::FromStr;
    use bitcoin::bip32::{Xpriv, DerivationPath};
    use bitcoin::secp256k1::Secp256k1;

    let seed = hex::decode(&master_seed_hex).map_err(|_| "Seed invalide")?;
    let secp = Secp256k1::new();
    let root = Xpriv::new_master(Network::Testnet, &seed).unwrap();
    let path = DerivationPath::from_str("m/84'/1'/0'/0/0").unwrap();
    let child = root.derive_priv(&secp, &path).unwrap();
    let privkey = bitcoin::PrivateKey::new(child.private_key, Network::Testnet);
    let pubkey = privkey.public_key(&secp);
    let compressed_pubkey = bitcoin::CompressedPublicKey::try_from(pubkey).unwrap();
    let my_address = Address::p2wpkh(&compressed_pubkey, Network::Testnet).to_string();
    let my_addr_obj = Address::from_str(&my_address).unwrap().require_network(Network::Testnet).unwrap();

    let buyer_pk = bitcoin::PublicKey::from_str(&swap.buyer_btc_pubkey).map_err(|_| "Buyer PK invalide")?;
    let seller_pk = bitcoin::PublicKey::from_str(&swap.seller_btc_pubkey).map_err(|_| "Seller PK invalide")?;
    let hash_bytes = hex::decode(&swap.htlc_hash).map_err(|_| "Hash invalide")?;
    let locktime = 144i64;

    // Conversion PushBytes pour bitcoin 0.32[cite: 12]
    let push_hash = <&bitcoin::script::PushBytes>::try_from(hash_bytes.as_slice())
        .map_err(|_| "Erreur de conversion PushBytes")?;

    let witness_script = Builder::new()
        .push_opcode(OP_IF)
        .push_opcode(OP_SHA256)
        .push_slice(push_hash)
        .push_opcode(OP_EQUALVERIFY)
        .push_key(&seller_pk)
        .push_opcode(OP_CHECKSIG)
        .push_opcode(OP_ELSE)
        .push_int(locktime)
        .push_opcode(OP_CLTV)
        .push_opcode(OP_DROP)
        .push_key(&buyer_pk)
        .push_opcode(OP_CHECKSIG)
        .push_opcode(OP_ENDIF)
        .into_script();

    let htlc_addr = Address::p2wsh(&witness_script, Network::Testnet).to_string();
    let utxos_str = node_call("GET", &format!("/btc/utxos?address={}", htlc_addr), None).await?;
    let utxos: Vec<EsploraUtxo> = serde_json::from_str(&utxos_str).map_err(|_| "Erreur parsing UTXOs HTLC")?;

    if utxos.is_empty() {
        return Err("⏳ En attente de la confirmation des BTC sur le réseau...".to_string());
    }

    let utxo = &utxos[0]; 
    let txid = Txid::from_str(&utxo.txid).unwrap();
    let value = utxo.value;
    let fee_sats = 500u64;
    
    if value <= fee_sats {
        return Err("❌ Montant HTLC trop faible pour payer les frais.".to_string());
    }

    let txin = TxIn {
        previous_output: OutPoint { txid, vout: utxo.vout },
        script_sig: bitcoin::ScriptBuf::new(),
        sequence: Sequence::MAX, 
        witness: Witness::new(),
    };

    let txout = TxOut {
        value: Amount::from_sat(value - fee_sats),
        script_pubkey: my_addr_obj.script_pubkey(),
    };

    let mut tx = BtcTransaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: vec![txin],
        output: vec![txout],
    };

    let mut sighash_cache = SighashCache::new(&mut tx);
    let sighash = sighash_cache.p2wsh_signature_hash(
        0,
        &witness_script,
        Amount::from_sat(value),
        EcdsaSighashType::All,
    ).unwrap();

    let msg = bitcoin::secp256k1::Message::from_digest_slice(sighash.as_ref()).unwrap();
    let sig = secp.sign_ecdsa(&msg, &privkey.inner);

    let mut sig_with_hashtype = sig.serialize_der().to_vec();
    sig_with_hashtype.push(EcdsaSighashType::All as u8);

    let secret_bytes = hex::decode(&secret).map_err(|_| "Secret invalide")?;

    let mut witness = Witness::new();
    witness.push(sig_with_hashtype);          
    witness.push(secret_bytes);               
    witness.push(vec![1]);                    
    witness.push(witness_script.into_bytes());

    *sighash_cache.witness_mut(0).unwrap() = witness;

    // 7. Broadcast via le proxy Tor
    let raw_tx_hex = bitcoin::consensus::encode::serialize_hex(&tx);
    let payload = serde_json::json!({ "raw_tx": raw_tx_hex });

    match node_call("POST", "/btc/broadcast", Some(serde_json::to_string(&payload).unwrap().into_bytes())).await {
        Ok(resp) => {
            // 💡 CORRECTION DU MIXNET : On "épluche" le double encodage JSON !
            let mut json: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
            
            if let Some(inner_str) = json.as_str() {
                if let Ok(parsed) = serde_json::from_str(inner_str) {
                    json = parsed;
                }
            }

            if json["success"].as_bool().unwrap_or(false) {
                Ok(format!("🎉 CLAIM BTC RÉUSSI ! TXID : {}...", &json["txid"].as_str().unwrap_or("")[0..10]))
            } else {
                Err(format!("❌ Erreur Nœud : {}", json["error"].as_str().unwrap_or("Rejeté")))
            }
        },
        Err(e) => Err(e),
    }
}

pub async fn send_btc_direct(
    recipient_address: String, 
    amount_btc: f64,
    master_seed_hex: String // On a besoin de la seed pour signer !
) -> Result<String, String> {
    
    use bitcoin::{Network, Address, Amount, OutPoint, Sequence, TxIn, TxOut, Witness, Txid};
    use bitcoin::transaction::{Transaction as BtcTransaction, Version};
    use bitcoin::absolute::LockTime;
    use bitcoin::sighash::{SighashCache, EcdsaSighashType};
    use std::str::FromStr;

    let amount_sats = (amount_btc * 100_000_000.0) as u64;
    let fee_sats = 500u64; // Frais fixe sécurisé (500 sats)

    // 1. DÉRIVATION DES CLÉS
    let seed = hex::decode(&master_seed_hex).map_err(|_| "Seed invalide")?;
    let secp = bitcoin::secp256k1::Secp256k1::new();
    let root = bitcoin::bip32::Xpriv::new_master(Network::Testnet, &seed).unwrap();
    let path = bitcoin::bip32::DerivationPath::from_str("m/84'/1'/0'/0/0").unwrap();
    let child = root.derive_priv(&secp, &path).unwrap();
    let privkey = bitcoin::PrivateKey::new(child.private_key, Network::Testnet);
    let pubkey = privkey.public_key(&secp);
    let compressed_pubkey = bitcoin::CompressedPublicKey::try_from(pubkey).unwrap();
    let my_address = Address::p2wpkh(&compressed_pubkey, Network::Testnet).to_string();
    
    let my_addr_obj = Address::from_str(&my_address).unwrap().require_network(Network::Testnet).unwrap();
    let my_script_pubkey = my_addr_obj.script_pubkey();

    // 2. RÉCUPÉRATION DES UTXOs (Via notre Nœud Proxy Tor)
    let utxos_str = node_call("GET", &format!("/btc/utxos?address={}", my_address), None).await?;
    let utxos: Vec<EsploraUtxo> = serde_json::from_str(&utxos_str).map_err(|_| "Erreur parsing UTXOs")?;

    let mut selected_utxos = Vec::new();
    let mut total_in = 0u64;
    for utxo in utxos {
        total_in += utxo.value; // On lit la valeur AVANT de déplacer l'objet
        selected_utxos.push(utxo); // Le "move" se fait ici en toute sécurité
        if total_in >= amount_sats + fee_sats { break; }
    }

    if total_in < amount_sats + fee_sats {
        return Err("❌ Fonds BTC insuffisants.".to_string());
    }

    // 3. CONSTRUCTION DE LA TRANSACTION
    let mut txin = Vec::new();
    let mut prevouts = Vec::new();

    for utxo in &selected_utxos {
        let txid = Txid::from_str(&utxo.txid).unwrap();
        txin.push(TxIn {
            previous_output: OutPoint { txid, vout: utxo.vout },
            script_sig: bitcoin::ScriptBuf::new(), // Vide car SegWit
            sequence: Sequence::MAX,
            witness: Witness::new(),
        });
        prevouts.push(TxOut {
            value: Amount::from_sat(utxo.value),
            script_pubkey: my_script_pubkey.clone(),
        });
    }

    let dest_addr = Address::from_str(&recipient_address)
        .map_err(|_| "❌ Adresse BTC invalide")?
        .require_network(Network::Testnet)
        .map_err(|_| "❌ Adresse non compatible Testnet")?;

    let mut txout = vec![
        TxOut {
            value: Amount::from_sat(amount_sats),
            script_pubkey: dest_addr.script_pubkey(),
        }
    ];

    let change = total_in - amount_sats - fee_sats;
    if change > 546 { // Limite anti-poussière (dust limit)
        txout.push(TxOut {
            value: Amount::from_sat(change),
            script_pubkey: my_script_pubkey.clone(),
        });
    }

    let mut tx = BtcTransaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: txin,
        output: txout,
    };

    // 4. SIGNATURE SEGWIT (P2WPKH)
    let mut sighash_cache = SighashCache::new(&mut tx);
    for (i, _) in selected_utxos.iter().enumerate() {
        let prevout = &prevouts[i];
        let sighash = sighash_cache.p2wpkh_signature_hash(
            i,
            &prevout.script_pubkey,
            prevout.value,
            EcdsaSighashType::All,
        ).unwrap();

        let msg = bitcoin::secp256k1::Message::from_digest_slice(sighash.as_ref()).unwrap();
        let sig = secp.sign_ecdsa(&msg, &privkey.inner);
        
        let mut sig_with_hashtype = sig.serialize_der().to_vec();
        sig_with_hashtype.push(EcdsaSighashType::All as u8);

        let mut witness = Witness::new();
        witness.push(sig_with_hashtype);
        witness.push(pubkey.to_bytes());
        
        *sighash_cache.witness_mut(i).unwrap() = witness;
    }

    // 5. ENVOI AU NŒUD POUR DIFFUSION TOR
    let raw_tx_hex = bitcoin::consensus::encode::serialize_hex(&tx);
    
    let payload = serde_json::json!({ "raw_tx": raw_tx_hex });
    match node_call("POST", "/btc/broadcast", Some(serde_json::to_string(&payload).unwrap().into_bytes())).await {
        Ok(resp) => {
            // 💡 CORRECTION DU MIXNET : On "épluche" le double encodage JSON !
            let mut json: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
            
            // Si la réponse est une chaîne de caractères (String), on la re-parse en objet JSON
            if let Some(inner_str) = json.as_str() {
                if let Ok(parsed) = serde_json::from_str(inner_str) {
                    json = parsed;
                }
            }

            if json["success"].as_bool().unwrap_or(false) {
                Ok(format!("✅ BTC envoyés ! TXID : {}...", &json["txid"].as_str().unwrap_or("")[0..10]))
            } else {
                Err(format!("❌ Erreur Nœud : {}", json["error"].as_str().unwrap_or("Rejeté")))
            }
        },
        Err(e) => Err(e),
    }
}


pub async fn get_revealed_secret(htlc_hash: String) -> Result<String, String> {
    let res_str = node_call("GET", &format!("/htlc/secret/{}", htlc_hash), None).await
        .unwrap_or_else(|_| r#"{"success":false}"#.to_string());
    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    if json["success"].as_bool().unwrap_or(false) {
        Ok(json["secret"].as_str().unwrap_or_default().to_string())
    } else {
        Err(json["message"].as_str().unwrap_or("Secret pas encore révélé par Alice").to_string())
    }
}


pub fn get_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}


pub async fn stake_l2(
    l2_name: String,
    stake_amount: f64,
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    sequencer_pubkey_hex: String,
    master_seed_hex: String,    
) -> Result<String, String> {
    
    let schedule = get_fee_schedule().await?; 
    let amount_flames = (stake_amount * 1_000_000_000.0) as u64;
    let mut num_inputs = 0;
    let mut fee = calculate_dynamic_fee(1, 2, false, &schedule);
    let mut required_total = amount_flames + fee;
	
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);

    let mut cache = load_cache();
    let mut cache_updated = false;
    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
    let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect();
    
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
	let decoded_pub = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap_or_default();
	let my_short_address = format!("Wq{}", bs58::encode(sha2::Sha256::digest(&decoded_pub)).into_string());
    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64; 
    let mut input_blinding_factors = Vec::new(); 

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
		let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
		let micro_index = item["micro_index"].as_u64().unwrap_or(0);

		if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
		if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut commit_hasher = sha2::Sha512::new();
			for val in &out.lattice_commitment.t_vector { commit_hasher.update(val.to_le_bytes()); }
			let expected_key_image = hex::encode(commit_hasher.finalize());

			if spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image) { continue; }
            
            let is_valid_source = out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_");
            if !is_valid_source { continue; }
            
            let mut is_mature = true;
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.kyber_capsule.starts_with("MICRO_COINBASE_");
            
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { 
                is_mature = false; 
            }
            if !is_mature { continue; }

            let mut is_mine = false;
            let mut val = 0u64;
            let mut my_bf = vec![0u64; LATTICE_DIM];

            if out.stealth_address == format!("COINBASE_{}", sender_kyber_public_hex) 
                || out.stealth_address == format!("COINBASE_{}", my_short_address) 
				|| out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) 
				|| out.stealth_address == format!("JACKPOT_{}", my_short_address)
                || out.stealth_address == sender_kyber_public_hex 
            {
                val = out.aes_vault.parse::<u64>().unwrap_or(0); is_mine = true;
            } else if out.stealth_address.starts_with("pq_watt_") {
				if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
					let parts: Vec<&str> = payload_str.split('|').collect();
					if parts.len() >= 2 {
						if let Ok(amt) = parts[0].parse::<u64>() { 
							val = amt; is_mine = true; 
							if parts.len() == 3 {
								if let Ok(parsed_bf) = serde_json::from_str::<Vec<u64>>(parts[2]) { my_bf = parsed_bf; }
							}
						}
					}
				}
			}

            if is_mine && val > 0 {
                let actual_source_height = if is_system_reward { height } else { 0 };
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), actual_source_height));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                num_inputs += 1;
                
                fee = calculate_dynamic_fee(num_inputs, 2, false, &schedule);
                required_total = amount_flames + fee;

                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
	
	if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
	if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
	if cache_updated { save_cache(&cache); }

    if collected_flames < required_total { return Err("❌ Fonds insuffisants pour Staker sur le L1.".to_string()); }

    let change_amount = collected_flames - required_total;
    
    let mut sum_in_bf = vec![0u64; crate::lattice::LATTICE_DIM];
    for bf in &input_blinding_factors {
        for i in 0..crate::lattice::LATTICE_DIM {
            sum_in_bf[i] = sum_in_bf[i].wrapping_add(bf[i]);
        }
    }

    let mut outputs = Vec::new();

    let stake_bf = vec![0u64; crate::lattice::LATTICE_DIM]; 
    
    outputs.push(TransactionOutput {
        stealth_address: format!("L2_STAKE_{}", sender_kyber_public_hex),
        kyber_capsule: "L2_STAKE_LOCK".to_string(),
        aes_vault: amount_flames.to_string(), 
        lattice_commitment: LWECommitment::commit(amount_flames, &stake_bf), 
    });

    if change_amount > 0 {
        let change_bf = &sum_in_bf; 
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
        let (kyber_capsule_change, my_shared_secret) = pqc_kyber::encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();
        let mut otp2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp2);
        
        let bf_json2 = serde_json::to_string(change_bf).unwrap();
        let payload2 = format!("{}|{}|{}", change_amount, hex::encode(otp2), bf_json2);
        
        let aes_key2 = aes_gcm::Key::<Aes256Gcm>::from_slice(&my_shared_secret);
        let mut nonce_bytes2 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes2);
        use aes_gcm::aead::Aead;
        let encrypted_data2 = aes_gcm::Aes256Gcm::new(aes_key2).encrypt(aes_gcm::Nonce::from_slice(&nonce_bytes2), payload2.as_bytes()).unwrap();
        let mut final_vault2 = nonce_bytes2.to_vec(); final_vault2.extend_from_slice(&encrypted_data2);

        outputs.push(TransactionOutput {
            stealth_address: format!("pq_watt_{}", hex::encode(&otp2[0..8])),
            kyber_capsule: hex::encode(&kyber_capsule_change),
            aes_vault: hex::encode(final_vault2),
            lattice_commitment: LWECommitment::commit(change_amount, change_bf)
        });
    }

    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);

    let mut current_index = 0u64;
    let wots_keys = loop {
        let keys = wots::Wots::generate_keypair(&seed_bytes, current_index);
        let pk_hex = hex::encode(&keys.1);
        if !spent_keys_snapshot.contains(&pk_hex) && !pending_snapshot.contains(&pk_hex) {
            break keys;
        }
        current_index += 1;
    };
    let pubkey_hex = hex::encode(&wots_keys.1);

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        final_inputs.push(TransactionInput { commitment: utxo.2.clone(), source_height: utxo.3 });
    }

    let mut tx_pq = Transaction { 
        tx_type: TransactionType::L2Stake { l2_name: l2_name.clone(), sequencer_pubkey: sequencer_pubkey_hex.clone() }, 
        inputs: final_inputs, 
        outputs, 
        fee, 
        wots_signature: None, 
        public_key: pubkey_hex.clone() 
    };
    
    let tx_hash_64 = tx_pq.hash_data();
    let mut tx_hash_32 = [0u8; 32];
    tx_hash_32.copy_from_slice(&tx_hash_64[0..32]);

    tx_pq.wots_signature = Some(wots::Wots::sign(&wots_keys.0, current_index, &tx_hash_32, &wots_keys.1));

    let tx_bytes = bincode::serialize(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_bytes)).await?;

    crate::mark_tx_as_pending_in_ram(&tx_pq);

    Ok(format!("🎉 Caution verrouillée avec succès ! La L2 '{}' est prête à être ancrée.", l2_name))
}


pub async fn unstake_l2(
    l2_name: String,
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    master_seed_hex: String,       
) -> Result<String, String> {
    
    let schedule = get_fee_schedule().await?; 
    let fee = calculate_dynamic_fee(1, 1, false, &schedule);
	
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;

    let mut cache = load_cache();
    let mut cache_updated = false;
    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
    let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect();
    
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    
    let mut selected_utxo = None;
    let mut stake_amount = 0u64;
    let mut old_bf = vec![0u64; crate::lattice::LATTICE_DIM];

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
		let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
		let micro_index = item["micro_index"].as_u64().unwrap_or(0);

		if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
		if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut commit_hasher = sha2::Sha512::new();
            for val in &out.lattice_commitment.t_vector { commit_hasher.update(val.to_le_bytes()); }
            let expected_key_image = hex::encode(commit_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image) { continue; }
            
            if out.stealth_address == format!("L2_STAKE_{}", sender_kyber_public_hex) {
                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                    stake_amount = amt;
                    old_bf = vec![0u64; crate::lattice::LATTICE_DIM]; 
                    selected_utxo = Some((out.lattice_commitment.clone(), height));
                    break;
                }
            }

			if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
				let parts: Vec<&str> = payload_str.split('|').collect();
				if parts.len() >= 3 {
					if let Ok(amt) = parts[0].parse::<u64>() {
						stake_amount = amt;
						if let Ok(parsed_bf) = serde_json::from_str::<Vec<u64>>(parts[2]) {
							old_bf = parsed_bf;
						}
						selected_utxo = Some((out.lattice_commitment.clone(), height));
						break;
					}
				}
			}
        }
        if selected_utxo.is_some() { break; }
    }
	
	if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
	if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
	if cache_updated { save_cache(&cache); }

    let (commitment, source_height) = selected_utxo.ok_or(format!("❌ Aucune caution trouvée pour la L2 '{}'.", l2_name))?;
    
    if stake_amount <= fee { return Err("❌ Caution trop faible pour payer les frais de retrait.".to_string()); }

    let return_amount = stake_amount - fee;

    let balanced_bfs = generate_balanced_blinding_factors(&vec![old_bf], 1);
    let out_bf = &balanced_bfs[0];

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).map_err(|_| "Clé publique invalide")?;
    
    let (new_capsule, shared_secret) = encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();
    let mut otp = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp);
    let bf_json = serde_json::to_string(out_bf).unwrap();
    let payload = format!("{}|{}|{}", return_amount, hex::encode(otp), bf_json);
    
    let aes_key = Key::<Aes256Gcm>::from_slice(&shared_secret);
    let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let encrypted_data = Aes256Gcm::new(aes_key).encrypt(Nonce::from_slice(&nonce_bytes), payload.as_bytes()).unwrap();
    let mut final_vault = nonce_bytes.to_vec(); final_vault.extend_from_slice(&encrypted_data);

    let output = TransactionOutput {
        stealth_address: format!("pq_watt_{}", hex::encode(&otp[0..8])), 
        kyber_capsule: hex::encode(&new_capsule),
        aes_vault: hex::encode(final_vault),
        lattice_commitment: LWECommitment::commit(return_amount, out_bf),
    };

    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);

    let mut current_index = 0u64;
    let wots_keys = loop {
        let keys = wots::Wots::generate_keypair(&seed_bytes, current_index);
        let pk_hex = hex::encode(&keys.1);
        if !spent_keys_snapshot.contains(&pk_hex) && !pending_snapshot.contains(&pk_hex) {
            break keys;
        }
        current_index += 1;
    };
    let pubkey_hex = hex::encode(&wots_keys.1);

    let mut tx_pq = Transaction {
        tx_type: TransactionType::L2Unstake { l2_name: l2_name.clone() },
        inputs: vec![TransactionInput { commitment, source_height }],
        outputs: vec![output],
        fee,
        wots_signature: None,
        public_key: pubkey_hex.clone(),
    };
    
    let tx_hash_64 = tx_pq.hash_data();
    let mut tx_hash_32 = [0u8; 32];
    tx_hash_32.copy_from_slice(&tx_hash_64[0..32]);

    tx_pq.wots_signature = Some(wots::Wots::sign(&wots_keys.0, current_index, &tx_hash_32, &wots_keys.1));

    let tx_bytes = bincode::serialize(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_bytes)).await?;

    crate::mark_tx_as_pending_in_ram(&tx_pq);

    Ok(format!("🔓 Caution récupérée avec succès ! La L2 '{}' a été désactivée.", l2_name))
}

// 3. Ajoute cette nouvelle fonction pour lire l'API du nœud :
pub async fn get_l2_status(l2_name: &str) -> Result<serde_json::Value, String> {
    let res_str = node_call("GET", &format!("/l2/status/{}", l2_name), None).await?;
    serde_json::from_str(&res_str).map_err(|e| e.to_string())
}

// ===================================================================
// OPTIMISATION : CACHE RÉSEAU & CRYPTOGRAPHIQUE (Propulsé par SLED)
// ===================================================================

pub fn get_wallet_db_path() -> Result<PathBuf, String> {
    let mut path = crate::get_base_dir().ok_or("Impossible de trouver le dossier système".to_string())?;
    path.push("wattcoin_wallet");
    let name = CURRENT_WALLET.lock().unwrap().clone();
    path.push(format!("{}.db", name)); // Base de données embarquée Sled
    Ok(path)
}

// Évite de re-télécharger toute la chaîne sur Tor si le bloc n'a pas changé
pub async fn get_all_transactions_cached() -> Result<String, String> {
    let current_wallet_name = CURRENT_WALLET.lock().unwrap().clone();
    let info_str = node_call("GET", "/info", None).await?;
    let info: serde_json::Value = serde_json::from_str(&info_str).unwrap_or_default();
    
    let current_height = info["blocks"].as_u64().unwrap_or(0);
    let current_l2_blocks = info["l2_blocks"].as_u64().unwrap_or(0);
    
    let mut cache_ram = CACHED_CHAIN.lock().await;
    // Si la RAM est déjà à jour, on la renvoie instantanément
    if cache_ram.0 == current_wallet_name && cache_ram.1 == current_height && cache_ram.2 == current_l2_blocks && !cache_ram.3.is_empty() {
        return Ok(cache_ram.3.clone()); 
    }
    
    // 1. Ouverture de la base Sled
    let db_path = get_wallet_db_path()?;
    let db = sled::open(&db_path).map_err(|e| e.to_string())?;
    let tx_tree = db.open_tree("transactions").unwrap();
    let meta_tree = db.open_tree("metadata").unwrap();

    // --- MIGRATION AUTOMATIQUE DE L'ANCIEN JSON VERS SLED ---
    let mut old_json_path = crate::get_base_dir().unwrap();
    old_json_path.push("wattcoin_wallet");
    old_json_path.push(format!("{}_chain.json", current_wallet_name));
    
    if old_json_path.exists() && tx_tree.is_empty() {
        println!("🔄 Migration de l'historique vers Sled en cours...");
        if let Ok(data) = std::fs::read_to_string(&old_json_path) {
            if let Ok(old_txs) = serde_json::from_str::<Vec<serde_json::Value>>(&data) {
                for tx in old_txs {
                    let key = db.generate_id().unwrap().to_be_bytes();
                    let _ = tx_tree.insert(key, serde_json::to_vec(&tx).unwrap());
                }
                let _ = std::fs::remove_file(&old_json_path); // Nettoyage de l'ancien fichier
            }
        }
    }
    // --------------------------------------------------------

    let mut last_l1 = meta_tree.get(b"last_l1").unwrap().map(|v| u64::from_be_bytes(v.as_ref().try_into().unwrap())).unwrap_or(0);
    let mut last_l2 = meta_tree.get(b"last_l2").unwrap().map(|v| u64::from_be_bytes(v.as_ref().try_into().unwrap())).unwrap_or(0);

    // Si on vient de migrer, on recalcule les curseurs
    if last_l1 == 0 && !tx_tree.is_empty() {
        for item in tx_tree.iter() {
            if let Ok((_, v)) = item {
                if let Ok(tx) = serde_json::from_slice::<serde_json::Value>(&v) {
                    let is_l2 = tx["is_l2"].as_bool().unwrap_or(false);
                    if !is_l2 {
                        let h = tx["height"].as_u64().unwrap_or(0);
                        if h > last_l1 { last_l1 = h; }
                    } else {
                        let idx = tx["micro_index"].as_u64().unwrap_or(0);
                        if idx > last_l2 { last_l2 = idx; }
                    }
                }
            }
        }
    }

    // 2. Si on est en retard, on demande le delta au nœud
    if last_l1 < current_height || last_l2 < current_l2_blocks {
        crate::set_status(&format!("🔄 Tél. différentiel (L1: {}->{}, L2: {}->{})...", last_l1, current_height, last_l2, current_l2_blocks));
        
        let endpoint = format!("/sync_blocks?last_l1={}&last_l2={}", last_l1, last_l2);
        let new_txs_str = node_call("GET", &endpoint, None).await?;
        
        if let Ok(new_txs) = serde_json::from_str::<Vec<serde_json::Value>>(&new_txs_str) {
            if !new_txs.is_empty() {
                for tx in new_txs {
                    let key = db.generate_id().unwrap().to_be_bytes();
                    let _ = tx_tree.insert(key, serde_json::to_vec(&tx).unwrap());
                }
                let _ = meta_tree.insert(b"last_l1", &current_height.to_be_bytes());
                let _ = meta_tree.insert(b"last_l2", &current_l2_blocks.to_be_bytes());
            }
        }
    }

    // 3. Mettre en RAM et renvoyer
    let mut local_txs = Vec::new();
    for item in tx_tree.iter() {
        if let Ok((_, v)) = item {
            if let Ok(tx) = serde_json::from_slice::<serde_json::Value>(&v) {
                local_txs.push(tx);
            }
        }
    }

    let final_json = serde_json::to_string(&local_txs).unwrap_or_default();
    *cache_ram = (current_wallet_name, current_height, current_l2_blocks, final_json.clone());
    Ok(final_json)
}

pub fn load_cache() -> WalletCache {
    let db_path = match get_wallet_db_path() {
        Ok(p) => p,
        Err(_) => return WalletCache::default(),
    };

    if let Ok(db) = sled::open(&db_path) {
        let meta_tree = db.open_tree("metadata").unwrap();

        // --- MIGRATION DE L'ANCIEN .CACHE ---
        let mut old_cache_path = crate::get_base_dir().unwrap();
        let name = CURRENT_WALLET.lock().unwrap().clone();
        old_cache_path.push("wattcoin_wallet");
        old_cache_path.push(format!("{}.cache", name));

        if old_cache_path.exists() && meta_tree.get(b"wallet_cache").unwrap().is_none() {
            println!("🔄 Migration du Cache Kyber vers Sled en cours...");
            if let Ok(data) = std::fs::read_to_string(&old_cache_path) {
                if let Ok(cache) = serde_json::from_str::<WalletCache>(&data) {
                    let _ = meta_tree.insert(b"wallet_cache", serde_json::to_vec(&cache).unwrap());
                    let _ = std::fs::remove_file(&old_cache_path);
                }
            }
        }
        // ------------------------------------

        if let Ok(Some(data)) = meta_tree.get(b"wallet_cache") {
            if let Ok(cache) = serde_json::from_slice(&data) {
                return cache;
            }
        }
    }
    WalletCache::default()
}

pub fn save_cache(cache: &WalletCache) {
    if let Ok(db_path) = get_wallet_db_path() {
        if let Ok(db) = sled::open(&db_path) {
            if let Ok(meta_tree) = db.open_tree("metadata") {
                if let Ok(data) = serde_json::to_vec(cache) {
                    let _ = meta_tree.insert(b"wallet_cache", data);
                    let _ = db.flush(); // On écrit sur le disque !
                }
            }
        }
    }
}

pub fn try_decrypt_output(
    out: &TransactionOutput, sk_bytes: &[u8], height: u64, is_l2: bool, micro_index: u64,
    cache: &mut WalletCache, cache_updated: &mut bool
) -> Option<String> {
    
    // Le L2 a son propre tempo ! On sépare les vérifications de cache :
    let is_old = if is_l2 {
        micro_index > 0 && micro_index <= cache.last_scanned_micro_index
    } else {
        height > 0 && height <= cache.last_scanned_height
    };

    if is_old {
        return cache.my_decrypted_payloads.get(&out.kyber_capsule).cloned();
    }
    
    // Sinon, nouveau bloc : on lance la cryptographie
    if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
        if let Ok(shared_secret) = decapsulate(&capsule_bytes, sk_bytes) {
            if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                if vault_bytes.len() > 12 {
                    let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&shared_secret));
                    if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                        if let Ok(payload_str) = String::from_utf8(plaintext) {
                            
                            // On enregistre dans le cache si c'est un vrai bloc miné (L1 ou L2)
                            if (is_l2 && micro_index > 0) || (!is_l2 && height > 0) {
                                cache.my_decrypted_payloads.insert(out.kyber_capsule.clone(), payload_str.clone());
                                *cache_updated = true;
                            }
                            return Some(payload_str);
                        }
                    }
                }
            }
        }
    }
    None
}

pub async fn bridge_to_l2(
    l2_target_name: String,
    receiver_pubkey: String,
    amount_watt: f64,
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    master_seed_hex: String,
) -> Result<String, String> {
    
    let clean_recipient = receiver_pubkey.replace("wattcoin:", "").replace("L2_WATT_", "").replace("pq_watt_", "");
    if clean_recipient.starts_with("Wq") {
        return Err("❌ Erreur : Le Bridge nécessite l'adresse Kyber complète du destinataire L2, pas l'adresse courte de minage.".to_string());
    }

    let schedule = get_fee_schedule().await?; 
    let amount_flames = (amount_watt * 1_000_000_000.0) as u64;
    let mut num_inputs = 0;
    let mut fee = calculate_dynamic_fee(1, 2, false, &schedule);
    let mut required_total = amount_flames + fee;

    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);

    let mut cache = load_cache();
    let mut cache_updated = false;
    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
    let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect();
    
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let decoded_pub = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap_or_default();
    let my_short_address = format!("Wq{}", bs58::encode(sha2::Sha256::digest(&decoded_pub)).into_string());
    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64; 
    let mut input_blinding_factors = Vec::new(); 

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0);

        if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
        if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut commit_hasher = sha2::Sha512::new();
            for val in &out.lattice_commitment.t_vector { commit_hasher.update(val.to_le_bytes()); }
            let expected_key_image = hex::encode(commit_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image) { continue; }
            
            let is_valid_source = out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.stealth_address == sender_kyber_public_hex;
            if !is_valid_source { continue; }
            
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_");
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { continue; }

            let mut is_mine = false;
            let mut val = 0u64;
            let mut my_bf = vec![0u64; LATTICE_DIM];

            if out.stealth_address.starts_with("pq_watt_") {
                if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
                    let parts: Vec<&str> = payload_str.split('|').collect();
                    if parts.len() >= 2 {
                        if let Ok(amt) = parts[0].parse::<u64>() { 
                            val = amt; is_mine = true; 
                            if parts.len() == 3 {
                                if let Ok(parsed_bf) = serde_json::from_str::<Vec<u64>>(parts[2]) { my_bf = parsed_bf; }
                            }
                        }
                    }
                }
            } else if out.stealth_address == format!("COINBASE_{}", sender_kyber_public_hex) 
                || out.stealth_address == format!("COINBASE_{}", my_short_address) 
                || out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) 
                || out.stealth_address == format!("JACKPOT_{}", my_short_address) 
                {
                val = out.aes_vault.parse::<u64>().unwrap_or(0); is_mine = true;
            }

            if is_mine && val > 0 {
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), if is_system_reward { height } else { 0 }));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                num_inputs += 1;
                
                fee = calculate_dynamic_fee(num_inputs, 2, false, &schedule);
                required_total = amount_flames + fee;

                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
    
    if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
    if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
    if cache_updated { save_cache(&cache); }

    if collected_flames < required_total { return Err("❌ Fonds insuffisants.".to_string()); }

    let change_amount = collected_flames - required_total;
    
    let mut sum_in_bf = vec![0u64; crate::lattice::LATTICE_DIM];
    for bf in &input_blinding_factors {
        for i in 0..crate::lattice::LATTICE_DIM {
            sum_in_bf[i] = sum_in_bf[i].wrapping_add(bf[i]);
        }
    }
    
    let mut outputs = Vec::new();
    let bridge_bf = vec![0u64; crate::lattice::LATTICE_DIM];
    
    outputs.push(TransactionOutput {
        stealth_address: format!("BRIDGE_L2_{}", l2_target_name.to_uppercase()),
        kyber_capsule: "L2_BRIDGE_LOCK".to_string(),
        aes_vault: amount_flames.to_string(),
        lattice_commitment: LWECommitment::commit(amount_flames, &bridge_bf), 
    });

    if change_amount > 0 {
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
        let change_bf = &sum_in_bf; 
        
        let (kyber_capsule_change, my_shared_secret) = pqc_kyber::encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();
        let mut otp2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp2);
        
        let bf_json2 = serde_json::to_string(change_bf).unwrap();
        let payload2 = format!("{}|{}|{}", change_amount, hex::encode(otp2), bf_json2);
        
        let aes_key2 = aes_gcm::Key::<aes_gcm::Aes256Gcm>::from_slice(&my_shared_secret);
        let mut nonce_bytes2 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes2);
        use aes_gcm::aead::Aead;
        let encrypted_data2 = aes_gcm::Aes256Gcm::new(aes_key2).encrypt(aes_gcm::Nonce::from_slice(&nonce_bytes2), payload2.as_bytes()).unwrap();
        let mut final_vault2 = nonce_bytes2.to_vec(); final_vault2.extend_from_slice(&encrypted_data2);

        outputs.push(TransactionOutput {
            stealth_address: format!("pq_watt_{}", hex::encode(&otp2[0..8])),
            kyber_capsule: hex::encode(&kyber_capsule_change),
            aes_vault: hex::encode(final_vault2),
            lattice_commitment: LWECommitment::commit(change_amount, change_bf)
        });
    }

    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);

    let mut current_index = 0u64;
    let wots_keys = loop {
        let keys = wots::Wots::generate_keypair(&seed_bytes, current_index);
        let pk_hex = hex::encode(&keys.1);
        if !spent_keys_snapshot.contains(&pk_hex) && !pending_snapshot.contains(&pk_hex) {
            break keys;
        }
        current_index += 1;
    };
    let pubkey_hex = hex::encode(&wots_keys.1);
    
    let mut final_l2_receiver = clean_recipient.clone();
    if final_l2_receiver == sender_kyber_public_hex {
        final_l2_receiver = format!("{}|{}", sender_kyber_public_hex, pubkey_hex);
    }

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        final_inputs.push(TransactionInput { commitment: utxo.2.clone(), source_height: utxo.3 });
    }

    let mut tx_pq = Transaction { 
        tx_type: TransactionType::L2BridgeLock { l2_target_name: l2_target_name.clone(), l2_receiver_pubkey: final_l2_receiver }, 
        inputs: final_inputs, 
        outputs, 
        fee, 
        wots_signature: None, 
        public_key: pubkey_hex.clone() 
    };
    
    let tx_hash_64 = tx_pq.hash_data();
    let mut tx_hash_32 = [0u8; 32];
    tx_hash_32.copy_from_slice(&tx_hash_64[0..32]);

    tx_pq.wots_signature = Some(wots::Wots::sign(&wots_keys.0, current_index, &tx_hash_32, &wots_keys.1));

    let tx_bytes = bincode::serialize(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_bytes)).await?;

    crate::mark_tx_as_pending_in_ram(&tx_pq);

    Ok(format!("✅ {} WATT verrouillés avec succès pour le réseau L2 {} !", amount_watt, l2_target_name))
}

pub async fn get_history_offline(keys: WalletKeys) -> Result<Vec<HistoryItem>, String> {
	use chrono::{DateTime, Utc, Local};
	
	let mut enriched = Vec::new();
	if let Ok(db_path) = get_wallet_db_path() {
		if let Ok(db) = sled::open(&db_path) {
			if let Ok(tx_tree) = db.open_tree("transactions") {
				for item in tx_tree.iter() {
					if let Ok((_, v)) = item {
						if let Ok(tx) = serde_json::from_slice::<serde_json::Value>(&v) {
							enriched.push(tx);
						}
					}
				}
			}
		}
	}

    // On estime la hauteur actuelle au maximum local pour le calcul de maturité
    let mut current_height = 0;
    for item in &enriched {
        if let Some(h) = item.get("height").and_then(|h| h.as_u64()) {
            if h > current_height { current_height = h; }
        }
    }

    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
    let mut cache = load_cache();
    let mut cache_updated = false;

    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
	let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect();

    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;
    
    // On utilise directement un Vec
    let mut final_history: Vec<HistoryItem> = Vec::new();

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let timestamp = item["timestamp"].as_i64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0);

        if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
        if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }

        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) {
            Ok(t) => t, Err(_) => continue,
        };
        
        for out in tx.outputs.iter() {
			let mut commit_hasher = sha2::Sha512::new();
			for val in &out.lattice_commitment.t_vector {
				commit_hasher.update(val.to_le_bytes());
			}
			let expected_key_image = hex::encode(commit_hasher.finalize());

            let is_spent = spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image);
            let status_text = if is_spent { "Dépensé" } else { "Disponible" };

            let mut is_mature = true;
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.kyber_capsule.starts_with("MICRO_COINBASE_");
            
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { 
                is_mature = false; 
            }
            if !is_mature { continue; }

            let date_str = if timestamp > 0 {
                let dt: DateTime<Utc> = DateTime::from_timestamp(timestamp, 0).unwrap_or_default();
                dt.with_timezone(&Local).format("%d/%m/%Y %H:%M").to_string()
            } else {
                "En attente".to_string()
            };

            let mut amt_to_add = 0f64;
            let mut label = String::new();

            if out.stealth_address == format!("COINBASE_{}", keys.watt_address) 
                || out.stealth_address == format!("JACKPOT_{}", keys.watt_address) 
                || out.stealth_address == keys.watt_address 
            {
                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                    amt_to_add = amt as f64 / 1_000_000_000.0;
                    if out.stealth_address.starts_with("JACKPOT") { label = "Jackpot gagné ! 🎰".to_string(); } 
                    else if out.stealth_address == keys.watt_address { label = "Swap Atomique Réclamé ⚡".to_string(); } 
                    else if out.kyber_capsule.starts_with("SHARE_") { label = "Part de minage (P2Pool) ⛏".to_string(); } 
                    else { label = "Récompense bloc + Frais ⛏".to_string(); }
                }
            } 
            else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("L2_WATT_") {
                if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
                    let parts: Vec<&str> = payload_str.split('|').collect();
                    if parts.len() >= 2 {
                        if let Ok(amt) = parts[0].parse::<u64>() {
                            amt_to_add = amt as f64 / 1_000_000_000.0;
                            if matches!(tx.tx_type, TransactionType::MicroCoinbase) { label = "Frais Séquenceur ⚡".to_string(); } 
                            else if out.stealth_address.starts_with("L2_WATT_") && !is_l2 { label = "Dépôt (Bridge L1 ➡ L2) 🌉".to_string(); } 
                            else { label = "Transfert".to_string(); }
                        }
                    }
                }
            }

            if amt_to_add > 0.0 {
                let current_layer = if out.stealth_address.starts_with("L2_WATT_") { "L2".to_string() } else { "L1".to_string() };
                let display_id = if is_l2 && micro_index > 0 { format!("MicroBloc #{}", micro_index) } else { format!("Bloc #{}", height) };
                let status_full = format!("{} ({})", label, status_text);
                
                final_history.push(HistoryItem {
                    id: display_id,
                    tx_type: "receive".to_string(),
                    amount: amt_to_add,
                    coin: "WATT".to_string(),
                    date: date_str,
                    status: status_full,
                    layer: current_layer,
                    raw_timestamp: timestamp,
                });
            }
        }
    }
    
    if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
    if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
    if cache_updated { save_cache(&cache); }

    final_history.sort_by(|a, b| b.raw_timestamp.cmp(&a.raw_timestamp));

    Ok(final_history)
}

// Dépôt d'un Alias de portefeuille
pub async fn register_wns_alias(
    domain: String, 
    target_wallet: String, 
    fee: u64, 
    keys: WalletKeys,
    action: WnsAction
) -> Result<String, String> {
    
    // L'adresse de réception Kyber n'a pas de préfixe pq_watt_ ou L2_WATT_
    // On vérifie juste qu'il ne s'agit pas de l'adresse courte de minage
    if target_wallet.starts_with("Wq") {
        return Err("❌ L'adresse cible doit être votre longue adresse de réception Kyber, pas l'adresse courte de minage.".to_string());
    }
    
    submit_wns_transaction(domain, target_wallet, fee, keys, action).await
}

// Dépôt d'un Serveur Relais
pub async fn register_wns_relay(
    domain: String, 
    ip_port: String, 
    node_pubkey: String, 
    fee: u64, 
    keys: WalletKeys,
    action: WnsAction
) -> Result<String, String> {
    if !ip_port.contains(':') {
        return Err("❌ Format IP invalide (ex: 82.12.34.56:8000).".to_string());
    }
    if node_pubkey.is_empty() {
        return Err("❌ La clé publique du nœud est requise.".to_string());
    }
    let record_data = format!("{}|{}", ip_port, node_pubkey);
    submit_wns_transaction(domain, record_data, fee, keys, action).await
}

// La vraie mécanique interne (l'ancienne register_wns_domain)
async fn submit_wns_transaction(
    domain: String, 
    record_data: String, 
    fee: u64, 
    keys: WalletKeys,
    action: WnsAction
) -> Result<String, String> {
    let resolver = if LOCAL_DEV_MODE { "http://127.0.0.1:8200" } else { "http://80.78.26.243/wns" };
    
    crate::set_status("🔍 Synchronisation avec l'état du L2 WNS...");
    let balance_url = format!("{}/balance/{}", resolver, keys.watt_address);
    let res = HTTP_CLIENT.get(&balance_url).send().await.map_err(|_| "Séquenceur WNS injoignable")?;
    let json: serde_json::Value = res.json().await.map_err(|_| "Erreur JSON WNS")?;
    
    let balance = json["balance"].as_u64().unwrap_or(0);
    let auth_key = json["authorized_lattice_key"].as_str().unwrap_or("");
    let nonce = json["nonce"].as_u64().unwrap_or(0);

    if balance < fee {
        return Err(format!("Fonds insuffisants sur le WNS (Solde: {}). Utilisez l'onglet Bridge.", balance));
    }

    if auth_key.is_empty() {
        return Err("Votre compte WNS n'est pas initialisé. Faites un premier Bridge.".to_string());
    }

    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&keys.master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);
    
    // On cherche la clé WOTS+ exacte attendue par le WNS !
    let mut current_index = 0u64;
    let wots_keys = loop {
        let keys_tmp = wots::Wots::generate_keypair(&seed_bytes, current_index);
        let pk_hex = hex::encode(&keys_tmp.1);
        if pk_hex == auth_key {
            break keys_tmp;
        }
        current_index += 1;
        
        // Sécurité anti-boucle infinie (au cas où)
        if current_index > 2000 {
            return Err("❌ Impossible de retrouver la clé WOTS+ synchronisée avec le WNS.".to_string());
        }
    };
    let pubkey_hex = hex::encode(&wots_keys.1);

    // KEY ROLLING (La clé pour la PROCHAINE transaction)
    // WOTS est à usage unique, on doit avancer l'index !
    let next_wots_keys = wots::Wots::generate_keypair(&seed_bytes, current_index + 1);
    let next_pubkey_hex = hex::encode(&next_wots_keys.1);

    crate::set_status("🏷️ Signature et soumission de la transaction WNS...");

    let mut l2_tx = L2Transaction {
        account_address: keys.watt_address.clone(), 
        sender_pubkey: pubkey_hex.clone(),      
        next_pubkey: next_pubkey_hex, // KEY ROLLING APPLIQUÉ !
        nonce: nonce + 1, 
        action, 
        domain_name: domain.clone(),
        record_data,
        amount: 0,
        fee,
        signature: String::new(),
    };

    let hash = l2_tx.hash_data(); 
    let wots_sig = wots::Wots::sign(&wots_keys.0, current_index, &hash, &wots_keys.1);
    l2_tx.signature = serde_json::to_string(&wots_sig).unwrap();

    let url = format!("{}/send", resolver);
    let res = HTTP_CLIENT.post(&url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&l2_tx).unwrap())
        .send().await.map_err(|e| format!("Erreur réseau WNS : {}", e))?;

    if res.status().is_success() {
        let mut instant_cache = load_cache();
        instant_cache.known_used_lattice_pubkeys.insert(pubkey_hex);
        save_cache(&instant_cache);
        Ok(format!("✅ Réservation/Mise à jour réussie pour '{}' !", domain))
    } else {
        let err_text = res.text().await.unwrap_or_default();
        Err(format!("❌ Rejeté par le WNS : {}", err_text))
    }
}

pub async fn resolve_wns_domain_opsec(domain: &str) -> Result<String, String> {
    // 1. On cherche d'abord dans la RAM silencieusement
    {
        let cache = WNS_CACHE.lock().await;
        if let Some((record_data, _owner)) = cache.get(domain) {
            return Ok(record_data.clone());
        }
    }

    // 2. Si le nom n'y est pas (ou si le cache est vide), on télécharge TOUT l'annuaire
    // OpSec : Le serveur ne sait pas quel nom on cherche !
    crate::set_status("🔄 Téléchargement sécurisé de l'annuaire WNS...");
    sync_wns_directory(LOCAL_DEV_MODE).await;

    // 3. On revérifie dans la RAM mise à jour
    let cache = WNS_CACHE.lock().await;
    if let Some((record_data, _owner)) = cache.get(domain) {
        Ok(record_data.clone())
    } else {
        Err(format!("Le domaine '{}' n'existe pas.", domain))
    }
}

pub async fn estimate_tx_weight(
    amount: f64,
    tip_watt: f64, 
    sender_kyber_secret_hex: &str,
    sender_kyber_public_hex: &str, 
    spend_from_l2: bool,
    send_to_l2: bool
) -> Result<(usize, f64, f64), String> { 

    let schedule = get_fee_schedule().await?; 
    let is_pure_l2 = spend_from_l2 && send_to_l2;

    let amount_in_flames = (amount * 1_000_000_000.0) as u64;
    let tip_flames = (tip_watt * 1_000_000_000.0) as u64; 
    
    let mut fee = calculate_dynamic_fee(1, 2, is_pure_l2, &schedule) + tip_flames; 
    let mut required_total = amount_in_flames + fee;

    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).unwrap_or_default();
    let current_height = get_current_block_height().await.unwrap_or(0);

    let sk_bytes = hex::decode(sender_kyber_secret_hex).unwrap_or_default();
    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let decoded_pub = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap_or_default();
    let my_short_address = format!("Wq{}", bs58::encode(sha2::Sha256::digest(&decoded_pub)).into_string());
    
    let mut cache = load_cache();
    let mut cache_updated = false;
    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
    let pending_snapshot: std::collections::HashSet<String> = PENDING_SPENDS.lock().unwrap().keys().cloned().collect();

    let mut num_inputs = 0;
    let mut collected_flames = 0u64;

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0);

        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            // Le réseau fonctionne en Lattice Commitment, pas en Kyber Capsule !
            let mut commit_hasher = sha2::Sha512::new();
            for val in &out.lattice_commitment.t_vector {
                commit_hasher.update(val.to_le_bytes());
            }
            let expected_key_image = hex::encode(commit_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) || pending_snapshot.contains(&expected_key_image) { continue; }

            let is_valid_source = if spend_from_l2 {
				out.stealth_address.starts_with("L2_WATT_")
			} else {
				// On autorise sender_kyber_public_hex (Les fonds du DEX !)
				out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.stealth_address == sender_kyber_public_hex
			};
            if !is_valid_source { continue; }

            let mut is_mature = true;
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.kyber_capsule.starts_with("MICRO_COINBASE_");
            
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { 
                is_mature = false; 
            }
            if !is_mature { continue; }

            let mut is_mine = false;
            let mut val = 0u64;

            if out.stealth_address == format!("COINBASE_{}", my_short_address) 
                || out.stealth_address == format!("JACKPOT_{}", my_short_address) 
                || out.stealth_address == sender_kyber_public_hex 
            {
                val = out.aes_vault.parse::<u64>().unwrap_or(0); is_mine = true;
            } 
            else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("L2_WATT_") {
                if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
                    let parts: Vec<&str> = payload_str.split('|').collect();
                    if parts.len() >= 2 {
                        if let Ok(amt) = parts[0].parse::<u64>() { 
                            val = amt; is_mine = true; 
                        }
                    }
                }
            }

            if is_mine && val > 0 {
                num_inputs += 1;
                collected_flames += val;
                
                fee = calculate_dynamic_fee(num_inputs, 2, is_pure_l2, &schedule) + tip_flames; 
                required_total = amount_in_flames + fee;

                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }

    let wots_signature_size = 90_000.0; 
    let exact_bytes = (num_inputs as f64 * 8_200.0) + (2.0 * 8_250.0) + wots_signature_size + 500.0; 
    let exact_size_mb = exact_bytes / (1024.0 * 1024.0);
    let fee_watt = fee as f64 / 1_000_000_000.0;

    Ok((num_inputs, exact_size_mb, fee_watt))
}


