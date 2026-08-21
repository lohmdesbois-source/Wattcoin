// tests/ring_signature_tests.rs
// Stress tests du Bouclier d'Anonymat (MpcRingSignature)
// Lancer avec : cargo test --test ring_signature_tests

use wattcoin_core::merkle_ring::MpcRingSignature;
use wattcoin_core::wots::WotsKeyPair;
use sha2::{Sha512, Digest};

#[test]
fn test_ring_signature_decoy_shield() {
    // ====================================================================
    // 1. PRÉPARATION : Génération de la vraie clé et du hash de transaction
    // ====================================================================
    let real_keypair = WotsKeyPair::generate();
    
    let message = b"Transaction test pour les signatures d'anneau";
    let mut hasher = Sha512::new();
    hasher.update(message);
    let mut tx_hash = [0u8; 64];
    tx_hash.copy_from_slice(&hasher.finalize());
    
    let kyber_secret = b"secret_permanent_utilisateur_123";
    let real_capsule = "capsule_de_test";

    // ====================================================================
    // 2. CAS BLOQUANT : L'Anneau est trop faible (ex: 10 leurres)
    // ====================================================================
    let mut weak_decoys = Vec::new();
    weak_decoys.push(real_keypair.public_key.clone()); // La vraie clé
    for _ in 1..10 { 
        // Ajout de 9 faux leurres
        weak_decoys.push(WotsKeyPair::generate().public_key);
    }

    // Le wallet signe la transaction avec seulement 10 leurres (interdit)
    let weak_ring_sig = MpcRingSignature::sign(
        &real_keypair.secret_key,
        &tx_hash,
        &weak_decoys,
        0, // L'index de notre vraie clé
        real_capsule,
        kyber_secret
    );

    // Le Tribunal vérifie
    let is_valid_weak = weak_ring_sig.verify(&tx_hash);
    assert!(!is_valid_weak, "🚨 ALERTE FATALE : Le Tribunal a accepté une transaction avec un niveau d'anonymat trop faible (10 leurres) !");

    // ====================================================================
    // 3. CAS NOMINAL : L'Anneau est conforme à la règle (64 leurres min)
    // ====================================================================
    let mut strong_decoys = Vec::new();
    strong_decoys.push(real_keypair.public_key.clone());
    for _ in 1..64 { 
        // Ajout de 63 faux leurres (total 64)
        strong_decoys.push(WotsKeyPair::generate().public_key);
    }

    let strong_ring_sig = MpcRingSignature::sign(
        &real_keypair.secret_key,
        &tx_hash,
        &strong_decoys,
        0,
        real_capsule,
        kyber_secret
    );

    let is_valid_strong = strong_ring_sig.verify(&tx_hash);
    assert!(is_valid_strong, "✅ ÉCHEC : Le Tribunal a rejeté une signature d'anneau parfaitement valide avec 64 leurres !");
}