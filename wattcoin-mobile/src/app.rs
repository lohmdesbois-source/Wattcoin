use eframe::egui;
use tokio::sync::mpsc;
use bip39::Language;
use unicode_normalization::UnicodeNormalization;
use rand::RngCore;

// PONT JNI : Variables globales pour communiquer avec Android
#[cfg(target_os = "android")]
use std::sync::Mutex;
#[cfg(target_os = "android")]
use once_cell::sync::Lazy;

#[cfg(target_os = "android")]
static ANDROID_APP: Lazy<Mutex<Option<android_activity::AndroidApp>>> = Lazy::new(|| Mutex::new(None));
#[cfg(target_os = "android")]
static APP_TX: Lazy<Mutex<Option<tokio::sync::mpsc::Sender<AppMessage>>>> = Lazy::new(|| Mutex::new(None));




#[derive(PartialEq)]
enum AppView {
	WalletSelection,
    Onboarding,
    Unlock,
    Dashboard,
    Dex,
    History,
    Settings,
	Transfer,
	Lottery,
	Messages,
	L2Manager,
	Bridge,
	WnsManager,
}

#[derive(PartialEq)]
enum OnboardingStep {
    Selection,
    Create,
    RestoreSeed,
}

// 1. LE NOUVEAU SYSTÈME DE MESSAGERIE ROBUSTE
#[allow(dead_code)]
enum AppMessage {
    UnlockSuccess(crate::WalletKeys),
    Error(String),
    Info(String),
    QrScanned(String),
    DashboardData { 
        balance_l1: f64, 
        balance_l2: f64, 
        balance_btc: f64,   
		balance_wns: f64,
        price_usd: f64, 
        btc_price_usd: f64,
		total_supply: u64, 
        jackpot: u64,
		current_block_height: u64
    },
	HistoryData(Vec<crate::HistoryItem>),
	DexData { pool: Vec<crate::Order>, swaps: Vec<crate::SwapContract> },
	SwapCompleted(String),
	DataFetched(Vec<crate::DataItem>), // Pour recevoir l'historique messages
    FileHashed(String, String),
	L2StatusFetched(String),
	DomainStatus(String),
	TxWeightEstimated(usize, f64),
}

struct WattcoinApp {
	new_wallet_name: String,
    view: AppView,
    onboarding_step: OnboardingStep,
    
    // STOCKAGE DES DONNÉES DU WALLET
    wallet_keys: Option<crate::WalletKeys>,
    balance_l1: f64,
	balance_l2: f64,
    balance_btc: f64,     
	balance_wns: f64,	
    watt_price_usd: f64,
    btc_price_usd: f64,
	total_supply: u64,
    jackpot: u64,
	current_block_height: u64,
    is_refreshing: bool,
    
    password_input: String,
    password_confirm: String,
    seed_input: String,
    
    show_seed: bool,
    show_seed_qr: bool,
    decrypted_seed: String,
	
	show_payment_qr: bool,   
    payment_qr_data: String,
	payment_qr_amount: String,
	
	is_dark_mode: bool,
	
	recipient_input: String,
    amount_input: String,
    transfer_from_l2: bool,
    transfer_to_l2: bool,
	transfer_asset: String, // "WATT" ou "BTC"
	tx_weight_estimate: Option<(usize, f64)>,
	
	history_items: Vec<crate::HistoryItem>,
    history_tab: String, // "L1" ou "L2"
    is_loading_history: bool,
	
	dex_amount_input: String,
    dex_price_input: String,
    dex_order_type: String, // "buy" ou "sell"
    dark_pool: Vec<crate::Order>,
    active_swaps: Vec<crate::SwapContract>,
    is_loading_dex: bool,
    swap_secrets: std::collections::HashMap<String, String>, // Stocke temporairement les secrets générés
	completed_swaps: std::collections::HashSet<String>, // Historique des swaps finis
    last_watchtower_tick: Option<std::time::Instant>,   // Chronomètre
	
	// VARIABLES POUR MESSAGES ET NOTAIRE
    data_items: Vec<crate::DataItem>,
    data_tab: String, // "inbox", "send", "notary"
    is_loading_data: bool,
    
    msg_recipient: String,
    msg_content: String,
    msg_use_l2: bool,
    
    notary_filename: String,
    notary_hash: String,
    notary_use_l2: bool,
	
	// VARIABLES POUR LE L2
    l2_target_name: String,
    l2_stake_amount: String,
    l2_sequencer_pubkey: String,
    l2_status_result: String,
	
	bridge_l2_name: String,
    bridge_receiver_pubkey: String,
    bridge_amount: String,
	
	wns_tab: String, // "wallet" ou "server"
    wns_domain_input: String,
    wns_ip_input: String,
    wns_server_pubkey_input: String,
    wns_bid_amount: String,
	wns_domain_status: String,
    
    sync_message: String,
    tx: mpsc::Sender<AppMessage>,
    rx: mpsc::Receiver<AppMessage>,
}

impl WattcoinApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        egui_extras::install_image_loaders(&cc.egui_ctx);
		let mut fonts = egui::FontDefinitions::default();

		// 1. On charge le fichier TTF depuis les assets en l'intégrant au binaire
		fonts.font_data.insert(
			"ma_police_universelle".to_owned(),
			egui::FontData::from_static(include_bytes!("../assets/NotoSans-Regular.ttf")),
		);

		// 2. On dit à egui d'utiliser notre police EN PREMIER pour le texte normal (Proportional)
		fonts.families
			.entry(egui::FontFamily::Proportional)
			.or_default()
			.insert(0, "ma_police_universelle".to_owned());

		// 3. On fait pareil pour le texte typé code (Monospace)
		fonts.families
			.entry(egui::FontFamily::Monospace)
			.or_default()
			.insert(0, "ma_police_universelle".to_owned());

		// 4. On applique la configuration
		cc.egui_ctx.set_fonts(fonts);
        let (tx, rx) = mpsc::channel(100);
		
		#[cfg(target_os = "android")]
        {
            *APP_TX.lock().unwrap() = Some(tx.clone());
        }
        
        // NOUVELLE LOGIQUE DE DÉMARRAGE
        let wallets = crate::list_wallets();
        let initial_view = if wallets.is_empty() {
            crate::set_active_wallet("Principal");
            AppView::Onboarding
        } else {
            AppView::WalletSelection
        };
        
        Self {
			new_wallet_name: String::new(),
            view: initial_view,
            onboarding_step: OnboardingStep::Selection,
            
            wallet_keys: None,
            balance_l1: 0.0,
			balance_l2: 0.0,
            balance_btc: 0.0,    
			balance_wns: 0.0,
            watt_price_usd: 0.0,
            btc_price_usd: 0.0,
			total_supply: 0, 
            jackpot: 0,
			current_block_height: 0,
            is_refreshing: false,
            
            password_input: String::new(),
            password_confirm: String::new(),
            seed_input: String::new(),
            
            show_seed: false,
            show_seed_qr: false,
            decrypted_seed: String::new(),
			
			show_payment_qr: false, 
            payment_qr_data: String::new(),
			payment_qr_amount: String::new(),
			
			is_dark_mode: true,
			
			recipient_input: String::new(),
			amount_input: String::new(),
			transfer_from_l2: false,
			transfer_to_l2: false,
			transfer_asset: "WATT".to_string(), // Transfert WATT par défaut
			tx_weight_estimate: None,
			
			history_items: Vec::new(),
            history_tab: "L1".to_string(), // Par défaut sur L1
            is_loading_history: false,
			
			dex_amount_input: String::new(),
            dex_price_input: String::new(),
            dex_order_type: "buy".to_string(), // Achat par défaut
            dark_pool: Vec::new(),
            active_swaps: Vec::new(),
            is_loading_dex: false,
            swap_secrets: std::collections::HashMap::new(),
			completed_swaps: std::collections::HashSet::new(),
            last_watchtower_tick: None,
			
			data_items: Vec::new(),
            data_tab: "inbox".to_string(),
            is_loading_data: false,
            
            msg_recipient: String::new(),
            msg_content: String::new(),
            msg_use_l2: true, // L2 par défaut, moins cher
            
            notary_filename: String::new(),
            notary_hash: String::new(),
            notary_use_l2: true,
			
			l2_target_name: String::new(),
			l2_stake_amount: String::new(),
			l2_sequencer_pubkey: String::new(),
			l2_status_result: String::new(),
			
			bridge_l2_name: String::new(),
            bridge_receiver_pubkey: String::new(),
            bridge_amount: String::new(),
			
			wns_tab: "wallet".to_string(),
			wns_domain_input: String::new(),
			wns_ip_input: String::new(),
			wns_server_pubkey_input: String::new(),
			wns_bid_amount: "0.0000015".to_string(), // 1500 FLAME
			wns_domain_status: String::new(),
            
            sync_message: String::new(),
            tx,
            rx,
        }
    }

    // 3. FONCTION DE RAFRAÎCHISSEMENT DU DASHBOARD
    fn refresh_dashboard(&mut self, ctx: egui::Context) {
        if self.is_refreshing { return; }
        self.is_refreshing = true;
        
        if let Some(keys) = self.wallet_keys.clone() {
            let tx = self.tx.clone();
            
            // On extrait l'ancienne balance
            let old_btc_balance = self.balance_btc; 
            
            tokio::spawn(async move {
                // A. Récupération des Soldes WATT
                let mut l1_balance = 0.0;
                let mut l2_balance = 0.0;
                
                if let Ok(balances) = crate::get_balances(keys.clone()).await {
                    l1_balance = balances.l1;
                    l2_balance = balances.l2;
                }
                
				// B. Récupération du prix BTC/USD (Avec Timeout de sécurité !)
                let mut btc_usd = 60_000.0; 
                let binance_client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().unwrap();
                if let Ok(resp) = binance_client.get("https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT").send().await {
                    if let Ok(json) = resp.json::<serde_json::Value>().await {
                        if let Some(price_str) = json["price"].as_str() {
                            btc_usd = price_str.parse().unwrap_or(60_000.0);
                        }
                    }
                }

                // C. Récupération du Solde BITCOIN (Via Tor/Proxy)
                let mut btc_balance = old_btc_balance;
                if let Ok(b) = crate::get_btc_balance(keys.master_seed_hex.clone(), Some(keys.btc_address.clone())).await {
                    btc_balance = b; 
                }
                
                // Récupération du Solde WNS !
                let mut wns_balance = 0.0;
                let resolver = if crate::LOCAL_DEV_MODE { "http://127.0.0.1:8200" } else { "http://80.78.26.243/wns" };
                let wns_url = format!("{}/balance/{}", resolver, keys.watt_address);
                let wns_client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().unwrap();
                if let Ok(res) = wns_client.get(&wns_url).send().await {
                    if let Ok(json) = res.json::<serde_json::Value>().await {
                        wns_balance = json["balance"].as_u64().unwrap_or(0) as f64 / 1_000_000_000.0;
                    }
                }
                
                // D. Calcul des prix
                let mut price_sats = 0;
                let mut current_block_height = 0; 
                if let Ok(info) = crate::get_network_info().await {
                    let price_val = &info["last_price_sats"];
                    price_sats = price_val.as_u64()
                        .or_else(|| price_val.as_f64().map(|f| f as u64))
                        .or_else(|| price_val.as_str().and_then(|s| s.parse::<u64>().ok()))
                        .unwrap_or(0);
                    current_block_height = info["blocks"].as_u64().unwrap_or(0); 
                }
                
                // FIX DU PRIX : S'il est à 0, on envoie -1 pour dire au Wallet de garder l'ancien !
                let watt_price_usd = if price_sats > 0 {
                    (price_sats as f64 / 100_000_000.0) * btc_usd
                } else {
                    -1.0 
                };
				
				// Récupération des métriques réseau
                let mut total_supply = 0;
                let mut jackpot = 0;
                if let Ok(s) = crate::get_total_supply().await { total_supply = s; }
                if let Ok(j) = crate::get_current_jackpot().await { jackpot = j; }
                
                // Envoi des données à l'interface
                let _ = tx.send(AppMessage::DashboardData { 
                    balance_l1: l1_balance, 
                    balance_l2: l2_balance,
                    balance_btc: btc_balance,  
					balance_wns: wns_balance,
                    price_usd: watt_price_usd,
                    btc_price_usd: btc_usd,
					total_supply,
                    jackpot,
					current_block_height
                }).await;
				crate::set_status("");
                ctx.request_repaint(); 
            });
        }
    }
	
	fn refresh_history(&mut self, ctx: egui::Context) {
		if self.is_loading_history { return; }
		self.is_loading_history = true;
		
		if let Some(keys) = self.wallet_keys.clone() {
			let tx = self.tx.clone();
			tokio::spawn(async move {
				// 1. AFFICHAGE INSTANTANÉ (Cache local)
				let local_data = crate::get_history_offline(keys.clone()).await.unwrap_or_default();
				let _ = tx.send(AppMessage::HistoryData(local_data.clone())).await;
				
				// 2. MISE À JOUR RÉSEAU (Arrière-plan)
				match crate::get_history(keys).await {
					Ok(new_history) => {
						let _ = tx.send(AppMessage::HistoryData(new_history)).await;
					}
					Err(e) => {
						let _ = tx.send(AppMessage::Error(e)).await;
						// On renvoie la donnée locale en cas d'erreur
						// pour forcer is_loading_history à repasser sur false !
						let _ = tx.send(AppMessage::HistoryData(local_data)).await;
					}
				}
				
				crate::set_status("");
				ctx.request_repaint();
			});
		}
	}
	
	fn refresh_dex(&mut self, ctx: egui::Context) {
        if self.is_loading_dex { return; }
        self.is_loading_dex = true;

        // CHARGEMENT ANTI-AMNÉSIE
		if let Ok(path) = crate::get_swap_secrets_path() {
			if let Ok(data) = std::fs::read_to_string(path) {
				if let Ok(saved_secrets) = serde_json::from_str::<std::collections::HashMap<String, String>>(&data) {
					self.swap_secrets.extend(saved_secrets);
				}
			}
		}
        
        if let Some(keys) = self.wallet_keys.clone() {
            let tx = self.tx.clone();
            let secrets = self.swap_secrets.clone();
            let completed = self.completed_swaps.clone(); 
            
            tokio::spawn(async move {
                let mut pool = Vec::new();
                if let Ok(p) = crate::get_dark_pool().await { pool = p; }
                
                let mut swaps = Vec::new();
                if let Ok(s) = crate::get_active_swaps(keys.btc_address.clone(), keys.watt_address.clone()).await { 
                    swaps = s; 
                }

                // LE WATCHTOWER BAVARD
                for swap in &swaps {
                    if completed.contains(&swap.htlc_hash) { continue; }

                    let is_buyer = keys.watt_address == swap.buyer_watt_address;
                    
					if is_buyer {
                        // ACHETEUR
                        if let Ok(true) = crate::check_watt_lock_exists(swap.htlc_hash.clone()).await {
                            println!("🔍 [WATCHTOWER] Verrou WATT détecté sur la blockchain pour le hash {} !", &swap.htlc_hash[0..10]);
                            
                            if let Some(secret) = secrets.get(&swap.htlc_hash) {
								println!("🔑 [WATCHTOWER] Secret trouvé ! Tir de la transaction Claim...");
								let _ = tx.send(AppMessage::Info("👁️ Watchtower: Auto-Claim en cours...".to_string())).await;
								
								match crate::claim_wattcoin_swap(secret.clone(), swap.htlc_hash.clone(), swap.watt_amount_flames, swap.buyer_watt_address.clone()).await {
									Ok(_) => {
                                        // 💡 LE WATCHTOWER NETTOIE SON CACHE !
                                        crate::remove_swap_from_cache(&swap.htlc_hash);
										let _ = tx.send(AppMessage::SwapCompleted(swap.htlc_hash.clone())).await;
									}
									Err(e) => {
										println!("🚨 [WATCHTOWER ERROR] Le nœud a rejeté le Claim : {}", e);
										let _ = tx.send(AppMessage::Error(format!("Erreur Watchtower: {}", e))).await;
									}
								}
							} else {
								println!("🚨 [WATCHTOWER FATAL] Les WATT sont verrouillés, mais le secret est introuvable !");
								let _ = tx.send(AppMessage::Error("Erreur Watchtower: Secret HTLC introuvable en local !".to_string())).await;
							}
                        }
                    } else {
                        // VENDEUR
                        if let Ok(secret) = crate::get_revealed_secret(swap.htlc_hash.clone()).await {
                            println!("🔍 [WATCHTOWER] Secret de l'acheteur révélé sur le réseau : {} !", secret);
                            let _ = tx.send(AppMessage::Info("👁️ Watchtower: Secret révélé ! Auto-Claim BTC en cours...".to_string())).await;
                            
                            // ON GÈRE LE RÉSULTAT DU CLAIM BTC POUR NETTOYER LE CACHE
                            match crate::auto_claim_btc_swap(swap.htlc_hash.clone(), "".to_string()).await {
                                Ok(_) => {
                                    crate::remove_swap_from_cache(&swap.htlc_hash);
                                    let _ = tx.send(AppMessage::SwapCompleted(swap.htlc_hash.clone())).await;
                                }
                                Err(e) => {
                                    let _ = tx.send(AppMessage::Error(format!("Erreur Watchtower BTC: {}", e))).await;
                                }
                            }
                        }
                    }
                }
                
                let _ = tx.send(AppMessage::DexData { pool, swaps }).await;
				crate::set_status("");
                ctx.request_repaint();
            });
        }
    }
	
	fn refresh_data(&mut self, ctx: egui::Context) {
        if self.is_loading_data { return; }
        self.is_loading_data = true;
        
        if let Some(keys) = self.wallet_keys.clone() {
            let tx = self.tx.clone();
            tokio::spawn(async move {
                if let Ok(items) = crate::get_messages(keys).await {
                    let _ = tx.send(AppMessage::DataFetched(items)).await;
                } else {
                    let _ = tx.send(AppMessage::DataFetched(vec![])).await;
                }
				crate::set_status("");
                ctx.request_repaint();
            });
        }
    }
	
	
}

impl eframe::App for WattcoinApp {
	fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        
        // GESTION DU THÈME
        let mut visuals = if self.is_dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };

        // PERSONNALISATION DES CHAMPS DE TEXTE (TextEdit)
        // 'extreme_bg_color' est la couleur de fond des zones où l'on tape au clavier
        if self.is_dark_mode {
            // Un gris foncé texturé pour bien ressortir sur le fond noir
            visuals.extreme_bg_color = egui::Color32::from_rgb(45, 50, 55); 
        } else {
            // Un gris perle pour se démarquer du fond blanc
            visuals.extreme_bg_color = egui::Color32::from_rgb(225, 230, 235);
        }

        ctx.set_visuals(visuals);

       // Palette dynamique qui s'adapte au mode
        let panel_bg = if self.is_dark_mode { egui::Color32::from_black_alpha(240) } else { egui::Color32::from_white_alpha(245) };
        let item_bg = if self.is_dark_mode { egui::Color32::from_black_alpha(150) } else { egui::Color32::from_white_alpha(220) };
        let text_color = if self.is_dark_mode { egui::Color32::WHITE } else { egui::Color32::BLACK };
        let text_muted = if self.is_dark_mode { egui::Color32::LIGHT_GRAY } else { egui::Color32::DARK_GRAY };
		
        // Auto-Tick GLOBAL (Toutes les 15 secondes)
        let now = std::time::Instant::now();
        let should_refresh = self.last_watchtower_tick
            .map_or(true, |last| now.duration_since(last).as_secs() >= 15);

        if should_refresh {
            self.last_watchtower_tick = Some(now);
            
            // 1. Rafraîchissement du DEX et du Watchtower (Uniquement si on y est)
            if self.view == AppView::Dex {
                self.refresh_dex(ctx.clone());
            }

            // 2. Rafraîchissement GLOBAL de l'économie (Prix WATT, Jackpot, Blocs, Soldes)
            // Tourne en tâche de fond en permanence tant que le portefeuille est déverrouillé !
            if self.wallet_keys.is_some() && !self.is_refreshing {
                self.refresh_dashboard(ctx.clone());
            }
        }
        
        // Maintient l'interface éveillée pour que le chrono tourne
        ctx.request_repaint_after(std::time::Duration::from_secs(1)); 
        
        // ==========================================
        // LECTURE DES MESSAGES ASYNCHRONES
        // ==========================================
        // UTILISER 'WHILE' AU LIEU DE 'IF' POUR VIDER LE CACHE INSTANTANÉMENT
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                AppMessage::UnlockSuccess(keys) => {

					self.decrypted_seed = keys.mnemonic.clone();
					self.wallet_keys = Some(keys);
					self.view = AppView::Dashboard;
					self.sync_message.clear();
					self.password_input.clear();
					
					self.refresh_dashboard(ctx.clone());
				},
                AppMessage::Error(err) => {
                    self.sync_message = err;
                },
                AppMessage::Info(info) => {
					self.sync_message = info.clone();
				},
                AppMessage::QrScanned(content) => {
					// SÉCURITÉ : Le comportement dépend STRICTEMENT de l'écran actuel
					if self.view == AppView::Onboarding || self.view == AppView::Settings {
						
						// 1. Contexte Restauration : On refuse catégoriquement les paiements
						if content.starts_with("watt://") || content.starts_with("bitcoin:") {
							self.sync_message = "❌ Erreur : QR Code de paiement détecté. Veuillez scanner une Phrase Secrète.".to_string();
						} else {
							self.seed_input = content.replace("\n", " "); 
							self.onboarding_step = OnboardingStep::RestoreSeed;
							self.view = AppView::Onboarding;
							self.sync_message = "✅ QR Code de restauration décodé !".to_string();
						}
						
					} else if self.view == AppView::Transfer {
    
						// 2. Contexte Paiement : On refuse catégoriquement les Seeds
						if content.starts_with("watt://") || content.starts_with("bitcoin:") {
							let asset = if content.starts_with("watt://") { "WATT" } else { "BTC" };
							
							// On retire le préfixe du protocole
							let raw_data = content.replace("watt://", "").replace("bitcoin:", "");
							
							// SÉPARATION DE L'ADRESSE ET DU MONTANT
							if let Some(question_mark_pos) = raw_data.find('?') {
								// L'adresse est tout ce qui se trouve avant le '?'
								self.recipient_input = raw_data[0..question_mark_pos].to_string();
								
								// On cherche le paramètre "amount=" après le '?'
								let params = &raw_data[question_mark_pos + 1..];
								if params.starts_with("amount=") {
									self.amount_input = params.replace("amount=", "");
								}
							} else {
								// S'il n'y a pas de '?', c'est juste une adresse simple
								self.recipient_input = raw_data;
								self.amount_input.clear();
							}

							self.transfer_asset = asset.to_string();
							self.sync_message = format!("✅ QR Code {} scanné, prêt à envoyer !", asset);
						} else {
							self.sync_message = "❌ Erreur : Ce QR Code ne contient pas une adresse de paiement valide.".to_string();
						}
						
					} else {
						// Au cas où l'utilisateur scanne depuis un endroit imprévu
						self.sync_message = "❌ Scan refusé : Contexte inattendu.".to_string();
					}
				},
                AppMessage::DashboardData { balance_l1, balance_l2, balance_btc, balance_wns, price_usd, btc_price_usd, total_supply, jackpot, current_block_height } => {
                    self.balance_l1 = balance_l1;
                    self.balance_l2 = balance_l2;
                    self.balance_btc = balance_btc; 
					self.balance_wns = balance_wns;
                    
                    // On ne met à jour que si c'est valide
                    if price_usd >= 0.0 {
                        self.watt_price_usd = price_usd;
                    }
                    
                    self.btc_price_usd = btc_price_usd; 
					self.total_supply = total_supply; 
					self.jackpot = jackpot;
					self.current_block_height = current_block_height;
                    self.is_refreshing = false;
                },
				AppMessage::HistoryData(items) => {
					self.history_items = items;
					self.is_loading_history = false;
				},
				AppMessage::DexData { pool, swaps } => {
                    self.dark_pool = pool;
                    self.active_swaps = swaps;
                    self.is_loading_dex = false;
                },
				AppMessage::SwapCompleted(hash) => {
                    self.completed_swaps.insert(hash);
                    self.sync_message = "✅ Swap Atomique terminé et fonds récupérés !".to_string();
                },
				AppMessage::DataFetched(items) => {
                    self.data_items = items;
                    self.is_loading_data = false;
                },
                AppMessage::FileHashed(filename, hash) => {
                    self.notary_filename = filename;
                    self.notary_hash = hash;
                    self.sync_message = "✅ Document analysé et haché en mémoire !".to_string();
                },
				AppMessage::L2StatusFetched(status) => {
					self.l2_status_result = status;
					self.sync_message = "✅ Statut L2 récupéré.".to_string();
				},
				AppMessage::DomainStatus(status) => { 
					self.wns_domain_status = status;
				},
				AppMessage::TxWeightEstimated(utxo_count, size_mb) => {
                    self.tx_weight_estimate = Some((utxo_count, size_mb));
                }
            }
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            // Marge pour la barre de l'heure Android
            #[cfg(target_os = "android")]
            ui.add_space(35.0); 

            ui.add_space(5.0);
            ui.horizontal(|ui| {
                ui.menu_button("☰", |ui| {
                    ui.set_min_width(200.0);
                    if ui.button("🔒 Portefeuilles").clicked() { 
                        self.view = AppView::Dashboard; 
                        self.refresh_dashboard(ctx.clone()); 
                        ui.close_menu(); 
                    }
					if ui.button("❌ Changer de portefeuille").clicked() { 
						self.view = AppView::WalletSelection; 
						self.wallet_keys = None; 
						self.decrypted_seed.clear();
						self.password_input.clear();
						ui.close_menu(); 
					}
					if ui.button("💸 Envoyer des fonds").clicked() { self.view = AppView::Transfer; ui.close_menu(); }
                    if ui.button("📜 Historique").clicked() { 
						self.view = AppView::History; 
						// On ne bloque plus le réseau si on a déjà l'historique en cache !
						if self.history_items.is_empty() {
							self.refresh_history(ctx.clone()); 
						}
						ui.close_menu(); 
					}
					if ui.button("⚡ DEX").clicked() { self.view = AppView::Dex; ui.close_menu(); }
					if ui.button("🎰 Loterie").clicked() { self.view = AppView::Lottery; ui.close_menu(); } 
					if ui.button("✉ Messages & Notaire").clicked() { 
						self.view = AppView::Messages; 
						self.refresh_data(ctx.clone());
						ui.close_menu(); 
					}
					if ui.button("🌐 Séquenceur L2").clicked() { self.view = AppView::L2Manager; ui.close_menu(); }
					if ui.button("🌉 Bridge L1 ↔ L2").clicked() { self.view = AppView::Bridge; ui.close_menu(); } 
					if ui.button("🏷 Noms de Domaine (WNS)").clicked() { self.view = AppView::WnsManager; ui.close_menu(); }
                    ui.separator();
                    if ui.button("⚙ Paramètres").clicked() { self.view = AppView::Settings; ui.close_menu(); }
                    if ui.button("❌ Verrouiller").clicked() { 
                        self.view = AppView::Unlock; 
                        self.wallet_keys = None; // 🛡️ On purge les clés de la mémoire !
                        self.decrypted_seed.clear();
                        self.password_input.clear();
                        ui.close_menu(); 
                    }
                });
                ui.separator();
				
                // Bouton de thème à droite et Titre au centre
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(if self.is_dark_mode { "☀" } else { "🌙" }).clicked() {
                        self.is_dark_mode = !self.is_dark_mode;
                    }
                    
                    ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| { 
                        ui.heading("WATTCOIN NETWORK"); 
                    });
                });
            });
            ui.add_space(5.0);
        });

        egui::CentralPanel::default().frame(egui::Frame::none()).show(ctx, |ui| {
            let rect = ui.max_rect();
            let background_image = egui::Image::new(egui::include_image!("../assets/coffre_fort.png")).maintain_aspect_ratio(false);
            ui.put(rect, background_image);

            let mut overlay_ui = ui.new_child(egui::UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(egui::Align::Center)));

            match self.view {
                AppView::WalletSelection => {
					overlay_ui.add_space(rect.height() * 0.20);
					egui::Frame::none().fill(panel_bg).inner_margin(30.0).rounding(12.0).show(&mut overlay_ui, |ui| {
						ui.heading("🗄 Sélectionnez un Sanctuaire");
						ui.add_space(20.0);

						let wallets = crate::list_wallets();
						
						// Liste des portefeuilles existants
						for wallet in wallets {
							ui.horizontal(|ui| {
								// Bouton principal pour ouvrir le portefeuille
								if ui.add_sized([250.0, 45.0], egui::Button::new(format!("🔓 Ouvrir : {}", wallet))).clicked() {
									crate::set_active_wallet(&wallet);
									self.view = AppView::Unlock;
									self.sync_message.clear();
								}
								
								// Petit bouton rouge pour supprimer
								if ui.add_sized([45.0, 45.0], egui::Button::new("🗑").fill(egui::Color32::from_rgb(150, 40, 40))).clicked() {
									let _ = crate::delete_wallet(&wallet);
									self.sync_message = format!("🗑 Portefeuille '{}' supprimé.", wallet);
								}
							});
							ui.add_space(10.0);
						}

						// Affichage des messages système (comme la confirmation de suppression)
						if !self.sync_message.is_empty() {
							ui.add_space(5.0);
							ui.colored_label(egui::Color32::from_rgb(255, 100, 100), &self.sync_message);
						}

						ui.separator();
						ui.add_space(15.0);
						
						// Création d'un nouveau portefeuille
						ui.label(egui::RichText::new("Créer un nouveau portefeuille :").color(egui::Color32::GRAY));
						ui.add_space(5.0);
						ui.horizontal(|ui| {
							ui.add(egui::TextEdit::singleline(&mut self.new_wallet_name).hint_text("Ex: DeFi, Épargne..."));
							if ui.button("➕ Ajouter").clicked() && !self.new_wallet_name.is_empty() {
								crate::set_active_wallet(&self.new_wallet_name);
								self.onboarding_step = OnboardingStep::Selection;
								self.view = AppView::Onboarding;
								self.sync_message.clear();
								self.new_wallet_name.clear(); // On nettoie le champ
							}
						});
					});
				}
				
				AppView::Onboarding => {
                    overlay_ui.add_space(rect.height() * 0.35);
                    egui::Frame::none().fill(panel_bg).inner_margin(25.0).rounding(12.0).show(&mut overlay_ui, |ui| {
                        
                        if self.onboarding_step == OnboardingStep::Selection {
                            ui.heading("Bienvenue dans le Sanctuaire");
                            ui.add_space(20.0);

                            if ui.add_sized([300.0, 50.0], egui::Button::new("⚡ Créer un nouveau Coffre")).clicked() {
                                self.onboarding_step = OnboardingStep::Create;
                                self.sync_message.clear();
                            }
                            ui.add_space(10.0);

                            if ui.add_sized([300.0, 50.0], egui::Button::new("📜 Restaurer avec 48 mots")).clicked() {
                                self.onboarding_step = OnboardingStep::RestoreSeed;
                                self.sync_message.clear();
                            }
                            ui.add_space(10.0);

                            if ui.add_sized([300.0, 50.0], egui::Button::new("📷 Scanner un QR Code (Image)")).clicked() {
								#[allow(unused_variables)]
								let tx = self.tx.clone();
								
								#[cfg(not(target_os = "android"))]
								tokio::task::spawn_blocking(move || {
									if let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg"]).pick_file() {
										if let Ok(img) = image::open(&path) {
											let img_luma = img.into_luma8();
											let (w, h) = img_luma.dimensions();
											let w_usize = w as usize;
											let pixels = img_luma.into_raw();
											
											let mut prepared_img = rqrr::PreparedImage::prepare_from_greyscale(w_usize, h as usize, |x, y| pixels[y * w_usize + x]);
											let grids = prepared_img.detect_grids();
											
											if let Some(grid) = grids.first() {
												if let Ok((_, content)) = grid.decode() {
													let content_nfc = content.nfc().collect::<String>();
													let _ = tx.blocking_send(AppMessage::QrScanned(content_nfc));
												} else { let _ = tx.blocking_send(AppMessage::Error("❌ Données illisibles.".into())); }
											} else { let _ = tx.blocking_send(AppMessage::Error("❌ Aucun QR détecté.".into())); }
										}
									}
								});

								#[cfg(target_os = "android")]
								{
									if let Some(app) = ANDROID_APP.lock().unwrap().clone() {
										// On tape sur l'épaule de notre diplomate Java !
										let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM).unwrap() };
										let mut env = vm.attach_current_thread().unwrap();
										let activity = unsafe { jni::objects::JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
										
										let _ = env.call_method(activity, "openGallery", "()V", &[]);
									}
								}
							}

                            if !self.sync_message.is_empty() {
                                ui.add_space(15.0);
                                ui.colored_label(egui::Color32::RED, &self.sync_message);
                            }
                        }

                        if self.onboarding_step == OnboardingStep::Create {
                            if ui.button("⬅ Retour").clicked() { self.onboarding_step = OnboardingStep::Selection; }
                            ui.add_space(10.0);
                            ui.heading("Créer un Nouveau Sanctuaire");
                            ui.add_space(15.0);
                            
                            // On dessine le champ
                            ui.add(egui::TextEdit::singleline(&mut self.password_input).password(true).hint_text("Mot de passe robuste"));
							ui.add(egui::TextEdit::singleline(&mut self.password_confirm).password(true).hint_text("Confirmer le mot de passe"));
                            
                            
                            ui.add_space(20.0);
                            
                            if ui.add_sized([300.0, 40.0], egui::Button::new("Générer les Clés Quantiques")).clicked() {
                                // SÉCURITÉ : On bloque les mots de passe vides
                                if self.password_input.trim().is_empty() {
                                    self.sync_message = "❌ Le mot de passe ne peut pas être vide.".to_string();
                                } else if self.password_input != self.password_confirm {
                                    self.sync_message = "❌ Les mots de passe ne correspondent pas.".to_string();
                                } else {
                                    self.sync_message = "Génération des clés en cours (Kyber + BIP39)...".to_string();
                                    

                                    let pwd = self.password_input.clone();
                                    let tx = self.tx.clone();
                                    tokio::spawn(async move {
                                        match crate::generate_pro_wallet(None, pwd.clone()).await {
                                            Ok(keys) => {
                                                let keys_json = serde_json::to_string(&keys).unwrap();
                                                match crate::encrypt_vault(pwd, keys_json) {
                                                    Ok(_) => { let _ = tx.send(AppMessage::UnlockSuccess(keys)).await; }
                                                    Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
                                                }
                                            }
                                            Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
                                        }
                                    });
                                }
                            }
                            if !self.sync_message.is_empty() {
                                ui.add_space(10.0);
                                ui.colored_label(egui::Color32::from_rgb(0, 240, 255), &self.sync_message);
                            }
                        }

                        if self.onboarding_step == OnboardingStep::RestoreSeed {
                            if ui.button("⬅ Retour").clicked() { self.onboarding_step = OnboardingStep::Selection; }
                            ui.add_space(10.0);
                            ui.heading("Restaurer un Sanctuaire");
                            ui.add_space(15.0);

                            let response = ui.add_sized([300.0, 100.0], egui::TextEdit::multiline(&mut self.seed_input).hint_text("mot1 mot2 mot3..."));

							response.context_menu(|ui| {
								if ui.button("📋 Coller").clicked() {
									if let Some(pasted_text) = get_clipboard_text() {
										self.seed_input = pasted_text;
									}
									ui.close_menu();
								}
								if ui.button("📋 Copier").clicked() {
									set_clipboard_text(ui.ctx(), &self.seed_input);
									ui.close_menu();
								}
							});
                            
                            
                            self.seed_input = self.seed_input.nfc().collect::<String>();

                            let words: Vec<String> = self.seed_input
                                .split_whitespace()
                                .map(|w| w.to_string()) 
                                .collect();
                                
                            let mut invalid_words = Vec::new();
                            for word in &words {
                                let word_nfkd = word.nfkd().collect::<String>();
                                if Language::French.find_word(&word_nfkd.to_lowercase()).is_none() { 
                                    invalid_words.push(word.to_string()); 
                                }
                            }

                            if !invalid_words.is_empty() {
                                ui.add_space(10.0);
                                ui.colored_label(egui::Color32::RED, format!("⚠️ Mots inconnus : {}", invalid_words.join(", ")));
                            } else if words.len() > 0 && words.len() != 48 {
                                ui.add_space(10.0);
                                ui.colored_label(egui::Color32::from_rgb(255, 165, 0), format!("Compteur : {} / 48 mots", words.len()));
                            }

                            ui.add_space(15.0);
                            ui.add(egui::TextEdit::singleline(&mut self.password_input).password(true).hint_text("Nouveau Mot de passe"));
                            ui.add_space(5.0);
                            ui.add(egui::TextEdit::singleline(&mut self.password_confirm).password(true).hint_text("Confirmer"));
                            ui.add_space(20.0);

                            if ui.add_sized([300.0, 40.0], egui::Button::new("Restaurer le Coffre")).clicked() {
                                if self.password_input.trim().is_empty() {
                                    self.sync_message = "❌ Le mot de passe ne peut pas être vide.".to_string();
                                } else if self.password_input != self.password_confirm {
                                    self.sync_message = "❌ Les mots de passe ne correspondent pas.".to_string();
                                } else if words.len() != 48 {
                                    self.sync_message = "❌ Vous devez fournir exactement 48 mots.".to_string();
                                } else if !invalid_words.is_empty() {
                                    self.sync_message = "❌ Veuillez corriger les mots invalides.".to_string();
                                } else {
                                    self.sync_message = "Restauration et dérivation des clés...".to_string();
                                    

                                    let pwd = self.password_input.clone();
                                    let seed = self.seed_input.clone();
                                    let tx = self.tx.clone();
                                    tokio::spawn(async move {
                                        match crate::generate_pro_wallet(Some(seed), pwd.clone()).await {
                                            Ok(keys) => {
                                                let keys_json = serde_json::to_string(&keys).unwrap();
                                                match crate::encrypt_vault(pwd, keys_json) {
                                                    Ok(_) => { let _ = tx.send(AppMessage::UnlockSuccess(keys)).await; }
                                                    Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
                                                }
                                            }
                                            Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
                                        }
                                    });
                                }
                            }
                            if !self.sync_message.is_empty() { ui.add_space(10.0); ui.colored_label(egui::Color32::from_rgb(0, 240, 255), &self.sync_message); }
                        }
                    });
                }
                
                AppView::Unlock => {
                    overlay_ui.add_space(rect.height() * 0.65);
                    egui::Frame::none().fill(panel_bg).inner_margin(20.0).rounding(10.0).show(&mut overlay_ui, |ui| {
                        ui.heading("Déchiffrement du Sanctuaire");
                        ui.add_space(20.0); 

                        ui.add(egui::TextEdit::singleline(&mut self.password_input).password(true).hint_text("Mot de passe robuste"));
                        
                        ui.add_space(20.0);
                        
                        if ui.button("Ouvrir le Coffre").clicked() {
                            if self.password_input.trim().is_empty() {
                                self.sync_message = "❌ Mot de passe requis.".to_string();
                            } else {
                                self.sync_message = "Déchiffrement en cours...".to_string();
                                

                                let pwd = self.password_input.clone();
                                let tx = self.tx.clone();
                                
                                tokio::spawn(async move {
                                    match crate::unlock_vault(pwd).await {
                                        Ok(keys) => { let _ = tx.send(AppMessage::UnlockSuccess(keys)).await; }
                                        Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
                                    }
                                });
                            }
                        }

                        if !self.sync_message.is_empty() {
                            ui.add_space(10.0);
                            ui.colored_label(egui::Color32::RED, &self.sync_message);
                        }
                    });
                }

                // 4. LE NOUVEAU DASHBOARD COMPLET
                AppView::Dashboard => {
                    overlay_ui.add_space(rect.height() * 0.05); 
                    
                    egui::Frame::none()
                        .fill(panel_bg) // Fond très sombre pour contraster avec le coffre
                        .inner_margin(5.0)
                        .rounding(12.0)
                        .show(&mut overlay_ui, |ui| {
                            
                            // On active le défilement vertical pour pouvoir tout voir !
                            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                                
                                // A. VALEUR GLOBALE (USD)
                                ui.vertical_centered(|ui| {
                                    ui.label(egui::RichText::new("VALEUR TOTALE DU COFFRE").color(egui::Color32::GRAY).size(12.0));
                                    
                                    let total_usd = (self.balance_l1 * self.watt_price_usd) + 
                                                    (self.balance_l2 * self.watt_price_usd) + 
                                                    (self.balance_btc * self.btc_price_usd);
                                                    
                                    ui.heading(egui::RichText::new(format!("$ {:.2} USD", total_usd)).size(32.0).color(text_color));
                                    
                                    if self.is_refreshing {
                                        ui.add_space(5.0);
                                        let status = crate::get_status();
                                        let display_text = if status.is_empty() { "🔄 Synchronisation en cours..." } else { &status };
                                        ui.label(egui::RichText::new(display_text).color(egui::Color32::from_rgb(0, 240, 255)).size(10.0));
                                        ui.ctx().request_repaint(); 
                                    }
                                });

                                // STATISTIQUES RÉSEAU
                                ui.add_space(15.0);
                                ui.vertical(|ui| {
                                    let supply_watts = self.total_supply as f64 / 1_000_000_000.0;
                                    let jackpot_watts = self.jackpot as f64 / 1_000_000_000.0;

                                    ui.label(egui::RichText::new(format!("🌍 Supply : {:.9} WATT", supply_watts)).color(text_muted));
                                    ui.label(egui::RichText::new(format!("🎰 Jackpot : {:.9} WATT", jackpot_watts)).color(egui::Color32::GOLD));
                                });

                                ui.add_space(25.0);
                                ui.separator();
                                ui.add_space(15.0);

                                // B. WATTCOIN L1 (BALANCE & ADRESSE)
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("⚡ WATTCOIN L1 (Testnet)").strong().color(egui::Color32::from_rgb(0, 240, 255)).size(16.0));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(egui::RichText::new(format!("Prix : $ {:.4}", self.watt_price_usd)).color(egui::Color32::GRAY).size(12.0));
                                    });
                                });
                                
                                ui.add_space(10.0);

                                if let Some(keys) = &self.wallet_keys {
                                    let addr = &keys.watt_address;
                                    let display_addr = if addr.len() > 16 { format!("{}...", &addr[0..16]) } else { addr.clone() };
                                    
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(display_addr).monospace().color(egui::Color32::DARK_GRAY));
                                        
                                        if ui.button("📋 Copier").clicked() {
                                            #[cfg(not(target_os = "android"))]
                                            ui.output_mut(|o| o.copied_text = addr.clone());
                                            #[cfg(target_os = "android")]
                                            copy_to_android_clipboard(&addr);
                                            self.sync_message = "Adresse copiée dans le presse-papier !".to_string();
                                        }
                                        if ui.button("📱 QR").clicked() {
                                            self.payment_qr_data = format!("watt://{}", addr);
                                            self.payment_qr_amount.clear();
                                            self.show_payment_qr = true;
                                        }
                                    });
                                }

                                ui.add_space(15.0);
                                
                                ui.vertical_centered(|ui| {
                                    ui.heading(egui::RichText::new(format!("{:.9} WATT", self.balance_l1)).size(24.0).color(text_color));
                                    let l1_usd = self.balance_l1 * self.watt_price_usd;
                                    ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", l1_usd)).color(egui::Color32::GRAY));
                                });
                                
                                ui.add_space(15.0);
                                ui.separator();
                                ui.add_space(15.0);

                                // C. BLOC WATTCOIN L2
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("🌐 WATTCOIN L2 (Testnet)").strong().color(egui::Color32::from_rgb(150, 240, 50)).size(16.0));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(egui::RichText::new(format!("Prix : $ {:.4}", self.watt_price_usd)).color(egui::Color32::GRAY).size(12.0));
                                    });
                                });

                                ui.add_space(10.0);

                                if let Some(keys) = &self.wallet_keys {
                                    let addr = &keys.watt_address;
                                    let display_addr = if addr.len() > 16 { format!("{}...", &addr[0..16]) } else { addr.clone() };
                                    
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(display_addr).monospace().color(egui::Color32::DARK_GRAY));
                                        
                                        if ui.button("📋 Copier").clicked() {
                                            #[cfg(not(target_os = "android"))]
                                            ui.output_mut(|o| o.copied_text = addr.clone());
                                            #[cfg(target_os = "android")]
                                            copy_to_android_clipboard(&addr);
                                            self.sync_message = "Adresse copiée dans le presse-papier !".to_string();
                                        }
                                        if ui.button("📱 QR").clicked() {
                                            self.payment_qr_data = format!("watt://{}", addr);
                                            self.payment_qr_amount.clear();
                                            self.show_payment_qr = true;
                                        }
                                    });
                                }
                                
                                ui.add_space(15.0);
								
                                ui.vertical_centered(|ui| {
                                    ui.heading(egui::RichText::new(format!("{:.9} WATT", self.balance_l2)).size(24.0).color(text_color));
                                    let l2_usd = self.balance_l2 * self.watt_price_usd;
                                    ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", l2_usd)).color(egui::Color32::GRAY));
                                });
                                
                                ui.add_space(15.0);
                                ui.separator();
                                ui.add_space(15.0);

                                // D. BLOC WNS (NOMS DE DOMAINE)
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("🏷 REGISTRE WNS L2 (Testnet)").strong().color(egui::Color32::from_rgb(200, 100, 255)).size(16.0));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(egui::RichText::new(format!("Prix : $ {:.4}", self.watt_price_usd)).color(egui::Color32::GRAY).size(12.0));
                                    });
                                });

                                ui.add_space(10.0);

                                ui.vertical_centered(|ui| {
                                    ui.heading(egui::RichText::new(format!("{:.9} WATT", self.balance_wns)).size(24.0).color(text_color));
                                    let wns_usd = self.balance_wns * self.watt_price_usd;
                                    ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", wns_usd)).color(egui::Color32::GRAY));
                                });
                                
                                ui.add_space(15.0);
                                ui.separator();
                                ui.add_space(15.0);

                                // E. BLOC BITCOIN (Natif SegWit)
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new("₿ BITCOIN (Testnet)").strong().color(egui::Color32::from_rgb(247, 147, 26)).size(16.0));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        ui.label(egui::RichText::new(format!("Prix : $ {:.2}", self.btc_price_usd)).color(egui::Color32::GRAY).size(12.0));
                                    });
                                });

                                ui.add_space(10.0);

                                if let Some(keys) = &self.wallet_keys {
                                    let addr = &keys.btc_address;
                                    let display_addr = if addr.len() > 16 { format!("{}...", &addr[0..16]) } else { addr.clone() };
                                    
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(display_addr).monospace().color(egui::Color32::DARK_GRAY));
                                        
                                        if ui.button("📋 Copier").clicked() {
                                            #[cfg(not(target_os = "android"))]
                                            ui.output_mut(|o| o.copied_text = addr.clone());
                                            #[cfg(target_os = "android")]
                                            copy_to_android_clipboard(&addr);
                                            self.sync_message = "Adresse copiée dans le presse-papier !".to_string();
                                        }
                                        if ui.button("📱 QR").clicked() {
                                            self.payment_qr_data = format!("bitcoin:{}", addr);
                                            self.payment_qr_amount.clear();
                                            self.show_payment_qr = true;
                                        }
                                    });
                                }
                                
                                ui.add_space(15.0);

                                ui.vertical_centered(|ui| {
                                    ui.heading(egui::RichText::new(format!("{:.8} BTC", self.balance_btc)).size(24.0).color(text_color));
                                    let btc_usd = self.balance_btc * self.btc_price_usd;
                                    ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", btc_usd)).color(egui::Color32::GRAY));
                                });
                                
                                if !self.sync_message.is_empty() {
                                    ui.add_space(15.0);
                                    ui.vertical_centered(|ui| {
                                        ui.colored_label(egui::Color32::GREEN, &self.sync_message);
                                    });
                                }
                                
                            }); // FIN DU SCROLL ICI
                        });
                }
				
				AppView::Transfer => {
					overlay_ui.add_space(rect.height() * 0.15);
					egui::Frame::none().fill(panel_bg).inner_margin(25.0).rounding(12.0).show(&mut overlay_ui, |ui| {
						ui.heading("💸 Envoyer des fonds");
						ui.add_space(15.0);

						// Sélecteur de cryptomonnaie
						ui.horizontal(|ui| {
							ui.selectable_value(&mut self.transfer_asset, "WATT".to_string(), "⚡ WATTCOIN");
							ui.selectable_value(&mut self.transfer_asset, "BTC".to_string(), "₿ BITCOIN");
						});
						ui.separator();
						ui.add_space(15.0);

						if self.transfer_asset == "WATT" {
							ui.label("Adresse Kyber du destinataire :");
						} else {
							ui.label("Adresse Bitcoin du destinataire (Testnet) :");
						}
						
						ui.horizontal(|ui| {
						    let hint = if self.transfer_asset == "WATT" { "pq_watt_... ou L2_WATT_..." } else { "tb1... ou m..." };

							// On stocke la réponse de l'UI
							let response = ui.add(egui::TextEdit::singleline(&mut self.recipient_input).hint_text(hint));

							response.context_menu(|ui| {
								if ui.button("📋 Coller").clicked() {
									if let Some(pasted_text) = get_clipboard_text() {
										self.recipient_input = pasted_text;
									}
									ui.close_menu();
								}
								if ui.button("📋 Copier").clicked() {
									set_clipboard_text(ui.ctx(), &self.recipient_input);
									ui.close_menu();
								}
							});
							
							if ui.button("📷 Scanner").clicked() {
								let tx = self.tx.clone();
								
								#[cfg(not(target_os = "android"))]
								tokio::task::spawn_blocking(move || {
									if let Some(path) = rfd::FileDialog::new().add_filter("Images", &["png", "jpg", "jpeg"]).pick_file() {
										if let Ok(img) = image::open(&path) {
											let img_luma = img.into_luma8();
											let (w, h) = img_luma.dimensions();
											let w_usize = w as usize;
											let pixels = img_luma.into_raw();
											
											let mut prepared_img = rqrr::PreparedImage::prepare_from_greyscale(w_usize, h as usize, |x, y| pixels[y * w_usize + x]);
											let grids = prepared_img.detect_grids();
											
											if let Some(grid) = grids.first() {
												if let Ok((_, content)) = grid.decode() {
													use unicode_normalization::UnicodeNormalization;
													let _ = tx.blocking_send(AppMessage::QrScanned(content.nfc().collect::<String>()));
												} else { let _ = tx.blocking_send(AppMessage::Error("❌ Données illisibles.".into())); }
											} else { let _ = tx.blocking_send(AppMessage::Error("❌ Aucun QR détecté.".into())); }
										}
									}
								});

								#[cfg(target_os = "android")]
								{
									if let Some(app) = ANDROID_APP.lock().unwrap().clone() {
										let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM).unwrap() };
										let mut env = vm.attach_current_thread().unwrap();
										let activity = unsafe { jni::objects::JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
										let _ = env.call_method(activity, "openGallery", "()V", &[]);
									}
								}
							}
						});
						
						ui.add_space(10.0);

						ui.vertical(|ui| {
							ui.label(format!("Montant ({}) :", self.transfer_asset));
							
							// ON CAPTURE LA FRAPPE POUR ESTIMER LE POIDS
							let response = ui.add(egui::TextEdit::singleline(&mut self.amount_input).hint_text("0.0"));
							
							if response.changed() && self.transfer_asset == "WATT" {
								if let (Some(keys), Ok(amt)) = (&self.wallet_keys, self.amount_input.parse::<f64>()) {
									let tx = self.tx.clone();
									let keys = keys.clone();
									let from_l2 = self.transfer_from_l2;
									tokio::spawn(async move {
										// ON LUI PASSE LA VRAIE ADRESSE (keys.watt_address)
										if let Ok((utxos, size)) = crate::estimate_tx_weight(amt, &keys.kyber_secret_hex, &keys.watt_address, from_l2, false).await {
											let _ = tx.send(AppMessage::TxWeightEstimated(utxos, size)).await;
										}
									});
								} else {
									self.tx_weight_estimate = None;
								}
							}
							
							// Conversion USD en direct !
							if let Ok(amt) = self.amount_input.parse::<f64>() {
								let price = if self.transfer_asset == "WATT" { self.watt_price_usd } else { self.btc_price_usd };
								if price > 0.0 {
									ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", amt * price)).color(egui::Color32::GRAY));
								}
							}
						});
						
						// L'AFFICHAGE DU POIDS ESTIMÉ
						if let Some((utxos, size_mb)) = self.tx_weight_estimate {
						    ui.add_space(10.0);
						    let color = if size_mb > 16.0 { egui::Color32::RED } else if size_mb > 8.0 { egui::Color32::from_rgb(255, 165, 0) } else { egui::Color32::GREEN };
						    egui::Frame::none().fill(item_bg).inner_margin(8.0).rounding(4.0).show(ui, |ui| {
						        ui.label(egui::RichText::new(format!("⚖ Poids estimé : {:.2} Mo ({} UTXOs utilisés)", size_mb, utxos)).color(color));
						        if size_mb > 16.0 {
						            ui.label(egui::RichText::new("⚠ Transaction trop lourde ! Réduisez le montant pour utiliser moins d'UTXOs.").size(10.0).color(egui::Color32::RED));
						        }
						    });
						}

						ui.add_space(15.0);

						// Options spécifiques au WATT
						if self.transfer_asset == "WATT" {
							ui.checkbox(&mut self.transfer_from_l2, "Dépenser depuis mon solde L2");
							ui.checkbox(&mut self.transfer_to_l2, "Envoyer vers le réseau L2 du destinataire");
						}
						
						ui.add_space(20.0);

						if ui.add_sized([300.0, 40.0], egui::Button::new("🔥 Signer et Envoyer")).clicked() {
							// 1. VRAIE REMONTÉE D'ERREURS DÉTAILLÉE
							if self.wallet_keys.is_none() {
								self.sync_message = "❌ Erreur : Portefeuille verrouillé.".to_string();
							} else if self.recipient_input.trim().is_empty() {
								self.sync_message = "❌ Erreur : L'adresse de destination est vide.".to_string();
							} else if self.amount_input.trim().is_empty() {
								self.sync_message = "❌ Erreur : Le montant est vide.".to_string();
							} else if let Err(_) = self.amount_input.trim().replace(",", ".").parse::<f64>() {
								// Le .trim() nettoie les espaces invisibles, le replace() gère les virgules accidentelles
								self.sync_message = "❌ Erreur : Format du montant invalide (ex: 10.5).".to_string();
							} else {
								// 2. TOUT EST BON, ON RÉCUPÈRE LES VARIABLES PROPRES
								let amount = self.amount_input.trim().replace(",", ".").parse::<f64>().unwrap();
								let keys = self.wallet_keys.as_ref().unwrap().clone();
								let recipient = self.recipient_input.trim().to_string();
								let tx = self.tx.clone();
								
								if self.transfer_asset == "WATT" {
									self.sync_message = "Création de la transaction quantique...".to_string();
									let from_l2 = self.transfer_from_l2;
									let to_l2 = self.transfer_to_l2;

									tokio::spawn(async move {
										let mut final_recipient = recipient.clone();

										// LA MAGIE WNS OPSEC : Traduction automatique de l'alias en RAM !
										if final_recipient.ends_with(".watt") {
											let _ = tx.send(AppMessage::Info("🔍 Recherche locale (OpSec) dans l'annuaire...".to_string())).await;
											match crate::resolve_wns_domain_opsec(&final_recipient).await {
												Ok(pubkey) => {
													final_recipient = pubkey; // On remplace le nom par la vraie clé Kyber
												},
												Err(e) => {
													let _ = tx.send(AppMessage::Error(e)).await;
													return; // On annule l'envoi
												}
											}
										}

										// On envoie à la vraie adresse finale
										match crate::send_wattcoin(final_recipient, amount, keys.kyber_secret_hex, keys.watt_address, keys.master_seed_hex, None, None, from_l2, to_l2).await {
											Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; },
											Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
										}
									});
								} else if self.transfer_asset == "BTC" {
									self.sync_message = "Création de la transaction Bitcoin via Tor...".to_string();
									
									tokio::spawn(async move {
										match crate::send_btc_direct(recipient, amount, keys.master_seed_hex).await {
											Ok(msg) => {
												let _ = tx.send(AppMessage::Info(msg)).await;
											},
											Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
										}
									});
								}
							}
						}

						// Affichage des statuts d'envoi
						if !self.sync_message.is_empty() {
							ui.add_space(15.0);
							let color = if self.sync_message.starts_with("✅") { egui::Color32::GREEN } else { egui::Color32::RED };
							ui.colored_label(color, &self.sync_message);
						}
					});
				}
				
				AppView::History => {
					overlay_ui.add_space(rect.height() * 0.10);
					egui::Frame::none().fill(panel_bg).inner_margin(20.0).rounding(12.0).show(&mut overlay_ui, |ui| {
						ui.horizontal(|ui| {
							ui.heading("📜 Historique des Transactions");
							ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
								if ui.button("🔄 Rafraîchir").clicked() {
									self.refresh_history(ui.ctx().clone());
								}
							});
						});
						ui.add_space(15.0);

						// Les fameux sous-onglets !
						ui.horizontal(|ui| {
							if ui.selectable_value(&mut self.history_tab, "L1".to_string(), "⚡ Réseau L1").clicked() {}
							if ui.selectable_value(&mut self.history_tab, "L2".to_string(), "🌐 Réseau L2").clicked() {}
						});
						ui.separator();
						ui.add_space(10.0);

						// 1. Petit indicateur discret si ça charge en arrière-plan
						if self.is_loading_history {
							ui.horizontal(|ui| {
								ui.spinner();
								ui.label(egui::RichText::new(" Recherche des nouvelles transactions...").color(egui::Color32::from_rgb(0, 240, 255)));
							});
							ui.add_space(10.0);
						}

						// 2. Zone de défilement (Scroll) TOUJOURS VISIBLE !
						egui::ScrollArea::vertical().max_height(450.0).show(ui, |ui| {
							let filtered_history: Vec<_> = self.history_items.iter().filter(|item| item.layer == self.history_tab).collect();
							
							if filtered_history.is_empty() {
								ui.vertical_centered(|ui| {
									ui.add_space(30.0);
									// On adapte le texte si c'est le tout premier chargement
									if self.is_loading_history {
										ui.label(egui::RichText::new("Exploration de la blockchain en cours...").color(egui::Color32::GRAY));
									} else {
										ui.label(egui::RichText::new("Aucune transaction trouvée sur ce réseau.").color(egui::Color32::GRAY));
									}
								});
							} else {
								for item in filtered_history {
									egui::Frame::none().fill(item_bg).inner_margin(12.0).rounding(8.0).show(ui, |ui| {
										ui.horizontal(|ui| {
											ui.vertical(|ui| {
												let status_color = if item.status.contains("Disponible") { egui::Color32::GREEN } else { egui::Color32::GRAY };
												ui.label(egui::RichText::new(&item.status).strong().color(status_color));
												ui.label(egui::RichText::new(&item.date).size(12.0).color(egui::Color32::DARK_GRAY));
												ui.label(egui::RichText::new(format!("Source: {}", item.id)).size(10.0).color(egui::Color32::GRAY));
											});
											ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
												ui.label(egui::RichText::new(format!("{:.9} {}", item.amount, item.coin)).strong().size(18.0).color(text_color));
											});
										});
									});
									ui.add_space(5.0);
								}
							}
						});
					});
				}
				
				AppView::Dex => {
					overlay_ui.add_space(rect.height() * 0.05);
					egui::Frame::none().fill(panel_bg).inner_margin(20.0).rounding(12.0).show(&mut overlay_ui, |ui| {
						ui.horizontal(|ui| {
							ui.heading("⚡ DEX : Échange (WATT ↔ BTC)");
							ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
								if ui.button("🔄 Rafraîchir").clicked() {
									self.refresh_dex(ui.ctx().clone());
								}
							});
						});
						ui.separator();
						
						egui::ScrollArea::vertical().max_height(600.0).show(ui, |ui| {
							// ==========================================
							// 1. PLACER UN ORDRE
							// ==========================================
							ui.add_space(10.0);
							ui.label(egui::RichText::new("📝 Placer un Ordre").strong().color(egui::Color32::from_rgb(0, 240, 255)));
							ui.add_space(5.0);
							
							ui.horizontal_wrapped(|ui| {
								ui.selectable_value(&mut self.dex_order_type, "buy".to_string(), "₿ ACHETER WATT (Payer BTC)");
								ui.selectable_value(&mut self.dex_order_type, "sell".to_string(), "⚡ VENDRE WATT (Recevoir BTC)");
							});
							ui.add_space(10.0);
							
							ui.vertical(|ui| {
								ui.vertical(|ui| {
									ui.label("Montant (WATT) :");
									ui.add(egui::TextEdit::singleline(&mut self.dex_amount_input).desired_width(200.0));
									
									if let Ok(amt) = self.dex_amount_input.parse::<f64>() {
										if self.watt_price_usd > 0.0 {
											ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", amt * self.watt_price_usd)).color(egui::Color32::GRAY));
										}
									}
								});
								ui.add_space(5.0);
								ui.horizontal(|ui| {
									ui.label("Prix (Sats/WATT) :");
									ui.add(egui::TextEdit::singleline(&mut self.dex_price_input).desired_width(200.0));
								});
							});
							ui.add_space(10.0);

							// APRÈS (Bouton grisé pendant le traitement)
							let is_submitting_order = self.sync_message == "Envoi de l'ordre...";

							if ui.add_enabled(!is_submitting_order, egui::Button::new("🚀 Soumettre l'ordre au réseau")).clicked() {
								if let (Some(keys), Ok(amt), Ok(price)) = (&self.wallet_keys, self.dex_amount_input.parse::<f64>(), self.dex_price_input.parse::<f64>()) {
									
									let o_type = self.dex_order_type.clone();
									let mut can_proceed = true;

									// VÉRIFICATION DES SOLDES AVANT ENVOI
									if o_type == "sell" {
										let total_watt_needed = amt;
										if (self.balance_l1 + self.balance_l2) < total_watt_needed {
											self.sync_message = format!("❌ Solde WATT insuffisant. Requis : {:.3} WATT", total_watt_needed);
											can_proceed = false;
										}
									}

									if can_proceed {
										self.sync_message = "Envoi de l'ordre...".to_string(); // Ça grise le bouton immédiatement !
                                        let tx = self.tx.clone();
                                        let keys = keys.clone();
                                        
                                        // GESTION DU SECRET : Si on achète, on doit générer le secret HTLC maintenant !
                                        let mut hash_to_send = None;
                                        if o_type == "buy" {
                                            let mut secret = [0u8; 32];
                                            rand::thread_rng().fill_bytes(&mut secret);
                                            use sha2::Digest;
                                            let htlc_hash_btc = hex::encode(sha2::Sha256::digest(&secret)); // Compatible BTC
                                            
                                            // On sauvegarde le secret localement pour le claim plus tard !
                                            self.swap_secrets.insert(htlc_hash_btc.clone(), hex::encode(secret));
                                            
                                            // SAUVEGARDE SUR DISQUE POUR SURVIVRE AUX REDÉMARRAGES
											if let Ok(json) = serde_json::to_string(&self.swap_secrets) {
												if let Ok(path) = crate::get_swap_secrets_path() {
													let _ = std::fs::write(path, json);
												}
											}
                                            
                                            hash_to_send = Some(htlc_hash_btc);
                                        }

                                        tokio::spawn(async move {
                                            match crate::submit_order(o_type, amt, price, keys.btc_address, keys.btc_pubkey_hex, keys.watt_address, hash_to_send).await {
                                                Ok(_) => { let _ = tx.send(AppMessage::Info("✅ Ordre placé dans le Dark Pool !".to_string())).await; },
                                                Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
                                            }
                                        });
                                    }
                                } else {
                                    self.sync_message = "❌ Champs invalides ou montants incorrects.".to_string();
                                }
                            }

							ui.add_space(20.0);
							ui.separator();

							// ==========================================
							// 2. MES CONTRATS ATOMIQUES ACTIFS
							// ==========================================
							ui.add_space(10.0);
							ui.label(egui::RichText::new("🔐 Mes Swaps Atomiques en cours").strong().color(egui::Color32::from_rgb(247, 147, 26)));
							ui.add_space(5.0);

							if self.active_swaps.is_empty() {
								ui.label(egui::RichText::new("Aucun swap en cours.").color(egui::Color32::GRAY));
							} else {
								for swap in &self.active_swaps {
									egui::Frame::none().fill(item_bg).inner_margin(10.0).rounding(5.0).show(ui, |ui| {
										let keys = self.wallet_keys.clone().unwrap();
										let is_buyer = keys.watt_address == swap.buyer_watt_address;
										
										let role = if is_buyer { "🔴 ACHETEUR (Vous payez BTC, recevez WATT)" } else { "🔴 VENDEUR (Vous payez WATT, recevez BTC)" };
										ui.label(egui::RichText::new(role).strong());
										let watt_amt = swap.watt_amount_flames as f64 / 1_000_000_000.0;
										let btc_amt = swap.btc_amount_sats as f64 / 100_000_000.0;
										let watt_usd = watt_amt * self.watt_price_usd;
										let btc_usd = btc_amt * self.btc_price_usd;

										ui.horizontal(|ui| {
											ui.label(format!("Montant : {} WATT", watt_amt));
											ui.label(egui::RichText::new(format!("(≈ $ {:.2})", watt_usd)).color(egui::Color32::GRAY));
											ui.label("↔");
											ui.label(format!("{} Sats", swap.btc_amount_sats));
											ui.label(egui::RichText::new(format!("(≈ $ {:.2})", btc_usd)).color(egui::Color32::GRAY));
										});
										ui.label(egui::RichText::new(format!("Hash HTLC : {}...", &swap.htlc_hash[0..15])).size(10.0).color(egui::Color32::GRAY));
										
										ui.add_space(5.0);
										ui.vertical(|ui| {
											if is_buyer {
												// ACTIONS ACHETEUR
												if ui.button("1. Verrouiller mes BTC").clicked() {
													self.sync_message = "Envoi des BTC vers le contrat HTLC...".to_string();
													let tx = self.tx.clone();
													let htlc_address = swap.htlc_hash.clone(); // Le noeud gèrera l'adresse finale via le hash
													let amount_btc = swap.btc_amount_sats as f64 / 100_000_000.0;
													
													tokio::spawn(async move {
														match crate::send_btc_to_htlc(htlc_address, amount_btc, None).await {
															Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; },
															Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
														}
													});
												}
												ui.add_space(5.0);
												ui.label(egui::RichText::new("👁 Watchtower en attente du dépôt WATT...").color(egui::Color32::GRAY).italics());
											} else {
												// ACTIONS VENDEUR
												if ui.button("1. Verrouiller mes WATT").clicked() {
													self.sync_message = "Création du HTLC Quantique en cours...".to_string();
													let tx = self.tx.clone();
													let keys_clone = keys.clone();
													let amount_watt = swap.watt_amount_flames as f64 / 1_000_000_000.0;
													let recipient = swap.buyer_watt_address.clone();
													let htlc_hash = swap.htlc_hash.clone();
													
													tokio::spawn(async move {
														match crate::send_wattcoin(recipient, amount_watt, keys_clone.kyber_secret_hex, keys_clone.watt_address, keys_clone.master_seed_hex, Some(htlc_hash), Some(999_999), false, false).await {
															Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; },
															Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
														}
													});
												}
												
												// BOUTON REFUND (si l'acheteur ne paie jamais)
												if ui.button("🔙 Remboursement WATT").on_hover_text("Si le délai HTLC est dépassé").clicked() {
													self.sync_message = "Demande de remboursement WATT...".to_string();
													let tx = self.tx.clone();
													let hash = swap.htlc_hash.clone();
													let addr = keys.watt_address.clone();
													let amt = swap.watt_amount_flames as f64 / 1_000_000_000.0;
													
													tokio::spawn(async move {
														match crate::refund_wattcoin_swap(hash, addr, amt).await {
															Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; },
															Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
														}
													});
												}
												ui.add_space(5.0);
												ui.label(egui::RichText::new("👁 Attente du secret acheteur...").color(egui::Color32::GRAY).italics());
											}
										});
									});
									ui.add_space(5.0);
								}
							}

							ui.add_space(20.0);
							ui.separator();

							// ==========================================
							// 3. LE CARNET D'ORDRES (DARK POOL)
							// ==========================================
							ui.add_space(20.0);
							ui.separator();
							ui.add_space(10.0);
							ui.label(egui::RichText::new("📖 Carnet d'Ordres Public").strong());
							ui.add_space(5.0);

							if self.dark_pool.is_empty() {
								ui.label(egui::RichText::new("Le carnet d'ordres est vide.").color(egui::Color32::GRAY));
							} else {
								for order in &self.dark_pool {
									let color = if order.order_type == "buy" { egui::Color32::GREEN } else { egui::Color32::RED };
									ui.horizontal(|ui| {
										ui.label(egui::RichText::new(&order.order_type.to_uppercase()).color(color).strong());
										let watt_amt = order.amount_flames as f64 / 1_000_000_000.0;
										let watt_usd = watt_amt * self.watt_price_usd;
										ui.label(format!("{:.3} WATT", watt_amt));
										ui.label(egui::RichText::new(format!("(≈ $ {:.2})", watt_usd)).color(egui::Color32::GRAY));
										ui.label(format!("@ {} Sats", order.price_sats));
										
										// Bouton Annuler si c'est NOTRE ordre
										if let Some(keys) = &self.wallet_keys {
											if keys.watt_address == order.watt_address {
												if ui.button("🗑").on_hover_text("Annuler mon ordre").clicked() {
													self.sync_message = "Annulation de l'ordre...".to_string();
													let tx = self.tx.clone();
													let order_id = order.id.clone();
													
													tokio::spawn(async move {
														match crate::cancel_order(order_id).await {
															Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; },
															Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
														}
													});
												}
											}
										}
									});
								}
							}
						});
						
						// Affichage des messages
						if !self.sync_message.is_empty() {
							ui.add_space(10.0);
							let color = if self.sync_message.starts_with("✅") { egui::Color32::GREEN } else { egui::Color32::RED };
							ui.colored_label(color, &self.sync_message);
						}
					});
				}
				
				AppView::Lottery => {
					overlay_ui.add_space(rect.height() * 0.15);
					egui::Frame::none().fill(panel_bg).inner_margin(25.0).rounding(12.0).show(&mut overlay_ui, |ui| {
						ui.heading("🎰 Loterie Cypherpunk");
						ui.add_space(15.0);

						// LOGIQUE DU PRIX DYNAMIQUE
						// Si le WATT a un prix (via le DEX), on calcule l'équivalent de 0.10 $ USD.
						// S'il n'y a pas encore eu d'échange (0.0), on force un prix fixe de 10 WATT.
						let ticket_price_watt = if self.watt_price_usd > 0.0000001 {
							0.10 / self.watt_price_usd
						} else {
							10.0 
						};

						ui.vertical_centered(|ui| {
							ui.label(egui::RichText::new("JACKPOT ACTUEL").color(egui::Color32::GRAY).size(14.0));
							ui.heading(egui::RichText::new(format!("{:.9} WATT", self.jackpot as f64 / 1_000_000_000.0)).size(32.0).color(egui::Color32::GOLD));
							
							let jackpot_usd = (self.jackpot as f64 / 1_000_000_000.0) * self.watt_price_usd;
							ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", jackpot_usd)).color(egui::Color32::GRAY));
							
							ui.add_space(15.0);
    
							// CALCUL DU COMPTE À REBOURS
							let blocks_remaining = 10 - (self.current_block_height % 10);
							let minutes_remaining = blocks_remaining * 2; // ~2 min par bloc
							ui.label(egui::RichText::new(format!("⏳ Prochain tirage dans : {} blocs (~{} min)", blocks_remaining, minutes_remaining))
								.color(egui::Color32::from_rgb(0, 240, 255)));
							
							ui.add_space(20.0);
							ui.separator();
							ui.add_space(20.0);

							ui.label(egui::RichText::new("Prix d'un ticket (Fixé à 0.10$) :").color(text_muted));
							ui.heading(egui::RichText::new(format!("{:.9} WATT", ticket_price_watt)).size(24.0).color(egui::Color32::from_rgb(0, 240, 255)));

							ui.add_space(30.0);

							if ui.add_sized([250.0, 50.0], egui::Button::new("🎟 Acheter un Ticket")).clicked() {
								if let Some(keys) = &self.wallet_keys {
									let ticket_price_flames = (ticket_price_watt * 1_000_000_000.0) as u64;
									
									// Vérification préalable du solde L1 de l'utilisateur
									if self.balance_l1 < ticket_price_watt {
										self.sync_message = format!("❌ Solde L1 insuffisant. Requis : {:.3} WATT", ticket_price_watt);
									} else {
										self.sync_message = "Création du ticket quantique en cours...".to_string();
										let tx = self.tx.clone();
										let keys_clone = keys.clone();
										
										tokio::spawn(async move {
											match crate::buy_lottery_ticket(keys_clone.kyber_secret_hex, keys_clone.watt_address, keys_clone.master_seed_hex, ticket_price_flames).await {
												Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; },
												Err(e) => {
													let _ = tx.send(AppMessage::Error(e)).await;
												}
											}
										});
									}
								} else {
									self.sync_message = "❌ Portefeuille non déverrouillé.".to_string();
								}
							}
						});

						if !self.sync_message.is_empty() {
							ui.add_space(15.0);
							let color = if self.sync_message.starts_with("✅") { egui::Color32::GREEN } else { egui::Color32::RED };
							ui.vertical_centered(|ui| {
								ui.colored_label(color, &self.sync_message);
							});
						}
					});
				}
				
				AppView::Messages => {
					overlay_ui.add_space(rect.height() * 0.10);
					egui::Frame::none().fill(panel_bg).inner_margin(20.0).rounding(12.0).show(&mut overlay_ui, |ui| {
						ui.horizontal(|ui| {
							ui.heading("✉ Messages & Notaire");
							ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
								if ui.button("🔄 Rafraîchir").clicked() {
									self.refresh_data(ui.ctx().clone());
								}
							});
						});
						ui.add_space(15.0);

						ui.horizontal(|ui| {
							ui.selectable_value(&mut self.data_tab, "inbox".to_string(), "📥 Mes messages");
							ui.selectable_value(&mut self.data_tab, "send".to_string(), "📤 Envoyer Message");
							ui.selectable_value(&mut self.data_tab, "notary".to_string(), "📜 Notariser (POE)");
						});
						ui.separator();
						ui.add_space(15.0);

						if self.data_tab == "inbox" {
							if self.is_loading_data {
								ui.vertical_centered(|ui| { ui.label(egui::RichText::new("🔄 Déchiffrement des données...").color(egui::Color32::from_rgb(0, 240, 255))); });
							} else {
								egui::ScrollArea::vertical().max_height(450.0).show(ui, |ui| {
									if self.data_items.is_empty() {
										ui.label(egui::RichText::new("Aucun message ou document trouvé.").color(egui::Color32::GRAY));
									} else {
										for item in &self.data_items {
											egui::Frame::none().fill(item_bg).inner_margin(12.0).rounding(8.0).show(ui, |ui| {
												ui.horizontal(|ui| {
													if item.data_type == "MSG" {
														ui.label(egui::RichText::new("💬 MESSAGE").strong().color(egui::Color32::from_rgb(0, 240, 255)));
													} else {
														ui.label(egui::RichText::new("📜 CERTIFICAT (POE)").strong().color(egui::Color32::GOLD));
													}
													ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
														ui.label(egui::RichText::new(&item.date).size(12.0).color(egui::Color32::DARK_GRAY));
													});
												});
												ui.add_space(5.0);
												ui.label(egui::RichText::new(&item.content).color(text_color));
												ui.add_space(5.0);
												ui.label(egui::RichText::new(format!("Ancré sur {} | Réf: {}", item.layer, item.id)).size(10.0).color(egui::Color32::GRAY));
											});
											ui.add_space(5.0);
										}
									}
								});
							}
						} 
						else if self.data_tab == "send" {
							ui.label("Adresse Kyber du destinataire :");
							let response = ui.add(egui::TextEdit::singleline(&mut self.msg_recipient).hint_text("pq_watt_... ou L2_WATT_..."));

							response.context_menu(|ui| {
								if ui.button("📋 Coller").clicked() {
									if let Some(pasted_text) = get_clipboard_text() {
										self.msg_recipient = pasted_text;
									}
									ui.close_menu();
								}
								if ui.button("📋 Copier").clicked() {
									set_clipboard_text(ui.ctx(), &self.msg_recipient);
									ui.close_menu();
								}
							});
							ui.add_space(10.0);
							
							ui.label("Message secret :");
							ui.add_sized([ui.available_width(), 100.0], egui::TextEdit::multiline(&mut self.msg_content));
							ui.add_space(10.0);
							
							ui.checkbox(&mut self.msg_use_l2, "Envoyer sur le réseau L2 (Plus rapide et moins cher)");
							ui.add_space(15.0);

							if ui.button("🚀 Chiffrer et Envoyer").clicked() {
								if let Some(keys) = &self.wallet_keys {
									if self.msg_recipient.is_empty() || self.msg_content.is_empty() {
										self.sync_message = "❌ Veuillez remplir tous les champs.".to_string();
									} else {
										self.sync_message = "Chiffrement et envoi en cours...".to_string();
										let tx = self.tx.clone();
										let keys = keys.clone();
										let recipient = self.msg_recipient.clone();
										let content = self.msg_content.clone();
										let use_l2 = self.msg_use_l2;

										tokio::spawn(async move {
											let mut final_recipient = recipient.clone();

											// MAGIE WNS OPSEC : Résolution locale pour les messages !
											if final_recipient.ends_with(".watt") {
												let _ = tx.send(AppMessage::Info("🔍 Recherche locale (OpSec)...".to_string())).await;
												match crate::resolve_wns_domain_opsec(&final_recipient).await {
													Ok(pubkey) => {
														final_recipient = pubkey; // On remplace par la clé Kyber !
													},
													Err(e) => {
														let _ = tx.send(AppMessage::Error(e)).await;
														return;
													}
												}
											}

											match crate::send_data(final_recipient, keys.kyber_secret_hex, keys.watt_address, keys.master_seed_hex, "MSG".to_string(), content, use_l2).await {
												Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; }
												Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
											}
										});
									}
								}
							}
						} 
						else if self.data_tab == "notary" {
							ui.label(egui::RichText::new("Ancrez l'empreinte mathématique d'un document sur la blockchain pour prouver son existence à cette date précise, sans jamais révéler son contenu.").color(egui::Color32::GRAY));
							ui.add_space(15.0);

							if ui.button("📂 Sélectionner un document (PDF, Image, etc.)").clicked() {
								let tx = self.tx.clone();
								
								#[cfg(not(target_os = "android"))]
								tokio::task::spawn_blocking(move || {
									if let Some(path) = rfd::FileDialog::new().pick_file() {
										let filename = path.file_name().unwrap_or_default().to_string_lossy().to_string();
										if let Ok(bytes) = std::fs::read(&path) {
											use sha2::{Sha256, Digest};
											let hash = hex::encode(Sha256::digest(&bytes));
											let _ = tx.blocking_send(AppMessage::FileHashed(filename, hash));
										} else {
											let _ = tx.blocking_send(AppMessage::Error("❌ Impossible de lire le fichier.".to_string()));
										}
									}
								});

								#[cfg(target_os = "android")]
								let _ = tx.try_send(AppMessage::Error("❌ Explorateur de fichiers non dispo sur mobile.".into()));
							}

							ui.add_space(10.0);
							
							if !self.notary_hash.is_empty() {
								egui::Frame::none().fill(item_bg).inner_margin(10.0).rounding(5.0).show(ui, |ui| {
									ui.label(egui::RichText::new(format!("Fichier : {}", self.notary_filename)).color(text_muted));
									ui.label(egui::RichText::new(format!("Hash (SHA-256) : {}", self.notary_hash)).monospace().color(egui::Color32::GOLD));
								});
								
								ui.add_space(15.0);
								ui.checkbox(&mut self.notary_use_l2, "Ancrer sur le réseau L2 (Plus rapide)");
								ui.add_space(10.0);

								if ui.add_sized([300.0, 40.0], egui::Button::new("⚖ Graver dans le Marbre Numérique")).clicked() {
									if let Some(keys) = &self.wallet_keys {
										self.sync_message = "Notarisation en cours...".to_string();
										let tx = self.tx.clone();
										let keys = keys.clone();
										let hash_content = self.notary_hash.clone();
										let use_l2 = self.notary_use_l2;
										
										// 💡 On s'envoie la preuve à nous-mêmes pour la retrouver dans notre propre historique !
										let recipient = keys.watt_address.clone();

										tokio::spawn(async move {
											match crate::send_data(recipient, keys.kyber_secret_hex, keys.watt_address, keys.master_seed_hex, "POE".to_string(), hash_content, use_l2).await {
												Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; }
												Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
											}
										});
									}
								}
							}
						}

						if !self.sync_message.is_empty() {
							ui.add_space(15.0);
							let color = if self.sync_message.starts_with("✅") { egui::Color32::GREEN } else { egui::Color32::RED };
							ui.colored_label(color, &self.sync_message);
						}
					});
				}
				
				AppView::L2Manager => {
					overlay_ui.add_space(rect.height() * 0.10);
					egui::Frame::none().fill(panel_bg).inner_margin(20.0).rounding(12.0).show(&mut overlay_ui, |ui| {
						ui.heading("🌐 Séquenceurs & Interopérabilité L2");
						ui.add_space(10.0);
						ui.label(egui::RichText::new("Verrouillez une caution en WATT sur le L1 pour gagner le droit d'ancrer les blocs de votre propre blockchain (L2). Le moteur VRF du L1 choisira aléatoirement les séquenceurs actifs à chaque tour.").color(egui::Color32::GRAY));
						ui.add_space(20.0);

						ui.label("Nom de la Blockchain L2 ciblée (ex: mon_rollup) :");
						ui.add(egui::TextEdit::singleline(&mut self.l2_target_name));
						ui.add_space(10.0);

						// 🔍 VÉRIFIER LE STATUT L2
						ui.horizontal(|ui| {
							if ui.button("🔍 Interroger le Tribunal VRF du L1").clicked() {
								if self.l2_target_name.is_empty() {
									self.sync_message = "❌ Veuillez entrer un nom de L2.".to_string();
								} else {
									self.sync_message = "Vérification du statut VRF...".to_string();
									let tx = self.tx.clone();
									let l2_name = self.l2_target_name.clone();
									tokio::spawn(async move {
										match crate::get_l2_status(&l2_name).await {
											Ok(json) => {
												let pretty = serde_json::to_string_pretty(&json).unwrap_or_default();
												let _ = tx.send(AppMessage::L2StatusFetched(pretty)).await;
											}
											Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
										}
									});
								}
							}
						});

						if !self.l2_status_result.is_empty() {
							ui.add_space(10.0);
							egui::Frame::none().fill(item_bg).inner_margin(10.0).rounding(5.0).show(ui, |ui| {
								ui.label(egui::RichText::new(&self.l2_status_result).monospace().color(egui::Color32::GOLD));
							});
						}

						ui.add_space(20.0);
						ui.separator();
						ui.add_space(20.0);

						// GÉRER SA CAUTION
						ui.horizontal(|ui| {
							ui.vertical(|ui| {
								ui.label("Montant de la caution à verrouiller (WATT) :");
								ui.add(egui::TextEdit::singleline(&mut self.l2_stake_amount).hint_text("ex: 1000.0"));
								
								if let Ok(amt) = self.l2_stake_amount.parse::<f64>() {
									if self.watt_price_usd > 0.0 {
										ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", amt * self.watt_price_usd)).color(egui::Color32::GRAY));
									}
								}
							});
						});
						ui.add_space(10.0);

						ui.label("Clé Publique de votre Séquenceur (WOTS+) :");
						let response = ui.add(egui::TextEdit::singleline(&mut self.l2_sequencer_pubkey).hint_text("La clé publique du serveur qui publiera vos blocs L2"));

						response.context_menu(|ui| {
							if ui.button("📋 Coller").clicked() {
								if let Some(pasted_text) = get_clipboard_text() {
									self.l2_sequencer_pubkey = pasted_text;
								}
								ui.close_menu();
							}
							if ui.button("📋 Copier").clicked() {
								set_clipboard_text(ui.ctx(), &self.l2_sequencer_pubkey);
								ui.close_menu();
							}
						});
						ui.add_space(20.0);

						ui.horizontal(|ui| {
							if ui.add_sized([180.0, 40.0], egui::Button::new("🔒 Staker & Rejoindre L2")).clicked() {
								if let (Some(keys), Ok(amt)) = (&self.wallet_keys, self.l2_stake_amount.parse::<f64>()) {
									if self.l2_target_name.is_empty() || self.l2_sequencer_pubkey.is_empty() {
										self.sync_message = "❌ Tous les champs sont requis.".to_string();
									} else {
										self.sync_message = "Verrouillage de la caution en cours...".to_string();
										let tx = self.tx.clone();
										let keys = keys.clone();
										let l2_name = self.l2_target_name.clone();
										let sequencer_pubkey = self.l2_sequencer_pubkey.clone();

										tokio::spawn(async move {
											match crate::stake_l2(l2_name, amt, keys.kyber_secret_hex, keys.watt_address, sequencer_pubkey, keys.master_seed_hex.clone()).await {
												Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; }
												Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
											}
										});
									}
								} else {
									self.sync_message = "❌ Montant invalide ou portefeuille verrouillé.".to_string();
								}
							}

							ui.add_space(10.0);

							if ui.add_sized([180.0, 40.0], egui::Button::new("🔓 Retirer (Unstake)")).clicked() {
								if let Some(keys) = &self.wallet_keys {
									if self.l2_target_name.is_empty() {
										self.sync_message = "❌ Le nom de la L2 est requis pour l'unstake.".to_string();
									} else {
										self.sync_message = "Récupération de la caution en cours...".to_string();
										let tx = self.tx.clone();
										let keys = keys.clone();
										let l2_name = self.l2_target_name.clone();

										tokio::spawn(async move {
											match crate::unstake_l2(l2_name, keys.kyber_secret_hex, keys.watt_address, keys.master_seed_hex.clone()).await {
												Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; }
												Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
											}
										});
									}
								} else {
									self.sync_message = "❌ Portefeuille verrouillé.".to_string();
								}
							}
						});

						if !self.sync_message.is_empty() {
							ui.add_space(15.0);
							let color = if self.sync_message.starts_with("✅") || self.sync_message.starts_with("🎉") || self.sync_message.starts_with("🔓") { egui::Color32::GREEN } else { egui::Color32::RED };
							ui.colored_label(color, &self.sync_message);
						}
					});
				}
				
				AppView::Bridge => {
					overlay_ui.add_space(rect.height() * 0.15);
					egui::Frame::none().fill(panel_bg).inner_margin(25.0).rounding(12.0).show(&mut overlay_ui, |ui| {
						ui.heading("🌉 Bridge : Transférer vers un L2");
						ui.add_space(10.0);
						ui.label(egui::RichText::new("Verrouillez vos WATT sur le réseau principal (L1) de manière transparente. Ils seront crédités sur la blockchain secondaire (L2) de votre choix.").color(egui::Color32::GRAY));
						ui.add_space(20.0);

						ui.label("Nom du réseau L2 de destination (ex: WNS) :");
						ui.add(egui::TextEdit::singleline(&mut self.bridge_l2_name));
						ui.add_space(10.0);

						ui.label("Clé publique du destinataire sur le L2 :");
						let response = ui.add(egui::TextEdit::singleline(&mut self.bridge_receiver_pubkey).hint_text("Clé Lattice ou adresse Kyber"));

						response.context_menu(|ui| {
							if ui.button("📋 Coller").clicked() {
								if let Some(pasted_text) = get_clipboard_text() {
									self.bridge_receiver_pubkey = pasted_text;
								}
								ui.close_menu();
							}
							if ui.button("📋 Copier").clicked() {
								set_clipboard_text(ui.ctx(), &self.bridge_receiver_pubkey);
								ui.close_menu();
							}
						});
						ui.add_space(10.0);

						ui.horizontal(|ui| {
							ui.vertical(|ui| {
								ui.label("Montant à transférer (WATT) :");
								ui.add(egui::TextEdit::singleline(&mut self.bridge_amount).hint_text("ex: 50.0"));
								
								if let Ok(amt) = self.bridge_amount.parse::<f64>() {
									if self.watt_price_usd > 0.0 {
										ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", amt * self.watt_price_usd)).color(egui::Color32::GRAY));
									}
								}
							});
						});
						ui.add_space(20.0);

						if ui.add_sized([250.0, 40.0], egui::Button::new("🔒 Verrouiller et Transférer")).clicked() {
							if let (Some(keys), Ok(amt)) = (&self.wallet_keys, self.bridge_amount.parse::<f64>()) {
								if self.bridge_l2_name.is_empty() || self.bridge_receiver_pubkey.is_empty() {
									self.sync_message = "❌ Tous les champs sont requis.".to_string();
								} else {
									self.sync_message = "Verrouillage des fonds pour le L2...".to_string();
									let tx = self.tx.clone();
									let keys = keys.clone();
									let l2_name = self.bridge_l2_name.clone();
									let receiver = self.bridge_receiver_pubkey.clone();

									tokio::spawn(async move {
										match crate::bridge_to_l2(l2_name, receiver, amt, keys.kyber_secret_hex, keys.watt_address, keys.master_seed_hex).await {
											Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; }
											Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
										}
									});
								}
							} else {
								self.sync_message = "❌ Montant invalide ou portefeuille verrouillé.".to_string();
							}
						}

						if !self.sync_message.is_empty() {
							ui.add_space(15.0);
							let color = if self.sync_message.starts_with("✅") { egui::Color32::GREEN } else { egui::Color32::RED };
							ui.colored_label(color, &self.sync_message);
						}
					});
				}
				
				AppView::WnsManager => {
					overlay_ui.add_space(rect.height() * 0.10);
					egui::Frame::none().fill(panel_bg).inner_margin(20.0).rounding(12.0).show(&mut overlay_ui, |ui| {
						ui.heading("🏷 Registre des Noms (WNS)");
						ui.add_space(10.0);
						ui.label(egui::RichText::new("Achetez un domaine en .watt sur le réseau L2. Utilisez-le pour remplacer votre longue adresse de portefeuille, ou pour déclarer publiquement votre propre serveur de routage (Mixnet).").color(egui::Color32::GRAY));
						ui.add_space(20.0);

						ui.horizontal(|ui| {
							ui.selectable_value(&mut self.wns_tab, "wallet".to_string(), "👛 Alias de Portefeuille");
							ui.selectable_value(&mut self.wns_tab, "server".to_string(), "🖥 Serveur Relais (Mixnet)");
						});
						ui.separator();
						ui.add_space(15.0);

						ui.label("Nom de domaine souhaité (doit finir par .watt) :");
						// On capture l'action de l'utilisateur sur le champ
						let response = ui.add(egui::TextEdit::singleline(&mut self.wns_domain_input).hint_text("ex: watty.watt"));

						// Dès que le texte change, on lance une vérification
						if response.changed() {
							let domain = self.wns_domain_input.clone();
							let tx = self.tx.clone();
							
							// On récupère notre propre adresse pour voir si le domaine est à nous
							let my_address = self.wallet_keys.as_ref().map(|k| k.watt_address.clone()).unwrap_or_default();
							
							if domain.is_empty() {
								self.wns_domain_status.clear();
							// AJOUT DU FILTRE DE LONGUEUR (len <= 5)
							} else if !domain.ends_with(".watt") || domain.len() <= 5 { 
								self.wns_domain_status = "⚠ Entrez un nom avant .watt (ex: watty.watt)".to_string();
							} else {
								self.wns_domain_status = "🔍 Vérification...".to_string();
								
								tokio::spawn(async move {
									// Un petit délai de 400ms pour laisser l'utilisateur finir de taper (Debounce)
									tokio::time::sleep(std::time::Duration::from_millis(400)).await;
									
									// On fouille dans l'annuaire
									match crate::resolve_wns_domain_opsec(&domain).await {
										Ok(record) => {
											// Le domaine est pris. Est-ce qu'il pointe vers nous ?
											if record.contains(&my_address) {
												let _ = tx.send(AppMessage::DomainStatus("👤 Ce domaine vous appartient !".to_string())).await;
											} else {
												let _ = tx.send(AppMessage::DomainStatus("❌ Domaine déjà pris.".to_string())).await;
											}
										}
										Err(_) => {
											// Erreur du WNS = Le domaine n'existe pas dans le registre !
											let _ = tx.send(AppMessage::DomainStatus("✅ Domaine disponible !".to_string())).await;
										}
									}
								});
							}
						}

						// L'affichage dynamique du résultat sous le champ de texte
						if !self.wns_domain_status.is_empty() {
							ui.add_space(2.0);
							let color = if self.wns_domain_status.starts_with("✅") || self.wns_domain_status.starts_with("👤") {
								egui::Color32::GREEN
							} else if self.wns_domain_status.starts_with("⚠️") || self.wns_domain_status.starts_with("🔍") {
								egui::Color32::from_rgb(255, 165, 0) // Orange
							} else {
								egui::Color32::RED
							};
							ui.colored_label(color, &self.wns_domain_status);
						}
						ui.add_space(10.0);

						if self.wns_tab == "server" {
							ui.label("IP et Port de votre Nœud (Clearnet ou Tor) :");
							ui.add(egui::TextEdit::singleline(&mut self.wns_ip_input).hint_text("ex: 82.12.34.56:8000"));
							ui.add_space(10.0);
							
							ui.label("Clé Publique Kyber de votre Nœud (node_kyber.pub) :");
							let response = ui.add(egui::TextEdit::singleline(&mut self.wns_server_pubkey_input).hint_text("ex: 82c681..."));

							response.context_menu(|ui| {
								if ui.button("📋 Coller").clicked() {
									if let Some(pasted_text) = get_clipboard_text() {
										self.wns_server_pubkey_input = pasted_text;
									}
									ui.close_menu();
								}
								if ui.button("📋 Copier").clicked() {
									set_clipboard_text(ui.ctx(), &self.wns_server_pubkey_input);
									ui.close_menu();
								}
							});
							ui.add_space(10.0);
						}

						ui.horizontal(|ui| {
							ui.vertical(|ui| {
								ui.label("Enchère / Frais d'enregistrement (WATT sur L2) :");
								ui.add(egui::TextEdit::singleline(&mut self.wns_bid_amount).hint_text("Min: 0.0000015"));
								
								if let Ok(amt) = self.wns_bid_amount.parse::<f64>() {
									if self.watt_price_usd > 0.0 {
										// On utilise 6 décimales pour l'USD car le montant est très petit
										ui.label(egui::RichText::new(format!("≈ $ {:.8} USD", amt * self.watt_price_usd)).color(egui::Color32::GRAY));
									}
								}
							});
						});
						ui.add_space(20.0);

						if ui.add_sized([250.0, 40.0], egui::Button::new("🔥 Enregistrer le Domaine")).clicked() {
							if let (Some(keys), Ok(fee_watt)) = (&self.wallet_keys, self.wns_bid_amount.parse::<f64>()) {
								
								// AJOUT DU MÊME FILTRE ICI
								if !self.wns_domain_input.ends_with(".watt") || self.wns_domain_input.len() <= 5 {
									self.sync_message = "❌ Le nom de domaine est invalide (trop court).".to_string();
								} else {
									self.sync_message = "Création de la transaction WNS en cours...".to_string();
									let tx = self.tx.clone();
									let keys = keys.clone();
									let domain = self.wns_domain_input.clone();
									
									// Conversion silencieuse : On repasse les WATT en FLAME pour la blockchain (u64)
									let fee_flames = (fee_watt * 1_000_000_000.0) as u64;
									
									let record_data = if self.wns_tab == "wallet" {
										keys.watt_address.clone()
									} else {
										format!("{}|{}", self.wns_ip_input, self.wns_server_pubkey_input)
									};

									tokio::spawn(async move {
										// On envoie bien fee_flames au réseau
										match crate::register_wns_domain(domain, record_data, fee_flames, keys).await {
											Ok(msg) => { let _ = tx.send(AppMessage::Info(msg)).await; }
											Err(e) => { let _ = tx.send(AppMessage::Error(e)).await; }
										}
									});
								}
							} else {
								self.sync_message = "❌ Montant invalide ou portefeuille verrouillé.".to_string();
							}
						}

						if !self.sync_message.is_empty() {
							ui.add_space(15.0);
							let color = if self.sync_message.starts_with("✅") { egui::Color32::GREEN } else { egui::Color32::RED };
							ui.colored_label(color, &self.sync_message);
						}
					});
				}
                
                AppView::Settings => {
                    overlay_ui.add_space(rect.height() * 0.15);
                    egui::Frame::none().fill(panel_bg).inner_margin(20.0).rounding(10.0).show(&mut overlay_ui, |ui| {
                        ui.heading("⚙ Paramètres du Sanctuaire");
						ui.add_space(10.0);
						ui.label(egui::RichText::new(format!("Version : {}", crate::get_version()))
							.size(20.0)
							.color(egui::Color32::DARK_GRAY)
						);
						ui.separator();
                        ui.add_space(20.0);

                        if self.show_seed {
                            ui.label("Votre phrase secrète (48 mots) :");
                            ui.add_space(5.0);
                            
                            egui::Frame::none().fill(item_bg).inner_margin(15.0).rounding(5.0).show(ui, |ui| {
                                ui.label(egui::RichText::new(&self.decrypted_seed).monospace().color(text_color));
                            });
                            
                            ui.add_space(15.0);
                            if ui.add_sized([300.0, 40.0], egui::Button::new("🖼 Afficher le QR Code")).clicked() { self.show_seed_qr = true; }
                            ui.add_space(5.0);
                            
                            if ui.add_sized([300.0, 40.0], egui::Button::new("💾 Télécharger le QR Code")).clicked() {
								let seed = self.decrypted_seed.clone();
								let tx = self.tx.clone();
								// 💡 SOLUTION ANTI-FREEZE OS : FileDialog déporté dans un thread !
								tokio::task::spawn_blocking(move || {
									save_qr_code_to_disk(&seed, "wattcoin_seed_qr.png");
									let _ = tx.blocking_send(AppMessage::Info("✅ QR Code sauvegardé !".into()));
								});
							}
                            ui.add_space(5.0);
                            
                            if ui.add_sized([300.0, 40.0], egui::Button::new("🙈 Cacher la phrase secrète")).clicked() {
                                self.show_seed = false;
                                self.show_seed_qr = false;
                                self.decrypted_seed.clear();
                            }
                        } else {
                            ui.label("Entrez votre mot de passe pour révéler le secret absolu :");
                            ui.add(egui::TextEdit::singleline(&mut self.password_input).password(true));
                            ui.add_space(10.0);
                            
                            // On vérifie le mot de passe localement (puisque la RAM a les clés si on est loggué)
                            if ui.button("👁 Déchiffrer le Coffre").clicked() {
                                self.sync_message = "Vérification en cours...".to_string();
                                let pwd = self.password_input.clone();
                                let tx = self.tx.clone();
                                
                                tokio::spawn(async move {
                                    match crate::unlock_vault(pwd).await {
                                        Ok(keys) => {
                                            // On renvoie juste la seed cette fois, pas de changement de vue
                                            let _ = tx.send(AppMessage::Info(format!("SEED_SHOW:{}", keys.mnemonic))).await;
                                        }
                                        Err(_) => { let _ = tx.send(AppMessage::Error("❌ Mot de passe incorrect".to_string())).await; }
                                    }
                                });
                            }
                        }
						
						// DEVENIR MINEUR
                        ui.add_space(20.0);
                        ui.separator();
                        ui.add_space(20.0);

                        ui.heading("⛏ Devenir Mineur / Nœud L1");
                        ui.add_space(10.0);
                        ui.label(egui::RichText::new("Générez le script de lancement automatique pour commencer à forger des blocs sur votre ordinateur. Les récompenses de minage seront envoyées directement sur ce portefeuille.").color(egui::Color32::GRAY));
                        ui.add_space(15.0);

                        ui.horizontal(|ui| {
                            if ui.add_sized([180.0, 40.0], egui::Button::new("🐧 Script Linux (.sh)")).clicked() {
                                if let Some(keys) = &self.wallet_keys {
                                    match crate::save_miner_script("linux".to_string(), keys.watt_address.clone()) {
                                        Ok(msg) => self.sync_message = format!("✅ {}", msg),
                                        Err(e) => self.sync_message = format!("❌ {}", e),
                                    }
                                } else {
                                    self.sync_message = "❌ Portefeuille verrouillé.".to_string();
                                }
                            }

                            if ui.add_sized([180.0, 40.0], egui::Button::new("💻 Script Windows (.bat)")).clicked() {
                                if let Some(keys) = &self.wallet_keys {
                                    match crate::save_miner_script("windows".to_string(), keys.watt_address.clone()) {
                                        Ok(msg) => self.sync_message = format!("✅ {}", msg),
                                        Err(e) => self.sync_message = format!("❌ {}", e),
                                    }
                                } else {
                                    self.sync_message = "❌ Portefeuille verrouillé.".to_string();
                                }
                            }
                        });

                        // Petite astuce pour lire le message spécifique de Settings
                        if self.sync_message.starts_with("SEED_SHOW:") {
                            self.decrypted_seed = self.sync_message.replace("SEED_SHOW:", "");
                            self.show_seed = true;
                            self.sync_message.clear();
                            self.password_input.clear();
                        } else if !self.sync_message.is_empty() {
                            ui.add_space(10.0);
                            let color = if self.sync_message.starts_with("✅") { egui::Color32::GREEN } else { egui::Color32::RED };
                            ui.colored_label(color, &self.sync_message);
                        }
                    });

                    if self.show_seed_qr {
                        egui::Window::new("🚨 SECRET ABSOLU")
                            .collapsible(false)
                            .resizable(false)
                            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                            .show(ctx, |ui| {
                                ui.colored_label(egui::Color32::RED, "Ne montrez ceci à personne !");
                                ui.add_space(10.0);
                                
                                let texture = get_qr_texture(ui.ctx(), &self.decrypted_seed);
                                ui.add(egui::Image::new(&texture).max_width(350.0));
                                
                                ui.add_space(15.0);
                                if ui.add_sized([350.0, 40.0], egui::Button::new("Fermer")).clicked() {
                                    self.show_seed_qr = false;
                                }
                            });
                    }
                }
            }
			
			if self.show_payment_qr {
                egui::Window::new("📱 Recevoir un paiement")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                    .show(ctx, |ui| {
                        ui.vertical_centered(|ui| {
                            let is_btc = self.payment_qr_data.starts_with("bitcoin:");
                            let title = if is_btc { "Bitcoin (Testnet)" } else { "Wattcoin" };
                            let color = if is_btc { egui::Color32::from_rgb(247, 147, 26) } else { egui::Color32::from_rgb(0, 240, 255) };
                            
                            ui.label(egui::RichText::new(title).strong().color(color).size(18.0));
                            ui.add_space(10.0);
                            
                            // 1. Calcul de l'URI complète avec le montant optionnel
                            let mut full_uri = self.payment_qr_data.clone();
                            if let Ok(amt) = self.payment_qr_amount.parse::<f64>() {
                                if amt > 0.0 {
                                    full_uri = format!("{}?amount={}", self.payment_qr_data, self.payment_qr_amount);
                                }
                            }

                            // 2. Affichage du QR Code BEAUCOUP PLUS GROS (350.0)
                            let texture = get_qr_texture(ui.ctx(), &full_uri);
                            ui.add(egui::Image::new(&texture).max_width(350.0));
                            
                            ui.add_space(15.0);
                            
                            // 3. Champ pour demander un montant spécifique
                            ui.horizontal(|ui| {
                                ui.label("Montant demandé (Optionnel) :");
                                ui.add(egui::TextEdit::singleline(&mut self.payment_qr_amount).desired_width(100.0));
                                ui.label(if is_btc { "BTC" } else { "WATT" });
                            });
                            
                            // Conversion USD en temps réel sous le champ
                            if let Ok(amt) = self.payment_qr_amount.parse::<f64>() {
                                let price = if is_btc { self.btc_price_usd } else { self.watt_price_usd };
                                if price > 0.0 {
                                    ui.label(egui::RichText::new(format!("≈ $ {:.2} USD", amt * price)).color(egui::Color32::GRAY));
                                }
                            }
                            
                            ui.add_space(15.0);

                            // 4. Affichage tronqué + Bouton Copier ultra propre
                            let display_text = if full_uri.len() > 60 {
                                format!("{}...{}", &full_uri[0..25], &full_uri[full_uri.len() - 25..])
                            } else {
                                full_uri.clone()
                            };

                            egui::Frame::none().fill(item_bg).inner_margin(8.0).rounding(4.0).show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(egui::RichText::new(display_text).monospace().size(12.0).color(text_muted));
                                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                        if ui.button("📋 Copier").clicked() {
                                            set_clipboard_text(ui.ctx(), &full_uri);
                                        }
                                    });
                                });
                            });
                            
                            ui.add_space(15.0);

                            // 5. Bouton de téléchargement Desktop
                            #[cfg(not(target_os = "android"))]
                            {
                                if ui.add_sized([250.0, 35.0], egui::Button::new("💾 Télécharger l'image QR")).clicked() {
                                    let qr_data = full_uri.clone();
                                    let default_name = if is_btc { "bitcoin_payment_qr.png" } else { "wattcoin_payment_qr.png" };
                                    let tx = self.tx.clone();
                                    tokio::task::spawn_blocking(move || {
                                        save_qr_code_to_disk(&qr_data, default_name);
                                        let _ = tx.blocking_send(AppMessage::Info("✅ QR Code de paiement sauvegardé !".into()));
                                    });
                                }
                                ui.add_space(5.0);
                            }

                            if ui.add_sized([250.0, 35.0], egui::Button::new("Fermer")).clicked() {
                                self.show_payment_qr = false;
                                self.payment_qr_amount.clear();
                            }
                        });
                    });
            }
        });
    }
}

// -------------------------------------------------------------
// HELPERS PRESSE-PAPIER UNIVERSAUX (DESKTOP + ANDROID)
// -------------------------------------------------------------
fn get_clipboard_text() -> Option<String> {
    #[cfg(not(target_os = "android"))]
    {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            return clipboard.get_text().ok();
        }
        None
    }
    #[cfg(target_os = "android")]
    {
        paste_from_android_clipboard()
    }
}

fn set_clipboard_text(ctx: &egui::Context, text: &str) {
    #[cfg(not(target_os = "android"))]
    {
        ctx.output_mut(|o| o.copied_text = text.to_string());
    }
    #[cfg(target_os = "android")]
    {
        copy_to_android_clipboard(text);
    }
}

// -------------------------------------------------------------
// FONCTIONS UTILITAIRES (DESSIN QR CODE)
// -------------------------------------------------------------
fn get_qr_texture(ctx: &egui::Context, data: &str) -> egui::TextureHandle {
    let code = qrcode::QrCode::new(data.as_bytes()).unwrap();
    let width = code.width();
    let colors = code.to_colors();
    
    let mut pixels = Vec::with_capacity(colors.len());
    for color in colors {
        if color == qrcode::Color::Dark {
            pixels.push(egui::Color32::BLACK);
        } else {
            pixels.push(egui::Color32::WHITE); // 👈 Toujours blanc pour les scanners
        }
    }
    
    let color_image = egui::ColorImage { size: [width, width], pixels };
    ctx.load_texture("qr_code_texture", color_image, Default::default())
}

#[cfg(not(target_os = "android"))]
fn save_qr_code_to_disk(data: &str, default_filename: &str) {
    if let Some(path) = rfd::FileDialog::new()
        .set_file_name(default_filename)
        .add_filter("Image PNG", &["png"])
        .save_file()
    {
        let seed_nfc = data.nfc().collect::<String>();
        let code = qrcode::QrCode::new(seed_nfc.as_bytes()).unwrap();
        let colors = code.to_colors();
        let width = code.width();
        
        let pixel_size = 10;
        let margin = 4;
        let img_width_with_margin = (width + 2 * margin) * pixel_size;
        
        let mut imgbuf = image::ImageBuffer::from_pixel(
            img_width_with_margin as u32, 
            img_width_with_margin as u32, 
            image::Luma([255u8])
        );
        
        for (i, color) in colors.into_iter().enumerate() {
            if color == qrcode::Color::Dark {
                let x = (i % width + margin) as u32;
                let y = (i / width + margin) as u32;
                
                for dx in 0..pixel_size as u32 {
                    for dy in 0..pixel_size as u32 {
                        imgbuf.put_pixel(
                            x * (pixel_size as u32) + dx, 
                            y * (pixel_size as u32) + dy, 
                            image::Luma([0u8])
                        );
                    }
                }
            }
        }
        let _ = imgbuf.save_with_format(path, image::ImageFormat::Png);
    }
}

#[cfg(target_os = "android")]
fn save_qr_code_to_disk(_data: &str, _default_filename: &str) {
    // Ignoré sur Android pour éviter le crash de compilation
}



// -------------------------------------------------------------
// POINT D'ENTRÉE
// -------------------------------------------------------------
pub fn run_desktop() -> eframe::Result<()> {
    // 1. On charge l'image en mémoire (utilise ton vrai logo ici, png recommandé)
    let icon_bytes = include_bytes!("../assets/logo_wattcoin.png"); 
    let icon_image = image::load_from_memory(icon_bytes).unwrap().into_rgba8();
    let (width, height) = icon_image.dimensions();
    
    // 2. On la convertit au format attendu par egui
    let icon_data = egui::IconData {
        rgba: icon_image.into_raw(),
        width,
        height,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
			.with_app_id("wattcoin")
            .with_inner_size([400.0, 800.0])
            .with_min_inner_size([320.0, 480.0])
            .with_icon(icon_data), 
        ..Default::default()
    };

    eframe::run_native(
        "Wattcoin Wallet",
        options,
        Box::new(|cc| Ok(Box::new(WattcoinApp::new(cc)) as Box<dyn eframe::App>)),
    )
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    // On sauvegarde l'app pour pouvoir appeler Java plus tard
    *ANDROID_APP.lock().unwrap() = Some(app.clone());
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info));

    use winit::platform::android::EventLoopBuilderExtAndroid;

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    let _guard = Box::leak(Box::new(rt.enter()));
    Box::leak(Box::new(rt));

    let mut options = eframe::NativeOptions::default();
    options.event_loop_builder = Some(Box::new(move |builder| {
        builder.with_android_app(app);
    }));

    let _ = eframe::run_native(
        "Wattcoin Wallet",
        options,
        Box::new(|cc| Ok(Box::new(WattcoinApp::new(cc)))),
    );
}

// RÉCEPTION DE L'IMAGE DEPUIS JAVA (JNI)
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "C" fn Java_com_ohm_wattcoin_MainActivity_onImageBytesReceived(
    env: jni::JNIEnv,
    _this: jni::objects::JObject,
    image_array: jni::objects::JByteArray,
) {
    // 1. On extrait les octets immédiatement en mémoire de manière synchrone
    let bytes = match env.convert_byte_array(&image_array) {
        Ok(b) => b,
        Err(_) => return,
    };

    // 2. Traitement asynchrone pour ne pas bloquer l'écran
    std::thread::spawn(move || {
        if let Ok(img) = image::load_from_memory(&bytes) {
            let img_luma = img.into_luma8();
            let (w, h) = img_luma.dimensions();
            let w_usize = w as usize;
            let pixels = img_luma.into_raw();
            
            let mut prepared_img = rqrr::PreparedImage::prepare_from_greyscale(w_usize, h as usize, |x, y| pixels[y * w_usize + x]);
            let grids = prepared_img.detect_grids();
            
            let msg = if let Some(grid) = grids.first() {
                if let Ok((_, content)) = grid.decode() {
                    use unicode_normalization::UnicodeNormalization;
                    AppMessage::QrScanned(content.nfc().collect())
                } else {
                    AppMessage::Error("❌ Données QR illisibles.".into())
                }
            } else {
                AppMessage::Error("❌ Aucun QR code détecté sur l'image.".into())
            };

            if let Ok(guard) = APP_TX.lock() {
                if let Some(tx) = &*guard {
                    let _ = tx.blocking_send(msg);
                }
            }
        }
    });
}

// FONCTION DE COPIE VERS LE PRESSE-PAPIER ANDROID
#[cfg(target_os = "android")]
fn copy_to_android_clipboard(text: &str) {
    if let Some(app) = ANDROID_APP.lock().unwrap().clone() {
        let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM).unwrap() };
        if let Ok(mut env) = vm.attach_current_thread() {
            let activity = unsafe { jni::objects::JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
            if let Ok(j_text) = env.new_string(text) {
                // On tire à travers le JNI pour exécuter la fonction Java !
                let _ = env.call_method(
                    activity, 
                    "copyToClipboard", 
                    "(Ljava/lang/String;)V", 
                    &[jni::objects::JValue::from(&j_text)]
                );
            }
        }
    }
}

// FONCTION DE COLLAGE DEPUIS LE PRESSE-PAPIER ANDROID
#[cfg(target_os = "android")]
fn paste_from_android_clipboard() -> Option<String> {
    if let Some(app) = ANDROID_APP.lock().unwrap().clone() {
        let vm = unsafe { jni::JavaVM::from_raw(app.vm_as_ptr() as *mut jni::sys::JavaVM).unwrap() };
        if let Ok(mut env) = vm.attach_current_thread() {
            let activity = unsafe { jni::objects::JObject::from_raw(app.activity_as_ptr() as jni::sys::jobject) };
            
            // On appelle la fonction Java qui retourne un String
            if let Ok(j_value) = env.call_method(activity, "pasteFromClipboard", "()Ljava/lang/String;", &[]) {
                if let Ok(j_string) = j_value.l() {
                    // On convertit le JString Java en String Rust
                    if let Ok(rust_string) = env.get_string((&j_string).into()) {
                        let text: String = rust_string.into();
                        if !text.is_empty() {
                            return Some(text);
                        }
                    }
                }
            }
        }
    }
    None
}
