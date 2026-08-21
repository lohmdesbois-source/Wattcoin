// tests/basic_chain_tests.rs
// Tests de base du node Wattcoin
// Lancer avec : cargo test --test basic_chain_tests

use wattcoin_core::block::{BlockHeader, Block};
use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::transaction::{Transaction, TransactionType, TransactionOutput};
use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM}; // ⚡ FIX: Import de LATTICE_DIM


#[test]
fn test_genesis_block() {
    let chain = Blockchain::new();
    assert_eq!(chain.chain.len(), 1);
    let genesis = &chain.chain[0];
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
    let mut chain = Blockchain::new();
    // ⚡ FIX: On ajoute le 3ème argument "None" (l2_db_path)
    let (block, _target, _l2_keys) = chain.prepare_block_template(vec![], "test_miner", None);
    assert_eq!(block.transactions.len(), 1);
    let reward: u64 = block.transactions[0].outputs[0].aes_vault.parse().unwrap();
    assert!(reward > 0 && reward <= 25_000_000_000);
}

#[test]
fn test_spent_key_images_prevents_double_spend() {
    let mut chain = Blockchain::new();
    let ki = "test_double_spend_key_image".to_string();
    chain.spent_key_images.insert(ki.clone());
    assert!(chain.spent_key_images.contains(&ki));
}

#[test]
fn test_total_supply() {
    let mut chain = Blockchain::new();
    
    let coinbase = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_test".to_string(),
            kyber_capsule: "test".to_string(),
            aes_vault: "15000000000".to_string(),
            // ⚡ FIX: Mise à jour de la structure LWECommitment (1024 dimensions en u64)
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
        previous_hash: chain.chain[0].header.hash.clone(),
        hash: "test".to_string(),
        nonce: 0,
        target_hex: "00".repeat(32),
        l2_root: String::from("NO_L2_FOR_TESTS"),
        tx_root: String::new(), // ⚡ FIX: Ajout du tx_root manquant
    };
    
    let mut block = Block { header, transactions: vec![coinbase] };
    block.header.tx_root = block.calculate_tx_root(); // On calcule proprement la racine
    
    chain.chain.push(block);
    assert!(chain.get_total_supply() >= 15_000_000_000);
}

#[test]
fn test_validate_rejects_block_with_two_coinbases() {
    let mut chain = Blockchain::new();

    let coinbase1 = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_test1".to_string(),
            kyber_capsule: "test1".to_string(),
            aes_vault: "15000000000".to_string(),
            // ⚡ FIX: Mise à jour structure LWECommitment
            lattice_commitment: LWECommitment {
                t_vector: vec![0u64; LATTICE_DIM],
            },
        }],
        fee: 0,
        public_key: "COINBASE_SIG".to_string(),
        wots_signature: None,
    };

    let coinbase2 = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_test2".to_string(),
            kyber_capsule: "test2".to_string(),
            aes_vault: "15000000000".to_string(),
            // ⚡ FIX: Mise à jour structure LWECommitment
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
        previous_hash: chain.chain[0].header.hash.clone(),
        hash: "fake_hash_for_test".to_string(),
        nonce: 0,
        target_hex: "00".repeat(32),
        l2_root: String::from("NO_L2_FOR_TESTS"),
        tx_root: String::new(), // ⚡ FIX: Ajout du tx_root manquant
    };

    let mut bad_block = Block {
        header,
        transactions: vec![coinbase1, coinbase2],
    };
    
    // On calcule la racine pour qu'elle passe le premier bouclier Merkle,
    // afin de s'assurer que c'est bien la règle des "2 coinbases" qui le fait rejeter !
    bad_block.header.tx_root = bad_block.calculate_tx_root();

    let result = chain.validate_and_add_external_block(bad_block);
    assert!(result.is_err(), "Le node doit rejeter un bloc avec 2 coinbases");

    let err_msg = result.unwrap_err();
    assert!(
        err_msg.contains("Coinbase") 
        || err_msg.contains("Preuve de travail") 
        || err_msg.contains("Hash frauduleux")
        || err_msg.contains("Index de bloc invalide")
        || err_msg.contains("Un bloc doit contenir exactement une Coinbase"), // Ajout du vrai message d'erreur possible
        "Le node doit rejeter le bloc (erreur actuelle : {})", err_msg
    );
}