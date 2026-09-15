// tests/l2_isolation_tests.rs
// Stress tests de l'isolation du Layer 2 (Séquenceur vs Nœud L1)
// Lancer avec : cargo test --test l2_isolation_tests

use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::block::{Block, BlockHeader};
use wattcoin_core::transaction::{Transaction, TransactionType, TransactionOutput};
use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM};
use randomx_rs::{RandomXFlag, RandomXCache, RandomXVM};
use num_bigint::BigUint;

#[test]
fn test_reject_microcoinbase_on_l1() {
    let _ = std::fs::remove_dir_all(".test_db_iso_1");
    let mut chain = Blockchain::new(".test_db_iso_1").unwrap();
    let genesis_hash = chain.get_block_by_height(0).unwrap().header.hash.clone();
    
    chain.target = BigUint::from_bytes_be(&[0xFF; 32]);

    let coinbase_tx = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_L1".to_string(), kyber_capsule: "CAPSULE".to_string(),
            aes_vault: "15000000000".to_string(), lattice_commitment: LWECommitment::commit(15_000_000_000, &[0u64; LATTICE_DIM]),
        }],
        fee: 0, public_key: "COINBASE_SIG".to_string(), wots_signature: None,
    };

    let fraud_micro_tx = Transaction {
        tx_type: TransactionType::MicroCoinbase,
        inputs: vec![], outputs: vec![], fee: 0,
        public_key: "MICRO_COINBASE".to_string(), wots_signature: None,
    };

    let mut bad_block = Block {
        header: BlockHeader {
            index: 1, timestamp: chrono::Utc::now().timestamp(), previous_hash: genesis_hash.clone(),
            hash: "".to_string(), nonce: 0, target_hex: "FF".repeat(32), l2_root: "NO_L2".to_string(), tx_root: "".to_string(),
        },
        transactions: vec![coinbase_tx, fraud_micro_tx], 
    };
    
    bad_block.header.tx_root = bad_block.calculate_tx_root();

    let flags = RandomXFlag::get_recommended_flags();
    let cache = RandomXCache::new(flags, genesis_hash.as_bytes()).unwrap();
    let vm = RandomXVM::new(flags, Some(cache), None).unwrap();
    let header_data = format!("{}{}{}{}{}{}", bad_block.header.index, bad_block.header.timestamp, bad_block.header.previous_hash, bad_block.header.nonce, bad_block.header.l2_root, bad_block.header.tx_root);
    let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
    bad_block.header.hash = hex::encode(&hash_bytes);

    let result = chain.validate_and_add_external_block(bad_block);
    assert!(result.is_err(), "🚨 ALERTE FATALE : Le nœud L1 a accepté une MicroCoinbase L2 !");
}

#[test]
fn test_l2_root_anchoring_integrity() {
    let _ = std::fs::remove_dir_all(".test_db_iso_2");
    let mut chain = Blockchain::new(".test_db_iso_2").unwrap();
    let genesis_hash = chain.get_block_by_height(0).unwrap().header.hash.clone();
    chain.target = BigUint::from_bytes_be(&[0xFF; 32]);

    let coinbase_tx = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_L1".to_string(), kyber_capsule: "CAPSULE".to_string(),
            aes_vault: "15000000000".to_string(), lattice_commitment: LWECommitment::commit(15_000_000_000, &[0u64; LATTICE_DIM]),
        }],
        fee: 0, public_key: "COINBASE_SIG".to_string(), wots_signature: None,
    };

    let mut block = Block {
        header: BlockHeader {
            index: 1, timestamp: chrono::Utc::now().timestamp(), previous_hash: genesis_hash.clone(),
            hash: "".to_string(), nonce: 0, target_hex: "FF".repeat(32), l2_root: "REAL_L2_ROOT_FROM_SEQUENCER".to_string(), tx_root: "".to_string(),
        },
        transactions: vec![coinbase_tx],
    };
    block.header.tx_root = block.calculate_tx_root();

    let flags = RandomXFlag::get_recommended_flags();
    let cache = RandomXCache::new(flags, genesis_hash.as_bytes()).unwrap();
    let vm = RandomXVM::new(flags, Some(cache), None).unwrap();
    let header_data = format!("{}{}{}{}{}{}", block.header.index, block.header.timestamp, block.header.previous_hash, block.header.nonce, block.header.l2_root, block.header.tx_root);
    let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
    block.header.hash = hex::encode(&hash_bytes);

    let mut corrupted_block = block.clone();
    corrupted_block.header.l2_root = "FAKE_L2_ROOT_HACKED".to_string();

    let result = chain.validate_and_add_external_block(corrupted_block);
    assert!(result.is_err(), "🚨 ALERTE FATALE : Le nœud a accepté un ancrage L2 altéré !");
}

#[test]
fn test_tx_root_merkle_shield() {
    let _ = std::fs::remove_dir_all(".test_db_iso_3");
    let mut chain = Blockchain::new(".test_db_iso_3").unwrap();
    let genesis_hash = chain.get_block_by_height(0).unwrap().header.hash.clone();
    chain.target = BigUint::from_bytes_be(&[0xFF; 32]);

    let coinbase_tx = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_L1".to_string(), kyber_capsule: "CAPSULE".to_string(),
            aes_vault: "15000000000".to_string(), lattice_commitment: LWECommitment::commit(15_000_000_000, &[0u64; LATTICE_DIM]),
        }],
        fee: 0, public_key: "COINBASE_SIG".to_string(), wots_signature: None,
    };

    let mut block = Block {
        header: BlockHeader {
            index: 1, timestamp: chrono::Utc::now().timestamp(), previous_hash: genesis_hash.clone(),
            hash: "".to_string(), nonce: 0, target_hex: "FF".repeat(32), l2_root: "REAL_L2_ROOT".to_string(), tx_root: "".to_string(),
        },
        transactions: vec![coinbase_tx],
    };
    
    block.header.tx_root = block.calculate_tx_root();
    block.header.tx_root = "FAKE_TX_ROOT_HACKED".to_string();

    let result = chain.validate_and_add_external_block(block);
    assert!(result.is_err(), "🚨 ALERTE FATALE : Le nœud a accepté un tx_root altéré !");
}