// tests/lattice_noise_tests.rs
// Validation du bruit PQC (Centered Binomial Distribution) et Homomorphisme
// Lancer avec : cargo test --test lattice_noise_tests

use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM};

#[test]
fn test_lwe_cbd_noise_homomorphism() {
    // ====================================================================
    // SCÉNARIO : Le "Bruit Quantique" casse-t-il la blockchain ?
    // Alice a 2 billets (10 et 5 WATT) et veut envoyer 12 WATT à Bob.
    // Elle récupère 3 WATT en monnaie rendue. La transaction est parfaitement équilibrée.
    // Mais chaque billet contient un bruit aléatoire CBD !
    // ====================================================================
    let bf_in_1 = vec![123u64; LATTICE_DIM];
    let bf_in_2 = vec![456u64; LATTICE_DIM];
    
    // Le bruit est injecté ici, au moment du "commit" !
    let c_in_1 = LWECommitment::commit(10, &bf_in_1);
    let c_in_2 = LWECommitment::commit(5, &bf_in_2);
    
    // BF de sortie s'équilibrent mathématiquement
    let mut bf_out_1 = vec![0u64; LATTICE_DIM];
    let mut bf_out_2 = vec![0u64; LATTICE_DIM];
    for i in 0..LATTICE_DIM {
        bf_out_1[i] = 100;
        bf_out_2[i] = bf_in_1[i].wrapping_add(bf_in_2[i]).wrapping_sub(bf_out_1[i]);
    }
    
    let c_out_1 = LWECommitment::commit(12, &bf_out_1);
    let c_out_2 = LWECommitment::commit(3, &bf_out_2);
    
    let is_valid = LWECommitment::verify_balance(
        &vec![c_in_1, c_in_2], 
        &vec![c_out_1, c_out_2], 
        0 // fee
    );
    
    assert!(is_valid, "🚨 ERREUR : La tolérance au bruit PQC est mal configurée. L'équation homomorphe est brisée !");
}

#[test]
fn test_inflation_protection() {
    // ====================================================================
    // SCÉNARIO : Le bruit permet-il d'imprimer de l'argent ?
    // Un hacker a 10 WATT, et essaie de se créer un billet de 100 WATT.
    // Ses BF s'équilibrent, mais la dimension 0 (montant) sera fausse.
    // Le nœud va-t-il confondre l'écart avec le "bruit normal" ?
    // ====================================================================
    let bf_in = vec![1u64; LATTICE_DIM];
    let bf_out = vec![1u64; LATTICE_DIM];
    
    let c_in = LWECommitment::commit(10, &bf_in);
    let c_out = LWECommitment::commit(100, &bf_out);
    
    let is_valid = LWECommitment::verify_balance(&vec![c_in], &vec![c_out], 0);
    
    assert!(!is_valid, "🚨 ALERTE FATALE : Le tribunal Lattice a laissé passer une impression de monnaie de 90 WATT !");
}