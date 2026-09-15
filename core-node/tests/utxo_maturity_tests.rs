// tests/utxo_maturity_tests.rs
// Stress tests de la règle de Maturité des Coinbases
// Lancer avec : cargo test --test utxo_maturity_tests

use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::transaction::{Transaction, TransactionType, TransactionInput, TransactionOutput};
use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM};

fn dummy_l2_keys() -> Vec<(Vec<[u8; 32]>, Vec<u8>)> {
    vec![(vec![[0u8; 32]; 34], vec![0u8; 32]); 128]
}

fn create_valid_tx_with_source(source_height: u64) -> Transaction {
    let seed = [1u8; 32];
    let (sk, pk) = wots::Wots::generate_keypair(&seed, 0);

    let bf_in = vec![2u64; LATTICE_DIM];
    let bf_out = vec![2u64; LATTICE_DIM];
    let in_commit = LWECommitment::commit(10, &bf_in);
    let out_commit = LWECommitment::commit(9, &bf_out);

    let input = TransactionInput {
        commitment: in_commit,
        source_height, 
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
        public_key: hex::encode(&pk),
        wots_signature: None,
    };

    let tx_hash_64 = tx.hash_data();
    let mut tx_hash_32 = [0u8; 32];
    tx_hash_32.copy_from_slice(&tx_hash_64[0..32]);

    tx.wots_signature = Some(wots::Wots::sign(&sk, 0, &tx_hash_32, &pk));

    tx
}

#[test]
fn test_coinbase_maturity_rule() {
    let _ = std::fs::remove_dir_all(".test_db_utxo");
    let mut chain = Blockchain::new(".test_db_utxo").unwrap();
    
    let utxo_source_height = 1;
    let tx = create_valid_tx_with_source(utxo_source_height);
    assert!(tx.is_valid(), "Erreur Interne : La TX n'est pas valide.");

    // 💡 CORRECTION DU TEST : Il faut qu'au bloc 1, la blockchain génère un output Coinbase
    // qui correspond à la clé publique de notre testeur, sinon la règle de maturité
    // ne s'enclenche pas ! (L'argent classique n'a pas de période de gel).
    let coinbase_tx = Transaction {
        tx_type: TransactionType::Coinbase,
        inputs: vec![],
        outputs: vec![TransactionOutput {
            stealth_address: format!("COINBASE_{}", tx.public_key), // 🔥 ON IDENTIFIE LES FONDS COMME ÉTANT IMMATURES !
            kyber_capsule: "capsule".to_string(),
            aes_vault: "10".to_string(),
            lattice_commitment: LWECommitment::commit(10, &[0u64; LATTICE_DIM]),
        }],
        fee: 0,
        public_key: "COINBASE_SIG".to_string(),
        wots_signature: None,
    };

    let mut fake_block1 = chain.get_block_by_height(0).unwrap().clone();
    fake_block1.header.index = 1;
    fake_block1.transactions = vec![coinbase_tx]; // On injecte notre Coinbase
    chain.push_block(&fake_block1).unwrap(); 
    
    // Bloc actuel de la chaîne = 1
    // On prépare le bloc 2
    let (template_block2, _, _) = chain.prepare_block_template(vec![tx.clone()], "miner_test", dummy_l2_keys());
    
    assert_eq!(
        template_block2.transactions.len(), 1, 
        "🚨 ALERTE : Le mineur a inclus une transaction immature (1 confirmation) !"
    );
    
    chain.push_block(&template_block2).unwrap(); 
    
    let mut fake_block3 = template_block2.clone();
    fake_block3.header.index = 3;
    chain.push_block(&fake_block3).unwrap();             

    let (template_block4, _, _) = chain.prepare_block_template(vec![tx.clone()], "miner_test", dummy_l2_keys());
    
    assert_eq!(
        template_block4.transactions.len(), 2, 
        "✅ ÉCHEC : Le mineur a rejeté une transaction qui a pourtant atteint sa maturité (3 confirmations) !"
    );
}