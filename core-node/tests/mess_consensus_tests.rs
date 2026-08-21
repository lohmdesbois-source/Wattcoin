// tests/mess_consensus_tests.rs
// Stress tests du Bouclier Anti-51% (MESS - Modified Exponential Subjective Scoring)
// Lancer avec : cargo test --test mess_consensus_tests

use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::block::Block;
use randomx_rs::{RandomXFlag, RandomXCache, RandomXVM};

/// 🛠️ Fonction utilitaire pour forger des blocs très rapidement dans le test
/// On simule le travail du mineur en générant un bloc avec un hash RandomX valide.
fn forge_block(vm: &RandomXVM, prev_block: &Block, index: u64, timestamp: i64) -> Block {
    let mut block = prev_block.clone();
    block.header.index = index;
    block.header.previous_hash = prev_block.header.hash.clone();
    block.header.timestamp = timestamp;
    block.header.nonce = 0;
    
    // Le tx_root doit impérativement être valide pour passer le bouclier Merkle
    block.header.tx_root = block.calculate_tx_root();

    // On hache les métadonnées avec le moteur RandomX
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
    let mut chain = Blockchain::new();

    // ====================================================================
    // 1. PRÉPARATION : Initialisation du moteur RandomX (Vitesse optimisée)
    // ====================================================================
    let seed = chain.chain[0].header.hash.clone();
    let flags = RandomXFlag::get_recommended_flags();
    let cache = RandomXCache::new(flags, seed.as_bytes()).unwrap();
    let vm = RandomXVM::new(flags, Some(cache), None).unwrap();

    // ====================================================================
    // 2. CHAÎNE SAINE : On simule la vie normale du réseau jusqu'au bloc 19
    // (Total de 20 blocs en comptant le Genesis)
    // ====================================================================
    let mut current_block = chain.chain[0].clone();
    let mut current_time = current_block.header.timestamp;

    for i in 1..=19 {
        current_time += 120; // 2 minutes par bloc (rythme normal)
        let new_block = forge_block(&vm, &current_block, i, current_time);
        chain.chain.push(new_block.clone());
        current_block = new_block;
    }
    assert_eq!(chain.chain.len(), 20, "La chaîne locale doit faire 20 blocs de long.");

    // ====================================================================
    // 3. L'ATTAQUE DES 51% : Une ferme de minage crée une chaîne cachée
    // ====================================================================
    // L'attaquant décide de bifurquer au bloc 5 pour annuler des transactions récentes.
    let mut attacker_blocks = Vec::new();
    let mut current_attacker_block = chain.chain[5].clone();
    let mut attacker_time = current_attacker_block.header.timestamp;

    // L'attaquant dispose d'une puissance de calcul énorme et parvient à miner 20 blocs de plus.
    // Sa chaîne va donc atteindre l'index 25 (elle est plus longue que la nôtre qui est à 19 !).
    for i in 6..=25 {
        attacker_time += 120;
        let new_block = forge_block(&vm, &current_attacker_block, i as u64, attacker_time);
        attacker_blocks.push(new_block.clone());
        current_attacker_block = new_block;
    }

    // ====================================================================
    // 4. LE CHOC : L'attaquant diffuse sa chaîne pour écraser le réseau
    // ====================================================================
    // Sans le MESS, la chaîne de l'attaquant serait acceptée (26 blocs de Proof of Work > 20 blocs).
    // Mais avec le MESS, comme l'attaque remonte loin dans le passé (profondeur de réorganisation = 14 blocs),
    // le réseau va lourdement pénaliser son poids mathématique.
    
    let is_accepted = chain.resolve_partial_fork(attacker_blocks);

    // ====================================================================
    // 5. VERDICT DU TRIBUNAL : Le MESS a-t-il protégé le réseau ?
    // ====================================================================
    assert!(!is_accepted, "🚨 ALERTE FATALE : Le nœud a accepté une réorganisation profonde (Attaque des 51% réussie) !");
    assert_eq!(chain.chain.len(), 20, "🚨 ALERTE : L'historique local a été écrasé par l'attaquant !");
}