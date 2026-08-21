#![allow(dead_code)]
use sha2::{Sha512, Digest};
use rand::{RngCore, thread_rng};
use serde::{Serialize, Deserialize};

// 64 chaînes pour le message (SHA-512) + 2 chaînes pour le Checksum (u16)
const MSG_LEN: usize = 64;
const CHECKSUM_LEN: usize = 2;
const WOTS_LEN: usize = MSG_LEN + CHECKSUM_LEN; // 66 chaînes au total

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WotsSignature {
    pub chains: Vec<String>, 
}

#[derive(Clone, Serialize, Deserialize)]
pub struct WotsKeyPair {
    pub secret_key: Vec<Vec<u8>>,
    pub public_seed: [u8; 32], // LE POIVRE : Rend chaque adresse unique face aux Rainbow Tables
    pub public_key: String,    // Format: "hex(public_seed)_hex(pk_hash)"
}

impl WotsKeyPair {
    /// Fonction de hachage tweakée (Domain Separation)
    /// Rend chaque coup de hachage absolument unique.
    fn hash_step(input: &[u8], public_seed: &[u8; 32], chain_index: usize, hash_index: u8) -> Vec<u8> {
        let mut hasher = Sha512::new();
        hasher.update(input);
        hasher.update(public_seed); // Lie le hash à cette adresse précise
        hasher.update(&(chain_index as u16).to_be_bytes()); // Lie le hash à sa colonne
        hasher.update(&[hash_index]); // Lie le hash à sa hauteur
        hasher.finalize().to_vec()
    }

    /// 1. Génère une paire de clés inviolable
    pub fn generate() -> Self {
        let mut sk = vec![vec![0u8; 64]; WOTS_LEN];
        let mut pk_chains = Vec::new();
        let mut rng = thread_rng();

        let mut public_seed = [0u8; 32];
        rng.fill_bytes(&mut public_seed);

        for i in 0..WOTS_LEN {
            rng.fill_bytes(&mut sk[i]);
            
            let mut current = sk[i].clone();
            // On monte tout en haut de la chaîne (256 itérations)
            for j in 0..256 {
                current = Self::hash_step(&current, &public_seed, i, j as u8);
            }
            pk_chains.push(current);
        }

        // L'empreinte finale
        let mut pk_hasher = Sha512::new();
        for chain in pk_chains {
            pk_hasher.update(&chain);
        }
        let pk_hash = hex::encode(pk_hasher.finalize());
        
        // La clé publique contient maintenant le poivre public + le hash final
        let public_key = format!("{}_{}", hex::encode(public_seed), pk_hash);

        WotsKeyPair { secret_key: sk, public_seed, public_key }
    }

    /// 2. Signe le hash d'un message + son CHECKSUM
    pub fn sign(secret_key: &[Vec<u8>], public_seed: &[u8; 32], message_hash: &[u8; 64]) -> WotsSignature {
        // 🛡️ CALCUL DU CHECKSUM (La somme des hachages restants)
        let mut checksum: u16 = 0;
        for byte in message_hash {
            checksum += 256 - (*byte as u16);
        }
        let checksum_bytes = checksum.to_be_bytes(); // 2 octets

        // On fusionne le message et le checksum
        let mut target_data = message_hash.to_vec();
        target_data.extend_from_slice(&checksum_bytes); // Taille totale : 66 octets

        let mut sig_chains = Vec::new();

        for i in 0..WOTS_LEN {
            let target_hash_count = target_data[i] as usize;
            let mut current = secret_key[i].clone();
            
            // On hache jusqu'à la valeur demandée
            for j in 0..target_hash_count {
                current = Self::hash_step(&current, public_seed, i, j as u8);
            }
            sig_chains.push(hex::encode(current));
        }

        WotsSignature { chains: sig_chains }
    }

    /// 3. Vérifie une signature (Le Tribunal Quantique)
    pub fn verify(public_key: &str, signature: &WotsSignature, message_hash: &[u8; 64]) -> bool {
        if signature.chains.len() != WOTS_LEN { return false; }

        // Extraction du poivre et du hash de la clé publique
        let parts: Vec<&str> = public_key.split('_').collect();
        if parts.len() != 2 { return false; }
        
        let public_seed_bytes = hex::decode(parts[0]).unwrap_or_default();
        if public_seed_bytes.len() != 32 { return false; }
        let mut public_seed = [0u8; 32];
        public_seed.copy_from_slice(&public_seed_bytes);
        
        let expected_pk_hash = parts[1];

        // RECALCUL DU CHECKSUM
        let mut checksum: u16 = 0;
        for byte in message_hash {
            checksum += 256 - (*byte as u16);
        }
        let checksum_bytes = checksum.to_be_bytes();
        let mut target_data = message_hash.to_vec();
        target_data.extend_from_slice(&checksum_bytes);

        let mut recovered_pk_chains = Vec::new();

        for i in 0..WOTS_LEN {
            let sig_chain_bytes = hex::decode(&signature.chains[i]).unwrap_or_default();
            if sig_chain_bytes.is_empty() { return false; }

            let target_hash_count = target_data[i] as usize;
            // Si le signataire a triché (ex: un octet > 255, ce qui est impossible en u8, mais par sécurité)
            if target_hash_count > 256 { return false; } 
            
            let remaining_hashes = 256 - target_hash_count;
            let mut current = sig_chain_bytes;
            
            // Le validateur reprend la chaîne EXACTEMENT là où le signataire s'est arrêté !
            for j in 0..remaining_hashes {
                let current_hash_index = (target_hash_count + j) as u8;
                current = Self::hash_step(&current, &public_seed, i, current_hash_index);
            }
            recovered_pk_chains.push(current);
        }

        let mut pk_hasher = Sha512::new();
        for chain in recovered_pk_chains {
            pk_hasher.update(&chain);
        }
        let recovered_pk = hex::encode(pk_hasher.finalize());

        recovered_pk == expected_pk_hash
    }
	
	/// Génération Déterministe (Portefeuille HD Cypherpunk)
    /// Recalcule instantanément et mathématiquement une clé WOTS unique
    /// à partir d'une Master Seed et d'un index d'utilisation.
    pub fn generate_deterministic(master_seed: &[u8; 32], key_index: u32) -> Self {
        let mut sk = vec![vec![0u8; 64]; WOTS_LEN];
        let mut pk_chains = Vec::new();

        // 1. Dérivation du "Poivre" (public_seed) unique pour CETTE clé
        let mut pepper_hasher = Sha512::new();
        pepper_hasher.update(master_seed);
        pepper_hasher.update(&key_index.to_be_bytes());
        pepper_hasher.update(b"PEPPER_DOMAIN"); // Séparation de domaine
        let pepper_hash = pepper_hasher.finalize();
        
        // On prend les 32 premiers octets du hash (qui en fait 64) pour le poivre
        let mut public_seed = [0u8; 32];
        public_seed.copy_from_slice(&pepper_hash[0..32]);

        // 2. Dérivation mathématique des 66 chaînes secrètes
        for i in 0..WOTS_LEN {
            let mut chain_hasher = Sha512::new();
            chain_hasher.update(master_seed);
            chain_hasher.update(&key_index.to_be_bytes());
            chain_hasher.update(&(i as u32).to_be_bytes());
            chain_hasher.update(b"SECRET_CHAIN_DOMAIN"); // Séparation de domaine
            
            let chain_secret = chain_hasher.finalize(); // Résultat = 64 octets (SHA-512)
            sk[i] = chain_secret.to_vec(); 
            
            // 3. Calcul de la chaîne publique (256 itérations) comme d'habitude
            let mut current = sk[i].clone();
            for j in 0..256 {
                current = Self::hash_step(&current, &public_seed, i, j as u8);
            }
            pk_chains.push(current);
        }

        // 4. L'empreinte finale de la clé publique
        let mut pk_hasher = Sha512::new();
        for chain in pk_chains {
            pk_hasher.update(&chain);
        }
        let pk_hash = hex::encode(pk_hasher.finalize());
        
        // La clé publique formatée
        let public_key = format!("{}_{}", hex::encode(public_seed), pk_hash);

        WotsKeyPair { secret_key: sk, public_seed, public_key }
    }
}