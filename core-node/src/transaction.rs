use serde::{Serialize, Deserialize};
use crate::lattice::LatticeCommitment;

const LATTICE_Q: u32 = 8380417; 
const LATTICE_DIM: usize = 4;   

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PQLatticeRingSignature {
    pub key_image: String,          
    pub c0: String,                 
    pub z_responses: Vec<Vec<u32>>, 
    pub p_keys: Vec<Vec<u32>>, // 💡 NOUVEAU : Les vraies clés Lattice algébriques
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub stealth_address: String,      
    pub kyber_capsule: String,        
    pub aes_vault: String,            
    pub lattice_commitment: LatticeCommitment, 
    pub fee: u64,                     
    pub pq_ring_inputs: Vec<String>,  
    pub pq_ring_signature: Option<PQLatticeRingSignature>, 
    pub dilithium_signature: String,  
}

impl Transaction {
    pub fn is_valid(&self) -> bool {
        if self.stealth_address == "GENESIS" || self.stealth_address.starts_with("COINBASE_") || self.dilithium_signature == "PRUNED" { 
            return true; 
        }

        let tx_data = format!("{}{}{}{}{}", self.stealth_address, self.kyber_capsule, self.aes_vault, self.lattice_commitment.c2, self.fee);

        if let Some(pq_ring) = &self.pq_ring_signature {
            let n = self.pq_ring_inputs.len();
            if n == 0 || pq_ring.z_responses.len() != n || pq_ring.p_keys.len() != n {
                return false;
            }

            let mut current_c = hex::decode(&pq_ring.c0).unwrap_or_default();

            for i in 0..n {
                let pk_hex = &self.pq_ring_inputs[i];
                let z_vec = &pq_ring.z_responses[i];
                let p_vector = &pq_ring.p_keys[i]; // 💡 On utilise la vraie clé Lattice

                let c_i = u32::from_le_bytes(current_c[0..4].try_into().unwrap_or([0;4])) % LATTICE_Q;

                // R_i = (Z_i * G) + (C_i * P_i) mod Q
                let mut r_i = vec![0u32; LATTICE_DIM];
                for j in 0..LATTICE_DIM {
                    let base_g = (j as u32 + 1) * 1337; 
                    let part1 = (z_vec[j] as u64 * base_g as u64) % LATTICE_Q as u64;
                    let part2 = (c_i as u64 * p_vector[j] as u64) % LATTICE_Q as u64;
                    r_i[j] = ((part1 + part2) % LATTICE_Q as u64) as u32;
                }

                // Hachage Fiat-Shamir strict
                let mut hasher = blake3::Hasher::new();
                hasher.update(tx_data.as_bytes());
                hasher.update(pk_hex.as_bytes()); // 💡 On lie Dilithium à la preuve !
                for val in r_i {
                    hasher.update(&val.to_le_bytes());
                }
                current_c = hasher.finalize().as_bytes().to_vec();
            }

            if hex::encode(&current_c) == pq_ring.c0 {
                return true; 
            } else {
                println!("🛑 REJET : L'équation du réseau euclidien (Lattice) est brisée !");
                return false;
            }
        }
        false
    }
}