use serde::{Serialize, Deserialize};
use crate::lattice::LatticeCommitment;

const LATTICE_Q: u32 = 8380417; 
const LATTICE_DIM: usize = 4;   

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PQLatticeRingSignature {
    pub key_image: String,          
    pub c0: String,                 
    pub z_responses: Vec<Vec<u32>>, 
    pub p_keys: Vec<Vec<u32>>, 
}

// 📦 L'ARGENT QUI ENTRE (Dépense d'un ancien UTXO)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInput {
    pub pq_ring_inputs: Vec<String>, // Les leurres + la vraie clé
    pub pq_ring_signature: PQLatticeRingSignature, // La preuve d'appartenance
    pub commitment: LatticeCommitment, // L'engagement mathématique de la somme dépensée
}

// 🎁 L'ARGENT QUI SORT (Le destinataire ET ton rendu de monnaie)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionOutput {
    pub stealth_address: String,      
    pub kyber_capsule: String,        
    pub aes_vault: String,            
    pub lattice_commitment: LatticeCommitment, // L'engagement de la nouvelle pièce
}

// 🔄 LA NOUVELLE TRANSACTION ZKP
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub is_coinbase: bool, // Vrai pour le Genesis et les Mineurs
    pub inputs: Vec<TransactionInput>,  
    pub outputs: Vec<TransactionOutput>, 
    pub fee: u64,                     
    pub dilithium_signature: String,  // Optionnel/Prunable
}

impl Transaction {
    pub fn is_valid(&self) -> bool {
        // 1. Passe-droit exclusif pour la création monétaire (Miner/Genesis)
        if self.is_coinbase || self.dilithium_signature == "PRUNED" { 
            return true; 
        }

        // =================================================================
        // ⚖️ 2. L'ILLUSION HOMOMORPHE (Vérification Zero-Knowledge ZKP)
        // =================================================================
        let mut sum_inputs_c2 = 0u64;
        for input in &self.inputs {
            sum_inputs_c2 = (sum_inputs_c2 + input.commitment.c2) % (LATTICE_Q as u64);
        }

        let mut sum_outputs_c2 = 0u64;
        for out in &self.outputs {
            sum_outputs_c2 = (sum_outputs_c2 + out.lattice_commitment.c2) % (LATTICE_Q as u64);
        }

        // Les frais sont en clair (r = 0), on les convertit dans le champ Lattice
        let fee_commitment_c2 = self.fee % (LATTICE_Q as u64);
        let required_output_sum = (sum_outputs_c2 + fee_commitment_c2) % (LATTICE_Q as u64);

        // LE COUPERET : On empêche la création magique de WATT !
        if sum_inputs_c2 != required_output_sum {
            println!("🛑 REJET ZKP : Les montants cachés ne s'équilibrent pas. Création de fausse monnaie détectée !");
            return false;
        }

        // =================================================================
        // 🌀 3. L'ÉPREUVE DU CERCLE LATTICE (Pour CHAQUE Input)
        // =================================================================
        
        // On hache les outputs pour figer la transaction (Oracle Fiat-Shamir)
        let tx_data = format!("{:?}{}", self.outputs, self.fee);

        for input in &self.inputs {
            let n = input.pq_ring_inputs.len();
            let pq_ring = &input.pq_ring_signature;
            
            if n == 0 || pq_ring.z_responses.len() != n || pq_ring.p_keys.len() != n {
                return false;
            }

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
                for val in r_i {
                    hasher.update(&val.to_le_bytes());
                }
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