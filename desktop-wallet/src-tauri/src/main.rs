#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};
use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::network::Network;
use bitcoin::{Address, PrivateKey};
use bitcoin::blockdata::script::Builder;
use bitcoin::opcodes::all::*;
use bitcoin::hashes::{sha256, Hash};
use bitcoin::secp256k1::Secp256k1; 
use rand::RngCore; 
use serde::{Serialize, Deserialize};
use std::str::FromStr;
use std::fs; 
use std::path::Path;
use std::time::Duration;
use tauri::Emitter;

use pqcrypto_traits::kem::{Ciphertext, SharedSecret, PublicKey as _, SecretKey as _};
use pqcrypto_traits::sign::{SignedMessage, PublicKey as _, SecretKey as _};

const NODE_URL: &str = "http://80.78.26.243:8100";
const VAULT_FILE: &str = ".wattcoin_vault";

// 🧮 CONSTANTES MATHÉMATIQUES POST-QUANTIQUE (Famille Kyber/Dilithium)
const LATTICE_Q: u32 = 8380417; 
const LATTICE_DIM: usize = 4;

#[derive(Serialize, Deserialize, Clone)]
struct WalletKeys {
    mnemonic: String,
    btc_address: String,
    watt_address: String, 
    master_seed_hex: String,
    kyber_secret_hex: String,
    dilithium_public_hex: String,
    dilithium_secret_hex: String,
}

// 💡 LA NOUVELLE STRUCTURE DE SIGNATURE LATTICE
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PQLatticeRingSignature {
    pub key_image: String,
    pub c0: String,
    pub z_responses: Vec<Vec<u32>>,
    pub p_keys: Vec<Vec<u32>>, // 💡 NOUVEAU
}

#[derive(Serialize, Deserialize, Debug)]
struct TransactionPQ {
    pub stealth_address: String,
    pub kyber_capsule: String,
    pub aes_vault: String,
    pub lattice_commitment: LatticeCommitment, 
    pub fee: u64,
    pub pq_ring_inputs: Vec<String>,
    pub pq_ring_signature: Option<PQLatticeRingSignature>, // 💡 LE GRAAL QUANTIQUE
    pub dilithium_signature: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Order { pub id: String, pub order_type: String, pub amount_flames: u64, pub price_sats: u64, pub btc_address: String, pub watt_address: String }

#[derive(Serialize, Deserialize, Clone)]
struct SwapContract { pub buyer_btc_address: String, pub seller_watt_address: String, pub watt_amount_flames: u64, pub btc_amount_sats: u64, pub htlc_secret: String, pub htlc_hash: String }

#[derive(Serialize, Deserialize, Clone)]
struct BatchResult { pub success: bool, pub message: String, pub clearing_price_sats: u64, pub total_volume_flames: u64, pub swaps: Vec<SwapContract> }

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LatticeCommitment {
    pub c1: Vec<u64>,
    pub c2: u64,
}

impl LatticeCommitment {
    pub fn commit(amount: u64, blinding_factor: u64) -> Self {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let q: u64 = 8380417; 
        let c1 = vec![
            (blinding_factor * rng.gen_range(1..100)) % q,
            (blinding_factor * rng.gen_range(1..100)) % q,
            (blinding_factor * rng.gen_range(1..100)) % q,
        ];
        let c2 = (blinding_factor * 12345 + amount) % q;
        LatticeCommitment { c1, c2 }
    }
}

#[tauri::command]
async fn submit_order(order_type: String, amount: f64, price: f64, btc_address: String, watt_address: String) -> Result<(), String> {
    let mut rand_bytes = [0u8; 4]; rand::thread_rng().fill_bytes(&mut rand_bytes);
    let amount_flames = (amount * 1_000_000_000.0) as u64; 
    let price_sats = (price * 100_000_000.0) as u64; 

    let order_data = serde_json::json!({
        "id": hex::encode(rand_bytes),
        "order_type": order_type,
        "amount_flames": amount_flames, 
        "price_sats": price_sats,       
        "btc_address": btc_address,
        "watt_address": watt_address
    });

    let client = reqwest::Client::new();
    let url = format!("{}/order", NODE_URL);
    client.post(&url) 
        .json(&order_data)
        .send().await.map_err(|_| "⚠️ Impossible de joindre le Nœud Relais !".to_string())?;
    Ok(())
}

#[tauri::command]
async fn get_dark_pool() -> Result<Vec<Order>, String> {
    let url = format!("{}/pool", NODE_URL); 
    let res = reqwest::get(&url)
        .await.map_err(|_| "⚠️ Nœud Relais hors ligne.".to_string())?;
    let pool = res.json::<Vec<Order>>().await.map_err(|e| e.to_string())?;
    Ok(pool)
}

#[tauri::command]
async fn resolve_batch() -> Result<BatchResult, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/resolve", NODE_URL); 
    let res = client.post(&url)
        .send().await.map_err(|_| "⚠️ Nœud Relais hors ligne.".to_string())?;
    let result = res.json::<BatchResult>().await.map_err(|e| e.to_string())?;
    Ok(result)
}

#[tauri::command]
async fn generate_pro_wallet(phrase_option: Option<String>) -> Result<WalletKeys, String> {
    use bip39::{Mnemonic, Language};
    use rand::RngCore;
    use bitcoin::Network as BtcNetwork;
    use bitcoin::bip32::{Xpriv, DerivationPath}; 
    use bitcoin::{PrivateKey as BtcPrivateKey, PublicKey as BtcPublicKey, Address as BtcAddress};
    use bitcoin::key::Secp256k1;
    use std::str::FromStr;
    
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
        master_seed_hex: hex::encode(seed),
        watt_address: hex::encode(kyber_pk.as_bytes()),
        kyber_secret_hex: hex::encode(kyber_sk.as_bytes()),
        dilithium_public_hex: hex::encode(dilithium_pk.as_bytes()),
        dilithium_secret_hex: hex::encode(dilithium_sk.as_bytes()),
    })
}

#[tauri::command] 
fn vault_exists() -> bool { 
    Path::new(VAULT_FILE).exists() 
}

#[tauri::command]
fn encrypt_vault(password: String, keys_json_string: String) -> Result<(), String> {
    let mut salt = [0u8; 16]; rand::thread_rng().fill_bytes(&mut salt);
    let mut key = [0u8; 32]; pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), &salt, 100_000, &mut key);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    
    let ciphertext = cipher.encrypt(nonce, keys_json_string.as_bytes()).map_err(|_| "Erreur d'encryptage")?;
    
    let mut final_data = Vec::new();
    final_data.extend_from_slice(&salt); 
    final_data.extend_from_slice(&nonce_bytes); 
    final_data.extend_from_slice(&ciphertext);
    
    fs::write(VAULT_FILE, final_data).unwrap(); 
    Ok(())
}

#[tauri::command]
async fn unlock_vault(password: String) -> Result<WalletKeys, String> {
    use std::fs;
    use pbkdf2::pbkdf2_hmac;
    use sha2::Sha256;

    let file_data = fs::read(VAULT_FILE).map_err(|_| "Coffre introuvable.".to_string())?;
    if file_data.len() < 28 { return Err("Fichier corrompu.".to_string()); }

    let salt = &file_data[0..16];
    let nonce_bytes = &file_data[16..28];
    let ciphertext = &file_data[28..];

    let mut key = [0u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, 100_000, &mut key);

    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|_| "Mot de passe incorrect.".to_string())?;
    
    let json_string = String::from_utf8(plaintext).map_err(|_| "Erreur UTF-8".to_string())?;
    let keys: WalletKeys = serde_json::from_str(&json_string).map_err(|_| "Erreur de lecture du Keystore".to_string())?;

    Ok(keys)
}

#[tauri::command]
async fn get_watt_balance(keys: WalletKeys) -> Result<f64, String> {
    let client = reqwest::Client::new();
    let url = format!("{}/all_transactions", NODE_URL); 
    
    let all_txs: Vec<TransactionPQ> = client.get(&url)
        .send().await.map_err(|_| "Nœud injoignable".to_string())?
        .json().await.map_err(|_| "Erreur JSON".to_string())?;

    let mut balance_flames: i64 = 0;

    use pqcrypto_kyber::kyber768;
    use pqcrypto_traits::kem::SecretKey as KemSecretKey;

    let sk_bytes = hex::decode(&keys.kyber_secret_hex).unwrap_or_default();
    let kyber_sk = kyber768::SecretKey::from_bytes(&sk_bytes).unwrap();

    for tx in all_txs {
        if tx.stealth_address == format!("COINBASE_{}", keys.watt_address) {
            if let Ok(amt) = tx.aes_vault.parse::<i64>() {
                balance_flames += amt;
            }
        }

        if tx.stealth_address.starts_with("pq_watt_") {
            if let Ok(capsule_bytes) = hex::decode(&tx.kyber_capsule) {
                if let Ok(ciphertext) = pqcrypto_kyber::kyber768::Ciphertext::from_bytes(&capsule_bytes) {
                    let shared_secret = pqcrypto_kyber::kyber768::decapsulate(&ciphertext, &kyber_sk);
                    
                    if let Ok(vault_bytes) = hex::decode(&tx.aes_vault) {
                        if vault_bytes.len() > 12 {
                            let nonce = Nonce::from_slice(&vault_bytes[0..12]);
                            let aes_key = Key::<Aes256Gcm>::from_slice(shared_secret.as_bytes());
                            let cipher = Aes256Gcm::new(aes_key);
                            
                            if let Ok(plaintext) = cipher.decrypt(nonce, &vault_bytes[12..]) {
                                if let Ok(payload_str) = String::from_utf8(plaintext) {
                                    let parts: Vec<&str> = payload_str.split('|').collect();
                                    if parts.len() == 2 {
                                        if let Ok(amt) = parts[0].parse::<i64>() {
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

    if let Ok(spends) = fs::read_to_string(".wattcoin_spends") {
        for line in spends.lines() {
            if let Ok(spent_amt) = line.parse::<i64>() {
                balance_flames -= spent_amt;
            }
        }
    }

    Ok(balance_flames as f64 / 1_000_000_000.0)
}

#[tauri::command]
async fn get_btc_balance(master_seed_hex: String) -> Result<f64, String> {
    tokio::task::spawn_blocking(move || {
        use bdk::bitcoin::Network as BdkNetwork;
        use bdk::bitcoin::bip32::ExtendedPrivKey as BdkXpriv;
        use bdk::blockchain::{EsploraBlockchain};
        use bdk::{Wallet, SyncOptions};
        use bdk::database::MemoryDatabase;

        let seed = hex::decode(&master_seed_hex).map_err(|_| "Erreur Seed".to_string())?;
        let xprv = BdkXpriv::new_master(BdkNetwork::Testnet, &seed).map_err(|e| e.to_string())?;
        
        let desc = format!("wpkh({}/84'/1'/0'/0/*)", xprv);
        let change_desc = format!("wpkh({}/84'/1'/0'/1/*)", xprv);

        let wallet = Wallet::new(&desc, Some(&change_desc), BdkNetwork::Testnet, MemoryDatabase::default())
            .map_err(|e| e.to_string())?;

        let blockchain = EsploraBlockchain::new("https://mempool.space/testnet/api", 20);
        
        wallet.sync(&blockchain, SyncOptions::default()).map_err(|e| e.to_string())?;

        let balance = wallet.get_balance().map_err(|e| e.to_string())?;
        let total_sats = balance.confirmed + balance.untrusted_pending + balance.trusted_pending;
        
        Ok(total_sats as f64 / 100_000_000.0)
    })
    .await
    .unwrap_or_else(|_| Err("Erreur critique du thread".to_string()))
}

#[tauri::command]
async fn send_wattcoin(
    recipient_kyber_hex: String, 
    amount: f64, 
    sender_dilithium_secret_hex: String,
    sender_dilithium_public_hex: String
) -> Result<String, String> {
    use pqcrypto_kyber::kyber768;
    use pqcrypto_dilithium::dilithium3;
    use rand::Rng;
    use rand::seq::SliceRandom;

    let recipient_bytes = hex::decode(&recipient_kyber_hex).map_err(|_| "Adresse invalide".to_string())?;
    let bob_pk = kyber768::PublicKey::from_bytes(&recipient_bytes).map_err(|_| "Clé Kyber corrompue".to_string())?;

    let (alice_shared_secret, kyber_capsule) = kyber768::encapsulate(&bob_pk);

    let mut one_time_private_key = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut one_time_private_key);
    let stealth_address = format!("pq_watt_{}", hex::encode(&one_time_private_key[0..8]));

    let amount_in_flames = (amount * 1_000_000_000.0) as u64; 
    let fee: u64 = 1000;

    let payload = format!("{}|{}", amount_in_flames, hex::encode(one_time_private_key));

    let aes_key = Key::<Aes256Gcm>::from_slice(alice_shared_secret.as_bytes());
    let cipher = Aes256Gcm::new(aes_key);
    let mut nonce_bytes = [0u8; 12]; rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let encrypted_data = cipher.encrypt(nonce, payload.as_bytes()).unwrap();

    let mut final_vault = nonce_bytes.to_vec();
    final_vault.extend_from_slice(&encrypted_data);
    let aes_vault = hex::encode(final_vault);

    let blinding_factor = rand::thread_rng().gen_range(100..9999);
    let lattice_commitment = LatticeCommitment::commit(amount_in_flames, blinding_factor);

    let sk_bytes = hex::decode(&sender_dilithium_secret_hex).unwrap();
    let dilithium_secret = dilithium3::SecretKey::from_bytes(&sk_bytes).unwrap();
    
    let tx_data_to_sign = format!("{}{}{}{}{}", stealth_address, hex::encode(kyber_capsule.as_bytes()), aes_vault, lattice_commitment.c2, fee);
    let dilithium_signature = dilithium3::sign(tx_data_to_sign.as_bytes(), &dilithium_secret);

    let decoy_url = format!("{}/get_decoys/10", NODE_URL);
    let mut pq_ring: Vec<String> = Vec::new();
    let client = reqwest::Client::new(); 

    if let Ok(res) = client.get(&decoy_url).send().await {
        if let Ok(real_decoys) = res.json::<Vec<String>>().await {
            pq_ring.extend(real_decoys);
        }
    }

    while pq_ring.len() < 10 {
        let (fake_pk, _) = dilithium3::keypair();
        pq_ring.push(hex::encode(fake_pk.as_bytes()));
    }

    pq_ring.push(sender_dilithium_public_hex.clone());
    pq_ring.shuffle(&mut rand::thread_rng());

    // ==========================================================
    // 🧠 LE MOTEUR DE GÉNÉRATION LATTICE (Post-Quantique)
    // ==========================================================
    let my_real_index = pq_ring.iter().position(|r| r == &sender_dilithium_public_hex).unwrap();

    let n = pq_ring.len();
    let mut z_responses = vec![vec![0u32; LATTICE_DIM]; n];
    let mut p_keys = vec![vec![0u32; LATTICE_DIM]; n]; // 💡 Le conteneur pour les clés P
    let mut challenges_c = vec![vec![0u8; 32]; n];
    
    // 1. Notre vraie clé Lattice (S et P)
    let mut s_vector = [0u32; LATTICE_DIM];
    let mut my_p = vec![0u32; LATTICE_DIM];
    for j in 0..LATTICE_DIM {
        let offset = j * 4;
        s_vector[j] = u32::from_le_bytes(sk_bytes[offset..offset+4].try_into().unwrap_or([0; 4])) % LATTICE_Q;
        let base_g = (j as u32 + 1) * 1337;
        my_p[j] = ((s_vector[j] as u64 * base_g as u64) % LATTICE_Q as u64) as u32; // P = s * G
    }
    p_keys[my_real_index] = my_p;

    let mut alpha = vec![0u32; LATTICE_DIM];
    for j in 0..LATTICE_DIM {
        alpha[j] = rand::thread_rng().gen_range(0..LATTICE_Q);
    }

    let mut current_index = my_real_index;
    
    // L'Oracle pour le premier Hachage
    let mut hasher = blake3::Hasher::new();
    hasher.update(tx_data_to_sign.as_bytes());
    hasher.update(pq_ring[my_real_index].as_bytes()); // 💡 Liaison forte
    for j in 0..LATTICE_DIM {
        let base_g = (j as u32 + 1) * 1337; 
        let r_val = (alpha[j] as u64 * base_g as u64) % LATTICE_Q as u64;
        hasher.update(&(r_val as u32).to_le_bytes());
    }
    
    let mut next_c = hasher.finalize().as_bytes().to_vec();
    challenges_c[(current_index + 1) % n] = next_c.clone();

    // 3. Boucle sur les LEURRES
    for _ in 1..n {
        current_index = (current_index + 1) % n;
        let pk_hex = &pq_ring[current_index];
        
        // On génère des leurres parfaits
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
        hasher_sim.update(pk_hex.as_bytes()); // 💡 Liaison forte
        for val in r_i {
            hasher_sim.update(&val.to_le_bytes());
        }
        
        next_c = hasher_sim.finalize().as_bytes().to_vec();
        challenges_c[(current_index + 1) % n] = next_c.clone();
    }

    // 4. LA FERMETURE DU COLLIER (L'Équation est enfin possible !)
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
        key_image: hex::encode(blake3::hash(&sk_bytes).as_bytes()),
        c0: hex::encode(&challenges_c[0]),
        z_responses,
        p_keys, // 💡 Les clés sont transmises !
    };

    let tx_pq = TransactionPQ {
        stealth_address,
        kyber_capsule: hex::encode(kyber_capsule.as_bytes()),
        aes_vault,
        lattice_commitment, 
        fee,
        pq_ring_inputs: pq_ring,
        pq_ring_signature: Some(lattice_signature),
        dilithium_signature: hex::encode(dilithium_signature.as_bytes()),
    };

    let url = format!("{}/send_tx", NODE_URL); 
    let res = client.post(&url) 
        .json(&tx_pq).send().await.map_err(|_| "Nœud injoignable !".to_string())?;

    if res.status().is_success() {
        use std::fs::OpenOptions;
        use std::io::Write;
        let total_spend = amount_in_flames + fee;
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(".wattcoin_spends") {
            writeln!(file, "{}", total_spend).unwrap();
        }
        Ok(format!("☢️ TX POST-QUANTIQUE ENVOYÉE !\n\nPoids : {} octets\nFantômes impliqués : {}", serde_json::to_string(&tx_pq).unwrap().len(), tx_pq.pq_ring_inputs.len()))
    } else {
        Err("❌ Transaction rejetée.".to_string())
    }
}

#[tauri::command]
async fn create_btc_htlc(
    master_seed_hex: String, 
    hash_hex: String, 
    locktime: u32
) -> Result<String, String> {
    
    let seed = hex::decode(&master_seed_hex).map_err(|_| "Erreur Seed".to_string())?;
    let root = Xpriv::new_master(Network::Testnet, &seed).map_err(|e| e.to_string())?;
    let path = DerivationPath::from_str("m/84'/1'/0'/0/0").unwrap();
    let secp = Secp256k1::new();
    let child = root.derive_priv(&secp, &path).map_err(|e| e.to_string())?;
    let priv_key = PrivateKey::new(child.private_key, Network::Testnet);
    let buyer_pubkey = priv_key.public_key(&secp);

    let dummy_seller_hex = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
    let seller_pubkey = bitcoin::PublicKey::from_str(dummy_seller_hex).unwrap();

    let htlc_hash = sha256::Hash::from_str(&hash_hex).map_err(|e| e.to_string())?;

    let htlc_script = Builder::new()
        .push_opcode(OP_IF)
            .push_opcode(OP_SHA256)
            .push_slice(&htlc_hash.to_byte_array())
            .push_opcode(OP_EQUALVERIFY)
            .push_key(&buyer_pubkey)
        .push_opcode(OP_ELSE)
            .push_int(locktime as i64)
            .push_opcode(OP_CSV)
            .push_opcode(OP_DROP)
            .push_key(&seller_pubkey)
        .push_opcode(OP_ENDIF)
        .push_opcode(OP_CHECKSIG)
        .into_script();

    let address = Address::p2wsh(&htlc_script, Network::Testnet);
    
    Ok(address.to_string())
}

#[tauri::command]
async fn send_btc_to_htlc(
    master_seed_hex: String, 
    htlc_address: String, 
    amount_btc: f64
) -> Result<String, String> {
    tokio::task::spawn_blocking(move || {
        use bdk::bitcoin::Network as BdkNetwork;
        use bdk::bitcoin::bip32::ExtendedPrivKey as BdkXpriv;
        use bdk::bitcoin::Address as BdkAddress;
        use bdk::blockchain::{EsploraBlockchain, Blockchain};
        use bdk::{Wallet, SyncOptions, SignOptions, FeeRate};
        use bdk::database::MemoryDatabase;
        use std::str::FromStr;

        let seed = hex::decode(&master_seed_hex).map_err(|_| "Erreur Seed".to_string())?;
        
        let xprv = BdkXpriv::new_master(BdkNetwork::Testnet, &seed).map_err(|e| e.to_string())?;
        
        let desc = format!("wpkh({}/84'/1'/0'/0/*)", xprv);
        let change_desc = format!("wpkh({}/84'/1'/0'/1/*)", xprv);

        let wallet = Wallet::new(
            &desc, 
            Some(&change_desc), 
            BdkNetwork::Testnet, 
            MemoryDatabase::default()
        ).map_err(|e| format!("Erreur Init Wallet: {}", e))?;

        println!("⏳ [BDK] Scan de la blockchain...");
        let blockchain = EsploraBlockchain::new("https://mempool.space/testnet/api", 20);
        wallet.sync(&blockchain, SyncOptions::default()).map_err(|e| format!("Erreur Sync: {}", e))?;

        let target_address = BdkAddress::from_str(&htlc_address).map_err(|_| "Adresse HTLC invalide".to_string())?;
        let amount_sats = (amount_btc * 100_000_000.0) as u64;

        println!("🛠️ [BDK] Construction de la TX...");
        let (mut psbt, _details) = {
            let mut builder = wallet.build_tx();
            builder.add_recipient(target_address.payload.script_pubkey(), amount_sats);
            builder.fee_rate(FeeRate::from_sat_per_vb(2.0)); 
            builder.finish().map_err(|e| format!("Erreur TX Builder: {}", e))?
        };

        println!("✍️ [BDK] Signature SegWit...");
        let finalized = wallet.sign(&mut psbt, SignOptions::default()).map_err(|e| e.to_string())?;
        if !finalized {
            return Err("❌ BDK n'a pas pu signer.".to_string());
        }

        let raw_tx = psbt.extract_tx();
        let txid = raw_tx.txid();
        println!("🚀 [BDK] Diffusion...");
        blockchain.broadcast(&raw_tx).map_err(|e| format!("Erreur Broadcast: {}", e))?;

        Ok(format!("✅ TX Bitcoin validée !\nTXID: {}", txid))
    })
    .await
    .unwrap_or_else(|_| Err("Erreur critique du thread BDK".to_string()))
}
    
#[tauri::command]
async fn claim_wattcoin_swap(
    secret: String, 
    hash: String, 
    watt_address: String, 
    amount: f64
) -> Result<String, String> {
    let secret_bytes = hex::decode(&secret).unwrap_or_default();
    let calculated_hash = hex::encode(blake3::hash(&secret_bytes).as_bytes());
    if calculated_hash != hash {
        return Err("❌ Le secret révélé par le DEX est un faux !".to_string());
    }

    let amount_in_flames = (amount * 1_000_000_000.0) as u64; 
    let lattice_commitment = LatticeCommitment::commit(amount_in_flames, 0);

    let claim_tx = TransactionPQ {
        stealth_address: watt_address,
        kyber_capsule: format!("HTLC_{}", &hash[0..16]),
        aes_vault: hash.clone(), 
        lattice_commitment,
        fee: 0,
        pq_ring_inputs: vec!["HTLC_CONTRACT".to_string()], 
        pq_ring_signature: None, // 💡 HTLC n'utilise pas l'anneau Lattice
        dilithium_signature: secret, 
    };

    let client = reqwest::Client::new();
    let url = format!("{}/send_tx", NODE_URL);
    let res = client.post(&url) 
        .json(&claim_tx).send().await.map_err(|_| "Nœud injoignable !".to_string())?;

    if res.status().is_success() {
        Ok(format!("🎉 ATOMIC SWAP RÉUSSI ! Le réseau a validé votre Secret HTLC et débloqué {} WATT !", amount))
    } else {
        Err("❌ Le Tribunal a rejeté votre Secret !".to_string())
    }
}

#[tauri::command]
fn destroy_vault() -> Result<String, String> {
    use std::fs;
    use std::path::Path;
    
    if Path::new(VAULT_FILE).exists() {
        fs::remove_file(VAULT_FILE).map_err(|_| "⚠️ Impossible de supprimer le coffre.".to_string())?;
        Ok("🗑️ Coffre-fort nucléarisé avec succès. Adieu !".to_string())
    } else {
        Ok("Le coffre était déjà vide.".to_string())
    }
}


fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let app_handle = app.handle().clone();
            
            tauri::async_runtime::spawn(async move {
                let client = reqwest::Client::new();
                let mut last_blocks = 0;
                
                loop {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    let url = format!("{}/info", NODE_URL);
                    
                    if let Ok(res) = client.get(&url).send().await {
                        if let Ok(info) = res.json::<serde_json::Value>().await {
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
            generate_pro_wallet, encrypt_vault, unlock_vault, vault_exists,
            submit_order, get_dark_pool, resolve_batch, get_watt_balance, get_btc_balance,
            send_wattcoin, create_btc_htlc, send_btc_to_htlc, claim_wattcoin_swap,
            destroy_vault
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}