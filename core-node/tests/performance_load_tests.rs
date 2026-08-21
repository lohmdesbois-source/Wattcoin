// tests/performance_load_tests.rs
// Tests de performance, de charge et de TPS (Transactions Per Second)
// Lancer avec : cargo test --release --test performance_load_tests -- --nocapture

use wattcoin_core::transaction::{Transaction, TransactionType, TransactionInput, TransactionOutput};
use wattcoin_core::wots::{WotsKeyPair, WotsSignature};
use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM};
use wattcoin_core::merkle_ring::MpcRingSignature;
use wattcoin_core::blockchain::Blockchain;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Helper : Crée une transaction cryptographiquement parfaite pour les tests
fn build_heavy_valid_tx() -> Transaction {
    let keypair = WotsKeyPair::generate();
    let mut decoys = vec![keypair.public_key.clone()];
    for _ in 1..64 { decoys.push(WotsKeyPair::generate().public_key); }

    let bf_in = vec![1u64; LATTICE_DIM];
    let bf_out = vec![1u64; LATTICE_DIM];
    
    let in_commit = LWECommitment::commit(100, &bf_in);
    let out_commit = LWECommitment::commit(90, &bf_out);

    let input = TransactionInput {
        mpc_ring: MpcRingSignature {
            key_image: "test_ki".to_string(),
            ring_root: "root".to_string(),
            ring_decoys: decoys.clone(),
            real_wots_sig: WotsSignature { chains: vec![] },
            merkle_proof: vec![],
        },
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
        public_key: keypair.public_key.clone(),
        wots_signature: None,
    };

    let tx_hash = tx.hash_data();
    tx.inputs[0].mpc_ring = MpcRingSignature::sign(
        &keypair.secret_key, &tx_hash, &decoys, 0, "capsule", b"secret"
    );
    tx.wots_signature = Some(WotsKeyPair::sign(
        &keypair.secret_key, &keypair.public_seed, &tx_hash
    ));

    tx
}

#[test]
fn test_crypto_validation_tps() {
    println!("\n🚀 --- DÉMARRAGE DU BENCHMARK CRYPTO ---");
    let tx = build_heavy_valid_tx();
    assert!(tx.is_valid(), "La transaction de base doit être valide.");

    let iterations = 50; // On vérifie la même TX 50 fois pour moyenner le temps
    
    let start = Instant::now();
    for _ in 0..iterations {
        // Cela lance la vérification WOTS+, Lattice LWE (1024 dims) et Ring Signature (64 clés)
        let _is_valid = tx.is_valid(); 
    }
    let duration = start.elapsed();

    let time_per_tx = duration.as_secs_f64() / iterations as f64;
    let tps = 1.0 / time_per_tx;

    println!("⏱️ Temps total pour {} validations : {:?}", iterations, duration);
    println!("⚡ Temps par transaction : {:.2} ms", time_per_tx * 1000.0);
    println!("📈 Capacité théorique du CPU local (1 Thread) : {:.0} TPS L1", tps);
    println!("--------------------------------------\n");

    // On s'assure que le code ne tourne pas au ralenti extrême
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

    // On lance 10 threads (tâches asynchrones) qui spamment le mempool simultanément
    for i in 0..num_tasks {
        let pool_clone = Arc::clone(&mempool);
        let tx_clone = tx.clone();
        
        let handle = tokio::spawn(async move {
            for j in 0..txs_per_task {
                let mut my_tx = tx_clone.clone();
                my_tx.public_key = format!("SPAM_TX_{}_{}", i, j);
                
                // Embuscade sur le Mutex
                let mut p = pool_clone.lock().unwrap();
                p.push(my_tx);
            }
        });
        handles.push(handle);
    }

    // On attend que toutes les attaques simultanées soient terminées
    for handle in handles {
        handle.await.unwrap();
    }
    
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
    let mut chain = Blockchain::new();
    
    // On génère 500 fausses transactions (on ne fait pas la crypto pour tester juste la logique de tri)
    let mut massive_mempool = Vec::new();
    for i in 0..500 {
        let mut tx = Transaction {
            tx_type: TransactionType::Standard,
            inputs: vec![], outputs: vec![], fee: 1000,
            public_key: format!("TX_{}", i), wots_signature: None,
        };
        // On by-pass la validation lourde juste pour tester la tuyauterie de 'prepare_block_template'
        if i == 0 { tx.tx_type = TransactionType::HTLCRefund { hash: "missing_hash".to_string() }; } 
        massive_mempool.push(tx);
    }

    let start = Instant::now();
    
    // 💡 Astuce : On passe `None` pour le L2 DB pour isoler le benchmark du L1
    let (block, _, _) = chain.prepare_block_template(massive_mempool, "miner_bench", None);
    
    let duration = start.elapsed();
    
    println!("⏱️ Temps d'assemblage d'un bloc avec 500 TXs : {:?}", duration);
    println!("📦 Transactions retenues dans le bloc : {}", block.transactions.len());
    println!("--------------------------------------\n");

    // L'assemblage ne devrait prendre que quelques millisecondes maximum
    assert!(duration.as_millis() < 500, "🚨 ALERTE : L'assemblage du bloc est trop lent ! (> 500ms)");
}