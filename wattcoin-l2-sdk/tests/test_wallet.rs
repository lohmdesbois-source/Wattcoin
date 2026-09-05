use reqwest::Client;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wattcoin_core::lattice::LatticeKeyPair; 
use wattcoin_l2_sdk::state::L2State;
use wattcoin_l2_sdk::transaction::L2Transaction;

#[tokio::test] 
async fn test_economie_l2_sans_key_rolling() {
    let client = Client::new();
    let test_port = 8299; 
    let api_url = format!("http://127.0.0.1:{}", test_port);

    println!("🛠️ Génération des portefeuilles...");
    let alice_key_1 = LatticeKeyPair::generate(); 
    let bob = LatticeKeyPair::generate();         

    // 1. On injecte le Prémine directement (Alice a 10 000). On ajoute '0' pour le block_reward du test !
    let state = Arc::new(Mutex::new(L2State::new(Some(alice_key_1.public_key.clone()), 10_000, 0))); 
    let state_clone = Arc::clone(&state);

    // 2. On lance l'API du L2 en arrière-plan
    tokio::spawn(async move {
        wattcoin_l2_sdk::api::start_api_server(test_port, state_clone).await;
    });

    // On attend 1 seconde que le serveur web s'allume
    tokio::time::sleep(Duration::from_secs(1)).await;

    // 3. Alice forge une transaction vers Bob (sans Key Rolling)
    println!("💸 Alice envoie 500 jetons à Bob...");
    let mut tx = L2Transaction {
        sender_pubkey: alice_key_1.public_key.clone(),
        receiver_address: bob.public_key.clone(),
        amount: 500,
        fee: 10,
        signature: String::new(),
    };

    // Signature Lattice (Seulement 2 arguments)
    let hash = tx.hash_data();
    let sig = LatticeKeyPair::sign(&alice_key_1.secret_key, &hash); 
    tx.signature = serde_json::to_string(&sig).unwrap();

    // 4. Envoi au Séquenceur L2
    let res = client.post(&format!("{}/send", api_url)).json(&tx).send().await.unwrap();
    assert!(res.status().is_success());

    // 5. On simule le passage du temps (Le séquenceur traite le mempool)
    {
        let mut state_guard = state.lock().unwrap();
        state_guard.process_mempool("SEQUENCER_ADDRESS");
    }

    // 6. LES ASSERTIONS DU TEST (Le juge de paix)
    let final_state = state.lock().unwrap();
    
    let bob_balance = *final_state.balances.get(&bob.public_key).unwrap_or(&0);
    // On vérifie le solde directement sur l'unique clé d'Alice
    let alice_balance = *final_state.balances.get(&alice_key_1.public_key).unwrap_or(&0);
    let sequencer_balance = *final_state.balances.get("SEQUENCER_ADDRESS").unwrap_or(&0);

    // Bob a bien reçu ses 500
    assert_eq!(bob_balance, 500);
    // Alice a son reste sur sa clé d'origine (10000 - 500 - 10 = 9490)
    assert_eq!(alice_balance, 9490);
    // Le séquenceur a gagné 10 de frais purs (car block_reward = 0)
    assert_eq!(sequencer_balance, 10);

    println!("✅ TOUS LES TESTS SONT AU VERT ! Réutilisation des clés Validée !");
}