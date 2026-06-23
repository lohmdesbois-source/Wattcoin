use serde::{Serialize, Deserialize};
use crate::transaction::{Transaction, TransactionOutput};
use crate::wots::WotsSignature;
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
    pub target_hex: String,
    pub l2_root: String,           // La racine de l'Arbre du Séquenceur L2
}

// Le MicroBloc L2 (1 seconde par bloc)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicroBlock {
    pub l1_parent_hash: String,    // Le hash du bloc L1 qui a couronné ce séquenceur
    pub micro_index: u64,          // De 0 à 127 (1 par seconde)
    pub timestamp: i64,
    pub transactions: Vec<Transaction>, // Les transactions instantanées
    pub sequencer_pubkey: String,       // La clé WOTS publique spécifique à ce microbloc
	pub sequencer_reward_address: String, // Pour payer le bon séquenceur !
    pub sequencer_sig: WotsSignature,   // La signature WOTS de ce microbloc
    pub merkle_proof: Vec<String>,      // La preuve que cette clé fait partie du l2_root
}

impl Block {
    pub fn genesis() -> Self {
        let max_target = BigUint::from_bytes_be(&[0xFF; 32]);
        let initial_target = max_target >> 12_u32;           
        let target_hex = format!("{:0>64}", initial_target.to_str_radix(16));

        let header = BlockHeader {
            index: 0,
            timestamp: 1782011400,
            previous_hash: String::from("0000000000000000000000000000000000000000000000000000000000000000"),
            hash: String::from("GENESIS_HASH_WATTCOIN_000000000000000000000000000000000000000000"),
            nonce: 0,
            target_hex,
            l2_root: String::from("NO_L2_FOR_GENESIS"), // ⚡ Racine vide pour le Genesis
        };

        let tx = Transaction {
            tx_type: crate::transaction::TransactionType::Coinbase,
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
            public_key: "GENESIS".to_string(),
            wots_signature: None,
        };

        Block { header, transactions: vec![tx] }
    }
}