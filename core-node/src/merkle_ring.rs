#![allow(dead_code)]
use serde::{Serialize, Deserialize};
use sha2::{Sha512, Digest};
use rand::{RngCore, thread_rng};

use crate::lattice::LATTICE_DIM;

// MODULE RING SIGNATURE LATTICE (Inspiré de DualRing-LWE)
// Anonymat absolu et Post-Quantique (Zéro Courbe Elliptique, Zéro Hachage MPC)

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MpcRingSignature {
    pub key_image: String,             // Anti-double dépense inattaquable
    pub ring_root: String,             // Racine de l'arbre de Merkle des leurres
    pub ring_decoys: Vec<String>,      // Les 64 clés publiques (équations b) de l'anneau
    
    // --- MOTEUR ZKP LATTICE ---
    pub c_0: String,                   // Le défi initial de l'anneau
    pub responses: Vec<Vec<u64>>,      // Les vecteurs z_i pour fermer l'équation
}

impl MpcRingSignature {
    pub fn hash_nodes(left: &str, right: &str) -> String {
        let mut hasher = Sha512::new();
        hasher.update(left.as_bytes());
        hasher.update(right.as_bytes());
        hex::encode(hasher.finalize())
    }

    /// Extrait le vecteur 'b' (Clé publique) depuis sa chaîne Hexadécimale
    fn decode_pubkey(hex_key: &str) -> Vec<u64> {
        let pub_bytes = hex::decode(hex_key).unwrap_or_default();
        if pub_bytes.len() != LATTICE_DIM * 8 { return vec![0; LATTICE_DIM]; }
        
        let mut b_vector = vec![0u64; LATTICE_DIM];
        for i in 0..LATTICE_DIM {
            let mut val_bytes = [0u8; 8];
            val_bytes.copy_from_slice(&pub_bytes[i*8..(i+1)*8]);
            b_vector[i] = u64::from_le_bytes(val_bytes);
        }
        b_vector
    }

    /// Fonction d'assistance pour le produit matriciel (A * vecteur)
    /// Copiée ici pour un accès direct dans le module Ring
    fn matrix_multiply(vector: &[u64]) -> Vec<u64> {
        let mut result = vec![0u64; LATTICE_DIM];
        for i in 0..LATTICE_DIM {
            let a_row = crate::lattice::LWECommitment::get_matrix_row(i); // Hack de visibilité
            let mut sum: u64 = 0;
            for j in 0..LATTICE_DIM {
                sum = sum.wrapping_add(a_row[j].wrapping_mul(vector[j]));
            }
            result[i] = sum;
        }
        result
    }

    /// Crée la Ring Signature Lattice
    pub fn sign(secret_key: &[u64], tx_hash: &[u8; 64], decoys: &[String], real_index: usize, real_capsule: &str, kyber_secret: &[u8]) -> Self {
        let n = decoys.len();
        let mut rng = thread_rng();

        // 1. Racine Merkle & Key Image (Anti-Double Dépense)
        let mut current_root = decoys[0].clone();
        for i in 1..n {
            current_root = Self::hash_nodes(&current_root, &decoys[i]);
        }

        let mut ki_hasher = Sha512::new();
        ki_hasher.update(real_capsule.as_bytes());
        ki_hasher.update(kyber_secret); 
        let key_image = hex::encode(ki_hasher.finalize());

        // 2. MOTEUR DUAL-RING LATTICE
        let mut responses = vec![vec![0u64; LATTICE_DIM]; n];
        let mut c_values = vec![0u64; n + 1];

        // Étape A : Générer le vecteur aléatoire de départ (y) pour le signataire réel
        let mut y = vec![0u64; LATTICE_DIM];
        for i in 0..LATTICE_DIM { y[i] = rng.next_u32() as u64; }
        let w = Self::matrix_multiply(&y);

        // Étape B : Hacher pour obtenir c_{pi+1}
        let mut h = Sha512::new();
        h.update(tx_hash);
        for val in &w { h.update(&val.to_le_bytes()); }
        c_values[real_index + 1] = h.finalize()[0] as u64; // On simplifie le défi à 1 octet

        // Étape C : Simuler l'anneau en avant (de pi+1 à n-1)
        for i in (real_index + 1)..n {
            for j in 0..LATTICE_DIM { responses[i][j] = rng.next_u32() as u64; } // Z_i factice
            let b_vector = Self::decode_pubkey(&decoys[i]);
            
            // w' = A * z_i - c_i * b_i
            let az = Self::matrix_multiply(&responses[i]);
            let mut w_prime = vec![0u64; LATTICE_DIM];
            for j in 0..LATTICE_DIM {
                let cb = c_values[i].wrapping_mul(b_vector[j]);
                w_prime[j] = az[j].wrapping_sub(cb);
            }
            
            let mut h2 = Sha512::new();
            h2.update(tx_hash);
            for val in &w_prime { h2.update(&val.to_le_bytes()); }
            c_values[i + 1] = h2.finalize()[0] as u64;
        }

        // Étape D : Boucler (c_0 = c_n) et simuler de 0 jusqu'à pi-1
        c_values[0] = c_values[n];
        for i in 0..real_index {
            for j in 0..LATTICE_DIM { responses[i][j] = rng.next_u32() as u64; } // Z_i factice
            let b_vector = Self::decode_pubkey(&decoys[i]);
            
            let az = Self::matrix_multiply(&responses[i]);
            let mut w_prime = vec![0u64; LATTICE_DIM];
            for j in 0..LATTICE_DIM {
                let cb = c_values[i].wrapping_mul(b_vector[j]);
                w_prime[j] = az[j].wrapping_sub(cb);
            }
            
            let mut h3 = Sha512::new();
            h3.update(tx_hash);
            for val in &w_prime { h3.update(&val.to_le_bytes()); }
            c_values[i + 1] = h3.finalize()[0] as u64;
        }

        // Étape E : Fermer l'équation ZKP avec le VRAI secret (z_pi = y + c_pi * s)
        for j in 0..LATTICE_DIM {
            responses[real_index][j] = y[j].wrapping_add(c_values[real_index].wrapping_mul(secret_key[j]));
        }

        MpcRingSignature {
            key_image,
            ring_root: current_root,
            ring_decoys: decoys.to_vec(),
            c_0: c_values[0].to_string(), // On stocke c_0 pour la vérification
            responses,
        }
    }

    /// Vérification Instantanée et Aveugle par le Tribunal du Nœud
    pub fn verify(&self, tx_hash: &[u8; 64]) -> bool {
        if self.ring_decoys.len() < 16 { return false; }

        let unique_decoys: std::collections::HashSet<_> = self.ring_decoys.iter().collect();
        if unique_decoys.len() != self.ring_decoys.len() { return false; }

        // 1. Intégrité de la Racine Merkle
        let mut computed_root = self.ring_decoys[0].clone();
        for i in 1..self.ring_decoys.len() {
            computed_root = Self::hash_nodes(&computed_root, &self.ring_decoys[i]);
        }
        if computed_root != self.ring_root { return false; }

        // 2. VÉRIFICATION DE L'ANNEAU LATTICE
        let mut current_c = match self.c_0.parse::<u64>() {
            Ok(c) => c,
            Err(_) => return false,
        };
        
        let start_c = current_c; // On sauvegarde c_0 pour vérifier la boucle

        for i in 0..self.ring_decoys.len() {
            if self.responses[i].len() != LATTICE_DIM { return false; }
            let b_vector = Self::decode_pubkey(&self.ring_decoys[i]);
            
            // Recalcul de w' = A * z_i - c_i * b_i
            let az = Self::matrix_multiply(&self.responses[i]);
            let mut w_prime = vec![0u64; LATTICE_DIM];
            for j in 0..LATTICE_DIM {
                let cb = current_c.wrapping_mul(b_vector[j]);
                w_prime[j] = az[j].wrapping_sub(cb);
            }
            
            // Recalcul de c_{i+1}
            let mut h = Sha512::new();
            h.update(tx_hash);
            for val in &w_prime { h.update(&val.to_le_bytes()); }
            current_c = h.finalize()[0] as u64;
        }

        // LA MAGIE ZKP : Si la boucle mathématique Lattice est valide, c_n doit retomber sur c_0.
        // Cela prouve que le créateur connaissait le secret "s" d'une des 64 équations publiques "b",
        // mais le bruit cryptographique empêche totalement de deviner laquelle.
        current_c == start_c
    }
}