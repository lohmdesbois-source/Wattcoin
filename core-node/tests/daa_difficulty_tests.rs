// tests/daa_difficulty_tests.rs
// Stress tests de l'algorithme d'Ajustement de la Difficulté (DAA)
// Lancer avec : cargo test --test daa_difficulty_tests

use wattcoin_core::blockchain::Blockchain;
use num_bigint::BigUint;

#[test]
fn test_difficulty_adjustment_algorithm() {
    let _ = std::fs::remove_dir_all(".test_db_daa");
    let mut chain = Blockchain::new(".test_db_daa").unwrap();
    
    let initial_target = chain.target.clone();
    
    let mut current_block = chain.get_block_by_height(0).unwrap().clone();
    let mut current_time = current_block.header.timestamp;
    
    for i in 1..=20 {
        current_time += 10; 
        
        let mut fast_block = current_block.clone();
        fast_block.header.index = i;
        fast_block.header.timestamp = current_time;
        fast_block.header.previous_hash = current_block.header.hash.clone();
        
        chain.push_block(&fast_block).unwrap();
        chain.update_target(); 
        current_block = fast_block;
    }
    
    let final_target = chain.target.clone();
    assert!(final_target < initial_target, "🚨 ALERTE : La difficulté n'a pas augmenté !");
    
    let half_initial = initial_target / BigUint::from(2u32);
    assert!(final_target < half_initial, "🚨 ALERTE : L'ajustement est trop mou !");
}