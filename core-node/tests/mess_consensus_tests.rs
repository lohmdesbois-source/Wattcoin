// tests/mess_consensus_tests.rs
// Stress tests du Bouclier Anti-51% (MESS - Modified Exponential Subjective Scoring)
// Lancer avec : cargo test --test mess_consensus_tests

use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::block::Block;
use randomx_rs::{RandomXFlag, RandomXCache, RandomXVM};

fn forge_block(vm: &RandomXVM, prev_block: &Block, index: u64, timestamp: i64) -> Block {
    let mut block = prev_block.clone();
    block.header.index = index;
    block.header.previous_hash = prev_block.header.hash.clone();
    block.header.timestamp = timestamp;
    block.header.nonce = 0;
    
    block.header.tx_root = block.calculate_tx_root();

    let header_data = format!("{}{}{}{}{}{}",
        block.header.index,
        block.header.timestamp,
        block.header.previous_hash,
        block.header.nonce,
        block.header.l2_root,
        block.header.tx_root
    );

    let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
    block.header.hash = hex::encode(&hash_bytes);
    
    block
}

#[test]
fn test_mess_51_percent_shield() {
    let _ = std::fs::remove_dir_all(".test_db_mess");
    let mut chain = Blockchain::new(".test_db_mess").unwrap();

    let seed = chain.get_block_by_height(0).unwrap().header.hash.clone();
    let flags = RandomXFlag::get_recommended_flags();
    let cache = RandomXCache::new(flags, seed.as_bytes()).unwrap();
    let vm = RandomXVM::new(flags, Some(cache), None).unwrap();

    let mut current_block = chain.get_block_by_height(0).unwrap().clone();
    let mut current_time = current_block.header.timestamp;

    for i in 1..=19 {
        current_time += 120;
        let new_block = forge_block(&vm, &current_block, i, current_time);
        chain.push_block(&new_block).unwrap();
        current_block = new_block;
    }
    assert_eq!(chain.current_height + 1, 20, "La chaîne locale doit faire 20 blocs de long.");

    let mut attacker_blocks = Vec::new();
    let mut current_attacker_block = chain.get_block_by_height(5).unwrap().clone();
    let mut attacker_time = current_attacker_block.header.timestamp;

    for i in 6..=25 {
        attacker_time += 120;
        let new_block = forge_block(&vm, &current_attacker_block, i as u64, attacker_time);
        attacker_blocks.push(new_block.clone());
        current_attacker_block = new_block;
    }

    let is_accepted = chain.resolve_partial_fork(attacker_blocks);

    assert!(!is_accepted, "🚨 ALERTE FATALE : Le nœud a accepté une réorganisation profonde (Attaque des 51% réussie) !");
    assert_eq!(chain.current_height + 1, 20, "🚨 ALERTE : L'historique local a été écrasé par l'attaquant !");
}