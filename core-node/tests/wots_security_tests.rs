// tests/wots_security_tests.rs
// Stress tests de la cryptographie post-quantique WOTS+
// Lancer avec : cargo test --test wots_security_tests

use sha2::{Sha512, Digest};

#[test]
fn test_wots_signature_valid_and_falsified() {
    // 💡 Nouveau WOTS+ (Module externe)
    let seed = [0u8; 32];
    let (sk, pk) = wots::Wots::generate_keypair(&seed, 0);
    
    let message = "Ceci est une transaction WATTCOIN top secrète.".as_bytes();
    let mut hasher = Sha512::new();
    hasher.update(message);
    
    let hash_64 = hasher.finalize();
    let mut message_hash = [0u8; 32];
    message_hash.copy_from_slice(&hash_64[0..32]);

    // ====================================================================
    // 2. LE CAS PARFAIT : Signature et vérification nominales
    // ====================================================================
    let valid_signature = wots::Wots::sign(&sk, 0, &message_hash, &pk);
    let is_valid = wots::Wots::verify(&valid_signature, &message_hash);
    assert!(is_valid, "✅ ÉCHEC : Le Tribunal Quantique a rejeté une signature parfaitement valide !");

    // ====================================================================
    // 3. FALSIFICATION N°1 : Modification d'un seul octet dans la signature
    // ====================================================================
    let mut falsified_sig = valid_signature.clone();
    
    // On modifie sournoisement le tout premier bit
    if !falsified_sig.signature_bytes.is_empty() {
        falsified_sig.signature_bytes[0] ^= 0x01;
    }

    let is_valid_after_sig_tampering = wots::Wots::verify(&falsified_sig, &message_hash);
    assert!(!is_valid_after_sig_tampering, "🚨 ALERTE : Le Tribunal a accepté une signature altérée !");

    // ====================================================================
    // 4. FALSIFICATION N°2 : Modification du message (L'attaque classique)
    // ====================================================================
    let mut altered_message_hash = message_hash.clone();
    
    // Un hacker intercepte le message et change juste 1 bit (0x01) du hash
    altered_message_hash[0] ^= 0x01; 

    let is_valid_after_msg_tampering = wots::Wots::verify(&valid_signature, &altered_message_hash);
    assert!(!is_valid_after_msg_tampering, "🚨 ALERTE : Le Tribunal a accepté un hash corrompu !");
}