// tests/utxo_maturity_tests.rs
// Stress tests de la règle de Maturité des Coinbases
// Lancer avec : cargo test --test utxo_maturity_tests

use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::transaction::{Transaction, TransactionType, TransactionInput, TransactionOutput};
use wattcoin_core::merkle_ring::MpcRingSignature;
use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM};
use wattcoin_core::wots::{WotsKeyPair, WotsSignature};

/// 🛠️ Helper pour générer une transaction cryptographiquement parfaite
/// mais dont on peut truquer la date d'émission (source_height)
fn create_valid_tx_with_source(source_height: u64) -> Transaction {
    // 1. Crypto WOTS (La vraie clé + 63 leurres pour passer le Ring Shield)
    let keypair = WotsKeyPair::generate();
    let mut decoys = vec![keypair.public_key.clone()];
    for _ in 1..64 { decoys.push(WotsKeyPair::generate().public_key); }

    // 2. Crypto Lattice (Homomorphisme parfait : 10 = 9 + 1 de frais)
    let bf_in = vec![2u64; LATTICE_DIM];
    let bf_out = vec![2u64; LATTICE_DIM];
    let in_commit = LWECommitment::commit(10, &bf_in);
    let out_commit = LWECommitment::commit(9, &bf_out);

    let input = TransactionInput {
        mpc_ring: MpcRingSignature {
            key_image: "fake_ki_for_test".to_string(),
            ring_root: "temp".to_string(),
            ring_decoys: decoys.clone(),
            real_wots_sig: WotsSignature { chains: vec![] },
            merkle_proof: vec![],
        },
        commitment: in_commit,
        source_height, // 🔥 ON RÈGLE L'ÂGE DU BILLET ICI
    };

    let output = TransactionOutput {
        stealth_address: "DESTINATION".to_string(),
        kyber_capsule: "capsule".to_string(),
        aes_vault: "9".to_string(),
        lattice_commitment: out_commit,
    };

    let mut tx = Transaction {
        tx_type: TransactionType::Standard,
        inputs: vec![input],
        outputs: vec![output],
        fee: 1,
        public_key: keypair.public_key.clone(),
        wots_signature: None,
    };

    // 3. Signature Cryptographique Définitive
    let tx_hash = tx.hash_data();
    tx.inputs[0].mpc_ring = MpcRingSignature::sign(
        &keypair.secret_key, &tx_hash, &decoys, 0, "capsule", b"secret"
    );
    tx.wots_signature = Some(WotsKeyPair::sign(
        &keypair.secret_key, &keypair.public_seed, &tx_hash
    ));

    tx
}

#[test]
fn test_coinbase_maturity_rule() {
    let mut chain = Blockchain::new();
    
    // 1. On crée artificiellement le Bloc 1 (celui où l'UTXO a été miné)
    let utxo_source_height = 1;
    let mut fake_block1 = chain.chain[0].clone();
    fake_block1.header.index = 1;
    chain.chain.push(fake_block1); // La chaîne contient maintenant [Genesis, Bloc 1]. Taille = 2.
    
    // On génère la transaction de dépense.
    let tx = create_valid_tx_with_source(utxo_source_height);
    assert!(tx.is_valid(), "Erreur Interne : La TX n'est pas valide.");

    // ====================================================================
    // TENTATIVE 1 : Le mineur tente d'inclure la dépense dans le Bloc 2
    // ====================================================================
    // current_height = 2. Confirmations = 2 - 1 = 1. (Il en faut 3 !)
    let (template_block2, _, _) = chain.prepare_block_template(vec![tx.clone()], "miner_test", None);
    
    assert_eq!(
        template_block2.transactions.len(), 1, 
        "🚨 ALERTE : Le mineur a inclus une transaction immature (1 confirmation) !"
    );
    
    // On fait avancer le temps : on ajoute le Bloc 2 et le Bloc 3
    chain.chain.push(template_block2.clone()); // Ajout Bloc 2 (Taille de la chaîne = 3)
    
    let mut fake_block3 = template_block2.clone();
    fake_block3.header.index = 3;
    chain.chain.push(fake_block3);             // Ajout Bloc 3 (Taille de la chaîne = 4)

    // ====================================================================
    // TENTATIVE 2 : Le mineur tente d'inclure la dépense dans le Bloc 4
    // ====================================================================
    // current_height = 4. Confirmations = 4 - 1 = 3. 
    // L'argent est enfin décongelé !
    let (template_block4, _, _) = chain.prepare_block_template(vec![tx.clone()], "miner_test", None);
    
    // Le bloc doit contenir la Coinbase (idx 0) ET notre transaction publique (idx 1).
    assert_eq!(
        template_block4.transactions.len(), 2, 
        "✅ ÉCHEC : Le mineur a rejeté une transaction qui a pourtant atteint sa maturité (3 confirmations) !"
    );
}