use serde::{Serialize, Deserialize};
use crate::transaction::{Transaction, TransactionOutput};
use crate::lattice::LatticeSignature;
use num_bigint::BigUint;
use sha2::Digest;

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
	pub tx_root: String,           // La racine de Merkle des transactions L1 !
}

// Le MicroBloc L2 (1 seconde par bloc)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MicroBlock {
    pub l1_parent_hash: String,    // Le hash du bloc L1 qui a couronné ce séquenceur
    pub micro_index: u64,          // L'index global parfait (1, 2, 3, 4...)
    pub key_index: u32,            // L'index de la clé Séquenceur (0 à 127)
    pub timestamp: i64,
    pub transactions: Vec<Transaction>, // Les transactions instantanées
    pub sequencer_pubkey: String,       // La clé WOTS publique spécifique à ce microbloc
	pub sequencer_reward_address: String, // Pour payer le bon séquenceur !
    pub sequencer_sig: LatticeSignature,   // La signature Lattice de ce microbloc
    pub merkle_proof: Vec<String>,      // La preuve que cette clé fait partie du l2_root
}

impl Block {
    // Nouvelle fonction d'assistance pour calculer l'arbre de Merkle SHA-512
    pub fn calculate_tx_root(&self) -> String {
        if self.transactions.is_empty() {
            return hex::encode([0u8; 64]);
        }
        
        // On récupère le hash de chaque transaction
        let mut current_level: Vec<String> = self.transactions.iter()
            .map(|tx| hex::encode(tx.hash_data()))
            .collect();

        // On compresse l'arbre étage par étage
        while current_level.len() > 1 {
            let mut next_level = Vec::new();
            for chunk in current_level.chunks(2) {
                if chunk.len() == 2 {
                    let mut hasher = sha2::Sha512::new();
                    hasher.update(chunk[0].as_bytes());
                    hasher.update(chunk[1].as_bytes());
                    next_level.push(hex::encode(hasher.finalize()));
                } else {
                    // Si impair, le nœud remonte tel quel (règle standard Bitcoin)
                    next_level.push(chunk[0].clone());
                }
            }
            current_level = next_level;
        }
        current_level[0].clone()
    }

    pub fn genesis() -> Self {
        let max_target = BigUint::from_bytes_be(&[0xFF; 32]);
        let initial_target = max_target >> 12_u32;           
        let target_hex = format!("{:0>64}", initial_target.to_str_radix(16));

        let mut transactions = Vec::new();

        // 1. La vraie transaction Coinbase du Genesis (Index 0)
        let coinbase_tx = Transaction {
            tx_type: crate::transaction::TransactionType::Coinbase,
            inputs: vec![],
            outputs: vec![
                TransactionOutput {
                    stealth_address: "GENESIS".to_string(),
                    kyber_capsule: "GENESIS_KEY".to_string(),
                    aes_vault: "Wattcoin: L'énergie libre, anonyme et post-quantique. 09/Juillet/2026 - Le monde change aujourd'hui.".to_string(),
                    lattice_commitment: crate::lattice::LWECommitment::commit(0, &[0u64; crate::lattice::LATTICE_DIM]),
                }
            ],
            fee: 0,
            public_key: "GENESIS".to_string(),
            lattice_signature: None,
        };
        transactions.push(coinbase_tx);

        // 2. INJECTION MAINNET : 64 Transactions factices pour l'amorçage des leurres ZKP
        for i in 0..64 {
            let dummy_tx = Transaction {
                tx_type: crate::transaction::TransactionType::Standard,
                inputs: vec![], // Transaction fantôme, aucun input
                outputs: vec![], // Aucun output
                fee: 0,
                // On génère 64 clés publiques uniques qui serviront de leurres initiaux
                public_key: format!("GENESIS_DECOY_WOTS_PUBKEY_{:02}", i),
                lattice_signature: None,
            };
            transactions.push(dummy_tx);
        }

        let header = BlockHeader {
            index: 0,
            timestamp: 1787780400, 
            previous_hash: String::from("0000000000000000000000000000000000000000000000000000000000000000"),
            hash: String::from("GENESIS_HASH_WATTCOIN_000000000000000000000000000000000000000000"),
            nonce: 0,
            target_hex,
            l2_root: String::from("NO_L2_FOR_GENESIS"), 
            tx_root: String::new(), // On va le calculer juste en dessous
        };

        let mut genesis_block = Block { header, transactions };
        
        // 3. Calcul automatique du tx_root incluant les 65 transactions
        genesis_block.header.tx_root = genesis_block.calculate_tx_root();

        genesis_block
    }
}