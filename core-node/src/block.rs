use serde::{Serialize, Deserialize};
use crate::transaction::{Transaction, TransactionType, TransactionOutput};
use num_bigint::BigUint;

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
    pub target_hex: String,        // ← NOUVEAU CHAMP (stocké en hex comme dans /info)
}

impl Block {
    pub fn genesis() -> Self {
        let max_target = BigUint::from_bytes_be(&[0xFF; 32]);
        let initial_target = max_target >> 12_u32;           // INITIAL_DIFFICULTY_SHIFT
        let target_hex = format!("{:0>64}", initial_target.to_str_radix(16));

        let header = BlockHeader {
            index: 0,
            timestamp: 1779612120,
            previous_hash: String::from("0000000000000000000000000000000000000000000000000000000000000000"),
            hash: String::from("GENESIS_HASH_WATTCOIN_000000000000000000000000000000000000000000"),
            nonce: 0,
            target_hex,                                      // ← initial
        };

        let tx = Transaction {
            tx_type: TransactionType::Coinbase,
            inputs: vec![],
            outputs: vec![
                TransactionOutput {
                    stealth_address: "GENESIS".to_string(),
                    kyber_capsule: "GENESIS_KEY".to_string(),
                    aes_vault: "Wattcoin: L'énergie libre, anonyme et post-quantique. 03/Mai/2026 - Le monde change aujourd'hui.".to_string(),
                    lattice_commitment: crate::lattice::LWECommitment::commit(0, [0, 0, 0, 0]),
                }
            ],
            fee: 0,
            dilithium_signature: "GENESIS".to_string(),
        };

        Block { header, transactions: vec![tx] }
    }
}