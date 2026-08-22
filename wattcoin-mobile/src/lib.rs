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
pub use wattcoin_core::wots;
pub use wattcoin_core::lattice::{self, LWECommitment, LATTICE_DIM};
pub use wattcoin_core::merkle_ring;
pub use wattcoin_core::transaction::{Transaction, TransactionType, TransactionInput, TransactionOutput, SwapContract};

// 2. IMPORT DES OUTILS L2 WNS (Directement depuis le Séquenceur WNS !)
pub use wattcoin_name_service::transaction::{L2Transaction, WnsAction};

// NOS MODULES PROPRES !
pub mod app;


pub static CURRENT_WALLET: Lazy<StdMutex<String>> = Lazy::new(|| StdMutex::new("Principal".to_string()));
static CACHED_CHAIN: Lazy<AsyncMutex<(String, u64, u64, String)>> = Lazy::new(|| AsyncMutex::new((String::new(), 0, 0, String::new())));
// LE SYSTÈME DE SUIVI EN DIRECT
pub static SYNC_STATUS: Lazy<StdMutex<String>> = Lazy::new(|| StdMutex::new(String::new()));



const MATURITY_BLOCKS: u64 = 3; 
const FLAME: u64 = 1_000_000_000;
const MAX_BILL_WATT: u64 = 10_000; // Le plus gros billet autorisé (pour garantir le Ring Signature)

// ===================================================================
// 1. LES RÉSOLVEURS WNS ET LE CACHE
// ===================================================================
pub const WNS_RESOLVERS: &[&str] = &[
    "http://127.0.0.1:8200", // En local on tape direct sur le port 8200 !
    // "http://80.78.26.243/wns", // Pour la PROD plus tard
];

pub const NETWORK_SEEDS: &[&str] = &[
    "seed.watt", // Le nom de domaine de ton noeud fondateur
];

pub static WNS_CACHE: Lazy<AsyncMutex<HashMap<String, (String, String)>>> = Lazy::new(|| AsyncMutex::new(HashMap::new()));

#[derive(Deserialize)]
struct WnsDirectory {
    domains: HashMap<String, String>,
    owners: HashMap<String, String>,
}

/// Télécharge tout l'annuaire WNS en RAM (OpSec pure)
pub async fn sync_wns_directory() {
    // AUTOMATISATION : On choisit le bon WNS selon le mode
    let resolver = if LOCAL_DEV_MODE {
        "http://127.0.0.1:8200" // En local, on tape le Séquenceur local
    } else {
        "http://80.78.26.243/wns" // En prod, on tape NGINX
    };

    let url = format!("{}/directory", resolver);
    if let Ok(res) = HTTP_CLIENT.get(&url).send().await {
        if let Ok(directory) = res.json::<WnsDirectory>().await {
            let mut cache = WNS_CACHE.lock().await;
            cache.clear();
            for (domain, record) in directory.domains {
                if let Some(owner) = directory.owners.get(&domain) {
                    cache.insert(domain, (record, owner.clone()));
                }
            }
            println!("📖 [WNS] Annuaire téléchargé ({} domaines) depuis {}", cache.len(), resolver);
        }
    }
}

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
    pub master_seed_hex: String,
    pub kyber_secret_hex: String,
    pub wots_index: u32,
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
	pub known_used_wots_pubkeys: std::collections::HashSet<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HopPayload {
    pub next_hop_address: String,
    pub inner_data: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OnionPacket {
    pub kyber_capsule: String, 
    pub encrypted_payload: String, 
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

// PISTEUR D'INDEX WOTS+
pub fn get_wots_tracker_path() -> Result<PathBuf, String> {
    let mut path = crate::get_base_dir().ok_or("Impossible de trouver le dossier système")?;
    path.push("wattcoin_wallet");
    let name = CURRENT_WALLET.lock().unwrap().clone();
    path.push(format!("{}.wots", name));
    Ok(path)
}

pub fn get_saved_wots_index(base_index: u32) -> u32 {
    if let Ok(path) = get_wots_tracker_path() {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(idx) = content.trim().parse::<u32>() {
                return std::cmp::max(base_index, idx); // On prend toujours le plus grand
            }
        }
    }
    base_index
}

pub fn save_wots_index(new_index: u32) {
    if let Ok(path) = get_wots_tracker_path() {
        let _ = std::fs::write(path, new_index.to_string());
    }
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
        let old_wots = path.join(".wattcoin_wots_index");
        
        if old_vault.exists() {
            let _ = std::fs::rename(&old_vault, path.join("Principal.vault"));
            if old_spends.exists() { let _ = std::fs::rename(&old_spends, path.join("Principal.spends")); }
            if old_wots.exists() { let _ = std::fs::rename(&old_wots, path.join("Principal.wots")); }
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
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap()
});

async fn node_call(method: &str, endpoint: &str, body: Option<String>) -> Result<String, String> {
    
    // 1. On synchronise l'annuaire WNS s'il est vide
    {
        let cache = WNS_CACHE.lock().await;
        if cache.is_empty() {
            drop(cache); // On relâche le verrou pour ne pas bloquer
            sync_wns_directory().await;
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

			// MAGIE DE L'OIGNON : On emballe les requêtes POST
			let (final_url, final_body) = if method == "POST" && body.is_some() {
				
				// L'URL cible à l'intérieur de l'oignon DOIT rester 127.0.0.1:8100 (C'est ce que le Nœud comprend en le déballant)
				let internal_target_url = format!("http://127.0.0.1:8100{}", endpoint);
				
				let packet = wrap_in_onion(&internal_target_url, &body.clone().unwrap(), node_pubkey)?;
				let onion_json = serde_json::to_string(&packet).unwrap();
				
				// On l'envoie à l'extérieur via NGINX
				(format!("http://{}/relay_onion", target_ip), Some(onion_json))
			} else {
				(original_public_url, body.clone())
			};

            // Envoi HTTP
            let req = match method {
                "POST" => HTTP_CLIENT.post(&final_url).header("Content-Type", "application/json").body(final_body.unwrap_or_default()),
                "DELETE" => HTTP_CLIENT.delete(&final_url), 
                _ => HTTP_CLIENT.get(&final_url),           
            };

            // CORRECTION DU BORROW CHECKER RUST ICI :
            match req.send().await {
                Ok(resp) => {
                    if resp.status().is_success() {
                        return Ok(resp.text().await.unwrap_or_default());
                    } else {
                        // 1. On sauvegarde le statut (qui se copie facilement)
                        let status = resp.status(); 
                        // 2. On consomme la réponse
                        let error_msg = resp.text().await.unwrap_or_default(); 
                        
                        println!("⚠️ [RESEAU] Le nœud {} a rejeté la requête (HTTP {}) : {}", seed_domain, status, error_msg);
                        return Err(format!("❌ Rejeté par le Nœud : {}", error_msg)); 
                    }
                },
                Err(e) => {
                    println!("⚠️ [RESEAU] Erreur de connexion brute avec {} : {}", seed_domain, e);
                    // Si on a une vraie erreur de co, on laisse la boucle tester un autre nœud seed s'il y en a un
                }
            }
        }
    }

    Err("❌ Impossible de router la transaction : Vérifiez que le Séquenceur WNS tourne et contient seed.watt.".to_string())
}

pub fn wrap_in_onion(
    target_url: &str, 
    payload: &str, 
    node_pubkey_hex: &str
) -> Result<OnionPacket, String> {
    let pk_bytes = hex::decode(node_pubkey_hex).map_err(|_| "Clé publique du nœud invalide")?;
    
    // 1. Encapsulation Post-Quantique (Génère le secret partagé)
    let mut rng = rand::thread_rng();
    let (capsule, shared_secret) = pqc_kyber::encapsulate(&pk_bytes, &mut rng)
        .map_err(|_| "Erreur encapsulation Kyber")?;
    
    // 2. Préparation des instructions pour le Nœud 
    let hop = HopPayload {
        next_hop_address: target_url.to_string(),
        inner_data: payload.to_string(),
    };
    let hop_json = serde_json::to_string(&hop).unwrap();
    
    // 3. Chiffrement de la couche (AES-256-GCM)
    let aes_key = Key::<Aes256Gcm>::from_slice(&shared_secret);
    let cipher = Aes256Gcm::new(aes_key);
    
    let mut nonce_bytes = [0u8; 12];
    rng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    
    let ciphertext = cipher.encrypt(nonce, hop_json.as_bytes())
        .map_err(|_| "Erreur de chiffrement AES Oignon")?;
        
    let mut encrypted_payload = nonce_bytes.to_vec();
    encrypted_payload.extend(ciphertext);
    
    Ok(OnionPacket {
        kyber_capsule: hex::encode(capsule),
        encrypted_payload: hex::encode(encrypted_payload),
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
    
    // 💡 FIX : On gère les deux formats pour éviter le crash JSON
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

    node_call("POST", "/order", Some(order_data.to_string())).await?;
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
				.nfc() // 🛡️ TOUJOURS STOCKÉ EN NFC POUR L'UI
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
			
			// 🛡️ MAGIE CRYPTO : bip39 exige le format NFKD pour parser
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
			
			// 🛡️ MAGIE VISUELLE : On force le NFC dès la création pour que le coffre soit propre !
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

	Ok(WalletKeys {
		mnemonic, 
		btc_address,
		btc_pubkey_hex: btc_pub.to_string(),
		master_seed_hex: hex::encode(&master_seed),
		watt_address: URL_SAFE_NO_PAD.encode(kyber_keys.public),
		kyber_secret_hex: hex::encode(kyber_keys.secret),
        wots_index: 0, // On commence toujours à l'index 0
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
    
    let json_string = String::from_utf8(plaintext)
        .map_err(|_| WattError::Crypto("Erreur de décodage UTF-8 post-déchiffrement.".to_string()))?;
        
    let keys: WalletKeys = serde_json::from_str(&json_string).map_err(|e| WattError::Json(e))?;
    Ok(keys)
}


// Scanne uniquement les nouveautés de la blockchain en 0.01 seconde
pub fn update_spent_cache_fast(enriched: &[serde_json::Value], cache: &mut WalletCache, cache_updated: &mut bool) {
    // Si c'est la première fois (ou qu'un des deux caches est vide), on force un scan.
    let force_full_scan = (cache.known_spent_key_images.is_empty() || cache.known_used_wots_pubkeys.is_empty()) && cache.last_scanned_height > 0;

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0);

        let is_new = if is_l2 { micro_index > cache.last_scanned_micro_index } else { height > cache.last_scanned_height };
        
        if is_new || height == 0 || force_full_scan {
            if let Some(tx) = item.get("transaction") {
                // 1. Mise en cache des billets dépensés
                if let Some(inputs) = tx.get("inputs").and_then(|i| i.as_array()) {
                    for input in inputs {
                        if let Some(ki) = input.get("mpc_ring").and_then(|m| m.get("key_image")).and_then(|k| k.as_str()) {
                            cache.known_spent_key_images.insert(ki.to_string());
                            *cache_updated = true;
                        }
                    }
                }
                // 2. Mise en cache des clés WOTS+ (O(1) Check !)
                if let Some(pk) = tx.get("public_key").and_then(|p| p.as_str()) {
                    cache.known_used_wots_pubkeys.insert(pk.to_string());
                    *cache_updated = true;
                }
            }
        }
    }
}

pub async fn get_balances(keys: WalletKeys) -> Result<Balances, String> {
    // 1. Remplacement de l'appel réseau
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).unwrap_or_default();

    let current_height = get_current_block_height().await.unwrap_or(0);
    let mut l1_flames: u64 = 0;
    let mut l2_flames: u64 = 0;
    
    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
	
	crate::set_status("🔐 Déchiffrement quantique de vos fonds...");

    // 2. On charge le cache
    let mut cache = load_cache();
    let mut cache_updated = false;
	
	// On ne scan que ce qu'on a pas encore scanné
	crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
	
	// On extrait une copie immuable des dépenses 
    // pour pouvoir utiliser 'cache' (mutablement) dans le déchiffreur en même temps.
    let spent_keys_snapshot = cache.known_spent_key_images.clone();

    // On suit les deux réseaux en parallèle
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    // 3. On remplace la fonction decrypt_amount pour accepter le L2 :
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

        // Mise à jour de nos compteurs locaux
        if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
        if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }

        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            // Calcul déterministe : Ce billet a-t-il été dépensé (même depuis un autre appareil) ?
			let mut ki_hasher = sha2::Sha512::new();
			ki_hasher.update(out.kyber_capsule.as_bytes());
			ki_hasher.update(&sk_bytes); // Le secret Kyber est la clé mathématique
			let expected_key_image = hex::encode(ki_hasher.finalize());

			let is_spent = spent_keys_snapshot.contains(&expected_key_image);
			
			if is_spent { continue; }
            let mut is_mature = true;
            let is_system_reward = out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") || out.kyber_capsule.starts_with("MICRO_COINBASE_");
            
            if is_system_reward && height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) { 
                is_mature = false; 
            }
            if !is_mature { continue; }

            if out.stealth_address == format!("COINBASE_{}", keys.watt_address) 
                || out.stealth_address == format!("JACKPOT_{}", keys.watt_address) 
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

    // 4. On sauvegarde les deux réseaux !
    if current_max_l1 > cache.last_scanned_height {
        cache.last_scanned_height = current_max_l1;
        cache_updated = true;
    }
    if current_max_l2 > cache.last_scanned_micro_index {
        cache.last_scanned_micro_index = current_max_l2;
        cache_updated = true;
    }
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
    use std::collections::HashMap;

    // 1. Remplacement de l'appel réseau
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str)
        .map_err(|_| "Erreur JSON history".to_string())?;

    let current_height = get_current_block_height().await.unwrap_or(0);
    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
	let mut cache = load_cache();
    let mut cache_updated = false;
	
	crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
	let spent_keys_snapshot = cache.known_spent_key_images.clone();
    
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    // Le groupeur visuel d'UTXO
    let mut grouped_history: HashMap<String, HistoryItem> = HashMap::new();

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
            let mut ki_hasher = sha2::Sha512::new();
            ki_hasher.update(out.kyber_capsule.as_bytes());
            ki_hasher.update(&sk_bytes); // Le secret Kyber est la clé mathématique
            let expected_key_image = hex::encode(ki_hasher.finalize());

            let is_spent = spent_keys_snapshot.contains(&expected_key_image);
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

            // 1. Détection des montants en clair
            if out.stealth_address == format!("COINBASE_{}", keys.watt_address) 
                || out.stealth_address == format!("JACKPOT_{}", keys.watt_address) 
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

            // Si cet UTXO nous appartient, on l'ajoute au groupe correspondant
            if amt_to_add > 0.0 {
                
                // 💡 FIX ABSOLU : La couche dépend de la destination des fonds (stealth_address), PAS du bloc !
                let current_layer = if out.stealth_address.starts_with("L2_WATT_") { 
                    "L2".to_string() 
                } else { 
                    "L1".to_string() 
                };
                
                // NUMÉROTATION PROPRE
                let display_id = if is_l2 && micro_index > 0 {
                    format!("MicroBloc #{}", micro_index)
                } else {
                    format!("Bloc #{}", height) 
                };

                let status_full = format!("{} ({})", label, status_text);
                
                // La clé de groupe intègre la couche pour ne jamais mélanger un paiement et sa monnaie rendue cross-layer
                let group_key = format!("{}_{}_{}", current_layer, display_id, status_full);

                let entry = grouped_history.entry(group_key).or_insert(HistoryItem {
                    id: display_id,
                    tx_type: "receive".to_string(),
                    amount: 0.0,
                    coin: "WATT".to_string(),
                    date: date_str,
                    status: status_full,
                    layer: current_layer, // 💡 Filtre infaillible pour l'UI React
                    raw_timestamp: timestamp,
                });
                
                entry.amount += amt_to_add; 
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

    // Tri chronologique infaillible
    let mut final_history: Vec<HistoryItem> = grouped_history.into_values().collect();
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

/// 💵 Découpe un montant arbitraire en coupures standards (Billets)
/// Exemple : 13 WATT -> [10 WATT, 1 WATT, 1 WATT, 1 WATT]
pub fn break_into_denominations(mut amount_flames: u64) -> Vec<u64> {
    let mut denominations = Vec::new();
    
    // 💡 On lie notre constante au premier billet !
    let standard_bills = [
        MAX_BILL_WATT * FLAME, // <--- C'est ici qu'on utilise la constante !
        1_000 * FLAME,
        100 * FLAME,
        10 * FLAME,
        1 * FLAME,
        FLAME / 10,       
        FLAME / 100,      
        FLAME / 1_000,    
        100_000,          
        10_000,
        1_000,
        100,
        10,
        1
    ];

    for bill in standard_bills.iter() {
        while amount_flames >= *bill {
            denominations.push(*bill);
            amount_flames -= *bill;
        }
    }
    
    denominations
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

// DÉCOUVERTE CYPERPUNK OPTIMISÉE PAR LE CACHE
pub fn get_safe_wots_index(cache: &WalletCache, master_seed_hex: &str, saved_index: u32) -> u32 {
    let mut seed_bytes = [0u8; 32];
    if let Ok(decoded_seed) = hex::decode(master_seed_hex) {
        if decoded_seed.len() >= 32 { seed_bytes.copy_from_slice(&decoded_seed[0..32]); }
    } else { return saved_index; }

    let mut current_index = saved_index;
    loop {
        let wots_keys = crate::wots::WotsKeyPair::generate_deterministic(&seed_bytes, current_index);
        // Vérification instantanée en O(1) grâce à la RAM !
        if cache.known_used_wots_pubkeys.contains(&wots_keys.public_key) {
            current_index += 1;
        } else {
            break; 
        }
    }
    current_index
}

pub async fn send_wattcoin(
	recipient_kyber_hex: String,
    amount: f64,
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    master_seed_hex: String, 
    wots_index: u32,         
    htlc_hash_hex: Option<String>,
    htlc_timeout: Option<u64>,
    spend_from_l2: bool, 
    send_to_l2: bool   
) -> Result<u32, String> { 
    
    let amount_in_flames = (amount * 1_000_000_000.0) as u64; 
	if amount_in_flames > (MAX_BILL_WATT * FLAME * 5) {
		return Err(format!("❌ Transaction trop volumineuse ! Limite de sécurité : {} WATT par envoi. Veuillez faire plusieurs virements.", MAX_BILL_WATT * 5));
	}
    let fee: u64 = if spend_from_l2 && send_to_l2 { 100 } else { 1000 }; 
    let required_total = amount_in_flames + fee;

    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();

    // On charge l'état des compteurs
    let mut cache = load_cache();
    let mut cache_updated = false;
	let real_wots_index = get_safe_wots_index(&cache, &master_seed_hex, wots_index);
	crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
	let spent_keys_snapshot = cache.known_spent_key_images.clone();
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64; 
    let mut input_blinding_factors = Vec::new(); 

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0);

        // 💡 2. Mise à jour de nos compteurs locaux
        if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
        if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }

        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut ki_hasher = sha2::Sha512::new();
            ki_hasher.update(out.kyber_capsule.as_bytes());
            ki_hasher.update(&sk_bytes);
            let expected_key_image = hex::encode(ki_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) { continue; }
            
            let is_valid_source = if spend_from_l2 {
                out.stealth_address.starts_with("L2_WATT_")
            } else {
                out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_")
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
            let mut my_bf = vec![0u64; LATTICE_DIM];

            if out.stealth_address == format!("COINBASE_{}", sender_kyber_public_hex) 
                || out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) 
                || out.stealth_address == sender_kyber_public_hex 
            {
                val = out.aes_vault.parse::<u64>().unwrap_or(0); is_mine = true;
            } 
            else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("L2_WATT_") {
                // 💡 3. Le remplaçant magique : on utilise try_decrypt_output !
                if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
                    let parts: Vec<&str> = payload_str.split('|').collect();
                    if parts.len() >= 2 {
                        if let Ok(amt) = parts[0].parse::<u64>() { 
                            val = amt; 
                            is_mine = true; 
                            if parts.len() == 3 {
                                if let Ok(parsed_bf) = serde_json::from_str::<Vec<u64>>(parts[2]) {
                                    my_bf = parsed_bf;
                                }
                            }
                        }
                    }
                }
            }

            if is_mine && val > 0 {
                // Seules les vraies récompenses transmettent leur hauteur au nœud pour subir la règle de maturité.
                // Le reste (Transferts classiques L1/L2) passe à 0 et est dépensable instantanément !
                let actual_source_height = if is_system_reward { height } else { 0 };
                
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), actual_source_height));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
	
	// 💡 4. Sauvegarde finale des compteurs sur le disque
    if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
    if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
    if cache_updated { save_cache(&cache); }

    if collected_flames < required_total {
        return Err(format!("❌ Fonds insuffisants ! Besoin de {} WATT.", required_total as f64 / 1_000_000_000.0));
    }

    // RÉCUPÉRATION DES LEURRES (ASYNC) AVANT LE BLOCAGE CPU
    let mut fetched_decoys = Vec::new();
    if let Ok(res_str) = node_call("GET", "/get_decoys/63", None).await {
        if let Ok(real_decoys) = serde_json::from_str::<Vec<String>>(&res_str) {
            fetched_decoys = real_decoys;
        }
    }

    // On clone les utxos sélectionnés pour pouvoir les sauvegarder après le thread CPU
    let selected_utxos_clone = selected_utxos.clone();

    // ISOLATION DE LA CRYPTOGRAPHIE LOURDE (Empêche l'UI de freeze)
    let tx_pq_result = tokio::task::spawn_blocking(move || {
        let change_amount = collected_flames - required_total;
        let bills_to_send = break_into_denominations(amount_in_flames);
        let change_bills = break_into_denominations(change_amount);
        
        let total_outputs_count = bills_to_send.len() + change_bills.len();
        let balanced_bfs = generate_balanced_blinding_factors(&input_blinding_factors, total_outputs_count);
        
        let mut outputs = Vec::new();
        let mut bf_index = 0;
        
        let tx_type = match (htlc_hash_hex, htlc_timeout) {
            (Some(hash), Some(timeout)) => TransactionType::HTLCLock { hash, timeout_block: timeout },
            _ => TransactionType::Standard,
        };

        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let clean_recipient = recipient_kyber_hex.replace("wattcoin:", "").replace("L2_WATT_", "").replace("pq_watt_", "").replace("htlc_watt_", "");
        let recipient_bytes = URL_SAFE_NO_PAD.decode(&clean_recipient).map_err(|_| "Adresse WATT invalide".to_string())?;
        let stealth_prefix = if send_to_l2 { "L2_WATT_" } else if matches!(tx_type, TransactionType::HTLCLock { .. }) { "htlc_watt_" } else { "pq_watt_" };

        // CONSTRUCTION DES OUTPUTS POUR LE DESTINATAIRE
        for bill_amount in bills_to_send {
            let current_bf = &balanced_bfs[bf_index];
            let (kyber_capsule, shared_secret) = encapsulate(&recipient_bytes, &mut rand::thread_rng()).unwrap();
            let mut otp = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp);
            
            let bf_json = serde_json::to_string(current_bf).unwrap();
            let payload = format!("{}|{}|{}", bill_amount, hex::encode(otp), bf_json);
            
            let aes_key = Key::<Aes256Gcm>::from_slice(&shared_secret);
            let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
            let encrypted_data = Aes256Gcm::new(aes_key).encrypt(Nonce::from_slice(&nonce_bytes), payload.as_bytes()).map_err(|_| "Erreur AES".to_string())?;
            let mut final_vault = nonce_bytes.to_vec(); final_vault.extend_from_slice(&encrypted_data);

            let commitment = LWECommitment::commit(bill_amount, current_bf);

            outputs.push(TransactionOutput {
                stealth_address: format!("{}{}", stealth_prefix, hex::encode(&otp[0..8])),
                kyber_capsule: hex::encode(&kyber_capsule),
                aes_vault: hex::encode(final_vault),
                lattice_commitment: commitment,
            });
            bf_index += 1;
        }

        // CONSTRUCTION DES OUTPUTS DE CHANGE
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
        let change_prefix = if spend_from_l2 { "L2_WATT_" } else { "pq_watt_" };

        for bill_change in change_bills {
            let change_bf = &balanced_bfs[bf_index]; 
            let (kyber_capsule_change, my_shared_secret) = encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();
            let mut otp = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp);
            
            let bf_json = serde_json::to_string(change_bf).unwrap();
            let payload = format!("{}|{}|{}", bill_change, hex::encode(otp), bf_json);
            
            let aes_key = Key::<Aes256Gcm>::from_slice(&my_shared_secret);
            let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
            let encrypted_data = Aes256Gcm::new(aes_key).encrypt(Nonce::from_slice(&nonce_bytes), payload.as_bytes()).unwrap();
            let mut final_vault = nonce_bytes.to_vec(); final_vault.extend_from_slice(&encrypted_data);

            let commitment = LWECommitment::commit(bill_change, change_bf);

            outputs.push(TransactionOutput {
                stealth_address: format!("{}{}", change_prefix, hex::encode(&otp[0..8])),
                kyber_capsule: hex::encode(&kyber_capsule_change),
                aes_vault: hex::encode(final_vault),
                lattice_commitment: commitment
            });
            bf_index += 1;
        }

        // RING SIGNATURES & HASH FINAL
        let mut seed_bytes = [0u8; 32];
        let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
        seed_bytes.copy_from_slice(&decoded_seed[0..32]);
        
        // On utilise real_wots_index pour créer la clé !
        let wots_keys = crate::wots::WotsKeyPair::generate_deterministic(&seed_bytes, real_wots_index);
        let temp_tx = Transaction { tx_type: tx_type.clone(), inputs: vec![], outputs: outputs.clone(), fee, wots_signature: None, public_key: wots_keys.public_key.clone() };
        let tx_hash = temp_tx.hash_data();

        // ANNEAU DE MERKLE : Garantie absolue d'unicité
        let mut decoys = vec![wots_keys.public_key.clone()]; 
        let mut unique_set = std::collections::HashSet::new();
        unique_set.insert(wots_keys.public_key.clone());

        for decoy in fetched_decoys {
            if !unique_set.contains(&decoy) && decoys.len() < 64 {
                unique_set.insert(decoy.clone());
                decoys.push(decoy);
            }
        }
        
        while decoys.len() < 64 { 
            let new_decoy = crate::wots::WotsKeyPair::generate().public_key;
            if !unique_set.contains(&new_decoy) {
                unique_set.insert(new_decoy.clone());
                decoys.push(new_decoy);
            }
        }
        
        use rand::seq::SliceRandom;
        decoys.shuffle(&mut rand::thread_rng());
        let real_index = decoys.iter().position(|r| r == &wots_keys.public_key).unwrap();

        let mut final_inputs = Vec::new();
        for utxo in &selected_utxos_clone {
            let mpc_sig = crate::merkle_ring::MpcRingSignature::sign(
                &wots_keys.secret_key, 
                &tx_hash, 
                &decoys, 
                real_index, 
                &utxo.1,
                &sk_bytes 
            );
            final_inputs.push(TransactionInput { mpc_ring: mpc_sig, commitment: utxo.2.clone(), source_height: utxo.3 });
        }

        let mut tx_pq = Transaction { tx_type, inputs: final_inputs, outputs, fee, wots_signature: None, public_key: wots_keys.public_key.clone() };
        tx_pq.wots_signature = Some(crate::wots::WotsKeyPair::sign(&wots_keys.secret_key, &wots_keys.public_seed, &tx_hash));

        Ok::<Transaction, String>(tx_pq)
    }).await.map_err(|e| format!("Erreur du thread CPU : {}", e))?;

    let tx_pq = tx_pq_result?;

    // 7. BROADCAST
    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_json)).await?;

    // MISE À JOUR DU CACHE LOCAL INSTANTANÉE (Anti-double dépense locale)
    let mut instant_cache = load_cache();
    for input in &tx_pq.inputs {
        // On marque les billets comme "dépensés" localement
        instant_cache.known_spent_key_images.insert(input.mpc_ring.key_image.clone());
    }
    // On marque la clé publique WOTS+ comme "utilisée" localement
    instant_cache.known_used_wots_pubkeys.insert(tx_pq.public_key.clone());
    save_cache(&instant_cache);

    // Sauvegarde du nouvel index sur le disque !
    let next_index = real_wots_index + 1;
    crate::save_wots_index(next_index);

    Ok(next_index)
}


pub async fn send_data(
    recipient_kyber_hex: String, 
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    master_seed_hex: String, 
    wots_index: u32,       
    data_type: String, 
    content: String,
    use_l2: bool
) -> Result<u32, String> {   // ON RENVOIE u32 (le nouvel index)
    
    
    send_data_internal(
        recipient_kyber_hex, sender_kyber_secret_hex, 
        sender_kyber_public_hex, master_seed_hex, wots_index, data_type, content, use_l2
    ).await
}

// 2. Le Moteur Public (Testable par Cargo !)
pub async fn send_data_internal(
    recipient_kyber_hex: String, 
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    master_seed_hex: String, 
    wots_index: u32,         
    data_type: String, 
    content: String,
    use_l2: bool
) -> Result<u32, String> {   // ON RENVOIE u32
    
    let fee: u64 = if use_l2 { 100 } else { 1000 }; 
    let required_total = fee;

    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);
	
	// 1. On charge l'état des compteurs
	let mut cache = load_cache();
	let mut cache_updated = false;
	// On calcule le vrai index réseau
    let real_wots_index = get_safe_wots_index(&cache, &master_seed_hex, wots_index);
	crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
	let spent_keys_snapshot = cache.known_spent_key_images.clone();
	let mut current_max_l1 = cache.last_scanned_height;
	let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64; 
    let mut input_blinding_factors = Vec::new(); 

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
		let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
		let micro_index = item["micro_index"].as_u64().unwrap_or(0);

		// 2. Mise à jour de nos compteurs locaux
		if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
		if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut ki_hasher = sha2::Sha512::new();
            ki_hasher.update(out.kyber_capsule.as_bytes());
            ki_hasher.update(&sk_bytes);
            let expected_key_image = hex::encode(ki_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) { continue; }
            
            let is_valid_source = if use_l2 { out.stealth_address.starts_with("L2_WATT_") } else { out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_") };
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
                || out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) 
                || out.stealth_address == sender_kyber_public_hex 
            {
                val = out.aes_vault.parse::<u64>().unwrap_or(0); is_mine = true;
            } else if out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("L2_WATT_") {
				// 💡 3. Utilisation directe du cache L1/L2
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
                // Seules les vraies récompenses transmettent leur hauteur au nœud pour subir la règle de maturité.
                // Le reste (Transferts classiques L1/L2) passe à 0 et est dépensable instantanément !
                let actual_source_height = if is_system_reward { height } else { 0 };
                
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), actual_source_height));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
	
	// 💡 4. Sauvegarde finale des compteurs sur le disque
	if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
	if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
	if cache_updated { save_cache(&cache); }

    if collected_flames < required_total { return Err("❌ Fonds insuffisants pour payer les frais réseau.".to_string()); }

    let change_amount = collected_flames - fee;
    let total_outputs_count = 1 + if change_amount > 0 { 1 } else { 0 }; 
    let balanced_bfs = generate_balanced_blinding_factors(&input_blinding_factors, total_outputs_count);
    
    let mut outputs = Vec::new();
    let mut bf_index = 0;

    use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
    let clean_recipient = recipient_kyber_hex.replace("wattcoin:", "").replace("L2_WATT_", "").replace("pq_watt_", "");
    let recipient_bytes = URL_SAFE_NO_PAD.decode(&clean_recipient).map_err(|_| "Adresse WATT invalide".to_string())?;
    let stealth_prefix = if use_l2 { "L2_WATT_" } else { "pq_watt_" };

    let data_bf = &balanced_bfs[bf_index];
    let (kyber_capsule, shared_secret) = encapsulate(&recipient_bytes, &mut rand::thread_rng()).unwrap();
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
        let (kyber_capsule_change, my_shared_secret) = encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();
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
    // On utilise real_wots_index
    let wots_keys = crate::wots::WotsKeyPair::generate_deterministic(&seed_bytes, real_wots_index);
    let temp_tx = Transaction { tx_type: TransactionType::Standard, inputs: vec![], outputs: outputs.clone(), fee, wots_signature: None, public_key: wots_keys.public_key.clone() };
	let tx_hash = temp_tx.hash_data();

    // MODE PROD : 64 Vrais Leurres
	let mut decoys = vec![wots_keys.public_key.clone()]; 
	let mut unique_set = std::collections::HashSet::new();
	unique_set.insert(wots_keys.public_key.clone());

	if let Ok(res_str) = node_call("GET", "/get_decoys/63", None).await {
		if let Ok(real_decoys) = serde_json::from_str::<Vec<String>>(&res_str) {
			for decoy in real_decoys {
				if !unique_set.contains(&decoy) && decoys.len() < 64 {
					unique_set.insert(decoy.clone());
					decoys.push(decoy);
				}
			}
		}
	}

	while decoys.len() < 64 { 
		let new_decoy = crate::wots::WotsKeyPair::generate().public_key;
		if !unique_set.contains(&new_decoy) {
			unique_set.insert(new_decoy.clone());
			decoys.push(new_decoy);
		}
	}
    
    use rand::seq::SliceRandom;
    decoys.shuffle(&mut rand::thread_rng());
    let real_index = decoys.iter().position(|r| r == &wots_keys.public_key).unwrap();

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        let mpc_sig = crate::merkle_ring::MpcRingSignature::sign(&wots_keys.secret_key, &tx_hash, &decoys, real_index, &utxo.1, &sk_bytes);
        final_inputs.push(TransactionInput { mpc_ring: mpc_sig, commitment: utxo.2.clone(), source_height: utxo.3 });
    }

    let mut tx_pq = Transaction { tx_type: TransactionType::Standard, inputs: final_inputs, outputs, fee, wots_signature: None, public_key: wots_keys.public_key.clone() };
    tx_pq.wots_signature = Some(crate::wots::WotsKeyPair::sign(&wots_keys.secret_key, &wots_keys.public_seed, &tx_hash));

    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_json)).await?;

    // MISE À JOUR DU CACHE LOCAL INSTANTANÉE (Anti-double dépense locale)
    let mut instant_cache = load_cache();
    for input in &tx_pq.inputs {
        // On marque les billets comme "dépensés" localement
        instant_cache.known_spent_key_images.insert(input.mpc_ring.key_image.clone());
    }
    // On marque la clé publique WOTS+ comme "utilisée" localement
    instant_cache.known_used_wots_pubkeys.insert(tx_pq.public_key.clone());
    save_cache(&instant_cache);

    // Sauvegarde du nouvel index sur le disque !
    let next_index = real_wots_index + 1;
    crate::save_wots_index(next_index);

    // On retourne le nouvel index
    Ok(next_index)
}


pub async fn buy_lottery_ticket(
    sender_kyber_secret_hex: String, 
    sender_kyber_public_hex: String,
    master_seed_hex: String, 
    wots_index: u32,
    ticket_price_flames: u64	
) -> Result<u32, String> {   // ON RENVOIE u32
    
    let fee: u64 = 1000;
    let required_total = ticket_price_flames + fee;

    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);
	
	// 1. On charge l'état des compteurs
	let mut cache = load_cache();
	let mut cache_updated = false;
	// On calcule le vrai index réseau
    let real_wots_index = get_safe_wots_index(&cache, &master_seed_hex, wots_index);
	crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
	let spent_keys_snapshot = cache.known_spent_key_images.clone();
	let mut current_max_l1 = cache.last_scanned_height;
	let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();

    let mut selected_utxos = Vec::new();
    let mut input_blinding_factors = Vec::new(); // On stocke les BF secrets des inputs
    let mut collected_flames = 0u64;

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
		let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
		let micro_index = item["micro_index"].as_u64().unwrap_or(0);

		// 2. Mise à jour de nos compteurs locaux
		if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
		if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) {
            Ok(t) => t, Err(_) => continue,
        };

        for out in tx.outputs.iter() {
            let mut ki_hasher = sha2::Sha512::new();
            ki_hasher.update(out.kyber_capsule.as_bytes());
            ki_hasher.update(&sk_bytes);
            let expected_key_image = hex::encode(ki_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) { continue; }
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
                || out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) 
                || out.stealth_address == sender_kyber_public_hex 
            {
                val = out.aes_vault.parse::<u64>().unwrap_or(0);
                is_mine = true;
            } else if out.stealth_address.starts_with("pq_watt_") {
				// 3. Utilisation directe du cache L1/L2
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
                // Seules les vraies récompenses transmettent leur hauteur au nœud pour subir la règle de maturité.
                // Le reste (Transferts classiques L1/L2) passe à 0 et est dépensable instantanément !
                let actual_source_height = if is_system_reward { height } else { 0 };
                
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), actual_source_height));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
	
	// 4. Sauvegarde finale des compteurs sur le disque
	if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
	if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
	if cache_updated { save_cache(&cache); }

    if collected_flames < required_total { return Err(format!("❌ Fonds insuffisants. Besoin : {:.9} WATT", required_total as f64 / 1_000_000_000.0)); }

    let change_amount = collected_flames - required_total;
    let total_outputs_count = 1 + if change_amount > 0 { 1 } else { 0 };

    // LA MAGIE HOMOMORPHE EST LÀ
    let balanced_bfs = generate_balanced_blinding_factors(&input_blinding_factors, total_outputs_count);
    let mut bf_index = 0;

    let mut outputs = Vec::new();

    // OUTPUT 1 : Le Ticket (Va à la Réserve)
    let ticket_bf = &balanced_bfs[bf_index];
    let mut ticket_capsule = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut ticket_capsule);
    
    outputs.push(TransactionOutput {
        stealth_address: "LOTTERY_RESERVE".to_string(),
        kyber_capsule: hex::encode(ticket_capsule),
        aes_vault: ticket_price_flames.to_string(), // 👈 Utilise le nouveau paramètre
        lattice_commitment: LWECommitment::commit(ticket_price_flames, ticket_bf), // 👈 Utilise le nouveau paramètre
    });
    bf_index += 1;

    // OUTPUT 2 : La monnaie rendue
    if change_amount > 0 {
        let change_bf = &balanced_bfs[bf_index];
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
        
        let (kyber_capsule_2, my_shared_secret) = encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();

        let mut otp_2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_2);
        
        // On sauvegarde le BF du change pour plus tard
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
    // On utilise real_wots_index
    let wots_keys = crate::wots::WotsKeyPair::generate_deterministic(&seed_bytes, real_wots_index);
    let temp_tx = Transaction {
        tx_type: TransactionType::HTLCLottery { target_block, player_pubkey: sender_kyber_public_hex.clone() },
        inputs: vec![],
        outputs: outputs.clone(),
        fee,
        wots_signature: None,
        public_key: wots_keys.public_key.clone(),
    };
    let tx_hash = temp_tx.hash_data();

   // MODE PROD : 64 Vrais Leurres pour le ticket de Loto !
	let mut decoys = vec![wots_keys.public_key.clone()]; 
	let mut unique_set = std::collections::HashSet::new();
	unique_set.insert(wots_keys.public_key.clone());

	if let Ok(res_str) = node_call("GET", "/get_decoys/63", None).await {
		if let Ok(real_decoys) = serde_json::from_str::<Vec<String>>(&res_str) {
			for decoy in real_decoys {
				if !unique_set.contains(&decoy) && decoys.len() < 64 {
					unique_set.insert(decoy.clone());
					decoys.push(decoy);
				}
			}
		}
	}

	while decoys.len() < 64 { 
		let new_decoy = crate::wots::WotsKeyPair::generate().public_key;
		if !unique_set.contains(&new_decoy) {
			unique_set.insert(new_decoy.clone());
			decoys.push(new_decoy);
		}
	}
    
    use rand::seq::SliceRandom;
    decoys.shuffle(&mut rand::thread_rng());
    let real_index = decoys.iter().position(|r| r == &wots_keys.public_key).unwrap();

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        let mpc_sig = crate::merkle_ring::MpcRingSignature::sign(
            &wots_keys.secret_key, 
            &tx_hash, 
            &decoys, 
            real_index, 
            &utxo.1,
            &sk_bytes // ON FOURNIT LA CLÉ KYBER ICI !
        );
        final_inputs.push(TransactionInput { mpc_ring: mpc_sig, commitment: utxo.2.clone(), source_height: utxo.3 });
    }

    let mut tx_pq = Transaction {
        tx_type: TransactionType::HTLCLottery { target_block, player_pubkey: sender_kyber_public_hex.clone() }, 
        inputs: final_inputs, outputs, fee, wots_signature: None, public_key: wots_keys.public_key.clone()
    };
    tx_pq.wots_signature = Some(crate::wots::WotsKeyPair::sign(&wots_keys.secret_key, &wots_keys.public_seed, &tx_hash));

    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_json)).await?;

    // MISE À JOUR DU CACHE LOCAL INSTANTANÉE (Anti-double dépense locale)
    let mut instant_cache = load_cache();
    for input in &tx_pq.inputs {
        // On marque les billets comme "dépensés" localement
        instant_cache.known_spent_key_images.insert(input.mpc_ring.key_image.clone());
    }
    // On marque la clé publique WOTS+ comme "utilisée" localement
    instant_cache.known_used_wots_pubkeys.insert(tx_pq.public_key.clone());
    save_cache(&instant_cache);

    // Sauvegarde du nouvel index sur le disque !
    let next_index = real_wots_index + 1;
    crate::save_wots_index(next_index);

    // On renvoie la bonne valeur
    Ok(next_index)
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
    let tx_json = serde_json::to_string(&refund_tx).map_err(|e| e.to_string())?;
    let _ = node_call("POST", "/send_tx", Some(tx_json)).await?;
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
    // 💡 PAS DE KYBER/AES ICI ! Le Tribunal L1 doit pouvoir lire le montant exact.
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

    let tx_json = serde_json::to_string(&claim_tx).map_err(|e| e.to_string())?;
    node_call("POST", "/htlc/claim", Some(tx_json)).await?;
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
        let wots_path = path.join(format!("{}.wots", name));
		let cache_path = path.join(format!("{}.cache", name));
		let chain_path = path.join(format!("{}_chain.json", name));
		let swap_path = path.join(format!("{}_swap_secrets.json", name));
        
        if vault_path.exists() { let _ = std::fs::remove_file(vault_path); }
        if spends_path.exists() { let _ = std::fs::remove_file(spends_path); }
        if wots_path.exists() { let _ = std::fs::remove_file(wots_path); }
		if cache_path.exists() { let _ = std::fs::remove_file(cache_path); }
		if chain_path.exists() { let _ = std::fs::remove_file(chain_path); }
		if swap_path.exists() { let _ = std::fs::remove_file(swap_path); }
        
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


pub async fn send_btc_to_htlc(htlc_address: String, amount_btc: f64, raw_tx: Option<String>) -> Result<String, String> {
    let payload = serde_json::json!({
        "htlc_address": htlc_address,
        "amount_btc": amount_btc,
        "raw_tx": raw_tx.unwrap_or_default()
    });
    node_call("POST", "/btc/send/to_htlc", Some(serde_json::to_string(&payload).unwrap())).await
        .map(|_| "✅ BTC verrouillé dans le HTLC".to_string())
        .map_err(|e| format!("Erreur node : {}", e))
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
        total_in += utxo.value; // 💡 On lit la valeur AVANT de déplacer l'objet
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
    match node_call("POST", "/btc/broadcast", Some(serde_json::to_string(&payload).unwrap())).await {
        Ok(resp) => {
            let json: serde_json::Value = serde_json::from_str(&resp).unwrap_or_default();
            if json["success"].as_bool().unwrap_or(false) {
                Ok(format!("✅ BTC envoyés ! TXID : {}...", &json["txid"].as_str().unwrap_or("")[0..10]))
            } else {
                Err(format!("❌ Erreur Nœud : {}", json["error"].as_str().unwrap_or("Rejeté")))
            }
        },
        Err(e) => Err(e),
    }
}


pub async fn auto_claim_btc_swap(htlc_hash: String, _htlc_address: String) -> Result<String, String> {
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
    wots_index: u32,     
) -> Result<String, String> {
    
    // 1. Calcul des montants
    let amount_flames = (stake_amount * 1_000_000_000.0) as u64;
    let fee = 1000u64; // Frais de transaction sur le L1
    let required_total = amount_flames + fee;
	
	// ON TÉLÉCHARGE D'ABORD :
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);

    // ET ENSUITE ON CACHE :
    let mut cache = load_cache();
    let mut cache_updated = false;
	// On calcule le vrai index réseau
    let real_wots_index = get_safe_wots_index(&cache, &master_seed_hex, wots_index);
    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
    
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64; 
    let mut input_blinding_factors = Vec::new(); 

    // 3. Ramassage des UTXOs
    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
		let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
		let micro_index = item["micro_index"].as_u64().unwrap_or(0);

		// 💡 2. Mise à jour de nos compteurs locaux
		if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
		if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut ki_hasher = sha2::Sha512::new();
            ki_hasher.update(out.kyber_capsule.as_bytes());
            ki_hasher.update(&sk_bytes);
            let expected_key_image = hex::encode(ki_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) { continue; }
            
            // On ne prend que l'argent du L1 (Coinbase, Jackpot, ou WATT normaux)
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
                || out.stealth_address == format!("JACKPOT_{}", sender_kyber_public_hex) 
                || out.stealth_address == sender_kyber_public_hex 
            {
                val = out.aes_vault.parse::<u64>().unwrap_or(0); is_mine = true;
            } else if out.stealth_address.starts_with("pq_watt_") {
				// 💡 3. Utilisation directe du cache L1/L2
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
                // Seules les vraies récompenses transmettent leur hauteur au nœud pour subir la règle de maturité.
                // Le reste (Transferts classiques L1/L2) passe à 0 et est dépensable instantanément !
                let actual_source_height = if is_system_reward { height } else { 0 };
                
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), actual_source_height));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
	
	// Sauvegarde finale des compteurs sur le disque
	if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
	if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
	if cache_updated { save_cache(&cache); }

    if collected_flames < required_total { return Err("❌ Fonds insuffisants pour Staker sur le L1.".to_string()); }

    // 4. Équilibrage Lattice (Zero-Blinding Factor)
    let change_amount = collected_flames - required_total;
    
    // On calcule la somme exacte des secrets (BF) entrants
    let mut sum_in_bf = vec![0u64; crate::lattice::LATTICE_DIM];
    for bf in &input_blinding_factors {
        for i in 0..crate::lattice::LATTICE_DIM {
            sum_in_bf[i] = sum_in_bf[i].wrapping_add(bf[i]);
        }
    }

    let mut outputs = Vec::new();

    // 5. CRÉATION DU VERROU DE STAKING (Public, BF nul)
    let stake_bf = vec![0u64; crate::lattice::LATTICE_DIM]; // 💡 Le BF est publiquement Zéro !
    
    outputs.push(TransactionOutput {
        // On lie la caution publiquement à notre clé pour que notre Wallet la retrouve
        stealth_address: format!("L2_STAKE_{}", sender_kyber_public_hex),
        kyber_capsule: "L2_STAKE_LOCK".to_string(),
        aes_vault: amount_flames.to_string(), // 💡 Montant en texte CLAIR !
        lattice_commitment: LWECommitment::commit(amount_flames, &stake_bf), 
    });

    // 6. Rendu de Monnaie
    if change_amount > 0 {
        let change_bf = &sum_in_bf; // 💡 Le change de monnaie absorbe TOUT le secret d'entrée !
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&sender_kyber_public_hex).unwrap();
        let (kyber_capsule_change, my_shared_secret) = pqc_kyber::encapsulate(&my_pk_bytes, &mut rand::thread_rng()).unwrap();
        let mut otp2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp2);
        
        // ... (Suite inchangée pour le chiffrement AES du change)
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

    // 7. Signature (WOTS+ et Anneau de Merkle)
    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);
    
    // On génère de manière déterministe
    let wots_keys = crate::wots::WotsKeyPair::generate_deterministic(&seed_bytes, real_wots_index);
    
    let temp_tx = Transaction { 
        tx_type: TransactionType::L2Stake { 
            l2_name: l2_name.clone(), 
            sequencer_pubkey: sequencer_pubkey_hex.clone() 
        }, 
        inputs: vec![], 
        outputs: outputs.clone(), 
        fee, 
        wots_signature: None, 
        public_key: wots_keys.public_key.clone() 
    };
    let tx_hash = temp_tx.hash_data();

    // MODE PROD : 64 Vrais Leurres
	let mut decoys = vec![wots_keys.public_key.clone()]; 
	let mut unique_set = std::collections::HashSet::new();
	unique_set.insert(wots_keys.public_key.clone());

	if let Ok(res_str) = node_call("GET", "/get_decoys/63", None).await {
		if let Ok(real_decoys) = serde_json::from_str::<Vec<String>>(&res_str) {
			for decoy in real_decoys {
				if !unique_set.contains(&decoy) && decoys.len() < 64 {
					unique_set.insert(decoy.clone());
					decoys.push(decoy);
				}
			}
		}
	}

	while decoys.len() < 64 { 
		let new_decoy = crate::wots::WotsKeyPair::generate().public_key;
		if !unique_set.contains(&new_decoy) {
			unique_set.insert(new_decoy.clone());
			decoys.push(new_decoy);
		}
	}
    
    use rand::seq::SliceRandom;
    decoys.shuffle(&mut rand::thread_rng());
    let real_index = decoys.iter().position(|r| r == &wots_keys.public_key).unwrap();

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        let mpc_sig = crate::merkle_ring::MpcRingSignature::sign(&wots_keys.secret_key, &tx_hash, &decoys, real_index, &utxo.1, &sk_bytes);
        final_inputs.push(TransactionInput { mpc_ring: mpc_sig, commitment: utxo.2.clone(), source_height: utxo.3 });
    }

    let mut tx_pq = Transaction { 
        tx_type: TransactionType::L2Stake { 
            l2_name: l2_name.clone(), 
            sequencer_pubkey: sequencer_pubkey_hex.clone() 
        }, 
        inputs: final_inputs, 
        outputs, 
        fee, 
        wots_signature: None, 
        public_key: wots_keys.public_key.clone() 
    };
    tx_pq.wots_signature = Some(crate::wots::WotsKeyPair::sign(&wots_keys.secret_key, &wots_keys.public_seed, &tx_hash));

    // 8. Envoi au Node
    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_json)).await?;

    // MISE À JOUR DU CACHE LOCAL INSTANTANÉE (Anti-double dépense locale)
    let mut instant_cache = load_cache();
    for input in &tx_pq.inputs {
        // On marque les billets comme "dépensés" localement
        instant_cache.known_spent_key_images.insert(input.mpc_ring.key_image.clone());
    }
    // On marque la clé publique WOTS+ comme "utilisée" localement
    instant_cache.known_used_wots_pubkeys.insert(tx_pq.public_key.clone());
    save_cache(&instant_cache);

    // Sauvegarde du nouvel index sur le disque !
    let next_index = real_wots_index + 1;
    crate::save_wots_index(next_index);

    Ok(format!("🎉 Caution verrouillée avec succès ! La L2 '{}' est prête à être ancrée.", l2_name))
}


pub async fn unstake_l2(
    l2_name: String,
    sender_kyber_secret_hex: String,
    sender_kyber_public_hex: String,
    master_seed_hex: String, 
    wots_index: u32,        
) -> Result<String, String> {
    
    let fee = 1000u64; // Frais L1
	
	// ON TÉLÉCHARGE D'ABORD :
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;

    // ET ENSUITE ON CACHE :
    let mut cache = load_cache();
    let mut cache_updated = false;
	// On calcule le vrai index réseau
    let real_wots_index = get_safe_wots_index(&cache, &master_seed_hex, wots_index);
    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
    
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    
    let mut selected_utxo = None;
    let mut stake_amount = 0u64;
    let mut old_bf = vec![0u64; crate::lattice::LATTICE_DIM];

    // 2. Recherche du coffre "L2_STAKE_" qui t'appartient
    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
		let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
		let micro_index = item["micro_index"].as_u64().unwrap_or(0);

		// 2. Mise à jour de nos compteurs locaux
		if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
		if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut ki_hasher = sha2::Sha512::new();
            ki_hasher.update(out.kyber_capsule.as_bytes());
            ki_hasher.update(&sk_bytes);
            let expected_key_image = hex::encode(ki_hasher.finalize());

            if spent_keys_snapshot.contains(&expected_key_image) { continue; }
            
            // ON CHERCHE NOTRE CAUTION PUBLIQUE
            if out.stealth_address == format!("L2_STAKE_{}", sender_kyber_public_hex) {
                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                    stake_amount = amt;
                    old_bf = vec![0u64; crate::lattice::LATTICE_DIM]; // 💡 Le BF public est connu (zéro) !
                    selected_utxo = Some((out.kyber_capsule.clone(), out.lattice_commitment.clone(), height));
                    break;
                }
            }

			// Utilisation directe du cache L1/L2
			if let Some(payload_str) = try_decrypt_output(out, &sk_bytes, height, is_l2, micro_index, &mut cache, &mut cache_updated) {
				let parts: Vec<&str> = payload_str.split('|').collect();
				if parts.len() >= 3 {
					if let Ok(amt) = parts[0].parse::<u64>() {
						stake_amount = amt;
						if let Ok(parsed_bf) = serde_json::from_str::<Vec<u64>>(parts[2]) {
							old_bf = parsed_bf;
						}
						selected_utxo = Some((out.kyber_capsule.clone(), out.lattice_commitment.clone(), height));
						break;
					}
				}
			}
        }
        if selected_utxo.is_some() { break; }
    }
	
	// 4. Sauvegarde finale des compteurs sur le disque
	if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
	if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
	if cache_updated { save_cache(&cache); }

    let (kyber_capsule, commitment, source_height) = selected_utxo.ok_or(format!("❌ Aucune caution trouvée pour la L2 '{}'.", l2_name))?;
    if stake_amount <= fee { return Err("❌ Caution trop faible pour payer les frais de retrait.".to_string()); }

    let return_amount = stake_amount - fee;

    // 3. Création du nouveau billet (On ramène les fonds dans un pq_watt_ normal)
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
        stealth_address: format!("pq_watt_{}", hex::encode(&otp[0..8])), // 🔙 Retour à la normale !
        kyber_capsule: hex::encode(&new_capsule),
        aes_vault: hex::encode(final_vault),
        lattice_commitment: LWECommitment::commit(return_amount, out_bf),
    };

    // 4. Signatures et Leurres (Merkle Ring)
    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);
    
    // On génère de manière déterministe
    let wots_keys = crate::wots::WotsKeyPair::generate_deterministic(&seed_bytes, real_wots_index);
    
    let temp_tx = Transaction {
        tx_type: TransactionType::L2Unstake { l2_name: l2_name.clone() },
        inputs: vec![],
        outputs: vec![output.clone()],
        fee,
        wots_signature: None,
        public_key: wots_keys.public_key.clone(),
    };
    let tx_hash = temp_tx.hash_data();

    // MODE PROD : 64 Vrais Leurres
	let mut decoys = vec![wots_keys.public_key.clone()]; 
	let mut unique_set = std::collections::HashSet::new();
	unique_set.insert(wots_keys.public_key.clone());

	if let Ok(res_str) = node_call("GET", "/get_decoys/63", None).await {
		if let Ok(real_decoys) = serde_json::from_str::<Vec<String>>(&res_str) {
			for decoy in real_decoys {
				if !unique_set.contains(&decoy) && decoys.len() < 64 {
					unique_set.insert(decoy.clone());
					decoys.push(decoy);
				}
			}
		}
	}

	while decoys.len() < 64 { 
		let new_decoy = crate::wots::WotsKeyPair::generate().public_key;
		if !unique_set.contains(&new_decoy) {
			unique_set.insert(new_decoy.clone());
			decoys.push(new_decoy);
		}
	}
    
    use rand::seq::SliceRandom;
    decoys.shuffle(&mut rand::thread_rng());
    let real_index = decoys.iter().position(|r| r == &wots_keys.public_key).unwrap();

    let mpc_sig = crate::merkle_ring::MpcRingSignature::sign(&wots_keys.secret_key, &tx_hash, &decoys, real_index, &kyber_capsule, &sk_bytes);
    
    let mut tx_pq = Transaction {
        tx_type: TransactionType::L2Unstake { l2_name: l2_name.clone() },
        inputs: vec![TransactionInput { mpc_ring: mpc_sig, commitment, source_height }],
        outputs: vec![output],
        fee,
        wots_signature: None,
        public_key: wots_keys.public_key.clone(),
    };
    tx_pq.wots_signature = Some(crate::wots::WotsKeyPair::sign(&wots_keys.secret_key, &wots_keys.public_seed, &tx_hash));

    // 5. Envoi au Nœud L1
    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_json)).await?;

    // MISE À JOUR DU CACHE LOCAL INSTANTANÉE (Anti-double dépense locale)
    let mut instant_cache = load_cache();
    for input in &tx_pq.inputs {
        // On marque les billets comme "dépensés" localement
        instant_cache.known_spent_key_images.insert(input.mpc_ring.key_image.clone());
    }
    // On marque la clé publique WOTS+ comme "utilisée" localement
    instant_cache.known_used_wots_pubkeys.insert(tx_pq.public_key.clone());
    save_cache(&instant_cache);

    // Sauvegarde du nouvel index sur le disque !
    let next_index = real_wots_index + 1;
    crate::save_wots_index(next_index);

    Ok(format!("🔓 Caution récupérée avec succès ! La L2 '{}' a été désactivée.", l2_name))
}

// 3. Ajoute cette nouvelle fonction pour lire l'API du nœud :
pub async fn get_l2_status(l2_name: &str) -> Result<serde_json::Value, String> {
    let res_str = node_call("GET", &format!("/l2/status/{}", l2_name), None).await?;
    serde_json::from_str(&res_str).map_err(|e| e.to_string())
}

// ===================================================================
// OPTIMISATION : CACHE RÉSEAU & CRYPTOGRAPHIQUE
// ===================================================================
pub fn get_local_chain_path() -> Result<PathBuf, String> {
    let mut path = crate::get_base_dir().ok_or("Impossible de trouver le dossier système".to_string())?;
    path.push("wattcoin_wallet");
    let name = CURRENT_WALLET.lock().unwrap().clone();
    path.push(format!("{}_chain.json", name)); // Base de données locale par wallet
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
    // On vérifie que c'est bien LE MÊME wallet en plus de la hauteur
    if cache_ram.0 == current_wallet_name && cache_ram.1 == current_height && cache_ram.2 == current_l2_blocks && !cache_ram.3.is_empty() {
        return Ok(cache_ram.3.clone()); 
    }
    
    // 1. Charger l'historique local depuis le disque
    let local_chain_path = get_local_chain_path()?;
    let mut local_txs: Vec<serde_json::Value> = if local_chain_path.exists() {
        let data = std::fs::read_to_string(&local_chain_path).unwrap_or_else(|_| "[]".to_string());
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        Vec::new()
    };

    // 2. Déterminer où on s'est arrêté
    let mut last_l1 = 0;
    let mut last_l2 = 0;
    for tx in &local_txs {
        let is_l2 = tx["is_l2"].as_bool().unwrap_or(false);
        if !is_l2 {
            let h = tx["height"].as_u64().unwrap_or(0);
            if h > last_l1 { last_l1 = h; }
        } else {
            let idx = tx["micro_index"].as_u64().unwrap_or(0);
            if idx > last_l2 { last_l2 = idx; }
        }
    }

    // 3. Si on est en retard, on demande le delta au nœud
    if last_l1 < current_height || last_l2 < current_l2_blocks {
        crate::set_status(&format!("🔄 Tél. différentiel (L1: {}->{}, L2: {}->{})...", last_l1, current_height, last_l2, current_l2_blocks));
        
        let endpoint = format!("/sync_blocks?last_l1={}&last_l2={}", last_l1, last_l2);
        let new_txs_str = node_call("GET", &endpoint, None).await?;
        
        if let Ok(new_txs) = serde_json::from_str::<Vec<serde_json::Value>>(&new_txs_str) {
            if !new_txs.is_empty() {
                local_txs.extend(new_txs);
                // Sauvegarder la nouvelle base fusionnée sur le disque du mobile
                let json_to_save = serde_json::to_string(&local_txs).unwrap_or_default();
                let _ = std::fs::write(&local_chain_path, json_to_save);
            }
        }
    }

    // 4. Mettre en RAM et renvoyer
    let final_json = serde_json::to_string(&local_txs).unwrap_or_default();
    *cache_ram = (current_wallet_name, current_height, current_l2_blocks, final_json.clone());
    Ok(final_json)
}

pub fn get_cache_path() -> Result<PathBuf, String> {
    let mut path = crate::get_base_dir().ok_or("Impossible de trouver le dossier système".to_string())?;
    path.push("wattcoin_wallet");
    let name = CURRENT_WALLET.lock().unwrap().clone();
    path.push(format!("{}.cache", name));
    Ok(path)
}

pub fn load_cache() -> WalletCache {
    if let Ok(path) = get_cache_path() {
        if let Ok(data) = std::fs::read_to_string(path) {
            if let Ok(cache) = serde_json::from_str(&data) { return cache; }
        }
    }
    WalletCache::default()
}

pub fn save_cache(cache: &WalletCache) {
    if let Ok(path) = get_cache_path() {
        if let Ok(data) = serde_json::to_string(cache) { let _ = std::fs::write(path, data); }
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
    _sender_kyber_public_hex: String,
    master_seed_hex: String,
    wots_index: u32,
) -> Result<String, String> {
    let amount_flames = (amount_watt * 1_000_000_000.0) as u64;
    let fee = 1000u64; // Frais L1
    let required_total = amount_flames + fee;

    // 1. TÉLÉCHARGEMENT & SYNCHRO CACHE
    let res_str = get_all_transactions_cached().await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str).map_err(|_| "Erreur JSON".to_string())?;
    let current_height = get_current_block_height().await.unwrap_or(0);

    let mut cache = load_cache();
    let mut cache_updated = false;
    let real_wots_index = get_safe_wots_index(&cache, &master_seed_hex, wots_index);
    crate::update_spent_cache_fast(&enriched, &mut cache, &mut cache_updated);
    let spent_keys_snapshot = cache.known_spent_key_images.clone();
    
    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    let mut selected_utxos = Vec::new();
    let mut collected_flames = 0u64; 
    let mut input_blinding_factors = Vec::new(); 

    // 2. RAMASSAGE DES UTXOs (Identique à tes autres fonctions)
    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let is_l2 = item["is_l2"].as_bool().unwrap_or(false);
        let micro_index = item["micro_index"].as_u64().unwrap_or(0);

        if !is_l2 && height > current_max_l1 { current_max_l1 = height; }
        if is_l2 && micro_index > current_max_l2 { current_max_l2 = micro_index; }
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) { Ok(t) => t, Err(_) => continue, };

        for out in tx.outputs.iter() {
            let mut ki_hasher = sha2::Sha512::new();
            ki_hasher.update(out.kyber_capsule.as_bytes());
            ki_hasher.update(&sk_bytes);
            if spent_keys_snapshot.contains(&hex::encode(ki_hasher.finalize())) { continue; }
            
            let is_valid_source = out.stealth_address.starts_with("pq_watt_") || out.stealth_address.starts_with("COINBASE_") || out.stealth_address.starts_with("JACKPOT_");
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
            } else if out.stealth_address == format!("COINBASE_{}", _sender_kyber_public_hex) || out.stealth_address == format!("JACKPOT_{}", _sender_kyber_public_hex) {
                val = out.aes_vault.parse::<u64>().unwrap_or(0); is_mine = true;
            }

            if is_mine && val > 0 {
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), if is_system_reward { height } else { 0 }));
                input_blinding_factors.push(my_bf);
                collected_flames += val;
                if collected_flames >= required_total { break; }
            }
        }
        if collected_flames >= required_total { break; }
    }
    
    if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
    if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
    if cache_updated { save_cache(&cache); }

    if collected_flames < required_total { return Err("❌ Fonds insuffisants.".to_string()); }

    // 3. CONSTRUCTION DE LA TRANSACTION DE BRIDGE (Zero-Blinding Factor)
    let change_amount = collected_flames - required_total;
    
    // On calcule la somme exacte des secrets (BF) entrants
    let mut sum_in_bf = vec![0u64; crate::lattice::LATTICE_DIM];
    for bf in &input_blinding_factors {
        for i in 0..crate::lattice::LATTICE_DIM {
            sum_in_bf[i] = sum_in_bf[i].wrapping_add(bf[i]);
        }
    }
    
    let mut outputs = Vec::new();

    // OUTPUT 1 : L'Adresse morte du Bridge L2 (BF NUL et Montant en clair !)
    let bridge_bf = vec![0u64; crate::lattice::LATTICE_DIM]; // BF public à zéro
    
    outputs.push(TransactionOutput {
        stealth_address: format!("BRIDGE_L2_{}", l2_target_name.to_uppercase()),
        kyber_capsule: "L2_BRIDGE_LOCK".to_string(),
        aes_vault: amount_flames.to_string(), // LE MONTANT EN CLAIR
        lattice_commitment: LWECommitment::commit(amount_flames, &bridge_bf), 
    });

    // OUTPUT 2 : Rendu de Monnaie
    if change_amount > 0 {
        use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
        let my_pk_bytes = URL_SAFE_NO_PAD.decode(&_sender_kyber_public_hex).unwrap();
        
        let change_bf = &sum_in_bf; // 💡 Le change de monnaie absorbe TOUT le secret d'entrée !
        
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

    // 4. SIGNATURES & ENVOI
    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);
    let wots_keys = crate::wots::WotsKeyPair::generate_deterministic(&seed_bytes, real_wots_index);
    
    // MAGIE UX : Si l'utilisateur bridge vers sa propre adresse, on formate automatiquement !
    let mut final_l2_receiver = receiver_pubkey.clone();
    if final_l2_receiver == _sender_kyber_public_hex {
        // Le Wallet assemble "Kyber|WOTS+" silencieusement en arrière-plan
        final_l2_receiver = format!("{}|{}", _sender_kyber_public_hex, wots_keys.public_key);
    }

    let temp_tx = Transaction { 
        tx_type: TransactionType::L2BridgeLock { 
            l2_target_name: l2_target_name.clone(), 
            l2_receiver_pubkey: final_l2_receiver.clone() // 👈 On utilise l'adresse formatée
        }, 
        inputs: vec![], outputs: outputs.clone(), fee, wots_signature: None, public_key: wots_keys.public_key.clone() 
    };
    let tx_hash = temp_tx.hash_data();

    // 64 Leurres...
    let mut decoys = vec![wots_keys.public_key.clone()]; 
    let mut unique_set = std::collections::HashSet::new();
    unique_set.insert(wots_keys.public_key.clone());
    if let Ok(res_str) = node_call("GET", "/get_decoys/63", None).await {
        if let Ok(real_decoys) = serde_json::from_str::<Vec<String>>(&res_str) {
            for decoy in real_decoys {
                if !unique_set.contains(&decoy) && decoys.len() < 64 { unique_set.insert(decoy.clone()); decoys.push(decoy); }
            }
        }
    }
    while decoys.len() < 64 { 
        let new_decoy = crate::wots::WotsKeyPair::generate().public_key;
        if !unique_set.contains(&new_decoy) { unique_set.insert(new_decoy.clone()); decoys.push(new_decoy); }
    }
    
    use rand::seq::SliceRandom;
    decoys.shuffle(&mut rand::thread_rng());
    let real_index = decoys.iter().position(|r| r == &wots_keys.public_key).unwrap();

    let mut final_inputs = Vec::new();
    for utxo in &selected_utxos {
        let mpc_sig = crate::merkle_ring::MpcRingSignature::sign(&wots_keys.secret_key, &tx_hash, &decoys, real_index, &utxo.1, &sk_bytes);
        final_inputs.push(TransactionInput { mpc_ring: mpc_sig, commitment: utxo.2.clone(), source_height: utxo.3 });
    }

    let mut tx_pq = Transaction { 
        tx_type: TransactionType::L2BridgeLock { 
            l2_target_name: l2_target_name.clone(), 
            l2_receiver_pubkey: final_l2_receiver // On utilise l'adresse formatée ici aussi
        }, 
        inputs: final_inputs, outputs, fee, wots_signature: None, public_key: wots_keys.public_key.clone() 
    };
    tx_pq.wots_signature = Some(crate::wots::WotsKeyPair::sign(&wots_keys.secret_key, &wots_keys.public_seed, &tx_hash));

    let tx_json = serde_json::to_string(&tx_pq).unwrap();
    node_call("POST", "/send_tx", Some(tx_json)).await?;

    let mut instant_cache = load_cache();
    for input in &tx_pq.inputs { instant_cache.known_spent_key_images.insert(input.mpc_ring.key_image.clone()); }
    instant_cache.known_used_wots_pubkeys.insert(tx_pq.public_key.clone());
    save_cache(&instant_cache);
    crate::save_wots_index(real_wots_index + 1);

    Ok(format!("✅ {} WATT verrouillés avec succès pour le réseau L2 {} !", amount_watt, l2_target_name))
}

pub async fn get_history_offline(keys: WalletKeys) -> Result<Vec<HistoryItem>, String> {
    use chrono::{DateTime, Utc, Local};
    use std::collections::HashMap;

    let local_chain_path = get_local_chain_path()?;
    let res_str = std::fs::read_to_string(&local_chain_path).unwrap_or_else(|_| "[]".to_string());
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str)
        .map_err(|_| "Erreur JSON history offline".to_string())?;

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

    let mut current_max_l1 = cache.last_scanned_height;
    let mut current_max_l2 = cache.last_scanned_micro_index;
    let mut grouped_history: HashMap<String, HistoryItem> = HashMap::new();

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
            let mut ki_hasher = sha2::Sha512::new();
            ki_hasher.update(out.kyber_capsule.as_bytes());
            ki_hasher.update(&sk_bytes);
            let expected_key_image = hex::encode(ki_hasher.finalize());

            let is_spent = spent_keys_snapshot.contains(&expected_key_image);
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
                let group_key = format!("{}_{}_{}", current_layer, display_id, status_full);

                let entry = grouped_history.entry(group_key).or_insert(HistoryItem {
                    id: display_id,
                    tx_type: "receive".to_string(),
                    amount: 0.0,
                    coin: "WATT".to_string(),
                    date: date_str,
                    status: status_full,
                    layer: current_layer,
                    raw_timestamp: timestamp,
                });
                entry.amount += amt_to_add; 
            }
        }
    }
    
    if current_max_l1 > cache.last_scanned_height { cache.last_scanned_height = current_max_l1; cache_updated = true; }
    if current_max_l2 > cache.last_scanned_micro_index { cache.last_scanned_micro_index = current_max_l2; cache_updated = true; }
    if cache_updated { save_cache(&cache); }

    let mut final_history: Vec<HistoryItem> = grouped_history.into_values().collect();
    final_history.sort_by(|a, b| b.raw_timestamp.cmp(&a.raw_timestamp));

    Ok(final_history)
}

pub async fn register_wns_domain(
    domain: String, 
    record_data: String, 
    fee: u64, 
    keys: WalletKeys
) -> Result<String, String> {
    use wattcoin_name_service::transaction::{L2Transaction, WnsAction};
    
    let resolver = if LOCAL_DEV_MODE { "http://127.0.0.1:8200" } else { "http://80.78.26.243/wns" };
    
    // 1. On interroge le Séquenceur WNS pour connaître notre état réel
    crate::set_status("🔍 Synchronisation avec l'état du L2 WNS...");
    let balance_url = format!("{}/balance/{}", resolver, keys.watt_address);
    let res = HTTP_CLIENT.get(&balance_url).send().await.map_err(|_| "Séquenceur WNS injoignable")?;
    let json: serde_json::Value = res.json().await.map_err(|_| "Erreur JSON WNS")?;
    
    let balance = json["balance"].as_u64().unwrap_or(0);
    let auth_key = json["authorized_wots_key"].as_str().unwrap_or("");

    // 2. On vérifie si on a assez de fonds
    if balance < fee {
        return Err(format!("Fonds insuffisants sur le WNS (Solde: {}). Utilisez l'onglet Bridge pour recharger votre compte WNS.", balance));
    }

    if auth_key.is_empty() {
        return Err("Votre compte WNS n'est pas initialisé. Faites un premier Bridge vers le WNS.".to_string());
    }

    crate::set_status("🔐 Recherche de la clé WOTS+ correspondante...");

    // 3. ANTI-DESYNC : On cherche mathématiquement quelle est la bonne clé secrète !
    let mut seed_bytes = [0u8; 32];
    let decoded_seed = hex::decode(&keys.master_seed_hex).unwrap_or_default();
    seed_bytes.copy_from_slice(&decoded_seed[0..32]);
    
    let mut current_key_pair = None;
    let mut current_index = 0;
    
    // On scanne nos propres clés générées jusqu'à trouver celle que le L2 attend
    for i in 0..1000 {
        let kp = crate::wots::WotsKeyPair::generate_deterministic(&seed_bytes, i);
        if kp.public_key == auth_key {
            current_key_pair = Some(kp);
            current_index = i;
            break;
        }
    }

    let key_n = current_key_pair.ok_or("Clé WOTS+ introuvable en local. Avez-vous restauré avec la bonne seed ?")?;
    
    // La clé vers laquelle on fait rouler le compte
    let key_n_plus_1 = crate::wots::WotsKeyPair::generate_deterministic(&seed_bytes, current_index + 1);

    crate::set_status("🏷️ Signature et achat du domaine...");

    let mut l2_tx = L2Transaction {
        account_address: keys.watt_address.clone(), 
        sender_pubkey: key_n.public_key.clone(),      
        next_pubkey: key_n_plus_1.public_key.clone(), // On fait rouler d'un seul cran !
        action: WnsAction::Register,
        domain_name: domain.clone(),
        record_data,
        amount: 0,
        fee,
        signature: String::new(),
    };

    let hash = l2_tx.hash_data();
    let wots_sig = crate::wots::WotsKeyPair::sign(&key_n.secret_key, &key_n.public_seed, &hash);
    l2_tx.signature = serde_json::to_string(&wots_sig).unwrap();

    let url = format!("{}/send", resolver);
    let res = HTTP_CLIENT.post(&url)
        .header("Content-Type", "application/json")
        .body(serde_json::to_string(&l2_tx).unwrap())
        .send().await.map_err(|e| format!("Erreur réseau WNS : {}", e))?;

    if res.status().is_success() {
        // On met à jour notre cache local pour dire que N et N+1 sont utilisées
        let mut instant_cache = load_cache();
        instant_cache.known_used_wots_pubkeys.insert(key_n.public_key);
        instant_cache.known_used_wots_pubkeys.insert(key_n_plus_1.public_key);
        save_cache(&instant_cache);
        
        // On sauvegarde l'index le plus élevé
        crate::save_wots_index(current_index + 2);
        
        Ok(format!("✅ Réservation réussie ! '{}' vous appartient.", domain))
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
    sync_wns_directory().await;

    // 3. On revérifie dans la RAM mise à jour
    let cache = WNS_CACHE.lock().await;
    if let Some((record_data, _owner)) = cache.get(domain) {
        Ok(record_data.clone())
    } else {
        Err(format!("Le domaine '{}' n'existe pas.", domain))
    }
}


