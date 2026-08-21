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
    let mut chain = Blockchain::new();
    let genesis_hash = chain.chain[0].header.hash.clone();
    
    // 💡 HACK DE TEST : On désactive la difficulté du PoW pour ce test 
    // afin que le Tribunal ne rejette le bloc QUE pour la MicroCoinbase.
    chain.target = BigUint::from_bytes_be(&[0xFF; 32]);

    // ====================================================================
    // 1. Création de la Coinbase L1 obligatoire
    // ====================================================================
    let coinbase_tx = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_L1".to_string(),
            kyber_capsule: "CAPSULE".to_string(),
            aes_vault: "15000000000".to_string(),
            lattice_commitment: LWECommitment::commit(15_000_000_000, &[0u64; LATTICE_DIM]),
        }],
        fee: 0,
        public_key: "COINBASE_SIG".to_string(),
        wots_signature: None,
    };

    // ====================================================================
    // 2. 🚨 LE CHEVAL DE TROIE : Une MicroCoinbase L2 injectée
    // ====================================================================
    let fraud_micro_tx = Transaction {
        tx_type: TransactionType::MicroCoinbase,
        inputs: vec![],
        outputs: vec![],
        fee: 0,
        public_key: "MICRO_COINBASE".to_string(),
        wots_signature: None,
    };

    // ====================================================================
    // 3. Assemblage et Minage (Forgeage) du bloc externe
    // ====================================================================
    let mut bad_block = Block {
        header: BlockHeader {
            index: 1,
            timestamp: chrono::Utc::now().timestamp(),
            previous_hash: genesis_hash.clone(),
            hash: "".to_string(), 
            nonce: 0,
            target_hex: "FF".repeat(32),
            l2_root: "NO_L2".to_string(),
            tx_root: "".to_string(),
        },
        transactions: vec![coinbase_tx, fraud_micro_tx], // Coinbase L1 + MicroCoinbase L2
    };
    
    // On calcule la vraie racine Merkle pour passer le premier filtre de structure
    bad_block.header.tx_root = bad_block.calculate_tx_root();

    // On hache avec RandomX pour que le bloc soit "validé" par le PoW
    let flags = RandomXFlag::get_recommended_flags();
    let cache = RandomXCache::new(flags, genesis_hash.as_bytes()).unwrap();
    let vm = RandomXVM::new(flags, Some(cache), None).unwrap();
    let header_data = format!("{}{}{}{}{}{}", 
        bad_block.header.index, bad_block.header.timestamp, bad_block.header.previous_hash, 
        bad_block.header.nonce, bad_block.header.l2_root, bad_block.header.tx_root
    );
    let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
    bad_block.header.hash = hex::encode(&hash_bytes);

    // ====================================================================
    // 4. LE TRIBUNAL : On soumet le bloc L1
    // ====================================================================
    let result = chain.validate_and_add_external_block(bad_block);

    assert!(result.is_err(), "🚨 ALERTE FATALE : Le nœud L1 a accepté une MicroCoinbase L2 !");
    
    let error_msg = result.unwrap_err();
    assert!(
        error_msg.contains("MicroCoinbase"), 
        "L'erreur retournée ne correspond pas au rejet L2. Erreur reçue: {}", error_msg
    );
}

#[test]
fn test_l2_root_anchoring_integrity() {
    let mut chain = Blockchain::new();
    let genesis_hash = chain.chain[0].header.hash.clone();
    chain.target = BigUint::from_bytes_be(&[0xFF; 32]);

    // 1. On crée un bloc normal et valide
    let coinbase_tx = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_L1".to_string(),
            kyber_capsule: "CAPSULE".to_string(),
            aes_vault: "15000000000".to_string(),
            lattice_commitment: LWECommitment::commit(15_000_000_000, &[0u64; LATTICE_DIM]),
        }],
        fee: 0,
        public_key: "COINBASE_SIG".to_string(),
        wots_signature: None,
    };

    let mut block = Block {
        header: BlockHeader {
            index: 1,
            timestamp: chrono::Utc::now().timestamp(),
            previous_hash: genesis_hash.clone(),
            hash: "".to_string(), 
            nonce: 0,
            target_hex: "FF".repeat(32),
            l2_root: "REAL_L2_ROOT_FROM_SEQUENCER".to_string(),
            tx_root: "".to_string(),
        },
        transactions: vec![coinbase_tx],
    };
    block.header.tx_root = block.calculate_tx_root();

    // 2. Le mineur scelle le bloc avec le PoW RandomX
    let flags = RandomXFlag::get_recommended_flags();
    let cache = RandomXCache::new(flags, genesis_hash.as_bytes()).unwrap();
    let vm = RandomXVM::new(flags, Some(cache), None).unwrap();
    let header_data = format!("{}{}{}{}{}{}", 
        block.header.index, block.header.timestamp, block.header.previous_hash, 
        block.header.nonce, block.header.l2_root, block.header.tx_root
    );
    let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
    block.header.hash = hex::encode(&hash_bytes);

    // ====================================================================
    // 3. L'ATTAQUE : Un hacker modifie l'historique L2
    // ====================================================================
    let mut corrupted_block = block.clone();
    corrupted_block.header.l2_root = "FAKE_L2_ROOT_HACKED".to_string();

    let result = chain.validate_and_add_external_block(corrupted_block);

    assert!(result.is_err(), "🚨 ALERTE FATALE : Le nœud a accepté un ancrage L2 altéré !");
    
    let error_msg = result.unwrap_err();
    assert!(
        error_msg.contains("Hash frauduleux"), 
        "L'erreur devrait être 'Hash frauduleux'. Reçu: {}", error_msg
    );
}

#[test]
fn test_tx_root_merkle_shield() {
    let mut chain = Blockchain::new();
    let genesis_hash = chain.chain[0].header.hash.clone();
    chain.target = BigUint::from_bytes_be(&[0xFF; 32]); // Bypass PoW target

    let coinbase_tx = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: "COINBASE_L1".to_string(),
            kyber_capsule: "CAPSULE".to_string(),
            aes_vault: "15000000000".to_string(),
            lattice_commitment: LWECommitment::commit(15_000_000_000, &[0u64; LATTICE_DIM]),
        }],
        fee: 0,
        public_key: "COINBASE_SIG".to_string(),
        wots_signature: None,
    };

    let mut block = Block {
        header: BlockHeader {
            index: 1,
            timestamp: chrono::Utc::now().timestamp(),
            previous_hash: genesis_hash.clone(),
            hash: "".to_string(), 
            nonce: 0,
            target_hex: "FF".repeat(32),
            l2_root: "REAL_L2_ROOT".to_string(),
            tx_root: "".to_string(),
        },
        transactions: vec![coinbase_tx],
    };
    
    // On calcule la vraie racine
    block.header.tx_root = block.calculate_tx_root();

    // LE HACK : On modifie la racine des transactions sans toucher aux transactions elles-mêmes
    block.header.tx_root = "FAKE_TX_ROOT_HACKED".to_string();

    // On soumet au nœud
    let result = chain.validate_and_add_external_block(block);

    assert!(result.is_err(), "🚨 ALERTE FATALE : Le nœud a accepté un tx_root altéré !");
    assert!(
        result.unwrap_err().contains("racine de Merkle"), 
        "Le bouclier Merkle n'a pas détecté la fraude !"
    );
}