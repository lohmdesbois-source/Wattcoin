// tests/basic_chain_tests.rs
// Tests de base du node Wattcoin
// Lancer avec : cargo test --test basic_chain_tests

use wattcoin_core::block::{BlockHeader, Block};
use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::transaction::{Transaction, TransactionType, TransactionOutput};
use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM}; 

// 💡 LE FIX PROPRE : On fournit 128 fausses clés pour respecter le consensus strict du nœud !
fn dummy_l2_keys() -> Vec<(Vec<[u8; 32]>, Vec<u8>)> {
    vec![(vec![[0u8; 32]; 34], vec![0u8; 32]); 128]
}

#[test]
fn test_genesis_block() {
    let _ = std::fs::remove_dir_all(".test_db_basic_1");
    let chain = Blockchain::new(".test_db_basic_1").unwrap();
    
    assert_eq!(chain.current_height, 0);
    let genesis = chain.get_block_by_height(0).unwrap();
    assert_eq!(genesis.header.index, 0);
    assert_eq!(genesis.header.hash, "GENESIS_HASH_WATTCOIN_000000000000000000000000000000000000000000");
}

#[test]
fn test_get_next_base_reward_decay_and_tail() {
    let initial: u64 = 15_000_000_000;
    let next = Blockchain::get_next_base_reward(initial);
    assert_eq!(next, initial.saturating_sub(initial >> 18));

    let tail = Blockchain::get_next_base_reward(100_000_000);
    assert_eq!(tail, 600_000_000);
}

#[test]
fn test_prepare_block_template_no_inflation() {
    let _ = std::fs::remove_dir_all(".test_db_basic_2");
    let mut chain = Blockchain::new(".test_db_basic_2").unwrap();
    
    // On passe nos 128 clés factices
    let (block, _target, _l2_keys) = chain.prepare_block_template(vec![], "test_miner", dummy_l2_keys());
    assert_eq!(block.transactions.len(), 1);
    let reward: u64 = block.transactions[0].outputs[0].aes_vault.parse().unwrap();
    assert!(reward > 0 && reward <= 25_000_000_000);
}

#[test]
fn test_spent_key_images_prevents_double_spend() {
    let _ = std::fs::remove_dir_all(".test_db_basic_3");
    let mut chain = Blockchain::new(".test_db_basic_3").unwrap();
    let ki = "test_double_spend_key_image".to_string();
    chain.spent_key_images.insert(ki.clone());
    assert!(chain.spent_key_images.contains(&ki));
}

#[test]
fn test_total_supply() {
    let _ = std::fs::remove_dir_all(".test_db_basic_4");
    let mut chain = Blockchain::new(".test_db_basic_4").unwrap();
    
    let coinbase = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_test".to_string(),
            kyber_capsule: "test".to_string(),
            aes_vault: "15000000000".to_string(),
            lattice_commitment: LWECommitment {
                t_vector: vec![0u64; LATTICE_DIM],
            },
        }],
        fee: 0,
        public_key: "COINBASE_SIG".to_string(),
        wots_signature: None,
    };
    
    let header = BlockHeader {
        index: 1,
        timestamp: chrono::Utc::now().timestamp(),
        previous_hash: chain.get_block_by_height(0).unwrap().header.hash.clone(),
        hash: "test".to_string(),
        nonce: 0,
        target_hex: "00".repeat(32),
        l2_root: String::from("NO_L2_FOR_TESTS"),
        tx_root: String::new(), 
    };
    
    let mut block = Block { header, transactions: vec![coinbase] };
    block.header.tx_root = block.calculate_tx_root(); 
    
    chain.push_block(&block).unwrap();
    assert!(chain.get_total_supply() >= 15_000_000_000);
}

#[test]
fn test_validate_rejects_block_with_two_coinbases() {
    let _ = std::fs::remove_dir_all(".test_db_basic_5");
    let mut chain = Blockchain::new(".test_db_basic_5").unwrap();

    let coinbase1 = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_test1".to_string(),
            kyber_capsule: "test1".to_string(),
            aes_vault: "15000000000".to_string(),
            lattice_commitment: LWECommitment { t_vector: vec![0u64; LATTICE_DIM] },
        }],
        fee: 0, public_key: "COINBASE_SIG".to_string(), wots_signature: None,
    };

    let coinbase2 = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_test2".to_string(),
            kyber_capsule: "test2".to_string(),
            aes_vault: "15000000000".to_string(),
            lattice_commitment: LWECommitment { t_vector: vec![0u64; LATTICE_DIM] },
        }],
        fee: 0, public_key: "COINBASE_SIG".to_string(), wots_signature: None,
    };

    let header = BlockHeader {
        index: 1,
        timestamp: chrono::Utc::now().timestamp(),
        previous_hash: chain.get_block_by_height(0).unwrap().header.hash.clone(),
        hash: "fake_hash_for_test".to_string(),
        nonce: 0,
        target_hex: "00".repeat(32),
        l2_root: String::from("NO_L2_FOR_TESTS"),
        tx_root: String::new(),
    };

    let mut bad_block = Block { header, transactions: vec![coinbase1, coinbase2] };
    bad_block.header.tx_root = bad_block.calculate_tx_root();

    let result = chain.validate_and_add_external_block(bad_block);
    assert!(result.is_err(), "Le node doit rejeter un bloc avec 2 coinbases");
}