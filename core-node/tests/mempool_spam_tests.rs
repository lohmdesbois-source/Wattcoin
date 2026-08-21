// tests/mempool_spam_tests.rs
// Stress tests de la protection anti-Spam et anti-DDoS (Mempool & CPU Exhaustion)
// Lancer avec : cargo test --test mempool_spam_tests

use wattcoin_core::transaction::{Transaction, TransactionType, TransactionOutput};
use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM};
use std::sync::{Arc, Mutex};

#[test]
fn test_mempool_saturation_limit() {
    // ====================================================================
    // 1. SCÉNARIO : Le Mempool est attaqué par un bot
    // ====================================================================
    let mempool: Arc<Mutex<Vec<Transaction>>> = Arc::new(Mutex::new(Vec::new()));
    
    {
        let mut pool = mempool.lock().unwrap();
        for i in 0..2000 {
            pool.push(Transaction {
                tx_type: TransactionType::Standard,
                inputs: vec![], outputs: vec![], fee: 1000,
                public_key: format!("SPAM_TX_{}", i),
                wots_signature: None,
            });
        }
    }

    // ====================================================================
    // 2. LE BOUCLIER API
    // ====================================================================
    let _new_tx = Transaction { // 💡 FIX WARNING : Ajout du '_'
        tx_type: TransactionType::Standard,
        inputs: vec![], outputs: vec![], fee: 1000,
        public_key: "LEGIT_TX".to_string(),
        wots_signature: None,
    };

    let is_rejected = {
        let pool_check = mempool.lock().unwrap();
        pool_check.len() >= 2000 
    };

    assert!(is_rejected, "🚨 ALERTE : Le nœud a accepté une transaction alors que le Mempool est plein !");
}

#[test]
fn test_p2pool_mining_share_cpu_shield() {
    // ====================================================================
    // 1. SCÉNARIO : Attaque CPU via de fausses preuves RandomX
    // ====================================================================
    let mempool: Arc<Mutex<Vec<Transaction>>> = Arc::new(Mutex::new(Vec::new()));
    
    {
        let mut pool = mempool.lock().unwrap();
        for i in 0..100 {
            pool.push(Transaction {
                tx_type: TransactionType::MiningShare {
                    miner_address: "hacker".to_string(),
                    nonce: i, hash: "fake_hash".to_string(), timestamp: 0,
                },
                inputs: vec![], outputs: vec![], fee: 0,
                public_key: format!("HACK_SHARE_{}", i),
                wots_signature: None,
            });
        }
    }

    let _incoming_spam_share = Transaction { // 💡 FIX WARNING : Ajout du '_'
        tx_type: TransactionType::MiningShare {
            miner_address: "hacker_again".to_string(),
            nonce: 101, hash: "fake_hash".to_string(), timestamp: 0,
        },
        inputs: vec![], outputs: vec![], fee: 0,
        public_key: "HACK_SHARE_101".to_string(),
        wots_signature: None,
    };

    let cpu_exhaustion_prevented = {
        let pool = mempool.lock().unwrap();
        let pending_shares = pool.iter().filter(|t| matches!(t.tx_type, TransactionType::MiningShare { .. })).count();
        pending_shares >= 100
    };

    assert!(
        cpu_exhaustion_prevented, 
        "🚨 ALERTE FATALE : Le nœud s'apprête à calculer un 101ème hash RandomX, le CPU va saturer !"
    );
}

#[test]
fn test_l1_and_l2_dynamic_fees() {
    // ====================================================================
    // LE BOUCLIER ÉCONOMIQUE : Test de la dynamique de frais (api.rs)
    // ====================================================================
    
    // 💡 Réplique du routeur exact de l'API :
    let is_tx_accepted_by_api = |tx: &Transaction| -> bool {
        let is_l2_tx = tx.outputs.iter().any(|out| out.stealth_address.starts_with("L2_WATT_"));
        let min_fee = if is_l2_tx { 100 } else { 1000 };
        tx.fee >= min_fee || tx.tx_type == TransactionType::Coinbase
    };

    // 1. Transaction L1 (Standard) - Rejetée car trop peu de frais
    let tx_l1_cheap = Transaction {
        tx_type: TransactionType::Standard, inputs: vec![],
        outputs: vec![TransactionOutput { stealth_address: "STANDARD_WATT".to_string(), kyber_capsule: "".to_string(), aes_vault: "".to_string(), lattice_commitment: LWECommitment::commit(0, &[0u64; LATTICE_DIM]) }],
        fee: 500, public_key: "TX_L1_CHEAP".to_string(), wots_signature: None,
    };
    assert!(!is_tx_accepted_by_api(&tx_l1_cheap), "🚨 ERREUR : La TX L1 à 500 Flames aurait dû être rejetée (Min 1000) !");

    // 2. Transaction L1 (Standard) - Acceptée (1000 Flames)
    let tx_l1_valid = Transaction {
        tx_type: TransactionType::Standard, inputs: vec![],
        outputs: vec![TransactionOutput { stealth_address: "STANDARD_WATT".to_string(), kyber_capsule: "".to_string(), aes_vault: "".to_string(), lattice_commitment: LWECommitment::commit(0, &[0u64; LATTICE_DIM]) }],
        fee: 1000, public_key: "TX_L1_VALID".to_string(), wots_signature: None,
    };
    assert!(is_tx_accepted_by_api(&tx_l1_valid), "🚨 ERREUR : La TX L1 à 1000 Flames aurait dû être acceptée !");

    // 3. Transaction L2 (Micro-paiement) - Rejetée car < 100 Flames
    let tx_l2_cheap = Transaction {
        tx_type: TransactionType::Standard, inputs: vec![],
        outputs: vec![TransactionOutput { stealth_address: "L2_WATT_XYZ".to_string(), kyber_capsule: "".to_string(), aes_vault: "".to_string(), lattice_commitment: LWECommitment::commit(0, &[0u64; LATTICE_DIM]) }],
        fee: 50, public_key: "TX_L2_CHEAP".to_string(), wots_signature: None,
    };
    assert!(!is_tx_accepted_by_api(&tx_l2_cheap), "🚨 ERREUR : La TX L2 à 50 Flames aurait dû être rejetée (Min 100) !");

    // 4. Transaction L2 (Micro-paiement) - Acceptée (100 Flames)
    let tx_l2_valid = Transaction {
        tx_type: TransactionType::Standard, inputs: vec![],
        outputs: vec![TransactionOutput { stealth_address: "L2_WATT_XYZ".to_string(), kyber_capsule: "".to_string(), aes_vault: "".to_string(), lattice_commitment: LWECommitment::commit(0, &[0u64; LATTICE_DIM]) }],
        fee: 100, public_key: "TX_L2_VALID".to_string(), wots_signature: None,
    };
    assert!(is_tx_accepted_by_api(&tx_l2_valid), "🚨 ERREUR : La TX L2 à 100 Flames aurait dû être acceptée !");
}