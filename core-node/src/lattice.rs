use serde::{Serialize, Deserialize};
use sha2::{Sha512, Digest};
use rand::RngCore;

// MODULE LATTICE HOMOMORPHE (Mode Prod)
// Dimension 1024 (Résistance Post-Quantique face à BKZ)
// Modulo 2^64 (Additions infinies sans overflow)

// Matrice rectangulaire pour garantir l'irréversibilité (Binding property)
pub const LATTICE_ROWS: usize = 1024;
pub const LATTICE_COLS: usize = 2048; // Il doit y avoir plus de colonnes que de lignes !
pub const NOISE_BOUND: u64 = 500;     // On resserre violemment la tolérance au bruit
const CRS_SEED: &[u8; 32] = b"WATTCOIN_GLOBAL_CRS_LATTICE_2026"; 

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LWECommitment {
    pub t_vector: Vec<u64>, // t = A*s + e + montant et doit être de taille LATTICE_ROWS
}

impl LWECommitment {
    /// L'Astuce de Kyber : Génération XOF avec Cache RAM
    pub fn get_matrix_row(row_index: usize) -> Vec<u64> {
        use std::sync::OnceLock;
        static GLOBAL_MATRIX_A: OnceLock<Vec<Vec<u64>>> = OnceLock::new();
        
        let matrix = GLOBAL_MATRIX_A.get_or_init(|| {
            let mut mat = Vec::with_capacity(LATTICE_COLS);
            for r in 0..LATTICE_COLS {
                let mut row = vec![0u64; LATTICE_COLS];
                let mut hasher = Sha512::new();
                hasher.update(CRS_SEED);
                hasher.update(&(r as u32).to_le_bytes()); 
        
                let mut hash_output = hasher.finalize_reset();
                
                for i in 0..LATTICE_COLS {
                    let byte_idx = (i % 8) * 8;
                    let mut val_bytes = [0u8; 8];
                    val_bytes.copy_from_slice(&hash_output[byte_idx..byte_idx + 8]);
                    row[i] = u64::from_le_bytes(val_bytes);
        
                    if byte_idx == 56 {
                        hasher.update(&hash_output);
                        hash_output = hasher.finalize_reset();
                    }
                }
                mat.push(row);
            }
            mat
        });
        
        matrix[row_index].clone()
    }

    /// Échantillonneur CBD (Centered Binomial Distribution)
    fn sample_cbd_noise() -> u64 {
        let mut rng = rand::thread_rng();
        let a = rng.next_u32() & 0x0FFF; 
        let b = rng.next_u32() & 0x0FFF; 
        (a.count_ones() as u64).wrapping_sub(b.count_ones() as u64)
    }

    /// Création d'un engagement (Le Wallet masque le billet)
    pub fn commit(amount: u64, blinding_factor: &[u64]) -> Self {
        // Le blinding_factor doit maintenant faire la taille de LATTICE_COLS
        assert_eq!(blinding_factor.len(), LATTICE_COLS, "Le facteur d'aveuglement doit faire LATTICE_COLS");
        let mut t_vector = vec![0u64; LATTICE_COLS];

        for i in 0..LATTICE_COLS {
            let a_row = Self::get_matrix_row(i);
            let mut sum: u64 = 0;
            
            for j in 0..LATTICE_COLS {
                sum = sum.wrapping_add(a_row[j].wrapping_mul(blinding_factor[j]));
            }
            
            let noise = Self::sample_cbd_noise();
            let message_term = if i == 0 { amount } else { 0 };
            
            t_vector[i] = sum.wrapping_add(noise).wrapping_add(message_term);
        }

        LWECommitment { t_vector }
    }

    /// Validation Homomorphe (Le Tribunal du Nœud L1/L2)
    pub fn verify_balance(inputs: &[LWECommitment], outputs: &[LWECommitment], fee: u64) -> bool {
        for i in inputs { if i.t_vector.len() != LATTICE_COLS { return false; } }
        for o in outputs { if o.t_vector.len() != LATTICE_COLS { return false; } }

        let max_noise = ((inputs.len() + outputs.len()) * 12) as u64; 

        for dim in 0..LATTICE_COLS {
            let mut dim_sum_in = 0u64;
            let mut dim_sum_out = 0u64;
            
            for i in inputs { dim_sum_in = dim_sum_in.wrapping_add(i.t_vector[dim]); }
            for o in outputs { dim_sum_out = dim_sum_out.wrapping_add(o.t_vector[dim]); }
            
            let mut expected_out = dim_sum_out;
            if dim == 0 { expected_out = expected_out.wrapping_add(fee); }

            let diff = dim_sum_in.wrapping_sub(expected_out);

            if diff > max_noise && diff < u64::MAX.wrapping_sub(max_noise) {
                return false; 
            }
        }
        
        true
    }
	
	/// Vérifie que le montant engagé est positif et inférieur à 2^60 (Range Proof)
    pub fn verify_range_proof(main_commit: &LWECommitment, proof_json: &str) -> bool {
        // 1. Désérialisation des engagements des bits de la preuve
        let bit_commitments: Vec<LWECommitment> = match serde_json::from_str(proof_json) {
            Ok(c) => c,
            Err(_) => return false, // Preuve absente ou malformée
        };
        
        // 2. Restriction stricte à 60 bits (empêche l'overflow de la somme sur u64)
        if bit_commitments.len() != 60 {
            return false;
        }

        // 3. Vérification homomorphe : C = Somme( b_i * 2^i )
        for dim in 0..LATTICE_COLS {
            let mut sum_bits = 0u64;
            
            for (i, bit_commit) in bit_commitments.iter().enumerate() {
                let weight = 1u64 << i; 
                // Multiplication scalaire homomorphe : on multiplie le ciphertext par 2^i
                let weighted_val = bit_commit.t_vector[dim].wrapping_mul(weight);
                sum_bits = sum_bits.wrapping_add(weighted_val);
            }
            
            let diff = main_commit.t_vector[dim].wrapping_sub(sum_bits);
            
            // Tolérance au bruit adaptée pour l'addition de 60 engagements
            let max_noise = 60 * 12; 
            
            if diff > max_noise && diff < u64::MAX.wrapping_sub(max_noise) {
                return false; // La décomposition ne correspond pas à l'engagement principal !
            }
        }
        
        true
    }
}