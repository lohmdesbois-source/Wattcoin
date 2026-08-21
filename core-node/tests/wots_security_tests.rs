// tests/wots_security_tests.rs
// Stress tests de la cryptographie post-quantique WOTS+
// Lancer avec : cargo test --test wots_security_tests

use wattcoin_core::wots::WotsKeyPair;
use sha2::{Sha512, Digest};

#[test]
fn test_wots_signature_valid_and_falsified() {
    // ====================================================================
    // 1. PRÉPARATION : Génération des clés et du message top secret
    // ====================================================================
    let keypair = WotsKeyPair::generate();
    
    // On crée un hash de message de 64 octets (simulation de tx_hash)
    let message = "Ceci est une transaction WATTCOIN top secrète.".as_bytes();
    let mut hasher = Sha512::new();
    hasher.update(message);
    let mut message_hash = [0u8; 64];
    message_hash.copy_from_slice(&hasher.finalize());

    // ====================================================================
    // 2. LE CAS PARFAIT : Signature et vérification nominales
    // ====================================================================
    let valid_signature = WotsKeyPair::sign(&keypair.secret_key, &keypair.public_seed, &message_hash);
    let is_valid = WotsKeyPair::verify(&keypair.public_key, &valid_signature, &message_hash);
    assert!(is_valid, "✅ ÉCHEC : Le Tribunal Quantique a rejeté une signature parfaitement valide !");

    // ====================================================================
    // 3. FALSIFICATION N°1 : Modification d'un seul octet dans la signature
    // ====================================================================
    let mut falsified_sig = valid_signature.clone();
    
    // On récupère la toute première chaîne hexadécimale de la signature
    let first_chain = &falsified_sig.chains[0];
    let mut altered_chain = first_chain.clone();
    
    // On modifie sournoisement le tout premier caractère
    let fake_char = if altered_chain.chars().next().unwrap() == '0' { '1' } else { '0' };
    altered_chain.replace_range(0..1, &fake_char.to_string());
    falsified_sig.chains[0] = altered_chain;

    let is_valid_after_sig_tampering = WotsKeyPair::verify(&keypair.public_key, &falsified_sig, &message_hash);
    assert!(!is_valid_after_sig_tampering, "🚨 ALERTE : Le Tribunal a accepté une signature altérée !");

    // ====================================================================
    // 4. FALSIFICATION N°2 : Modification du message (L'attaque classique)
    // ====================================================================
    let mut altered_message_hash = message_hash.clone();
    
    // Un hacker intercepte le message et change juste 1 bit (0x01) du hash
    altered_message_hash[0] ^= 0x01; 

    let is_valid_after_msg_tampering = WotsKeyPair::verify(&keypair.public_key, &valid_signature, &altered_message_hash);
    assert!(!is_valid_after_msg_tampering, "🚨 ALERTE : Le Tribunal a accepté un hash corrompu !");

    // ====================================================================
    // 5. FALSIFICATION N°3 : Le mauvais poivre (Clé publique différente)
    // ====================================================================
    let wrong_keypair = WotsKeyPair::generate();
    
    let is_valid_wrong_pubkey = WotsKeyPair::verify(&wrong_keypair.public_key, &valid_signature, &message_hash);
    assert!(!is_valid_wrong_pubkey, "🚨 ALERTE : Le Tribunal a validé la signature avec la clé d'un autre utilisateur !");
}