#![allow(dead_code)]
use sha2::{Sha512, Digest};
use rand::{RngCore, thread_rng};
use serde::{Serialize, Deserialize};

const WOTS_LEN: usize = 64; // Pour SHA-512, le digest fait 64 octets. On simplifie sans checksum complexe pour le L1.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WotsSignature {
    pub chains: Vec<String>, // 64 chaînes encodées en Hex
}

pub struct WotsKeyPair {
    pub secret_key: Vec<Vec<u8>>,
    pub public_key: String, // Le hash final de toutes les chaînes (empreinte)
}

impl WotsKeyPair {
    /// 1. Génère une paire de clés à usage unique
    pub fn generate() -> Self {
        let mut sk = vec![vec![0u8; 64]; WOTS_LEN];
        let mut pk_chains = Vec::new();
        let mut rng = thread_rng();

        for i in 0..WOTS_LEN {
            rng.fill_bytes(&mut sk[i]);
            
            // Pour la clé publique, on hache la clé privée 256 fois (valeur max d'un octet)
            let mut current = sk[i].clone();
            for _ in 0..256 {
                current = Sha512::digest(&current).to_vec();
            }
            pk_chains.push(current);
        }

        // L'adresse publique finale est le hash de toutes les chaînes publiques concaténées
        let mut pk_hasher = Sha512::new();
        for chain in pk_chains {
            pk_hasher.update(&chain);
        }
        let public_key = hex::encode(pk_hasher.finalize());

        WotsKeyPair { secret_key: sk, public_key }
    }

    /// 2. Signe le hash d'un message
    pub fn sign(secret_key: &[Vec<u8>], message_hash: &[u8; 64]) -> WotsSignature {
        let mut sig_chains = Vec::new();

        for i in 0..WOTS_LEN {
            let target_hash_count = message_hash[i] as usize;
            let mut current = secret_key[i].clone();
            
            // On hache 'x' fois, où 'x' est la valeur de l'octet du message
            for _ in 0..target_hash_count {
                current = Sha512::digest(&current).to_vec();
            }
            sig_chains.push(hex::encode(current));
        }

        WotsSignature { chains: sig_chains }
    }

    /// 3. Vérifie une signature
    pub fn verify(public_key: &str, signature: &WotsSignature, message_hash: &[u8; 64]) -> bool {
        if signature.chains.len() != WOTS_LEN { return false; }

        let mut recovered_pk_chains = Vec::new();

        for i in 0..WOTS_LEN {
            let sig_chain_bytes = hex::decode(&signature.chains[i]).unwrap_or_default();
            if sig_chain_bytes.is_empty() { return false; }

            let target_hash_count = message_hash[i] as usize;
            let remaining_hashes = 256 - target_hash_count;

            let mut current = sig_chain_bytes;
            // On complète les hachages manquants pour retrouver la clé publique
            for _ in 0..remaining_hashes {
                current = Sha512::digest(&current).to_vec();
            }
            recovered_pk_chains.push(current);
        }

        let mut pk_hasher = Sha512::new();
        for chain in recovered_pk_chains {
            pk_hasher.update(&chain);
        }
        let recovered_pk = hex::encode(pk_hasher.finalize());

        recovered_pk == public_key
    }
}