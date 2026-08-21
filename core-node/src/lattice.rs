use serde::{Serialize, Deserialize};
use sha2::{Sha512, Digest};
use rand::RngCore; // ⚡ Ajout nécessaire pour le bruit

// 🧮 MODULE LATTICE HOMOMORPHE (Mode Prod)
// Dimension 1024 (Résistance Post-Quantique face à BKZ)
// Modulo 2^64 (Additions infinies sans overflow)

pub const LATTICE_DIM: usize = 1024;
const CRS_SEED: &[u8; 32] = b"WATTCOIN_GLOBAL_CRS_LATTICE_2026"; 

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LWECommitment {
    pub t_vector: Vec<u64>, // t = A*s + e + montant
}

impl LWECommitment {
    /// 🌪️ L'Astuce de Kyber : Génération XOF (Extendable Output Function)
    fn get_matrix_row(row_index: usize) -> Vec<u64> {
        let mut row = vec![0u64; LATTICE_DIM];
        let mut hasher = Sha512::new();
        hasher.update(CRS_SEED);
        hasher.update(&(row_index as u32).to_le_bytes()); 

        let mut hash_output = hasher.finalize_reset();
        
        for i in 0..LATTICE_DIM {
            let byte_idx = (i % 8) * 8;
            let mut val_bytes = [0u8; 8];
            val_bytes.copy_from_slice(&hash_output[byte_idx..byte_idx + 8]);
            row[i] = u64::from_le_bytes(val_bytes);

            if byte_idx == 56 {
                hasher.update(&hash_output);
                hash_output = hasher.finalize_reset();
            }
        }
        row
    }

    /// 🎲 Échantillonneur CBD (Centered Binomial Distribution) - STANDARD NIST (ML-KEM)
    /// Remplace la Gaussienne (Knuth-Yao) car c'est 100% "Constant-Time" et immunisé aux Side-Channels.
    fn sample_cbd_noise() -> u64 {
        let mut rng = rand::thread_rng();
        // Paramètre Eta (η) = 12. On tire 12 bits aléatoires pour A et 12 pour B.
        let a = rng.next_u32() & 0x0FFF; 
        let b = rng.next_u32() & 0x0FFF; 
        
        // noise = popcount(a) - popcount(b) (Donne une courbe en cloche parfaite entre -12 et +12)
        // En u64, les nombres négatifs "wrap" (ex: -1 devient u64::MAX)
        (a.count_ones() as u64).wrapping_sub(b.count_ones() as u64)
    }

    /// 🔒 Création d'un engagement (Le Wallet masque le billet)
    pub fn commit(amount: u64, blinding_factor: &[u64]) -> Self {
        assert_eq!(blinding_factor.len(), LATTICE_DIM);
        let mut t_vector = vec![0u64; LATTICE_DIM];

        for i in 0..LATTICE_DIM {
            let a_row = Self::get_matrix_row(i);
            let mut sum: u64 = 0;
            
            for j in 0..LATTICE_DIM {
                sum = sum.wrapping_add(a_row[j].wrapping_mul(blinding_factor[j]));
            }
            
            let noise = Self::sample_cbd_noise(); // ⚡ Bruit Cryptographique Immunisé BKZ !
            let message_term = if i == 0 { amount } else { 0 };
            
            t_vector[i] = sum.wrapping_add(noise).wrapping_add(message_term);
        }

        LWECommitment { t_vector }
    }

    /// ⚖️ Validation Homomorphe (Le Tribunal du Nœud L1/L2)
    pub fn verify_balance(inputs: &[LWECommitment], outputs: &[LWECommitment], fee: u64) -> bool {
        
        for i in inputs { if i.t_vector.len() != LATTICE_DIM { return false; } }
        for o in outputs { if o.t_vector.len() != LATTICE_DIM { return false; } }

        // Bruit max = η (12) multiplié par le nombre de billets
        let max_noise = ((inputs.len() + outputs.len()) * 12) as u64; 

        for dim in 0..LATTICE_DIM {
            let mut dim_sum_in = 0u64;
            let mut dim_sum_out = 0u64;
            
            for i in inputs { dim_sum_in = dim_sum_in.wrapping_add(i.t_vector[dim]); }
            for o in outputs { dim_sum_out = dim_sum_out.wrapping_add(o.t_vector[dim]); }
            
            let mut expected_out = dim_sum_out;
            if dim == 0 { expected_out = expected_out.wrapping_add(fee); }

            // Différence absolue avec wrapping. (Ex: 2 - 5 = u64::MAX - 2)
            let diff = dim_sum_in.wrapping_sub(expected_out);

            // 🛡️ BOUCLIER ANTI-INFLATION 
            // Si la différence dépasse le bruit acceptable (soit en positif, soit en négatif wrappé), 
            // c'est une fraude mathématique, on rejette !
            if diff > max_noise && diff < u64::MAX.wrapping_sub(max_noise) {
                return false; 
            }
        }
        
        true
    }
}