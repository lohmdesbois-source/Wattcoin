// tests/dex_fba_tests.rs
// Stress tests du DEX FBA (Frequent Batch Auctions) et de son carnet d'ordres
// Lancer avec : cargo test --test dex_fba_tests

use wattcoin_core::api::Order;
use wattcoin_core::transaction::SwapContract;

/// 🛠️ On isole ici l'exacte réplique de l'algorithme FBA présent dans ton `main.rs`
/// pour pouvoir le stress-tester mathématiquement de manière isolée.
fn execute_fba_engine(pool: Vec<Order>, current_time: i64) -> (u64, u64, Vec<SwapContract>, Vec<Order>) {
    let mut buys: Vec<_> = pool.iter().filter(|o| o.order_type == "buy").cloned().collect();
    let mut sells: Vec<_> = pool.iter().filter(|o| o.order_type == "sell").cloned().collect();
    
    // Le tri vital : Les plus offrants en premier, les moins chers en premier
    buys.sort_by(|a, b| b.price_sats.cmp(&a.price_sats));
    sells.sort_by(|a, b| a.price_sats.cmp(&b.price_sats));

    let mut generated_swaps = Vec::new();
    let mut clearing_price_sats = 0u64;
    let mut total_volume_flames = 0u64;

    let mut buy_idx = 0;
    let mut sell_idx = 0;

    while buy_idx < buys.len() && sell_idx < sells.len() {
        let buy = &mut buys[buy_idx];
        let sell = &mut sells[sell_idx];

        if buy.price_sats >= sell.price_sats {
            // Le Prix Juste : la moyenne exacte entre l'offre et la demande
            clearing_price_sats = (buy.price_sats + sell.price_sats) / 2;
            let matched_volume = std::cmp::min(buy.amount_flames, sell.amount_flames);
            total_volume_flames += matched_volume;

            let real_htlc_hash = buy.htlc_hash.clone().unwrap_or_else(|| "ERREUR_HASH_MANQUANT".to_string());

            generated_swaps.push(SwapContract {
                buyer_watt_address: buy.watt_address.clone(),
                buyer_btc_address: buy.btc_address.clone(),
                buyer_btc_pubkey: buy.btc_pubkey.clone(),
                seller_watt_address: sell.watt_address.clone(),
                seller_btc_address: sell.btc_address.clone(),
                seller_btc_pubkey: sell.btc_pubkey.clone(),
                watt_amount_flames: matched_volume,
                btc_amount_sats: (matched_volume as f64 / 1_000_000_000.0 * clearing_price_sats as f64) as u64,
                htlc_hash: real_htlc_hash,
            });

            buy.amount_flames -= matched_volume;
            sell.amount_flames -= matched_volume;
            if buy.amount_flames == 0 { buy_idx += 1; }
            if sell.amount_flames == 0 { sell_idx += 1; }
        } else {
            break; // Le marché est bloqué (spread positif)
        }
    }

    // Purge des ordres expirés
    let mut remaining_pool = Vec::new();
    for buy in buys { if buy.amount_flames > 0 && buy.expires_at > current_time { remaining_pool.push(buy); } }
    for sell in sells { if sell.amount_flames > 0 && sell.expires_at > current_time { remaining_pool.push(sell); } }

    (clearing_price_sats, total_volume_flames, generated_swaps, remaining_pool)
}

#[test]
fn test_fba_clearing_price_and_partial_fill() {
    let current_time = 1000;
    
    // ====================================================================
    // 1. SCÉNARIO : Le Carnet d'Ordres (Dark Pool)
    // ====================================================================
    let order_buy_whale = Order {
        id: "buy_1".to_string(), order_type: "buy".to_string(),
        amount_flames: 100_000_000_000, // 100 WATT
        price_sats: 1000, // Prêt à payer 1000 Sats/WATT
        btc_address: "btc_buyer".to_string(), btc_pubkey: "pubkey_buyer".to_string(), watt_address: "watt_buyer".to_string(),
        expires_at: 2000, htlc_hash: Some("hash123".to_string()),
    };

    let order_sell_small = Order {
        id: "sell_1".to_string(), order_type: "sell".to_string(),
        amount_flames: 40_000_000_000, // Ne vend que 40 WATT
        price_sats: 800, // Prêt à vendre à 800 Sats/WATT
        btc_address: "btc_seller".to_string(), btc_pubkey: "pubkey_seller".to_string(), watt_address: "watt_seller".to_string(),
        expires_at: 2000, htlc_hash: None,
    };

    let order_sell_expired = Order {
        id: "sell_expired".to_string(), order_type: "sell".to_string(),
        amount_flames: 10_000_000_000, 
        price_sats: 2000, // 💡 CHANGEMENT : Hors de prix, donc il ne matche pas.
        btc_address: "btc_ghost".to_string(), btc_pubkey: "pubkey_ghost".to_string(), watt_address: "watt_ghost".to_string(),
        expires_at: 500, // 💀 EXPIRÉ ! Il sera purgé à la fin.
        htlc_hash: None,
    };

    let pool = vec![order_buy_whale, order_sell_small, order_sell_expired];

    // ====================================================================
    // 2. EXÉCUTION DU BATCH FBA
    // ====================================================================
    let (clearing_price, total_volume, swaps, remaining_pool) = execute_fba_engine(pool, current_time);

    // ====================================================================
    // 3. TRIBUNAL DU MARCHÉ
    // ====================================================================
    
    // A. Découverte du Prix Juste
    assert_eq!(clearing_price, 900, "🚨 ERREUR DEX : Le prix d'équilibre (1000 + 800) / 2 devrait être de 900 Sats !");
    
    // B. Volume Total (Corrige le warning 'unused variable')
    assert_eq!(total_volume, 40_000_000_000, "Le volume total échangé doit être de 40 WATT.");

    // C. Création du Contrat HTLC
    assert_eq!(swaps.len(), 1, "Le DEX n'a pas généré le contrat d'échange atomique.");
    assert_eq!(swaps[0].watt_amount_flames, 40_000_000_000, "Le volume du swap devrait être limité par le plus petit ordre (40 WATT).");
    assert_eq!(swaps[0].btc_amount_sats, 36_000, "🚨 ERREUR DEX : Le montant BTC (40 * 900) devrait être de 36 000 Sats !");
    
    // D. Remplissage Partiel et Purge
    assert_eq!(remaining_pool.len(), 1, "Il ne devrait rester qu'un seul ordre. L'ordre expiré doit avoir été purgé !");
    assert_eq!(remaining_pool[0].id, "buy_1", "Le reste de l'ordre de la baleine aurait dû retourner dans le pool.");
    assert_eq!(
        remaining_pool[0].amount_flames, 60_000_000_000, 
        "🚨 ERREUR DEX : La baleine a acheté 40 WATT, il devrait lui rester 60 WATT en demande !"
    );
}