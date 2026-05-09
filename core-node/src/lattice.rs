use serde::{Serialize, Deserialize};
use rand::{Rng, SeedableRng, RngCore};
use rand::rngs::StdRng;

// 🧮 MODULE LATTICE (Learning With Errors)
pub const LATTICE_Q: u32 = 8380417; // Module premier (Idem Kyber)
pub const LATTICE_DIM: usize = 4;   // Dimension vectorielle

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LWECommitment {
    pub a_matrix_seed: [u8; 32], // Graine publique pour générer A
    pub t_vector: Vec<u32>,      // t = As + e + m(Q/2)
}

impl LWECommitment {
    /// 🔒 Création d'un engagement sur un montant (Wallet)
    pub fn commit(amount: u64, blinding_factor: [u32; LATTICE_DIM]) -> Self {
        let mut rng = rand::thread_rng();
        let mut a_matrix_seed = [0u8; 32];
        rng.fill_bytes(&mut a_matrix_seed);
        
        let a_matrix = Self::generate_matrix(a_matrix_seed);
        let mut t_vector = vec![0u32; LATTICE_DIM];
        
        for i in 0..LATTICE_DIM {
            let mut sum: u64 = 0;
            for j in 0..LATTICE_DIM {
                sum += (a_matrix[i][j] as u64 * blinding_factor[j] as u64) % LATTICE_Q as u64;
            }
            // LWE Error (Bruit gaussien simulé par petite plage)
            let error_term = rng.gen_range(0..5); 
            // Encodage du montant sur la composante principale
            let message_term = if i == 0 { (amount * (LATTICE_Q as u64 / 2)) % LATTICE_Q as u64 } else { 0 };
            
            t_vector[i] = ((sum + error_term as u64 + message_term) % LATTICE_Q as u64) as u32;
        }

        LWECommitment { a_matrix_seed, t_vector }
    }

    /// 🛠️ Génération déterministe de la Matrice Publique
    pub fn generate_matrix(seed: [u8; 32]) -> Vec<Vec<u32>> {
        let mut a_matrix = vec![vec![0u32; LATTICE_DIM]; LATTICE_DIM];
        let mut seed_rng = StdRng::from_seed(seed);
        for row in a_matrix.iter_mut() {
            for val in row.iter_mut() { *val = seed_rng.gen_range(0..LATTICE_Q); }
        }
        a_matrix
    }

    /// ⚖️ Validation Homomorphe (Vérifie Input_Sum == Output_Sum) (Nœud)
	pub fn verify_balance(inputs: &[LWECommitment], outputs: &[LWECommitment], fee: u64) -> bool {
		let mut sum_in = 0u64;
		let mut sum_out = 0u64;
		
		// On additionne les composantes t[0] (là où le message est encodé)
		for i in inputs { sum_in = (sum_in + i.t_vector[0] as u64) % LATTICE_Q as u64; }
		for o in outputs { sum_out = (sum_out + o.t_vector[0] as u64) % LATTICE_Q as u64; }
		
		// Le montant des frais est encodé de la même manière que les messages (m * Q/2)
		let fee_encoded = (fee * (LATTICE_Q as u64 / 2)) % LATTICE_Q as u64;
		let expected_out = (sum_out + fee_encoded) % LATTICE_Q as u64;

		// Calcul de la distance sur le cercle du modulo Q
		let diff = if sum_in > expected_out {
			let d = sum_in - expected_out;
			std::cmp::min(d, LATTICE_Q as u64 - d)
		} else {
			let d = expected_out - sum_in;
			std::cmp::min(d, LATTICE_Q as u64 - d)
		};

		// Tolérance : On accepte une dérive liée au bruit e. 
		// Plus il y a d'inputs/outputs, plus le bruit augmente.
		let noise_threshold = (inputs.len() + outputs.len()) as u64 * 10; 
		diff < noise_threshold
	}
 }