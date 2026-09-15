use reqwest::Client;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wattcoin_core::wots::WotsKeyPair;
use wattcoin_l2_sdk::state::L2State;
use wattcoin_l2_sdk::transaction::L2Transaction;

#[tokio::test] // 💡 C'est ÇA qui dit à Cargo que c'est un test !
async fn test_economie_l2_avec_key_rolling() {
    let client = Client::new();
    // On utilise un port différent pour ne pas gêner le vrai serveur s'il tourne
    let test_port = 8299; 
    let api_url = format!("http://127.0.0.1:{}", test_port);

    println!("🛠️ Génération des portefeuilles...");
    let alice_key_1 = WotsKeyPair::generate();
    let alice_key_2 = WotsKeyPair::generate(); // La clé de roulement d'Alice !
    let bob = WotsKeyPair::generate();

    // 1. On injecte le Prémine directement dans l'état (Alice a 10 000)
    let state = Arc::new(Mutex::new(L2State::new(Some(alice_key_1.public_key.clone()), 10_000)));
    let state_clone = Arc::clone(&state);

    // 2. On lance l'API du L2 en arrière-plan
    tokio::spawn(async move {
        wattcoin_l2_sdk::api::start_api_server(test_port, state_clone).await;
    });

    // On attend 1 seconde que le serveur web s'allume
    tokio::time::sleep(Duration::from_secs(1)).await;

    // 3. Alice forge une transaction vers Bob (Et roule son reste vers key_2)
    println!("💸 Alice envoie 500 jetons à Bob...");
    let mut tx = L2Transaction {
        sender_pubkey: alice_key_1.public_key.clone(),
        next_pubkey: alice_key_2.public_key.clone(), // 💡 KEY ROLLING
        receiver_address: bob.public_key.clone(),
        amount: 500,
        fee: 10,
        signature: String::new(),
    };

    // Signature WOTS+
    let hash = tx.hash_data();
    let sig = WotsKeyPair::sign(&alice_key_1.secret_key, &alice_key_1.public_seed, &hash);
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
    let alice_new_balance = *final_state.balances.get(&alice_key_2.public_key).unwrap_or(&0);
    let sequencer_balance = *final_state.balances.get("SEQUENCER_ADDRESS").unwrap_or(&0);
    let alice_old_balance = final_state.balances.get(&alice_key_1.public_key);

    // Bob a bien reçu ses 500
    assert_eq!(bob_balance, 500);
    // Alice a son reste sur sa nouvelle clé (10000 - 500 - 10 = 9490)
    assert_eq!(alice_new_balance, 9490);
    // Le séquenceur a gagné 10 de frais
    assert_eq!(sequencer_balance, 10);
    // L'ancienne clé d'Alice a été supprimée (None)
    assert!(alice_old_balance.is_none());

    println!("✅ TOUS LES TESTS SONT AU VERT ! Key Rolling Validé !");
}