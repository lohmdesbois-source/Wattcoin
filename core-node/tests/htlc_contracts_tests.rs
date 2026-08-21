// tests/htlc_contracts_tests.rs
// Stress tests des Contrats Intelligents HTLC (Atomic Swaps)
// Lancer avec : cargo test --test htlc_contracts_tests

use wattcoin_core::transaction::{Transaction, TransactionType};
use wattcoin_core::blockchain::Blockchain;
use sha2::{Sha256, Digest};

#[test]
fn test_htlc_claim_cryptographic_proof() {
    // ====================================================================
    // 1. PRÉPARATION : L'utilisateur Bob a le secret du cadenas
    // ====================================================================
    let secret = b"my_super_secret_preimage_for_atomic_swap";
    let secret_hex = hex::encode(secret);
    
    // Le hash SHA-256 du secret (C'est ce hash qui verrouille le contrat)
    let valid_hash = hex::encode(Sha256::digest(secret));

    // ====================================================================
    // 2. LE CAS PARFAIT : Bob réclame les fonds avec le bon secret
    // ====================================================================
    let tx_claim = Transaction { // 💡 CORRECTION : Plus besoin de "mut" ici
        tx_type: TransactionType::HTLCClaim { secret: secret_hex.clone() },
        inputs: vec![],
        outputs: vec![],
        fee: 0,
        public_key: valid_hash.clone(), // Le validateur va comparer le hash du secret avec ceci
        wots_signature: None,
    };

    assert!(
        tx_claim.is_valid(), 
        "✅ ÉCHEC : Le validateur a rejeté un HTLCClaim alors que le secret est parfaitement valide !"
    );

    // ====================================================================
    // 3. LA FALSIFICATION : Un pirate essaie de deviner ou modifier
    // ====================================================================
    let mut tx_hack = tx_claim.clone();
    // Le pirate change la clé publique ciblée ou utilise un mauvais secret
    tx_hack.public_key = "fake_hash_of_a_fake_secret".to_string();

    assert!(
        !tx_hack.is_valid(), 
        "🚨 ALERTE FATALE : Le validateur a accepté de débloquer les fonds pour un mauvais secret !"
    );
}

#[test]
fn test_htlc_refund_timelock() {
    let mut chain = Blockchain::new();
    
    let htlc_hash = "hash_du_contrat_atomic_swap".to_string();
    let timeout_block = 10; // Le contrat expire au bloc 10

    // ====================================================================
    // 1. SCELLEMENT DU CONTRAT (Au Bloc 1)
    // ====================================================================
    let lock_tx = Transaction {
        tx_type: TransactionType::HTLCLock { hash: htlc_hash.clone(), timeout_block },
        inputs: vec![], outputs: vec![], fee: 0,
        public_key: "ALICE_LOCKER".to_string(), wots_signature: None,
    };
    
    // On avance artificiellement la chaîne jusqu'au bloc 1 et on y inclut le HTLCLock
    let mut block1 = chain.chain[0].clone();
    block1.header.index = 1;
    block1.transactions.push(lock_tx);
    chain.chain.push(block1);

    // ====================================================================
    // 2. PREMIÈRE TENTATIVE DE REMBOURSEMENT (Au Bloc 5 - TROP TÔT)
    // ====================================================================
    let refund_tx = Transaction {
        tx_type: TransactionType::HTLCRefund { hash: htlc_hash.clone() },
        inputs: vec![], outputs: vec![], fee: 0,
        public_key: "ALICE_REFUNDER".to_string(), wots_signature: None,
    };

    // On fait avancer le temps jusqu'au bloc 4
    for i in 2..=4 {
        let mut b = chain.chain[0].clone();
        b.header.index = i;
        chain.chain.push(b);
    }
    
    // Alice panique et essaie de récupérer ses fonds au bloc 5
    // 💡 CORRECTION : Ajout du "None" comme 3ème argument
    let (template_early, _, _) = chain.prepare_block_template(vec![refund_tx.clone()], "miner_test", None);
    
    // Le mineur DOIT rejeter la transaction, il ne reste que la Coinbase dans le template
    assert_eq!(
        template_early.transactions.len(), 1, 
        "🚨 ALERTE : Le mineur a accepté un HTLCRefund avant l'expiration du délai !"
    );

    // ====================================================================
    // 3. SECONDE TENTATIVE (Au Bloc 11 - DÉLAI EXPIRÉ)
    // ====================================================================
    // On fait avancer le temps jusqu'au bloc 10
    for i in 5..=10 {
        let mut b = chain.chain[0].clone();
        b.header.index = i;
        chain.chain.push(b);
    }

    // Alice demande son remboursement au bloc 11
    // 💡 CORRECTION : Ajout du "None" comme 3ème argument
    let (template_valid, _, _) = chain.prepare_block_template(vec![refund_tx.clone()], "miner_test", None);
    
    // Le mineur accepte la transaction (Coinbase + Refund)
    assert_eq!(
        template_valid.transactions.len(), 2, 
        "✅ ÉCHEC : Le mineur a rejeté un HTLCRefund alors que le délai est expiré !"
    );
}