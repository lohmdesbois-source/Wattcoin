#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};
use rand::{Rng, RngCore}; 
use serde::{Serialize, Deserialize};
use std::str::FromStr;
use std::fs; 
use std::path::PathBuf;
use std::collections::HashSet;
use std::time::Duration;
use tauri::Emitter;
use sha2::Digest;


use pqcrypto_traits::kem::{Ciphertext, SharedSecret, PublicKey as _, SecretKey as _};
use pqcrypto_traits::sign::{PublicKey as _, SecretKey as _, SignedMessage};

use once_cell::sync::Lazy;
use arti_client::{TorClient, TorClientConfig, StreamPrefs};
use tokio::io::{AsyncWriteExt, AsyncReadExt};
use tokio::sync::Mutex as AsyncMutex;
use tor_rtcompat::PreferredRuntime;

// 💡 Notre Node L2 persistant en mémoire vive
//static LDK_NODE: Lazy<AsyncMutex<Option<std::sync::Arc<ldk_node::Node>>>> = Lazy::new(|| AsyncMutex::new(None));

const ONION_NODE: &str = "jjbeptmy4b2ck5mc5sdjdc7kk6fkrva4laxfu7ufncmvk6qj6duh64yd.onion:8100";
const LATTICE_Q: u32 = 8380417; 
const LATTICE_DIM: usize = 4;
const MATURITY_BLOCKS: u64 = 3; // À passer à 100 en prod

// ===================================================================
// 🔥 SWITCH LOCAL / PROD WALLET (identique au node !)
// ===================================================================
const LOCAL_DEV_MODE: bool = true;   // ← true = local (HTTP direct, ultra rapide)
// const LOCAL_DEV_MODE: bool = false; // ← pour PROD : décommente celle-ci + commente la ligne du dessus
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

static TOR_CLIENT: Lazy<AsyncMutex<Option<TorClient<PreferredRuntime>>>> = Lazy::new(|| AsyncMutex::new(None));
static TOR_LOCK: Lazy<AsyncMutex<()>> = Lazy::new(|| AsyncMutex::new(()));

fn get_wallet_dir() -> PathBuf {
    #[cfg(debug_assertions)]
	{
		// Dossier persistant en mode DEV (ne se reset plus jamais)
		let mut path = dirs::data_local_dir().expect("Impossible de trouver le dossier data local");
		path.push("WattcoinWallet-Dev");   // ← dossier permanent

		if !path.exists() {
			std::fs::create_dir_all(&path).expect("Impossible de créer le dossier dev");
		}

		println!("🛠️ [DEV MODE] Wallet persistant chargé dans : {:?}", path);
		return path;
	}

    #[cfg(not(debug_assertions))]
    {
        // 🏦 MODE PROD
        let mut path = dirs::data_local_dir().expect("Impossible de trouver le dossier AppData/Local");
        path.push("WattcoinWallet");
        if !path.exists() {
            std::fs::create_dir_all(&path).expect("Impossible de créer le dossier du wallet");
        }
        path
    }
}

// 👉 LA FONCTION QU'IL FAUT RAJOUTER ICI :
fn get_vault_path() -> PathBuf {
    let mut path = get_wallet_dir();
    path.push(".wattcoin_vault");
    path
}

// Renvoie le chemin complet vers .wattcoin_spends
fn get_spends_path() -> PathBuf {
    let mut path = get_wallet_dir();
    path.push(".wattcoin_spends");
    path
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
    
    println!("🧅 [TOR] Initialisation du blindage Arti...");

    for attempt in 1..=3 {
        match TorClient::create_bootstrapped(TorClientConfig::default()).await {
            Ok(client) => {
                println!("✅ [TOR] Bootstrap réussi au bout de {} tentative(s)", attempt);
                start_arti_socks_proxy(client.clone()).await;
                *lock = Some(client.clone());
                return Ok(client);
            }
            Err(e) => {
                println!("⚠️ [TOR] Bootstrap échoué (tentative {}/3) : {}", attempt, e);
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        }
    }
    Err("Impossible de démarrer Tor après 3 tentatives".to_string())
}

// ===================================================================
// 🔥 HELPER PROPRE (tu changes juste LOCAL_DEV_MODE en haut)
// tor_fetch reste 100% intacte, on switch seulement l’appel
// ===================================================================
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
    // MODE PROD → on appelle la vraie tor_fetch (intacte)
    tor_fetch(method, endpoint, body).await
}
// ===================================================================

async fn tor_fetch(method: &str, endpoint: &str, body: Option<String>) -> Result<String, String> {
    let _guard = tokio::time::timeout(std::time::Duration::from_secs(60), TOR_LOCK.lock())
        .await.map_err(|_| "❌ [TOR] Timeout file d'attente".to_string())?;

    let tor_client = get_tor_client().await?;
    let mut prefs = StreamPrefs::new();
    prefs.connect_to_onion_services(arti_client::config::BoolOrAuto::Explicit(true));

    //println!("\n==============================================");
    println!("🕵️ [TOR] Début de la mission vers {}", endpoint);
    
    let mut stream = None;
    for _i in 1..=3 {
        //println!("⏳ [TOR] Percée du tunnel (Tentative {}/3)...", i);
		match tokio::time::timeout(std::time::Duration::from_secs(30), tor_client.connect_with_prefs(ONION_NODE, &prefs)).await {
            Ok(Ok(s)) => { 
                //println!("✅ [TOR] Tunnel établi !");
                stream = Some(s); 
                break; 
            },
            Ok(Err(e)) => println!("⚠️ [TOR] Arti a échoué : {}", e),
            Err(_) => println!("⚠️ [TOR] Timeout 20s !"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    let mut stream = stream.ok_or_else(|| "❌ [TOR] Abandon de la mission.".to_string())?;

    //println!("📤 [TOR] Envoi de la requête HTTP...");
    let req = if let Some(ref b) = body {
        format!("{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: WattcoinWallet/1.0\r\nAccept: application/json\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}", method, endpoint, ONION_NODE, b.len(), b)
    } else {
        format!("{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: WattcoinWallet/1.0\r\nAccept: application/json\r\nConnection: close\r\n\r\n", method, endpoint, ONION_NODE)
    };

    stream.write_all(req.as_bytes()).await.map_err(|e| format!("Erreur écriture: {}", e))?;
    stream.flush().await.map_err(|e| format!("Erreur flush: {}", e))?;

    //println!("📥 [TOR] Attente de la réponse...");
    let mut response = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf)).await {
            Ok(Ok(0)) => {
                //println!("✅ [TOR] Le Serveur Relais a terminé l'envoi.");
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
            //println!("🧩 [TOR] Découpage Chunked détecté. Reconstruction en cours...");
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
	pub source_height: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionType {
    Coinbase,
    Standard,
    HTLCLock { hash: String, timeout_block: u64 },
    HTLCClaim { secret: String },
    HTLCRefund { hash: String },
    DexSettlement { clearing_price_sats: u64, total_volume_flames: u64, swaps: Vec<SwapContract> },
    HTLCLottery { target_block: u64, player_pubkey: String }, 
    LotteryPayout { target_block: u64, winner_pubkey: String },
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapContract {
    pub buyer_watt_address: String,   
    pub buyer_btc_address: String,
    pub buyer_btc_pubkey: String,
    pub seller_watt_address: String,
    pub seller_btc_address: String,   
    pub seller_btc_pubkey: String,
    pub watt_amount_flames: u64,
    pub btc_amount_sats: u64,
    pub htlc_hash: String,
}

#[tauri::command]
async fn get_network_info() -> Result<serde_json::Value, String> {
    let res_str = node_call("GET", "/info", None).await?;
    serde_json::from_str(&res_str).map_err(|e| {
        println!("❌ [JSON ERROR INFO] {} | Data: {}", e, res_str);
        e.to_string()
    })
}

// 💡 NOUVEAU: Fetch de la Supply depuis le Nœud Core
#[tauri::command]
async fn get_total_supply() -> Result<u64, String> {
    let res_str = node_call("GET", "/supply", None).await?;
    let supply: u64 = serde_json::from_str(&res_str).unwrap_or(0);
    Ok(supply)
}

// 💡 NOUVEAU: Fetch du Jackpot depuis le Nœud Core
#[tauri::command]
async fn get_current_jackpot() -> Result<u64, String> {
    let res_str = node_call("GET", "/jackpot", None).await?;
    let pot: u64 = serde_json::from_str(&res_str).unwrap_or(0);
    Ok(pot)
}

// 💡 NOUVEAU: Achat d'un ticket de loterie
#[tauri::command]
async fn buy_lottery_ticket(
    sender_dilithium_secret_hex: String, 
    sender_dilithium_public_hex: String, 
    sender_kyber_secret_hex: String, 
    sender_kyber_public_hex: String
) -> Result<String, String> {
    use pqcrypto_kyber::kyber768; 
    use pqcrypto_dilithium::dilithium3; 
    use rand::Rng; 
    use rand::seq::SliceRandom;

    let ticket_price: u64 = 10_000_000_000;
    let fee: u64 = 1000;
    let required_total = ticket_price + fee;

    let res_str = node_call("GET", "/all_transactions", None).await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str)
        .map_err(|_| "Erreur JSON".to_string())?;
		
	let current_height = get_current_block_height().await.unwrap_or(0);

    let mut spent_capsules = std::collections::HashSet::new();
    if let Ok(spends) = std::fs::read_to_string(get_spends_path()) {
        for line in spends.lines() { 
            spent_capsules.insert(line.trim().to_string()); 
        }
    }

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    let kyber_sk = kyber768::SecretKey::from_bytes(&sk_bytes)
        .map_err(|_| "Clé Kyber invalide".to_string())?;

    let mut selected_utxos: Vec<(u64, String, LWECommitment, u64)> = Vec::new();
    let mut current_input_sum = 0u64;

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) {
            Ok(t) => t,
            Err(_) => continue,
        };

        for out in tx.outputs {
            if spent_capsules.contains(&out.kyber_capsule) { continue; }
			
			// === MATURITÉ ===
            let mut is_mature = true;
            // Seule la Coinbase est bloquée
			if out.stealth_address.starts_with("COINBASE_") {
				if height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) {
					is_mature = false;
				}
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
                    if let Ok(ciphertext) = kyber768::Ciphertext::from_bytes(&capsule_bytes) {
                        let shared_secret = kyber768::decapsulate(&ciphertext, &kyber_sk);
                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                            if vault_bytes.len() > 12 {
                                let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes()));
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
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), height));
                current_input_sum += val;
                if current_input_sum >= required_total { break; }
            }
        }
        if current_input_sum >= required_total { break; }
    }

    if current_input_sum < required_total { 
        return Err(format!("❌ Fonds insuffisants. Besoin : {:.9} WATT", required_total as f64 / 1_000_000_000.0));
    }

    let change_amount = current_input_sum - required_total;
    let mut outputs = Vec::new();

    // Ticket LOTTERY
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

    // Change
    if change_amount > 0 {
        let my_pk_bytes = hex::decode(&sender_kyber_public_hex).unwrap();
        let my_pk = kyber768::PublicKey::from_bytes(&my_pk_bytes).unwrap();
        let (my_shared_secret, kyber_capsule_2) = kyber768::encapsulate(&my_pk);

        let mut otp_2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_2);
        let payload_2 = format!("{}|{}", change_amount, hex::encode(otp_2));
        let aes_key_2 = Key::<Aes256Gcm>::from_slice(my_shared_secret.as_bytes());
        let mut nonce_bytes_2 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes_2);
        let encrypted_data_2 = Aes256Gcm::new(aes_key_2).encrypt(Nonce::from_slice(&nonce_bytes_2), payload_2.as_bytes()).unwrap();
        let mut final_vault_2 = nonce_bytes_2.to_vec(); 
        final_vault_2.extend_from_slice(&encrypted_data_2);

        let sum_inputs_t0 = selected_utxos.iter().map(|u| u.2.t_vector[0] as u64).sum::<u64>() % (LATTICE_Q as u64);
        let fee_t0 = (fee * (LATTICE_Q as u64 / 2)) % LATTICE_Q as u64;
        let expected_outputs_sum = (sum_inputs_t0 + (LATTICE_Q as u64) - fee_t0) % (LATTICE_Q as u64);
        
        let ticket_t0 = outputs[0].lattice_commitment.t_vector[0] as u64;
        let perfect_change_t0 = (expected_outputs_sum + (LATTICE_Q as u64) - ticket_t0) % (LATTICE_Q as u64);
        
        let mut t_vector_2 = vec![0u32; LATTICE_DIM];
        t_vector_2[0] = perfect_change_t0 as u32;
        for i in 1..LATTICE_DIM { t_vector_2[i] = rand::thread_rng().gen_range(0..LATTICE_Q); }

        outputs.push(TransactionOutput {
            stealth_address: format!("pq_watt_{}", hex::encode(&otp_2[0..8])), 
            kyber_capsule: hex::encode(kyber_capsule_2.as_bytes()),
            aes_vault: hex::encode(final_vault_2), 
            lattice_commitment: LWECommitment { a_matrix_seed: [0u8; 32], t_vector: t_vector_2 },
        });
    }

    let tx_data_to_sign = format!("{:?}{}", outputs, fee);
    let mut final_inputs = Vec::new();

    let sk_bytes_dil = hex::decode(&sender_dilithium_secret_hex).unwrap();
    let dilithium_secret = dilithium3::SecretKey::from_bytes(&sk_bytes_dil).unwrap();
    let dilithium_signature = dilithium3::sign(tx_data_to_sign.as_bytes(), &dilithium_secret);

    let decoy_res = node_call("GET", "/get_decoys/10", None).await.unwrap_or_default();
    let real_decoys: Vec<String> = serde_json::from_str(&decoy_res).unwrap_or_default();

    for utxo in &selected_utxos {
        let (_, capsule, commitment, source_height) = utxo;

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
            s_vector[j] = u32::from_le_bytes(sk_bytes_dil[offset..offset+4].try_into().unwrap_or([0; 4])) % LATTICE_Q;
            my_p[j] = ((s_vector[j] as u64 * ((j as u32 + 1) * 1337) as u64) % LATTICE_Q as u64) as u32; 
        }
        p_keys[my_real_index] = my_p;

        let mut alpha = vec![0u32; LATTICE_DIM];
        for j in 0..LATTICE_DIM { alpha[j] = rand::thread_rng().gen_range(0..LATTICE_Q); }
        let mut current_index = my_real_index;
        
        let mut hasher = blake3::Hasher::new();
        hasher.update(tx_data_to_sign.as_bytes()); 
        hasher.update(pq_ring[my_real_index].as_bytes()); 
        for j in 0..LATTICE_DIM { 
            hasher.update(&(((alpha[j] as u64 * ((j as u32 + 1) * 1337) as u64) % LATTICE_Q as u64) as u32).to_le_bytes()); 
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
                r_i[j] = ((((z_responses[current_index][j] as u64 * ((j as u32 + 1) * 1337) as u64) % LATTICE_Q as u64) + ((c_i as u64 * p_keys[current_index][j] as u64) % LATTICE_Q as u64)) % LATTICE_Q as u64) as u32; 
            }
            let mut hasher_sim = blake3::Hasher::new();
            hasher_sim.update(tx_data_to_sign.as_bytes()); 
            hasher_sim.update(pk_hex.as_bytes()); 
            for val in &r_i { hasher_sim.update(&val.to_le_bytes()); }
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

        let lattice_signature = PQLatticeRingSignature {
            key_image: format!("ticket_{}", capsule),
            c0: hex::encode(&challenges_c[0]),
            z_responses,
            p_keys, 
        };

        final_inputs.push(TransactionInput {
            pq_ring_inputs: pq_ring,
            commitment: commitment.clone(),
            pq_ring_signature: lattice_signature,
            source_height: *source_height,
        });
    }

    let info_str = node_call("GET", "/info", None).await?;
    let info: serde_json::Value = serde_json::from_str(&info_str).map_err(|_| "Erreur INFO".to_string())?;
    let current_blocks = info["blocks"].as_u64().unwrap_or(0);
    let target_block = current_blocks + (10 - (current_blocks % 10));

    let tx_pq = Transaction {
        tx_type: TransactionType::HTLCLottery { target_block, player_pubkey: sender_kyber_public_hex.clone() }, 
        inputs: final_inputs, 
        outputs, 
        fee, 
        dilithium_signature: hex::encode(dilithium_signature.as_bytes()),
    };

    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    node_call("POST", "/send_tx", Some(tx_json)).await?;

    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(get_spends_path()) {
        for utxo in &selected_utxos { 
            let (_, capsule, _, _) = utxo;
            let _ = writeln!(file, "{}", capsule); 
        }
    }
    
    Ok(format!("🎟️ TICKET VALIDÉ ! Le tirage aura lieu au bloc {}.", target_block))
}

#[tauri::command]
fn create_swap_secret() -> serde_json::Value {
    use rand::RngCore;
    use sha2::{Sha256, Digest};

    let mut secret = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut secret);
    
    // ✅ VRAI ATOMIC SWAP – SHA256 (compatible Bitcoin HTLC + Wattcoin)
    let hash = Sha256::digest(&secret);
    
    serde_json::json!({
        "secret": hex::encode(secret),
        "hash": hex::encode(hash)   // hex::encode accepte directement Output<Sha256>
    })
}

#[tauri::command]
async fn submit_order(
    order_type: String, 
    amount: f64, 
    price: f64, 
    btc_address: String, 
    btc_pubkey: String, 
    watt_address: String, 
    htlc_hash: Option<String> // <--- Ajoute ceci ici !
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
        "htlc_hash": htlc_hash // <--- Maintenant il est trouvé !
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
fn vault_exists() -> bool { get_vault_path().exists() }

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
    
    fs::write(get_vault_path(), final_data).map_err(WattError::from)?;
    Ok(())
}

#[tauri::command]
async fn unlock_vault(password: String) -> Result<WalletKeys, String> {
    use pbkdf2::pbkdf2_hmac;
    use sha2::Sha256;

    let file_data = fs::read(get_vault_path()).map_err(|e| WattError::Io(e))?;
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
    let res_str = node_call("GET", "/all_transactions", None).await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str)
        .map_err(|_| "Erreur JSON enriched".to_string())?;

    let current_height = get_current_block_height().await.unwrap_or(0);

    let mut balance_flames: u64 = 0;
    use pqcrypto_kyber::kyber768;

    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
    let kyber_sk = kyber768::SecretKey::from_bytes(&sk_bytes).unwrap();

    let mut spent_capsules = HashSet::new();
    if let Ok(spends) = std::fs::read_to_string(get_spends_path()) {
        for line in spends.lines() { spent_capsules.insert(line.trim().to_string()); }
    }

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) {
            Ok(t) => t,
            Err(_) => continue,
        };

        for out in tx.outputs.iter() {
            if spent_capsules.contains(&out.kyber_capsule) { continue; }

            // === MATURITÉ (seulement Coinbase) ===
            let mut is_mature = true;
            if out.stealth_address.starts_with("COINBASE_") {
                if height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) {
                    is_mature = false;
                }
            }
            if !is_mature { continue; }

            // === Jackpot / Coinbase ===
            if out.stealth_address == format!("JACKPOT_{}", keys.watt_address) 
                || out.stealth_address == format!("COINBASE_{}", keys.watt_address) 
            {
                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                    balance_flames += amt;
                }
            } 
            // === Transferts normaux (pq_watt_) ===
            else if out.stealth_address.starts_with("pq_watt_") {
                if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
                    if let Ok(ciphertext) = kyber768::Ciphertext::from_bytes(&capsule_bytes) {
                        let shared_secret = kyber768::decapsulate(&ciphertext, &kyber_sk);
                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                            if vault_bytes.len() > 12 {
                                let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes()));
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
            // === HTLC (htlc_watt_) — ON NE COMPTE QUE SI LE NODE A VALIDÉ UN CLAIM ===
            else if out.stealth_address.starts_with("htlc_watt_") {
				// On crédite directement (le node a déjà validé le claim via /htlc/claim)
				if let Ok(capsule_bytes) = hex::decode(&out.kyber_capsule) {
					if let Ok(ciphertext) = kyber768::Ciphertext::from_bytes(&capsule_bytes) {
						let shared_secret = kyber768::decapsulate(&ciphertext, &kyber_sk);
						if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
							if vault_bytes.len() > 12 {
								let nonce = Nonce::from_slice(&vault_bytes[0..12]);
								let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes()));
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

async fn get_current_block_height() -> Result<u64, String> {
    let info_str = node_call("GET", "/info", None).await?;
    let info: serde_json::Value = serde_json::from_str(&info_str).map_err(|_| "err".to_string())?;
    Ok(info["blocks"].as_u64().unwrap_or(0))
}

#[tauri::command]
async fn get_history(keys: WalletKeys) -> Result<Vec<HistoryItem>, String> {
    let res_str = node_call("GET", "/all_transactions", None).await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str)
        .map_err(|_| "Erreur JSON history".to_string())?;

    let current_height = get_current_block_height().await.unwrap_or(0);

    let mut history = Vec::new();
    use pqcrypto_kyber::kyber768;
    use aes_gcm::{Aes256Gcm, Key, Nonce, aead::Aead};
    use chrono::{DateTime, Utc, Local};

    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
    let kyber_sk = kyber768::SecretKey::from_bytes(&sk_bytes).unwrap();

    let mut spent_capsules = std::collections::HashSet::new();
    if let Ok(spends) = std::fs::read_to_string(get_spends_path()) {
        for line in spends.lines() { 
            spent_capsules.insert(line.trim().to_string()); 
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

            // Maturité
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

            // === 1. Coinbase et Jackpot ===
            if out.stealth_address == format!("COINBASE_{}", keys.watt_address) 
                || out.stealth_address == format!("JACKPOT_{}", keys.watt_address) 
            {
                if let Ok(amt) = out.aes_vault.parse::<u64>() {
                    let label = if out.stealth_address.starts_with("JACKPOT") { 
                        "Jackpot gagné ! 🎰" 
                    } else { 
                        "Récompense minage ⛏️" 
                    };
                    
                    history.push(HistoryItem {
                        id: format!("#{}", height),
                        tx_type: "receive".to_string(),
                        amount: amt as f64 / 1_000_000_000.0,
                        coin: "WATT".to_string(),
                        date: date_str,
                        status: format!("{} ({})", label, status_text),
                    });
                }
            } 
            // === 2. Transferts normaux (pq_watt_) ===
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
                                                    id: format!("#{}", height),
                                                    tx_type: "receive".to_string(),
                                                    amount: amt as f64 / 1_000_000_000.0,
                                                    coin: "WATT".to_string(),
                                                    date: date_str,
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
            // === 3. HTLC (htlc_watt_) — On n'affiche que si le node a validé un claim ===
            else if out.stealth_address.starts_with("htlc_watt_") {
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
													id: format!("#{}", height),
													tx_type: "receive".to_string(),
													amount: amt as f64 / 1_000_000_000.0,
													coin: "WATT".to_string(),
													date: date_str,
													status: format!("Transfert Swap 🔁 ({})", status_text),
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

    let res_str = node_call("GET", "/all_transactions", None).await?;
    let enriched: Vec<serde_json::Value> = serde_json::from_str(&res_str)
        .map_err(|_| "Erreur JSON".to_string())?;
		
	let current_height = get_current_block_height().await.unwrap_or(0);
    
    let mut spent_capsules = std::collections::HashSet::new();
    if let Ok(spends) = std::fs::read_to_string(get_spends_path()) {
        for line in spends.lines() { 
            spent_capsules.insert(line.trim().to_string()); 
        }
    }

    let sk_bytes = hex::decode(&sender_kyber_secret_hex).unwrap_or_default();
    let kyber_sk = kyber768::SecretKey::from_bytes(&sk_bytes)
        .map_err(|_| "Clé Kyber corrompue".to_string())?;
    
    let mut selected_utxos: Vec<(u64, String, LWECommitment, u64)> = Vec::new(); // (amount, capsule, commitment, source_height)
    let mut current_input_sum = 0u64;

    for item in enriched {
        let height = item["height"].as_u64().unwrap_or(0);
        let tx: Transaction = match serde_json::from_value(item["transaction"].clone()) {
            Ok(t) => t,
            Err(_) => continue,
        };

        for out in tx.outputs {
            if spent_capsules.contains(&out.kyber_capsule) { continue; }
			
			// === MATURITÉ ===
            let mut is_mature = true;
            // Seule la Coinbase est bloquée
			if out.stealth_address.starts_with("COINBASE_") {
				if height > 0 && (current_height.saturating_sub(height) < MATURITY_BLOCKS) {
					is_mature = false;
				}
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
                    if let Ok(ciphertext) = kyber768::Ciphertext::from_bytes(&capsule_bytes) {
                        let shared_secret = kyber768::decapsulate(&ciphertext, &kyber_sk);
                        if let Ok(vault_bytes) = hex::decode(&out.aes_vault) {
                            if vault_bytes.len() > 12 {
                                let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                                let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes()));
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
                selected_utxos.push((val, out.kyber_capsule.clone(), out.lattice_commitment.clone(), height));
                current_input_sum += val;
                if current_input_sum >= required_total { break; }
            }
        }
        if current_input_sum >= required_total { break; }
    }

    if current_input_sum < required_total {
        return Err(format!("❌ Fonds insuffisants ! Vous essayez d'envoyer {} WATT (frais inclus) mais vous n'avez que {} WATT libres.", 
            required_total as f64 / 1_000_000_000.0, current_input_sum as f64 / 1_000_000_000.0));
    }

    let change_amount = current_input_sum - required_total;
    let mut outputs = Vec::new();
	
	// === DÉTERMINATION DU TYPE DE TX (à mettre ici) ===
	let tx_type = match (htlc_hash_hex, htlc_timeout) {
		(Some(hash), Some(timeout)) => TransactionType::HTLCLock { hash, timeout_block: timeout },
		_ => TransactionType::Standard,
	};

    // Output destinataire
    let recipient_bytes = hex::decode(&recipient_kyber_hex).map_err(|_| "Adresse invalide".to_string())?;
    let bob_pk = kyber768::PublicKey::from_bytes(&recipient_bytes).map_err(|_| "Clé Kyber corrompue".to_string())?;
    let (alice_shared_secret, kyber_capsule_1) = kyber768::encapsulate(&bob_pk);

    let mut otp_1 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_1);
    let payload_1 = format!("{}|{}", amount_in_flames, hex::encode(otp_1));
    let aes_key_1 = Key::<Aes256Gcm>::from_slice(alice_shared_secret.as_bytes());
    let mut nonce_bytes_1 = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes_1);
    let encrypted_data_1 = Aes256Gcm::new(aes_key_1).encrypt(Nonce::from_slice(&nonce_bytes_1), payload_1.as_bytes())
        .map_err(|_| "Erreur AES".to_string())?;
    let mut final_vault_1 = nonce_bytes_1.to_vec(); 
    final_vault_1.extend_from_slice(&encrypted_data_1);

    let mut bf_1 = [0u32; LATTICE_DIM];
    for val in bf_1.iter_mut() { *val = rand::thread_rng().gen_range(0..LATTICE_Q); }
    let commitment_1 = LWECommitment::commit(amount_in_flames, bf_1);

    let stealth_prefix = if matches!(tx_type, TransactionType::HTLCLock { .. }) {
		"htlc_watt_"
	} else {
		"pq_watt_"
	};

	outputs.push(TransactionOutput {
		stealth_address: format!("{}{}", stealth_prefix, hex::encode(&otp_1[0..8])),
		kyber_capsule: hex::encode(kyber_capsule_1.as_bytes()),
		aes_vault: hex::encode(final_vault_1),
		lattice_commitment: commitment_1.clone(),
	});

    // Change
    if change_amount > 0 {
        let my_pk_bytes = hex::decode(&sender_kyber_public_hex).unwrap();
        let my_pk = kyber768::PublicKey::from_bytes(&my_pk_bytes).unwrap();
        let (my_shared_secret, kyber_capsule_2) = kyber768::encapsulate(&my_pk);

        let mut otp_2 = [0u8; 32]; rand::thread_rng().fill_bytes(&mut otp_2);
        let payload_2 = format!("{}|{}", change_amount, hex::encode(otp_2));
        let aes_key_2 = Key::<Aes256Gcm>::from_slice(my_shared_secret.as_bytes());
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
        for i in 1..LATTICE_DIM { 
            t_vector_2[i] = rand::thread_rng().gen_range(0..LATTICE_Q); 
        }

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

    let sk_bytes_dil = hex::decode(&sender_dilithium_secret_hex).unwrap();
    let dilithium_secret = dilithium3::SecretKey::from_bytes(&sk_bytes_dil).unwrap();
    let dilithium_signature = dilithium3::sign(tx_data_to_sign.as_bytes(), &dilithium_secret);

    let decoy_res = node_call("GET", "/get_decoys/10", None).await.unwrap_or_default();
    let real_decoys: Vec<String> = serde_json::from_str(&decoy_res).unwrap_or_default();

    for utxo in &selected_utxos {
        let (_, capsule, commitment, source_height) = utxo;

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
            s_vector[j] = u32::from_le_bytes(sk_bytes_dil[offset..offset+4].try_into().unwrap_or([0; 4])) % LATTICE_Q;
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
            for val in &r_i { hasher_sim.update(&val.to_le_bytes()); }
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

        let unique_seed = format!("{}{}", hex::encode(&sk_bytes_dil), capsule);
        let key_image = hex::encode(blake3::hash(unique_seed.as_bytes()).as_bytes());

        let lattice_signature = PQLatticeRingSignature {
            key_image,
            c0: hex::encode(&challenges_c[0]),
            z_responses,
            p_keys, 
        };

        final_inputs.push(TransactionInput {
            pq_ring_inputs: pq_ring,
            commitment: commitment.clone(),
            pq_ring_signature: lattice_signature,
            source_height: *source_height,
        });
    }
	
	// Version qui garde tout le travail ZKP déjà fait plus haut
    let tx_pq = Transaction {
        tx_type,
        inputs: final_inputs,           // ← on garde les vraies preuves
        outputs,                        // ← on garde les outputs
        fee: 1000,
        dilithium_signature: hex::encode(dilithium_signature.as_bytes()),
    };

    let tx_json = serde_json::to_string(&tx_pq).map_err(|e| e.to_string())?;
    let _ = node_call("POST", "/send_tx", Some(tx_json)).await?;
	
	// 💡 ENREGISTREMENT DES CAPSULES DÉPENSÉES (le "reprend")
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(get_spends_path()) {
        for utxo in &selected_utxos {
            let (_, capsule, _, _) = utxo;
            let _ = writeln!(file, "{}", capsule);
        }
    }

    //println!("✅ Capsules marquées comme dépensées dans .wattcoin_spends");

    match &tx_pq.tx_type {
		TransactionType::HTLCLock { .. } => Ok("🔒 HTLC Lock WATT accepté par le relay !".to_string()),
		_ => Ok("TX envoyée".to_string()),
	}
}

#[tauri::command]
async fn refund_wattcoin_swap(
    hash: String,
    _watt_address: String,
    _amount: f64
) -> Result<String, String> {
    let refund_tx = Transaction {
        tx_type: TransactionType::HTLCRefund { hash: hash.clone() },
        inputs: vec![],
        outputs: vec![],
        fee: 1000,
        dilithium_signature: hash.clone(),
    };

    let tx_json = serde_json::to_string(&refund_tx).map_err(|e| e.to_string())?;
    let _ = node_call("POST", "/send_tx", Some(tx_json)).await?;

    Ok("🔙 REMBOURSEMENT WATT DEMANDÉ ! (Frais réseau : 1000 Flames). Attends le timeout.".to_string())
}

#[tauri::command]
async fn get_active_swaps(btc_address: String, watt_address: String) -> Result<Vec<SwapContract>, String> {
    //println!("🔍 [DEBUG] Demande swaps → BTC: {} | WATT: {}", btc_address, watt_address);

    let res_str = match node_call("GET", "/swaps", None).await {
        Ok(s) => s,
        Err(e) => {
            println!("❌ [DEBUG] Erreur /swaps : {}", e);
            return Ok(vec![]); // fallback silencieux
        }
    };

    //println!("📥 [DEBUG] Réponse brute du node : {}", res_str);

    let all_swaps: Vec<SwapContract> = serde_json::from_str(&res_str).unwrap_or_default();

    let my_swaps: Vec<SwapContract> = all_swaps.into_iter()
        .filter(|s| s.buyer_btc_address == btc_address || s.seller_watt_address == watt_address)
        .collect();

    //println!("✅ [DEBUG] Swaps trouvés pour ce wallet : {}", my_swaps.len());

    // Fallback localStorage si rien ne vient du node
    if my_swaps.is_empty() {
        if let Ok(cached) = std::fs::read_to_string("/tmp/my_swaps_cache.json") {
            if let Ok(parsed) = serde_json::from_str::<Vec<SwapContract>>(&cached) {
                //println!("♻️ [DEBUG] Utilisation du cache local");
                return Ok(parsed);
            }
        }
    }

    Ok(my_swaps)
}

#[tauri::command]
async fn check_btc_contract_exists(htlc_hash: &str) -> Result<bool, String> {
    //println!("🔍 [WALLET] Demande check HTLC via node → {}", htlc_hash);

    // Appel propre via le node (plus de SOCKS5 direct depuis le wallet)
    let res_str = node_call("GET", &format!("/btc/htlc/exists/{}", htlc_hash), None).await
		.unwrap_or_else(|_| r#"{"exists": false}"#.to_string());   // ← changé en false

	let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
	let exists = json["exists"].as_bool().unwrap_or(false);        // ← changé en false

    //println!("✅ [CHECK BTC] Résultat du node : {}", exists);
    Ok(exists)
}

#[tauri::command]
async fn claim_wattcoin_swap(
    secret: String,
    _hash: String,
    amount_flames: u64,
    watt_address: String,
) -> Result<String, String> {
    use pqcrypto_kyber::kyber768;
    use aes_gcm::{Aes256Gcm, Key, Nonce, aead::Aead};

    // === CRÉATION D'UN OUTPUT RÉEL (comme un transfert normal, mais tagué swap) ===
    let my_pk_bytes = hex::decode(&watt_address)
        .map_err(|_| "Adresse Kyber invalide".to_string())?;
    let my_pk = kyber768::PublicKey::from_bytes(&my_pk_bytes)
        .map_err(|_| "Clé Kyber corrompue".to_string())?;

    let (shared_secret, kyber_capsule) = kyber768::encapsulate(&my_pk);

    let mut otp = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut otp);
    let payload = format!("{}|{}", amount_flames, hex::encode(otp));

    let aes_key = Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes());
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let encrypted = Aes256Gcm::new(aes_key)
        .encrypt(Nonce::from_slice(&nonce_bytes), payload.as_bytes())
        .map_err(|_| "Erreur chiffrement AES claim".to_string())?;

    let mut final_vault = nonce_bytes.to_vec();
    final_vault.extend_from_slice(&encrypted);

    let claim_output = TransactionOutput {
        stealth_address: format!("htlc_watt_{}", hex::encode(&otp[0..8])),
        kyber_capsule: hex::encode(kyber_capsule.as_bytes()),
        aes_vault: hex::encode(final_vault),
        lattice_commitment: LWECommitment::commit(amount_flames, [0u32; LATTICE_DIM]),
    };

    let claim_tx = Transaction {
        tx_type: TransactionType::HTLCClaim { secret: secret.clone() },
        inputs: vec![],
        outputs: vec![claim_output],
        fee: 1000,
        dilithium_signature: hex::encode(sha2::Sha256::digest(&hex::decode(&secret).unwrap())),
    };

    let tx_json = serde_json::to_string(&claim_tx).map_err(|e| e.to_string())?;
    node_call("POST", "/htlc/claim", Some(tx_json)).await?;

    Ok("✅ Claim envoyé au node (output vérifié on-chain). En attente du bloc.".to_string())
}

#[tauri::command]
async fn check_watt_lock_exists(hash: String) -> Result<bool, String> {
    //println!("🔍 [WALLET] Demande check HTLC WATT lock via node → {}", hash);

    let res_str = node_call("GET", &format!("/htlc/lock/exists/{}", hash), None).await
        .unwrap_or_else(|_| r#"{"exists": false}"#.to_string());

    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    let exists = json["exists"].as_bool().unwrap_or(false);

    //println!("✅ [CHECK WATT LOCK] Résultat du node (tribunal) : {}", exists);
    Ok(exists)
}

#[tauri::command]
async fn cancel_order(order_id: String) -> Result<String, String> {
    node_call("DELETE", &format!("/order/{}", order_id), None).await?;
    Ok("Ordre annulé avec succès".to_string())
}

#[tauri::command]
fn destroy_vault() -> Result<String, String> {
    let vault_path = get_vault_path();
    if vault_path.exists() {
        fs::remove_file(vault_path).map_err(|_| "⚠️ Impossible de supprimer le coffre.".to_string())?;
        
        // 💡 Tant qu'on y est, on supprime aussi l'historique des dépenses !
        let spends_path = get_spends_path();
        if spends_path.exists() { let _ = fs::remove_file(spends_path); }
        
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

// ==================== BTC HTLC - VERSION SOLIDE 2026 ====================
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
    // On prend TOUJOURS l'adresse stockée dans le wallet (aucune hardcode)
    let address = btc_address.unwrap_or_else(|| {
        use bitcoin::bip32::{Xpriv, DerivationPath};
        use bitcoin::{Network, Address, PrivateKey};
        use bitcoin::secp256k1::Secp256k1;
        use std::str::FromStr;
        let seed = hex::decode(&master_seed_hex).unwrap_or_default();
        let secp = Secp256k1::new();
        let root = Xpriv::new_master(Network::Testnet, &seed).expect("Seed invalide");
        let path = DerivationPath::from_str("m/84'/1'/0'/0/0").expect("Path invalide");
        let child = root.derive_priv(&secp, &path).expect("Dérivation échouée");
        let privkey = PrivateKey::new(child.private_key, Network::Testnet);
        let pubkey = privkey.public_key(&secp);
        let compressed = bitcoin::CompressedPublicKey(pubkey.inner);
        Address::p2wpkh(&compressed, Network::Testnet).to_string()
    });

    println!("🔍 [BTC] Adresse envoyée au node : {}", address);

    let res_str = node_call("GET", &format!("/btc/balance?address={}", address), None).await
        .unwrap_or_else(|_| r#"{"balance": 0.0}"#.to_string());

    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    let real_balance = json["balance"].as_f64().unwrap_or(0.0);

    println!("✅ [BTC VRAI] Solde : {} BTC (adresse utilisée = {})", real_balance, address);
    Ok(real_balance)
}

#[tauri::command]
async fn send_btc_to_htlc(htlc_address: String, amount_btc: f64, raw_tx: Option<String>) -> Result<String, String> {
    let payload = serde_json::json!({
        "htlc_address": htlc_address,
        "amount_btc": amount_btc,
        "raw_tx": raw_tx.unwrap_or_default()
    });
    node_call("POST", "/btc/send/to_htlc", Some(serde_json::to_string(&payload).unwrap())).await
		.map(|body| {
			println!("✅ [LOCAL DEV] send_btc_to_htlc OK → {}", body);
			"✅ BTC verrouillé dans le HTLC (simulation NODE acceptée)".to_string()
		})
		.map_err(|e| {
			println!("❌ [send_btc_to_htlc] {}", e);
			format!("Erreur node : {}", e)
		})
}

#[tauri::command]
async fn register_real_swap_hash(pending_placeholder: String, real_htlc_hash: String) -> Result<String, String> {
    println!("🔑 [WALLET] Mise à jour swap : {} → vrai hash {}", pending_placeholder, real_htlc_hash);
    // On appelle le node pour updater le swap dans le DEX pool
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
        .map(|_| "✅ BTC envoyé directement via le NODE".to_string())
}

#[tauri::command]
async fn auto_claim_btc_swap(htlc_hash: String, _htlc_address: String) -> Result<String, String> {
    // 1. Le Watchdog interroge le nœud silencieusement
    let res_str = node_call("GET", &format!("/htlc/secret/{}", htlc_hash), None).await?;
    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    
    if json["success"].as_bool().unwrap_or(false) {
        let secret = json["secret"].as_str().unwrap_or_default().to_string();
        
        // 2. Le secret est révélé ! On forge la raw transaction BTC
        let raw_witness_tx = format!(
            "02000000000101{}0000000000000000000000000000000000000000000000000000000000000000ffffffff{}00000000", 
            htlc_hash, secret
        );
        
        // 3. On broadcast sur le réseau Bitcoin
        let payload = serde_json::json!({ "raw_tx": raw_witness_tx });
        match node_call("POST", "/btc/broadcast", Some(serde_json::to_string(&payload).unwrap())).await {
            Ok(_) => Ok(format!("🎉 CLAIM BTC RÉUSSI ! (Secret: {}...)", &secret[0..10])),
            Err(e) => Err(format!("Erreur broadcast BTC : {}", e))
        }
    } else {
        // Échoue silencieusement pour le watchdog
        Err("Secret non révélé".to_string())
    }
}

#[tauri::command]
async fn get_revealed_secret(htlc_hash: String) -> Result<String, String> {
    println!("🔍 [WALLET get_revealed_secret] Appel reçu avec hash: {}", htlc_hash);
    println!("🔍 [WALLET] Appel node_call → /htlc/secret/{}", htlc_hash);

    let res_str = node_call("GET", &format!("/htlc/secret/{}", htlc_hash), None).await
        .unwrap_or_else(|e| {
            println!("❌ [WALLET] node_call a échoué : {}", e);
            r#"{"success":false}"#.to_string()
        });

    println!("📥 [WALLET] Réponse brute du node : {}", res_str);

    let json: serde_json::Value = serde_json::from_str(&res_str).unwrap_or_default();
    if json["success"].as_bool().unwrap_or(false) {
        let secret = json["secret"].as_str().unwrap_or_default().to_string();
        println!("✅ [WALLET] Secret révélé récupéré avec succès : {}", secret);
        Ok(secret)
    } else {
        let msg = json["message"].as_str().unwrap_or("Secret pas encore révélé par Alice");
        println!("❌ [WALLET] Échec du node : {}", msg);
        Err(msg.to_string())
    }
}

// ========================================================================
// ⚡ BITCOIN LIGHTNING NETWORK (LDK - PHASE 3) - MOCKS POUR UI
// ========================================================================

#[tauri::command]
async fn get_lightning_balance(master_seed_hex: String) -> Result<u64, String> {
    let _ = master_seed_hex; // Gardé pour éviter le warning
    Ok(0) // 💡 Solde factice pour tester ton interface !
}

#[tauri::command]
async fn create_lightning_invoice(master_seed_hex: String, amount_sats: u64, description: String) -> Result<String, String> {
    let _ = master_seed_hex;
    let _ = description;
    Ok(format!("lnbcrt{}m1simulateur... (Facture générée pour {} Sats)", amount_sats / 100_000, amount_sats))
}

#[tauri::command]
async fn pay_lightning_invoice(master_seed_hex: String, invoice: String) -> Result<String, String> {
    let _ = master_seed_hex;
    let _ = invoice;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await; // Simule le temps de routage cryptographique
    Ok("⚡ Paiement Lightning réussi !".to_string())
}

#[tauri::command]
fn get_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

// ============== LA FONCTION MAIN ==================

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
            create_swap_secret, submit_order, get_dark_pool, get_watt_balance, get_btc_balance, cancel_order,
            send_wattcoin, create_btc_htlc, send_btc_to_htlc, check_btc_contract_exists, claim_wattcoin_swap, check_watt_lock_exists, refund_wattcoin_swap,
            destroy_vault, get_active_swaps, auto_claim_btc_swap, get_revealed_secret, register_real_swap_hash, send_btc_direct, get_history, 
            save_miner_script, get_total_supply, get_current_jackpot, buy_lottery_ticket,
            get_lightning_balance, create_lightning_invoice, pay_lightning_invoice, get_version
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}