// tests/advanced_chain_tests.rs
// Tests de la Tokenomics avancée et de la théorie des jeux (Anti-Fermes / P2Pool)
// Lancer avec : cargo test --test advanced_chain_tests

use wattcoin_core::blockchain::Blockchain;
use wattcoin_core::transaction::{Transaction, TransactionType};

#[test]
fn test_robin_hood_slashing_for_fast_blocks() {
    let mut chain = Blockchain::new();
    
    // 1. On simule l'avancée de la blockchain jusqu'au bloc 18 
    // (pour dépasser la phase de grâce du lancement où le slashing est désactivé)
    for i in 1..=18 {
        let mut block = chain.chain.last().unwrap().clone();
        block.header.index = i;
        // On met le timestamp à "maintenant" pour que le prochain bloc semble ultra-rapide
        block.header.timestamp = chrono::Utc::now().timestamp();
        chain.chain.push(block);
    }
    
    // 2. Une "ferme de minage" trouve le bloc instantanément (time_taken ~ 1 seconde)
    let (template, _, _) = chain.prepare_block_template(vec![], "greedy_mining_farm");
    
    let mut has_reserve = false;
    let mut reserve_amount = 0;
    let mut miner_amount = 0;
    
    // 3. On analyse les récompenses distribuées
    for out in &template.transactions[0].outputs {
        if out.stealth_address == "LOTTERY_RESERVE" {
            has_reserve = true;
            reserve_amount += out.aes_vault.parse::<u64>().unwrap_or(0);
        } else if out.stealth_address == "COINBASE_greedy_mining_farm" {
            miner_amount += out.aes_vault.parse::<u64>().unwrap_or(0);
        }
    }
    
    // 4. VERDICT : La ferme doit être sévèrement punie et le Jackpot enrichi !
    assert!(has_reserve, "Le slashing n'a pas alimenté le Jackpot !");
    assert!(
        reserve_amount > miner_amount * 10, 
        "La ferme n'a pas été assez punie ! (Miner: {}, Reserve: {})", miner_amount, reserve_amount
    );
}

#[test]
fn test_p2pool_80_20_distribution() {
    let mut chain = Blockchain::new();
    // On reste au bloc 1 (donc pas de slashing Robin des bois pour ce test)
    
    // 1. Un petit PC portable trouve un "Share" (hachage proche)
    let share_tx = Transaction {
        tx_type: TransactionType::MiningShare {
            miner_address: "little_laptop".to_string(),
            nonce: 12345,
            hash: "fake_hash_proof".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
        },
        inputs: vec![],
        outputs: vec![],
        fee: 0,
        public_key: "MINING_SHARE".to_string(),
        wots_signature: None,
    };
    
    // 2. Le mineur principal ("main_miner") trouve le bloc et inclut la preuve du petit PC
    let (template, _, _) = chain.prepare_block_template(vec![share_tx], "main_miner");
    
    let mut main_miner_amount = 0;
    let mut little_laptop_amount = 0;
    
    // 3. On vérifie la distribution de la Coinbase
    for out in &template.transactions[0].outputs {
        if out.stealth_address == "COINBASE_main_miner" {
            main_miner_amount = out.aes_vault.parse::<u64>().unwrap_or(0);
        } else if out.stealth_address == "COINBASE_little_laptop" {
            little_laptop_amount = out.aes_vault.parse::<u64>().unwrap_or(0);
        }
    }
    
    // 4. VERDICT : Le petit PC doit gagner exactement 4 fois plus (80%) que le validateur (20%)
    assert!(main_miner_amount > 0, "Le mineur principal n'a rien reçu");
    assert!(little_laptop_amount > 0, "Le petit PC n'a rien reçu");
    assert_eq!(
        little_laptop_amount, main_miner_amount * 4, 
        "La règle d'or 80/20 n'est pas respectée !"
    );
}

#[test]
fn test_l2_micro_coinbase_validation_rule() {
    // On vérifie que la transaction "MicroCoinbase" est bien reconnue comme valide 
    // de manière intrinsèque par le protocole L1.
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