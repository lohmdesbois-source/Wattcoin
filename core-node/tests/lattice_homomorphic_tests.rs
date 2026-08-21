// tests/lattice_homomorphic_tests.rs
// Stress tests de la cryptographie Lattice LWE (Homomorphisme et Conservation de la masse)
// Lancer avec : cargo test --test lattice_homomorphic_tests

use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM};

#[test]
fn test_lattice_homomorphic_conservation() {
    // 💡 RÈGLE D'OR HOMOMORPHE : La somme des clés d'aveuglement (blinding factors) 
    // en entrée doit être égale à celle en sortie pour que l'équation s'annule.
    
    // Entrées : Somme = 5
    let bf1 = vec![2u64; LATTICE_DIM];
    let bf2 = vec![3u64; LATTICE_DIM];
    
    // Sorties : Somme = 5
    let bf3 = vec![4u64; LATTICE_DIM];
    let bf4 = vec![1u64; LATTICE_DIM];

    // ====================================================================
    // 1. CAS NOMINAL : Conservation parfaite de la masse (10 + 5 = 12 + 2 + 1)
    // ====================================================================
    let in1 = LWECommitment::commit(10, &bf1);
    let in2 = LWECommitment::commit(5, &bf2);
    
    let out1 = LWECommitment::commit(12, &bf3);
    let out2 = LWECommitment::commit(2, &bf4);
    
    let fee = 1;

    let is_valid = LWECommitment::verify_balance(&[in1.clone(), in2.clone()], &[out1.clone(), out2.clone()], fee);
    assert!(is_valid, "✅ ÉCHEC : Le Tribunal a rejeté une transaction parfaitement équilibrée.");

    // ====================================================================
    // 2. TENTATIVE D'INFLATION : Création monétaire illégale (10 + 5 < 20 + 0)
    // ====================================================================
    let fake_out1 = LWECommitment::commit(20, &bf3); // On tente de s'imprimer 20 WATT
    
    let is_valid_inflation = LWECommitment::verify_balance(&[in1.clone(), in2.clone()], &[fake_out1], 0);
    assert!(!is_valid_inflation, "🚨 ALERTE : Le Tribunal a validé la création d'argent magique !");

    // ====================================================================
    // 3. BOUCLIER DIMENSIONNEL : Vecteur altéré (1023 dimensions au lieu de 1024)
    // ====================================================================
    let mut corrupted_in = in1.clone();
    corrupted_in.t_vector.pop(); // Le hacker ampute le vecteur pour faire crasher la boucle

    let is_valid_corrupted = LWECommitment::verify_balance(&[corrupted_in], &[out1], 0);
    assert!(!is_valid_corrupted, "🚨 ALERTE : Le Bouclier a laissé passer une matrice aux dimensions invalides !");
}