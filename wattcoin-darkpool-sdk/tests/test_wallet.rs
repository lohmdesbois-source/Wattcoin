use std::sync::{Arc, Mutex};
use wattcoin_darkpool_sdk::state::DarkpoolState;
use wattcoin_core::transaction::{Transaction, TransactionType, TransactionInput};
use wattcoin_core::merkle_ring::MpcRingSignature;
use wattcoin_core::lattice::{LWECommitment, LATTICE_DIM};

#[tokio::test]
async fn test_darkpool_anti_double_spend() {
    println!("🛠️ Initialisation de l'état du Darkpool...");
    let state = Arc::new(Mutex::new(DarkpoolState::new()));

    // 1. On crée une fausse "Key Image" (l'empreinte anonyme d'une transaction)
    let dummy_ring = MpcRingSignature {
        key_image: "empreinte_anonyme_secrete_123".to_string(),
        ring_root: String::new(),
        ring_decoys: vec![],
        c_0: String::new(),
        responses: vec![],
    };

    // 2. On l'intègre dans un faux Input L1
    let dummy_input = TransactionInput {
        mpc_ring: dummy_ring,
        commitment: LWECommitment::commit(0, &[0; LATTICE_DIM]), 
        source_height: 0,
    };

    // 3. On crée la transaction qui passe par le Darkpool
    let tx = Transaction {
        tx_type: TransactionType::Standard,
        inputs: vec![dummy_input],
        outputs: vec![],
        fee: 100,
        lattice_signature: None,
        public_key: String::new(),
    };

    // =========================================================
    // ACTE I : PREMIÈRE DÉPENSE (Doit réussir)
    // =========================================================
    println!("💸 Envoi de la transaction anonyme au Séquenceur...");
    {
        let mut guard = state.lock().unwrap();
        guard.mempool.push(tx.clone()); // On l'injecte dans le mempool
        
        // Le séquenceur forge le bloc
        let (_, valid_count, total_fees) = guard.process_mempool();
        
        assert_eq!(valid_count, 1, "La transaction doit être acceptée.");
        assert_eq!(total_fees, 100, "Les frais doivent être récoltés.");
        assert!(guard.spent_key_images.contains("empreinte_anonyme_secrete_123"), "La Key Image doit être brûlée !");
        
        println!("✅ Première transaction acceptée ! Key Image brûlée.");
    }

    // =========================================================
    // ACTE II : TENTATIVE DE DOUBLE DÉPENSE (Doit échouer)
    // =========================================================
    println!("🚨 Tentative d'injection de la MÊME transaction (Double Dépense)...");
    {
        let mut guard = state.lock().unwrap();
        guard.mempool.push(tx.clone()); // On renvoie la même !
        
        let (_, valid_count, total_fees) = guard.process_mempool();
        
        // Le bouclier doit s'activer
        assert_eq!(valid_count, 0, "La transaction DOIT être rejetée !");
        assert_eq!(total_fees, 0, "Aucun frais ne doit être volé.");
        
        println!("🛡️ Bouclier actif : Double dépense interceptée et détruite !");
    }

    println!("✅ TOUS LES TESTS SONT AU VERT ! Darkpool 100% sécurisé !");
}