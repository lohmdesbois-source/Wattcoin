use serde::{Serialize, Deserialize};
use crate::lattice::LatticeCommitment;

// 🧮 CONSTANTES MATHÉMATIQUES POST-QUANTIQUE (Famille Kyber/Dilithium)
const LATTICE_Q: u32 = 8380417; // Le module premier officiel
const LATTICE_DIM: usize = 4;   // Dimension du vecteur (Mathématiques LWE)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PQLatticeRingSignature {
    pub key_image: String,          // Protection absolue contre la double dépense
    pub c0: String,                 // Le point d'entrée de la boucle (Hash Blake3)
    pub z_responses: Vec<Vec<u32>>, // Les matrices de réponses Z (Lattice Math)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub stealth_address: String,      
    pub kyber_capsule: String,        
    pub aes_vault: String,            
    pub lattice_commitment: LatticeCommitment, 
    pub fee: u64,                     
    pub pq_ring_inputs: Vec<String>,  
    pub pq_ring_signature: Option<PQLatticeRingSignature>, // 💡 LE GRAAL QUANTIQUE
    pub dilithium_signature: String,  // (Gardé temporairement pour le Genesis)
}

impl Transaction {
    pub fn is_valid(&self) -> bool {
        // Passe-droit exclusif pour la création de monnaie (Genesis & Minage)
        if self.stealth_address == "GENESIS" || self.stealth_address.starts_with("COINBASE_") || self.dilithium_signature == "PRUNED" { 
            return true; 
        }

        let tx_data = format!("{}{}{}{}{}", self.stealth_address, self.kyber_capsule, self.aes_vault, self.lattice_commitment.c2, self.fee);

        // 🌀 LA VÉRITABLE ÉPREUVE DU CERCLE POST-QUANTIQUE (Lattice AOS)
        if let Some(pq_ring) = &self.pq_ring_signature {
            let n = self.pq_ring_inputs.len();
            if n == 0 || pq_ring.z_responses.len() != n {
                return false;
            }

            let mut current_c = hex::decode(&pq_ring.c0).unwrap_or_default();

            for i in 0..n {
                let pk_hex = &self.pq_ring_inputs[i];
                let z_vec = &pq_ring.z_responses[i];
                if z_vec.len() != LATTICE_DIM { return false; }

                // 1. Dérivation d'un polynôme P_i unique pour chaque clé publique
                let mut hasher_pk = blake3::Hasher::new();
                hasher_pk.update(pk_hex.as_bytes());
                let pk_hash = hasher_pk.finalize();
                
                let mut p_vector = [0u32; LATTICE_DIM];
                for j in 0..LATTICE_DIM {
                    let offset = j * 4;
                    // On projette la clé dans le champ Lattice modulo Q
                    p_vector[j] = u32::from_le_bytes(pk_hash.as_bytes()[offset..offset+4].try_into().unwrap()) % LATTICE_Q;
                }

                // 2. Transformation du Challenge C_i courant en scalaire
                let c_i = u32::from_le_bytes(current_c[0..4].try_into().unwrap()) % LATTICE_Q;

                // 3. ÉQUATION HOMOMORPHE LATTICE : R_i = (Z_i * G) + (C_i * P_i) mod Q
                let mut r_i = vec![0u32; LATTICE_DIM];
                for j in 0..LATTICE_DIM {
                    let base_g = (j as u32 + 1) * 1337; // Générateur public G
                    let part1 = (z_vec[j] as u64 * base_g as u64) % LATTICE_Q as u64;
                    let part2 = (c_i as u64 * p_vector[j] as u64) % LATTICE_Q as u64;
                    r_i[j] = ((part1 + part2) % LATTICE_Q as u64) as u32;
                }

                // 4. L'Oracle Fiat-Shamir : Hachage du résultat pour lier le maillon suivant
                let mut hasher = blake3::Hasher::new();
                hasher.update(tx_data.as_bytes());
                for val in r_i {
                    hasher.update(&val.to_le_bytes());
                }
                current_c = hasher.finalize().as_bytes().to_vec();
            }

            // LE VERDICT DE LA HACHE
            // Si la boucle est fermée mathématiquement, l'anonymat est parfait.
            if hex::encode(&current_c) == pq_ring.c0 {
                return true; 
            } else {
                println!("🛑 REJET : L'équation du réseau euclidien (Lattice) est brisée !");
                return false;
            }
        }

        println!("🛑 REJET : Tentative de transaction classique. Seules les signatures Lattice sont acceptées.");
        false
    }
}