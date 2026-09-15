use sha2::{Sha256, Digest};
use rand::{RngCore, SeedableRng};
use rand::rngs::StdRng;
use serde::{Serialize, Deserialize};

pub const WOTS_CHAINS: usize = 32 + 2; // 32 octets pour le hash + 2 pour le checksum
pub const WOTS_W: usize = 256;         // Paramètre de Winternitz

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WotsSignature {
    pub index: u64,
    pub public_key: Vec<u8>,         // Fera désormais 32 octets !
    pub signature_bytes: Vec<u8>,    // Fera 1088 octets
}

pub struct Wots;

impl Wots {
    /// Génère une paire de clés WOTS+ déterministe avec clé publique compressée (32 octets)
    pub fn generate_keypair(master_seed: &[u8], index: u64) -> (Vec<[u8; 32]>, Vec<u8>) {
        let mut hasher = Sha256::new();
        hasher.update(master_seed);
        hasher.update(index.to_be_bytes());
        let sub_seed = hasher.finalize();

        let mut rng = StdRng::from_seed(sub_seed.into());
        let mut secret_key = Vec::with_capacity(WOTS_CHAINS);
        
        // On prépare le hasher qui va compresser tous les bouts de la clé publique
        let mut pk_hasher = Sha256::new();

        for _ in 0..WOTS_CHAINS {
            let mut sk_chunk = [0u8; 32];
            rng.fill_bytes(&mut sk_chunk);
            secret_key.push(sk_chunk);

            let mut pk_chunk = sk_chunk;
            for _ in 0..(WOTS_W - 1) {
                let mut h = Sha256::new();
                h.update(&pk_chunk);
                pk_chunk = h.finalize().into();
            }
            // Au lieu de stocker le chunk, on l'ajoute directement dans le hachoir final
            pk_hasher.update(&pk_chunk);
        }

        // La clé publique finale est l'empreinte unique de 32 octets
        let compressed_pk: [u8; 32] = pk_hasher.finalize().into();

        (secret_key, compressed_pk.to_vec()) 
    }

    /// Signe un message (la logique reste identique)
    pub fn sign(secret_key: &[[u8; 32]], index: u64, message_hash: &[u8; 32], public_key: &[u8]) -> WotsSignature {
        assert_eq!(secret_key.len(), WOTS_CHAINS, "Clé secrète invalide !");
        let mut signature = Vec::with_capacity(WOTS_CHAINS * 32);
        let mut checksum = 0u32;

        for i in 0..32 {
            let msg_byte = message_hash[i] as usize;
            checksum += (WOTS_W - 1 - msg_byte) as u32;
            
            let mut sig_chunk = secret_key[i];
            for _ in 0..msg_byte {
                let mut h = Sha256::new();
                h.update(&sig_chunk);
                sig_chunk = h.finalize().into();
            }
            signature.extend_from_slice(&sig_chunk);
        }

        let checksum_bytes = [(checksum >> 8) as u8, (checksum & 0xFF) as u8];
        for i in 0..2 {
            let msg_byte = checksum_bytes[i] as usize;
            let mut sig_chunk = secret_key[32 + i];
            for _ in 0..msg_byte {
                let mut h = Sha256::new();
                h.update(&sig_chunk);
                sig_chunk = h.finalize().into();
            }
            signature.extend_from_slice(&sig_chunk);
        }

        WotsSignature {
            index,
            public_key: public_key.to_vec(),
            signature_bytes: signature,
        }
    }

    /// Vérifie la signature WOTS+ à partir de la clé publique compressée
    pub fn verify(wots_sig: &WotsSignature, message_hash: &[u8; 32]) -> bool {
        let pk_bytes = &wots_sig.public_key;

        // La clé publique DOIT faire 32 octets (compression), et la signature 1088 octets
        if pk_bytes.len() != 32 || wots_sig.signature_bytes.len() != WOTS_CHAINS * 32 {
            return false;
        }

        let sig_bytes = &wots_sig.signature_bytes;
        let mut checksum = 0u32;
        
        // Hasher pour reconstruire l'empreinte compressée
        let mut pk_hasher = Sha256::new();

        for i in 0..32 {
            let msg_byte = message_hash[i] as usize;
            checksum += (WOTS_W - 1 - msg_byte) as u32;

            let mut current_chunk = [0u8; 32];
            current_chunk.copy_from_slice(&sig_bytes[i*32..(i+1)*32]);

            for _ in 0..(WOTS_W - 1 - msg_byte) {
                let mut h = Sha256::new();
                h.update(&current_chunk);
                current_chunk = h.finalize().into();
            }
            pk_hasher.update(&current_chunk);
        }

        let checksum_bytes = [(checksum >> 8) as u8, (checksum & 0xFF) as u8];
        for i in 0..2 {
            let msg_byte = checksum_bytes[i] as usize;
            let mut current_chunk = [0u8; 32];
            current_chunk.copy_from_slice(&sig_bytes[(32+i)*32..(33+i)*32]);

            for _ in 0..(WOTS_W - 1 - msg_byte) {
                let mut h = Sha256::new();
                h.update(&current_chunk);
                current_chunk = h.finalize().into();
            }
            pk_hasher.update(&current_chunk);
        }

        // On finalise le hachage et on compare avec la clé de 32 octets fournie
        let derived_pk: [u8; 32] = pk_hasher.finalize().into();
        derived_pk.as_slice() == pk_bytes.as_slice()
    }
}