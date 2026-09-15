// tests/performance_load_tests.rs
// Tests de performance, de charge et de TPS (Transactions Per Second)
// Lancer avec : cargo test --release --test performance_load_tests -- --nocapture

use wattcoin_core::transaction::{Transaction, TransactionType, TransactionInput, TransactionOutput};
use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM};
use wattcoin_core::blockchain::Blockchain;
use std::sync::{Arc, Mutex};
use std::time::Instant;

fn dummy_l2_keys() -> Vec<(Vec<[u8; 32]>, Vec<u8>)> {
    vec![(vec![[0u8; 32]; 34], vec![0u8; 32]); 128]
}

fn build_heavy_valid_tx() -> Transaction {
    // 💡 Nouveau WOTS+ (Module externe)
    let seed = [0u8; 32];
    let (sk, pk) = wots::Wots::generate_keypair(&seed, 0);

    let bf_in = vec![1u64; LATTICE_DIM];
    let bf_out = vec![1u64; LATTICE_DIM];
    
    let in_commit = LWECommitment::commit(100, &bf_in);
    let out_commit = LWECommitment::commit(90, &bf_out);

    let input = TransactionInput {
        // 💡 Suppression de mpc_ring
        commitment: in_commit,
        source_height: 0,
    };

    let output = TransactionOutput {
        stealth_address: "DEST".to_string(),
        kyber_capsule: "capsule".to_string(),
        aes_vault: "90".to_string(),
        lattice_commitment: out_commit,
    };

    let mut tx = Transaction {
        tx_type: TransactionType::Standard,
        inputs: vec![input], outputs: vec![output], fee: 10,
        public_key: hex::encode(&pk),
        wots_signature: None,
    };

    let tx_hash_64 = tx.hash_data();
    let mut tx_hash_32 = [0u8; 32];
    tx_hash_32.copy_from_slice(&tx_hash_64[0..32]);

    tx.wots_signature = Some(wots::Wots::sign(&sk, 0, &tx_hash_32, &pk));

    tx
}

#[test]
fn test_crypto_validation_tps() {
    println!("\n🚀 --- DÉMARRAGE DU BENCHMARK CRYPTO ---");
    let tx = build_heavy_valid_tx();
    assert!(tx.is_valid(), "La transaction de base doit être valide.");

    let iterations = 50; 
    let start = Instant::now();
    for _ in 0..iterations {
        let _is_valid = tx.is_valid(); 
    }
    let duration = start.elapsed();

    let time_per_tx = duration.as_secs_f64() / iterations as f64;
    let tps = 1.0 / time_per_tx;

    println!("⏱️ Temps total pour {} validations : {:?}", iterations, duration);
    println!("⚡ Temps par transaction : {:.2} ms", time_per_tx * 1000.0);
    println!("📈 Capacité théorique du CPU local (1 Thread) : {:.0} TPS L1", tps);
    println!("--------------------------------------\n");

    assert!(tps > 2.0, "🚨 ALERTE : Le nœud est trop lent (< 2 TPS). Optimisation requise !");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_mempool_concurrent_writes() {
    println!("\n🚀 --- DÉMARRAGE DU STRESS TEST MEMPOOL (CONCURRENCY) ---");
    let mempool: Arc<Mutex<Vec<Transaction>>> = Arc::new(Mutex::new(Vec::new()));
    
    let tx = Transaction {
        tx_type: TransactionType::Standard,
        inputs: vec![], outputs: vec![], fee: 1000,
        public_key: "DUMMY".to_string(), wots_signature: None,
    };

    let num_tasks = 10;
    let txs_per_task = 100;
    let mut handles = Vec::new();

    let start = Instant::now();

    for i in 0..num_tasks {
        let pool_clone = Arc::clone(&mempool);
        let tx_clone = tx.clone();
        
        let handle = tokio::spawn(async move {
            for j in 0..txs_per_task {
                let mut my_tx = tx_clone.clone();
                my_tx.public_key = format!("SPAM_TX_{}_{}", i, j);
                let mut p = pool_clone.lock().unwrap();
                p.push(my_tx);
            }
        });
        handles.push(handle);
    }

    for handle in handles { handle.await.unwrap(); }
    let duration = start.elapsed();
    let final_len = mempool.lock().unwrap().len();

    println!("⏱️ Temps d'écriture concurrentielle : {:?}", duration);
    println!("📝 Total des transactions dans le Mempool : {}", final_len);
    println!("--------------------------------------\n");

    assert_eq!(final_len, num_tasks * txs_per_task, "Des transactions ont été perdues dans la bataille des Threads !");
}

#[test]
fn test_block_preparation_speed() {
    println!("\n🚀 --- DÉMARRAGE DU BENCHMARK ASSEMBLAGE DE BLOC ---");
    let _ = std::fs::remove_dir_all(".test_db_perf");
    let mut chain = Blockchain::new(".test_db_perf").unwrap();
    
    let mut massive_mempool = Vec::new();
    for i in 0..500 {
        let mut tx = Transaction {
            tx_type: TransactionType::Standard,
            inputs: vec![], outputs: vec![], fee: 1000,
            public_key: format!("TX_{}", i), wots_signature: None,
        };
        if i == 0 { tx.tx_type = TransactionType::HTLCRefund { hash: "missing_hash".to_string() }; } 
        massive_mempool.push(tx);
    }

    let start = Instant::now();
    let (block, _, _) = chain.prepare_block_template(massive_mempool, "miner_bench", dummy_l2_keys());
    let duration = start.elapsed();
    
    println!("⏱️ Temps d'assemblage d'un bloc avec 500 TXs : {:?}", duration);
    println!("📦 Transactions retenues dans le bloc : {}", block.transactions.len());
    println!("--------------------------------------\n");

    assert!(duration.as_millis() < 500, "🚨 ALERTE : L'assemblage du bloc est trop lent ! (> 500ms)");
}