// tests/daa_difficulty_tests.rs
// Stress tests de l'algorithme d'Ajustement de la Difficulté (DAA)
// Lancer avec : cargo test --test daa_difficulty_tests

use wattcoin_core::blockchain::Blockchain;
use num_bigint::BigUint;

#[test]
fn test_difficulty_adjustment_algorithm() {
    let mut chain = Blockchain::new();
    
    // ====================================================================
    // 1. ÉTAT INITIAL
    // ====================================================================
    let initial_target = chain.target.clone();
    
    // ====================================================================
    // 2. ATTAQUE DE HASHRATE (Le réseau s'emballe)
    // ====================================================================
    // On simule l'arrivée massive de mineurs. Les 20 prochains blocs
    // sont trouvés en seulement 10 secondes chacun (au lieu de 120s).
    let mut current_block = chain.chain[0].clone();
    let mut current_time = current_block.header.timestamp;
    
    for i in 1..=20 {
        current_time += 10; // Seulement 10 secondes par bloc !
        
        let mut fast_block = current_block.clone();
        fast_block.header.index = i;
        fast_block.header.timestamp = current_time;
        fast_block.header.previous_hash = current_block.header.hash.clone();
        
        // On ajoute le bloc à la chaîne
        chain.chain.push(fast_block.clone());
        
        // 💡 Le réseau réagit et met à jour sa difficulté
        chain.update_target(); 
        
        current_block = fast_block;
    }
    
    // ====================================================================
    // 3. VERDICT DU TRIBUNAL : Le réseau s'est-il défendu ?
    // ====================================================================
    let final_target = chain.target.clone();
    
    // La cible doit avoir fondu (Target plus bas = Minage plus difficile)
    assert!(
        final_target < initial_target, 
        "🚨 ALERTE : La difficulté n'a pas augmenté malgré des blocs trouvés en 10 secondes !"
    );
    
    // On vérifie que la baisse est significative. 
    // Sur 20 blocs super rapides, la target doit avoir été divisée par plus de 2.
    let half_initial = initial_target / BigUint::from(2u32);
    assert!(
        final_target < half_initial, 
        "🚨 ALERTE : L'ajustement de la difficulté est trop mou ! Il faut resserrer la vis plus vite."
    );
}