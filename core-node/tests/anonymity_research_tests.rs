// tests/anonymity_research_tests.rs
// Tests de cryptographie avancée : Distribution Gamma et Résistance à l'analyse de chaîne
// Lancer avec : cargo test --test anonymity_research_tests

use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::block::{Block, BlockHeader};
use wattcoin_core::transaction::{Transaction, TransactionType, TransactionOutput};
use wattcoin_core::lattice::LWECommitment;

#[test]
fn test_gamma_distribution_decoy_selection() {
    let mut chain = Blockchain::new();

    // 1. On peuple la blockchain de 100 000 faux UTXOs pour simuler un historique riche.
    let mut all_outputs = Vec::new();
    for i in 0..100_000 {
        all_outputs.push(TransactionOutput {
            stealth_address: format!("STEALTH_{}", i),
            kyber_capsule: "CAPSULE".to_string(),
            aes_vault: "100".to_string(),
            lattice_commitment: LWECommitment { t_vector: vec![0u64; 1024] },
        });
    }

    let fake_tx = Transaction {
        tx_type: TransactionType::Standard,
        inputs: vec![],
        outputs: all_outputs,
        fee: 0, public_key: "FAKE".to_string(), wots_signature: None,
    };

    let fake_block = Block {
        header: BlockHeader {
            index: 1, timestamp: 0, previous_hash: "".to_string(), hash: "".to_string(),
            nonce: 0, target_hex: "".to_string(), l2_root: "".to_string(), tx_root: "".to_string(),
        },
        transactions: vec![fake_tx],
    };
    chain.chain.push(fake_block);

    // 2. On demande 1000 leurres au réseau
    let decoys = chain.get_random_decoys(1000);
    assert_eq!(decoys.len(), 1000, "Le nombre de leurres est incorrect.");

    // 3. TRIBUNAL STATISTIQUE : On calcule la moyenne des index sélectionnés.
    // Si la sélection était purement uniforme (hasard total), la moyenne serait autour de 50 000 (le milieu).
    // Avec notre patch Gamma/Maximum, la moyenne DOIT être poussée vers les ~75 000 (UTXOs récents).
    let mut sum_indices = 0u64;
    for decoy in &decoys {
        // On extrait l'index de la string "STEALTH_XXXX"
        let parts: Vec<&str> = decoy.split('_').collect();
        let idx: u64 = parts[1].parse().unwrap();
        sum_indices += idx;
    }
    
    let average_index = sum_indices / 1000;

    println!("📈 Moyenne de l'index des leurres : {} / 100 000", average_index);

    assert!(
        average_index > 65_000, 
        "🚨 ALERTE ANONYMAT : La distribution n'est pas biaisée vers la nouveauté (Moyenne: {}). Vulnérabilité temporelle !", average_index
    );
}

#[test]
fn test_ring_signature_key_image_determinism() {
    // ====================================================================
    // SCÉNARIO : Alice veut dépenser un billet. 
    // Pour ne pas révéler qui elle est, elle crée un anneau.
    // Si elle dépense le billet une 2ème fois (Double Dépense), la "Key Image"
    // générée DOIT être strictement identique, même si les leurres sont différents !
    // ====================================================================
    use wattcoin_core::wots::WotsKeyPair;
    use wattcoin_core::merkle_ring::MpcRingSignature;

    let alice_keys = WotsKeyPair::generate();
    let tx_hash = [0u8; 64]; // Hash de la transaction
    let real_capsule = "CAPSULE_SECRETE_UTXO_42";
    let kyber_secret = b"SECRET_PERMANENT_ALICE";

    // 1. Première signature (Anneau A)
    let mut ring_a = vec![alice_keys.public_key.clone()];
    for _ in 0..64 { ring_a.push(WotsKeyPair::generate().public_key); }

    let sig_a = MpcRingSignature::sign(
        &alice_keys.secret_key, &tx_hash, &ring_a, 0, real_capsule, kyber_secret
    );

    // 2. Seconde signature pour LE MÊME BILLET (Anneau B, leurres totalement différents)
    let mut ring_b = vec![alice_keys.public_key.clone()];
    for _ in 0..64 { ring_b.push(WotsKeyPair::generate().public_key); }

    let sig_b = MpcRingSignature::sign(
        &alice_keys.secret_key, &tx_hash, &ring_b, 0, real_capsule, kyber_secret
    );

    // 3. TRIBUNAL CYPERPUNK : Les Key Images doivent être identiques pour empêcher la double dépense
    assert_eq!(
        sig_a.key_image, sig_b.key_image,
        "🚨 ALERTE FATALE : Les Key Images diffèrent pour le même UTXO ! Double dépense possible !"
    );

    // 4. Par contre, les racines des anneaux DOIVENT être différentes (Anonymat EAE préservé)
    assert_ne!(
        sig_a.ring_root, sig_b.ring_root,
        "🚨 ALERTE ANONYMAT : L'empreinte de l'anneau est identique, risque de désanonymisation par intersection !"
    );
}