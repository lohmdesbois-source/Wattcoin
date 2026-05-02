use serde::{Serialize, Deserialize};
use crate::transaction::Transaction;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>, 
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub index: u64,
    pub timestamp: i64,
    pub previous_hash: String,
    pub hash: String,
    pub nonce: u64, 
}

impl Block {
    // 🌍 LE VÉRITABLE GENESIS BLOCK DE WATTCOIN
    pub fn genesis() -> Self {
        let header = BlockHeader {
            index: 0,
            timestamp: 1713000000, 
            previous_hash: String::from("0000000000000000000000000000000000000000000000000000000000000000"),
            hash: String::from("GENESIS_HASH_WATTCOIN_000000000000000000000000000000000000000000"), // Hash codé en dur
            nonce: 0,
        };

        // 🌍 LE VÉRITABLE GENESIS BLOCK QUANTIQUE DE WATTCOIN
        let tx = Transaction {
            stealth_address: "GENESIS".to_string(),
            kyber_capsule: "GENESIS_KEY".to_string(),
            
            // 📜 LA DÉCLARATION D'INDÉPENDANCE DE WATTCOIN
            aes_vault: "Wattcoin: L'énergie libre, anonyme et post-quantique. 03/Mai/2026 - Le monde change aujourd'hui.".to_string(),
            
            lattice_commitment: crate::lattice::LatticeCommitment::commit(0, 0), 
            fee: 0,
            pq_ring_inputs: vec!["GENESIS".to_string()], 
            dilithium_signature: "GENESIS".to_string(),
            pq_ring_signature: None,
        };

        Block {
            header,
            transactions: vec![tx],
        }
    }
}