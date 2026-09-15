// tests/htlc_contracts_tests.rs
// Stress tests des Contrats Intelligents HTLC (Atomic Swaps)
// Lancer avec : cargo test --test htlc_contracts_tests

use wattcoin_core::transaction::{Transaction, TransactionType};
use wattcoin_core::blockchain::Blockchain;
use sha2::{Sha256, Digest};

#[test]
fn test_htlc_claim_cryptographic_proof() {
    let secret = b"my_super_secret_preimage_for_atomic_swap";
    let secret_hex = hex::encode(secret);
    let valid_hash = hex::encode(Sha256::digest(secret));

    let tx_claim = Transaction { 
        tx_type: TransactionType::HTLCClaim { secret: secret_hex.clone() },
        inputs: vec![], outputs: vec![], fee: 0,
        public_key: valid_hash.clone(), 
        wots_signature: None,
    };

    assert!(tx_claim.is_valid(), "✅ ÉCHEC : Validateur a rejeté un HTLCClaim valide !");

    let mut tx_hack = tx_claim.clone();
    tx_hack.public_key = "fake_hash_of_a_fake_secret".to_string();

    assert!(!tx_hack.is_valid(), "🚨 ALERTE FATALE : Le validateur a accepté un mauvais secret !");
}

#[test]
fn test_htlc_refund_timelock() {
    let _ = std::fs::remove_dir_all(".test_db_htlc");
    let mut chain = Blockchain::new(".test_db_htlc").unwrap();
    
    let htlc_hash = "hash_du_contrat_atomic_swap".to_string();
    let timeout_block = 10; 

    let lock_tx = Transaction {
        tx_type: TransactionType::HTLCLock { hash: htlc_hash.clone(), timeout_block },
        inputs: vec![], outputs: vec![], fee: 0,
        public_key: "ALICE_LOCKER".to_string(), wots_signature: None,
    };
    
    let mut block1 = chain.get_block_by_height(0).unwrap().clone();
    block1.header.index = 1;
    block1.transactions.push(lock_tx);
    chain.push_block(&block1).unwrap();

    let refund_tx = Transaction {
        tx_type: TransactionType::HTLCRefund { hash: htlc_hash.clone() },
        inputs: vec![], outputs: vec![], fee: 0,
        public_key: "ALICE_REFUNDER".to_string(), wots_signature: None,
    };

    for i in 2..=4 {
        let mut b = chain.get_block_by_height(0).unwrap().clone();
        b.header.index = i;
        chain.push_block(&b).unwrap();
    }
    
    let (template_early, _, _) = chain.prepare_block_template(vec![refund_tx.clone()], "miner_test", vec![]);
    assert_eq!(template_early.transactions.len(), 1, "🚨 ALERTE : Le mineur a accepté un HTLCRefund avant l'expiration !");

    for i in 5..=10 {
        let mut b = chain.get_block_by_height(0).unwrap().clone();
        b.header.index = i;
        chain.push_block(&b).unwrap();
    }

    let (template_valid, _, _) = chain.prepare_block_template(vec![refund_tx.clone()], "miner_test", vec![]);
    assert_eq!(template_valid.transactions.len(), 2, "✅ ÉCHEC : Le mineur a rejeté un HTLCRefund expiré !");
}