use serde::{Serialize, Deserialize};
use crate::lattice::LatticeCommitment;

const LATTICE_Q: u32 = 8380417; 
const LATTICE_DIM: usize = 4;   

// 💡 NOUVEAU : Le typage strict des transactions !
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionType {
    Coinbase,
    Standard,
    HTLCLock { hash: String, timeout_block: u64 }, // Bloque les fonds avec un Hash
    HTLCClaim { secret: String },                  // Débloque les fonds avec le Secret
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PQLatticeRingSignature {
    pub key_image: String,          
    pub c0: String,                 
    pub z_responses: Vec<Vec<u32>>, 
    pub p_keys: Vec<Vec<u32>>, 
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInput {
    pub pq_ring_inputs: Vec<String>,
    pub pq_ring_signature: PQLatticeRingSignature,
    pub commitment: LatticeCommitment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionOutput {
    pub stealth_address: String,      
    pub kyber_capsule: String,        
    pub aes_vault: String,            
    pub lattice_commitment: LatticeCommitment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub tx_type: TransactionType, // 💡 FINI le 'is_coinbase' booléen !
    pub inputs: Vec<TransactionInput>,  
    pub outputs: Vec<TransactionOutput>, 
    pub fee: u64,                     
    pub dilithium_signature: String,  
}

impl Transaction {
    pub fn is_valid(&self) -> bool {
        if self.tx_type == TransactionType::Coinbase || self.dilithium_signature == "PRUNED" { 
            return true; 
        }

        // =================================================================
        // 🔐 1. LE TRIBUNAL DES CONTRATS INTELLIGENTS (HTLC)
        // =================================================================
        if let TransactionType::HTLCClaim { secret } = &self.tx_type {
            if secret.is_empty() { 
                println!("🛑 REJET HTLC : Le secret est vide !");
                return false; 
            }
            
            // On vérifie que la transaction tente bien de dépenser un UTXO
            if self.inputs.is_empty() {
                println!("🛑 REJET HTLC : Aucune pièce bloquée n'est ciblée en Input.");
                return false;
            }

            // On vérifie le contrat mathématique
            let secret_bytes = hex::decode(secret).unwrap_or_default();
            let calculated_hash = hex::encode(blake3::hash(&secret_bytes).as_bytes());

            // Pour valider le contrat, le secret révélé DOIT correspondre au Vault (Hash) 
            // stocké dans l'UTXO précédent (qu'on a récupéré lors de la création de la Tx).
            // Le DEX inscrira le hash du lock ciblé dans 'dilithium_signature' pour le contrôle
            if calculated_hash != self.dilithium_signature {
                println!("🛑 REJET HTLC FATAL : Le secret révélé '{}' ne produit pas le Hash d'origine ! Le voleur est repoussé.", secret);
                return false;
            }
            return true; // Le contrat est rempli ! Pas besoin du ZKP standard pour un Claim direct.
        }

        if let TransactionType::HTLCLock { hash, timeout_block: _ } = &self.tx_type {
            if hash.len() != 64 { // Un hash Blake3 fait 64 caractères hexadécimaux
                println!("🛑 REJET HTLC : Le Hash du contrat est invalide.");
                return false;
            }
            // Le Lock suit ensuite les règles ZKP normales ci-dessous pour cacher le montant bloqué
        }

        // =================================================================
        // ⚖️ 2. L'ILLUSION HOMOMORPHE (ZKP)
        // =================================================================
        let mut sum_inputs_c2 = 0u64;
        for input in &self.inputs { sum_inputs_c2 = (sum_inputs_c2 + input.commitment.c2) % (LATTICE_Q as u64); }

        let mut sum_outputs_c2 = 0u64;
        for out in &self.outputs { sum_outputs_c2 = (sum_outputs_c2 + out.lattice_commitment.c2) % (LATTICE_Q as u64); }

        let fee_commitment_c2 = self.fee % (LATTICE_Q as u64);
        let required_output_sum = (sum_outputs_c2 + fee_commitment_c2) % (LATTICE_Q as u64);

        if sum_inputs_c2 != required_output_sum {
            println!("🛑 REJET ZKP : Les montants cachés ne s'équilibrent pas !");
            return false;
        }

        // =================================================================
        // 🌀 3. L'ÉPREUVE DU CERCLE LATTICE
        // =================================================================
        let tx_data = format!("{:?}{}", self.outputs, self.fee);

        for input in &self.inputs {
            let n = input.pq_ring_inputs.len();
            let pq_ring = &input.pq_ring_signature;
            
            if n == 0 || pq_ring.z_responses.len() != n || pq_ring.p_keys.len() != n { return false; }

            let mut current_c = hex::decode(&pq_ring.c0).unwrap_or_default();

            for i in 0..n {
                let pk_hex = &input.pq_ring_inputs[i];
                let z_vec = &pq_ring.z_responses[i];
                let p_vector = &pq_ring.p_keys[i]; 

                let c_i = u32::from_le_bytes(current_c[0..4].try_into().unwrap_or([0;4])) % LATTICE_Q;

                let mut r_i = vec![0u32; LATTICE_DIM];
                for j in 0..LATTICE_DIM {
                    let base_g = (j as u32 + 1) * 1337; 
                    let part1 = (z_vec[j] as u64 * base_g as u64) % LATTICE_Q as u64;
                    let part2 = (c_i as u64 * p_vector[j] as u64) % LATTICE_Q as u64;
                    r_i[j] = ((part1 + part2) % LATTICE_Q as u64) as u32;
                }

                let mut hasher = blake3::Hasher::new();
                hasher.update(tx_data.as_bytes());
                hasher.update(pk_hex.as_bytes()); 
                for val in r_i { hasher.update(&val.to_le_bytes()); }
                current_c = hasher.finalize().as_bytes().to_vec();
            }

            if hex::encode(&current_c) != pq_ring.c0 {
                println!("🛑 REJET : L'équation du réseau euclidien est brisée sur l'un des Inputs !");
                return false;
            }
        }
        true 
    }
}