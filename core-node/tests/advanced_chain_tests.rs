// tests/advanced_chain_tests.rs
// Tests de la Tokenomics avancée et de la théorie des jeux (Anti-Fermes / P2Pool)
// Lancer avec : cargo test --test advanced_chain_tests

use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::transaction::{Transaction, TransactionType};
use randomx_rs::{RandomXFlag, RandomXCache, RandomXVM};

#[test]
fn test_robin_hood_slashing_for_fast_blocks() {
    let mut chain = Blockchain::new();
    for i in 1..=18 {
        let mut block = chain.chain.last().unwrap().clone();
        block.header.index = i;
        block.header.timestamp = chrono::Utc::now().timestamp();
        chain.chain.push(block);
    }
    let (template, _, _) = chain.prepare_block_template(vec![], "greedy_mining_farm", None);
    
    let mut has_reserve = false;
    let mut reserve_amount = 0;
    let mut miner_amount = 0;
    
    for out in &template.transactions[0].outputs {
        if out.stealth_address == "LOTTERY_RESERVE" {
            has_reserve = true;
            reserve_amount += out.aes_vault.parse::<u64>().unwrap_or(0);
        } else if out.stealth_address == "COINBASE_greedy_mining_farm" {
            miner_amount += out.aes_vault.parse::<u64>().unwrap_or(0);
        }
    }
    assert!(has_reserve, "Le slashing n'a pas alimenté le Jackpot !");
    assert!(reserve_amount > miner_amount * 10, "La ferme n'a pas été assez punie !");
}

#[test]
fn test_p2pool_80_20_distribution() {
    let mut chain = Blockchain::new();
    
    // ⚡ FIX : AUCUN UNDERSCORE DANS CES VARIABLES !
    // Si on met un '_', le nœud va lire de travers à cause du .split('_')
    let share_height = 0;
    let share_prev_hash = chain.chain[0].header.hash.clone();
    let timestamp = chrono::Utc::now().timestamp();
    let nonce = 12345;
    let l2_root = "L2ROOTTEST"; // Sans underscore
    let tx_root = "TXROOTTEST"; // Sans underscore

    let flags = RandomXFlag::get_recommended_flags();
    let cache = RandomXCache::new(flags, chain.get_epoch_seed(share_height).as_bytes()).unwrap();
    let vm = RandomXVM::new(flags, Some(cache), None).unwrap();

    let header_data = format!("{}{}{}{}{}{}", share_height, timestamp, share_prev_hash, nonce, l2_root, tx_root);
    let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
    let real_hash = hex::encode(&hash_bytes);

    let share_tx = Transaction {
        tx_type: TransactionType::MiningShare {
            miner_address: "little_laptop".to_string(),
            nonce,
            hash: real_hash, 
            timestamp,
        },
        inputs: vec![],
        outputs: vec![],
        fee: 0,
        public_key: format!("{}_{}_{}", l2_root, tx_root, nonce),
        wots_signature: None,
    };
    
    let (template, _, _) = chain.prepare_block_template(vec![share_tx], "main_miner", None);
    
    let mut main_miner_amount = 0;
    let mut little_laptop_amount = 0;
    
    for out in &template.transactions[0].outputs {
        if out.stealth_address == "COINBASE_main_miner" {
            main_miner_amount = out.aes_vault.parse::<u64>().unwrap_or(0);
        } else if out.stealth_address == "COINBASE_little_laptop" {
            little_laptop_amount = out.aes_vault.parse::<u64>().unwrap_or(0);
        }
    }
    
    assert!(main_miner_amount > 0, "Le mineur principal n'a rien reçu");
    assert!(little_laptop_amount > 0, "Le petit PC n'a rien reçu");
    assert_eq!(little_laptop_amount, main_miner_amount * 4, "La règle d'or 80/20 n'est pas respectée !");
}

#[test]
fn test_l2_micro_coinbase_validation_rule() {
    let micro_tx = Transaction {
        tx_type: TransactionType::MicroCoinbase,
        inputs: vec![],
        outputs: vec![],
        fee: 0,
        public_key: "MICRO_COINBASE".to_string(),
        wots_signature: None,
    };
    assert!(micro_tx.is_valid(), "La blockchain L1 refuse de reconnaître les MicroCoinbases du Séquenceur L2 !");
}