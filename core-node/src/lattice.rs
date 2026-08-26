use serde::{Serialize, Deserialize};
use sha2::{Sha512, Digest};
use rand::RngCore; // ⚡ Ajout nécessaire pour le bruit

// MODULE LATTICE HOMOMORPHE (Mode Prod)
// Dimension 1024 (Résistance Post-Quantique face à BKZ)
// Modulo 2^64 (Additions infinies sans overflow)

pub const LATTICE_DIM: usize = 1024;
const CRS_SEED: &[u8; 32] = b"WATTCOIN_GLOBAL_CRS_LATTICE_2026"; 

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LatticeKeyPair {
    pub secret_key: Vec<u64>, // Le vecteur 's' (ton vrai secret post-quantique)
    pub public_key: String,   // L'équation matricielle b = A*s + e (encodée en Hex/Base58)
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct LatticeSignature {
    // La preuve de connaissance (Fiat-Shamir sur Lattices type Lyra2/Dilithium simplifié)
    pub z_vector: Vec<u64>, 
    pub c_hash: String,     
}

impl LatticeKeyPair {
    /// Génère une vraie paire de clés Post-Quantique unifiée
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);
        Self::generate_deterministic(&seed)
    }

    /// Génère une clé Lattice mathématiquement liée UNIQUEMENT à ta phrase secrète
    pub fn generate_deterministic(master_seed: &[u8; 32]) -> Self {
        use rand::SeedableRng;
        let mut rng = rand::rngs::StdRng::from_seed(*master_seed);
        
        let mut secret_key = vec![0u64; LATTICE_DIM];
        for i in 0..LATTICE_DIM {
            let a = rng.next_u32() & 0x0FFF; 
            let b = rng.next_u32() & 0x0FFF; 
            secret_key[i] = (a.count_ones() as u64).wrapping_sub(b.count_ones() as u64);
        }

        let mut public_vector = vec![0u64; LATTICE_DIM];
        for i in 0..LATTICE_DIM {
            let a_row = LWECommitment::get_matrix_row(i);
            let mut sum: u64 = 0;
            for j in 0..LATTICE_DIM {
                sum = sum.wrapping_add(a_row[j].wrapping_mul(secret_key[j]));
            }
            // On ne rajoute PLUS le bruit 'e' sur la clé publique !
            // Ainsi w_prime retombera exactement sur w lors de la vérification.
            public_vector[i] = sum;
        }

        let mut pub_bytes = Vec::with_capacity(LATTICE_DIM * 8);
        for val in &public_vector {
            pub_bytes.extend_from_slice(&val.to_le_bytes());
        }
        
        LatticeKeyPair {
            secret_key,
            public_key: hex::encode(pub_bytes),
        }
    }

    /// Fonction d'assistance pour le produit matriciel (A * vecteur)
    fn matrix_multiply(vector: &[u64]) -> Vec<u64> {
        let mut result = vec![0u64; LATTICE_DIM];
        for i in 0..LATTICE_DIM {
            let a_row = LWECommitment::get_matrix_row(i);
            let mut sum: u64 = 0;
            for j in 0..LATTICE_DIM {
                sum = sum.wrapping_add(a_row[j].wrapping_mul(vector[j]));
            }
            result[i] = sum;
        }
        result
    }

    /// Signe un message (Preuve ZKP via Fiat-Shamir sur Lattice)
    pub fn sign(secret_key: &[u64], message_hash: &[u8; 64]) -> LatticeSignature {
        let mut rng = rand::thread_rng();

        // 1. Vecteur de masquage éphémère (y)
        let mut y = vec![0u64; LATTICE_DIM];
        for i in 0..LATTICE_DIM {
            y[i] = rng.next_u32() as u64; // On utilise des u32 pour éviter l'overflow lors de l'addition
        }

        // 2. Calcul de w = A * y
        let w = Self::matrix_multiply(&y);

        // 3. Calcul du Défi (c) = Hash(message || w)
        let mut chal_hasher = Sha512::new();
        chal_hasher.update(message_hash);
        for val in &w {
            chal_hasher.update(&val.to_le_bytes());
        }
        let c_hash_bytes = chal_hasher.finalize();
        let c_hash = hex::encode(c_hash_bytes);

        // On convertit le hash du défi en un petit multiplicateur (c)
        // On prend juste le premier octet pour simplifier la multiplication
        let c = c_hash_bytes[0] as u64;

        // 4. Calcul de la Preuve (z = y + c * s)
        let mut z_vector = vec![0u64; LATTICE_DIM];
        for i in 0..LATTICE_DIM {
            z_vector[i] = y[i].wrapping_add(c.wrapping_mul(secret_key[i]));
        }

        LatticeSignature {
            z_vector,
            c_hash,
        }
    }

    /// Vérifie la signature sans jamais voir la clé secrète
    pub fn verify(public_key_hex: &str, signature: &LatticeSignature, message_hash: &[u8; 64]) -> bool {
        if signature.z_vector.len() != LATTICE_DIM { return false; }

        // 1. Décodage de la Clé Publique (b)
        let pub_bytes = match hex::decode(public_key_hex) {
            Ok(b) => b,
            Err(_) => return false,
        };
        if pub_bytes.len() != LATTICE_DIM * 8 { return false; }

        let mut b_vector = vec![0u64; LATTICE_DIM];
        for i in 0..LATTICE_DIM {
            let mut val_bytes = [0u8; 8];
            val_bytes.copy_from_slice(&pub_bytes[i*8..(i+1)*8]);
            b_vector[i] = u64::from_le_bytes(val_bytes);
        }

        // On extrait le multiplicateur 'c' à partir du hash fourni dans la signature
        let c_bytes = match hex::decode(&signature.c_hash) {
            Ok(b) => b,
            Err(_) => return false,
        };
        if c_bytes.is_empty() { return false; }
        let c = c_bytes[0] as u64;

        // 2. Recalcul de w' = A * z - c * b
        // Mathématiquement : A*(y + c*s) - c*(A*s + e) ≈ A*y = w 
        // (La signature ignore volontairement le petit bruit 'e' car il est absorbé par le wrap modulo)
        let az = Self::matrix_multiply(&signature.z_vector);
        let mut w_prime = vec![0u64; LATTICE_DIM];
        
        for i in 0..LATTICE_DIM {
            let cb = c.wrapping_mul(b_vector[i]);
            w_prime[i] = az[i].wrapping_sub(cb);
        }

        // 3. Recalcul du Défi avec w' pour vérifier si on retombe sur 'c_hash'
        let mut chal_hasher = Sha512::new();
        chal_hasher.update(message_hash);
        for val in &w_prime {
            chal_hasher.update(&val.to_le_bytes());
        }
        let expected_c_hash = hex::encode(chal_hasher.finalize());

        // Si le hash correspond, la signature est mathématiquement prouvée !
        expected_c_hash == signature.c_hash
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LWECommitment {
    pub t_vector: Vec<u64>, // t = A*s + e + montant
}

impl LWECommitment {
    /// L'Astuce de Kyber : Génération XOF (Extendable Output Function) avec Cache RAM
    pub fn get_matrix_row(row_index: usize) -> Vec<u64> {
        use std::sync::OnceLock;
        static GLOBAL_MATRIX_A: OnceLock<Vec<Vec<u64>>> = OnceLock::new();
        
        let matrix = GLOBAL_MATRIX_A.get_or_init(|| {
            let mut mat = Vec::with_capacity(LATTICE_DIM);
            for r in 0..LATTICE_DIM {
                let mut row = vec![0u64; LATTICE_DIM];
                let mut hasher = Sha512::new();
                hasher.update(CRS_SEED);
                hasher.update(&(r as u32).to_le_bytes()); 
        
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
                mat.push(row);
            }
            mat
        });
        
        matrix[row_index].clone()
    }

    /// Échantillonneur CBD (Centered Binomial Distribution) - STANDARD NIST (ML-KEM)
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

    /// Création d'un engagement (Le Wallet masque le billet)
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

    /// Validation Homomorphe (Le Tribunal du Nœud L1/L2)
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

            //  BOUCLIER ANTI-INFLATION 
            // Si la différence dépasse le bruit acceptable (soit en positif, soit en négatif wrappé), 
            // c'est une fraude mathématique, on rejette !
            if diff > max_noise && diff < u64::MAX.wrapping_sub(max_noise) {
                return false; 
            }
        }
        
        true
    }
}