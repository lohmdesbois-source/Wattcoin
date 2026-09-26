#![recursion_limit = "1024"]

// On n'a plus besoin de déclarer les modules ici, ils sont dans lib.rs
use wattcoin_core::blockchain::{Blockchain, EPOCH_BLOCKS};
use wattcoin_core::transaction::{Transaction, TransactionType};
use wattcoin_core::api::SharedPool;

use std::env;
use std::sync::{Arc, Mutex};
use std::collections::{HashSet, HashMap}; 
use randomx_rs::{RandomXFlag, RandomXCache, RandomXDataset, RandomXVM};


pub type SharedMempool = Arc<Mutex<Vec<Transaction>>>;

// ===================================================================
// CONTENEUR UNSAFE POUR LE WARM-UP RANDOMX
// RandomX utilise des pointeurs C. Rust refuse de les changer de thread.
// En implémentant 'Send' de manière 'unsafe', on force l'autorisation.
// C'est sans danger ici car on transfère uniquement l'appartenance (Ownership).
// ===================================================================
struct WarmUpContainer {
    cache: RandomXCache,
    dataset: RandomXDataset,
}
unsafe impl Send for WarmUpContainer {}
unsafe impl Sync for WarmUpContainer {}





#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    let is_live_mode = args.contains(&"--live".to_string());
	let is_vps_mode = args.contains(&"--vps".to_string());
    let clean_args: Vec<String> = args.into_iter()
        .filter(|a| a != "--live" && a != "--vps") 
        .collect();

    if clean_args.len() < 3 {
        eprintln!("🛑 Usage Mineur : cargo run <PORT> <MINER_ADDRESS> [PEER_IP:PORT] [--live] [--vps]");
        eprintln!("🛡️  Usage Relais : cargo run <PORT> --relay [PEER_IP:PORT] [--live] [--vps]");
        return;
    }

    let port = clean_args[1].clone();
    let api_port = port.parse::<u16>().unwrap() + 100;
    let arg2 = clean_args[2].clone();
    let is_relay_mode = arg2 == "--relay";
    let miner_address = if is_relay_mode { String::from("RELAY_NODE_NO_MINING") } else { arg2 };
    let peer_target = clean_args.get(3).cloned();

    println!("🔥 DÉMARRAGE DU NŒUD CYPHERPUNK (v{}) ...", env!("CARGO_PKG_VERSION"));
	
	
    
    let (p2p_bind_ip, api_bind_ip) = if is_live_mode {
        println!("🌍 MODE LIVE ACTIVÉ : Le Nœud est ouvert sur Internet (0.0.0.0)");
        ("0.0.0.0", [0, 0, 0, 0])
    } else {
        println!("🏠 MODE LOCAL ACTIVÉ : Le Nœud est isolé sur ta machine (127.0.0.1)");
        ("127.0.0.1", [127, 0, 0, 1])
    };

    if is_relay_mode {
        println!("🛡️  MODE RELAIS ACTIVÉ : Minage désactivé. Le Nœud agira comme un routeur P2P.");
    }
    
    let home_dir = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let db_dir = format!("{}/.wattcoin", home_dir);
    if let Err(e) = std::fs::create_dir_all(&db_dir) {
        println!("⚠️ Impossible de créer le dossier .wattcoin : {}", e);
    }
    
    // ==============================================================
    // SÉCURITÉ MIXNET : Gestion KISS de l'identité Kyber du Nœud
    // ==============================================================
    let kyber_sec_path = format!("{}/node_kyber.secret", db_dir);
    let kyber_pub_path = format!("{}/node_kyber.pub", db_dir);
    
    let (node_kyber_secret, node_kyber_pub) = if std::path::Path::new(&kyber_sec_path).exists() {
        // Lecture silencieuse de la clé existante
        let sec = std::fs::read_to_string(&kyber_sec_path).unwrap().trim().to_string();
        let pubk = std::fs::read_to_string(&kyber_pub_path).unwrap().trim().to_string();
        (sec, pubk)
    } else {
        println!("🔑 Première exécution : Génération de l'identité quantique du Nœud Relais...");
        let mut rng = rand::thread_rng();
        let keys = pqc_kyber::keypair(&mut rng).expect("Erreur génération Kyber");
        let sec_hex = hex::encode(keys.secret);
        let pub_hex = hex::encode(keys.public);
        
        // Sauvegarde sur le disque
        std::fs::write(&kyber_sec_path, &sec_hex).unwrap();
        std::fs::write(&kyber_pub_path, &pub_hex).unwrap();
        
        // OS SHIELD : Application du CHMOD 600 (Lecture/Écriture pour le propriétaire uniquement)
        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(mut perms) = std::fs::metadata(&kyber_sec_path).map(|m| m.permissions()) {
                perms.set_mode(0o600); // chmod 600
                let _ = std::fs::set_permissions(&kyber_sec_path, perms);
                println!("🛡️ Permissions UNIX restreintes (chmod 600) appliquées sur le secret.");
            }
        }
        
        println!("============================================================");
        println!("🧅 NOUVELLE IDENTITÉ MIXNET GÉNÉRÉE ET SÉCURISÉE !");
        println!("Veuillez copier cette Clé Publique dans votre Wallet (SEED_NODES) :");
        println!("{}", pub_hex);
        println!("============================================================\n");
        
        (sec_hex, pub_hex)
    };

    // 1. ON LANCE L'UPnP SI ON EST EN MODE LIVE ET PAS SUR UN VPS
    if is_live_mode && !is_vps_mode {
        wattcoin_core::network::setup_upnp(port.parse::<u16>().unwrap());
    } else if is_vps_mode {
        println!("🌍 [VPS] UPnP désactivé (IP publique directe assumée).");
    }

    // 2. ON CHARGE LA MÉMOIRE LOCALE (ÉMANCIPATION DU SEED)
    let peers_file = format!("{}/known_peers.json", db_dir);
    let known_peers: wattcoin_core::SharedPeers = Arc::new(Mutex::new(HashSet::new()));
    
    if let Ok(data) = std::fs::read_to_string(&peers_file) {
        if let Ok(saved_peers) = serde_json::from_str::<Vec<String>>(&data) {
            let mut kp = known_peers.lock().unwrap();
            for peer in saved_peers { kp.insert(peer); }
            println!("💾 [BOOTSTRAP] {} adresses chargées depuis la mémoire locale.", kp.len());
        }
    }

    // On ajoute le seed cible (s'il y en a un fourni dans la console)
    if let Some(target) = &peer_target { 
        known_peers.lock().unwrap().insert(target.clone()); 
    }

    let role_prefix = if is_relay_mode { "relay" } else { "miner" };
    let l1_db_file = format!("{}/{}_l1_chain_{}", db_dir, role_prefix, port);

    // On utilise maintenant l1_db_file pour charger la chaîne (L1 + L2 unifié)
    let shared_chain = Arc::new(Mutex::new(Blockchain::new(&l1_db_file).unwrap()));
    let mempool: SharedMempool = Arc::new(Mutex::new(Vec::new()));
    let dex_pool: SharedPool = Arc::new(Mutex::new(Vec::new()));
    
    // ====================================================================
    // ⚛️ AFFICHAGE DU GENESIS ET GESTION DU LANCEMENT (MAINNET)
    // ====================================================================
    let (genesis_timestamp, genesis_hash) = {
        let chain = shared_chain.lock().unwrap();
        let genesis_block = &chain.get_block_by_height(0).unwrap();
        (genesis_block.header.timestamp, genesis_block.header.hash.clone())
    };

    let genesis_date = chrono::DateTime::from_timestamp(genesis_timestamp, 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
        .format("%d/%m/%Y %H:%M:%S")
        .to_string();

    println!("\n====================================================================");
    println!("⚛️  BLOC GENESIS PRÊT (STARTING BLOCK)");
    println!("====================================================================");
    println!("📦 Index       : 0");
    println!("🔗 Hash        : {}", genesis_hash);
    println!("🕒 Date Prévue : {}", genesis_date);
    println!("====================================================================\n");

    let now_ts = chrono::Utc::now().timestamp();
    if now_ts < genesis_timestamp {
        let wait_seconds = genesis_timestamp - now_ts;
        println!("⏳ [TESTNET STARTING BLOCK] Le réseau principal n'a pas encore démarré !");
        println!("⏳ Le nœud est en mode veille. Lancement automatique dans {} secondes...", wait_seconds);
        println!("⏳ Laissez ce terminal ouvert. Les moteurs s'allumeront à l'heure H.\n");
        
        tokio::time::sleep(tokio::time::Duration::from_secs(wait_seconds as u64)).await;
        
        println!("🚀 [TESTNET LIVE] C'EST PARTI ! Allumage des moteurs Cypherpunk !");
    }

    let active_peers: wattcoin_core::network::ActivePeers = Arc::new(Mutex::new(HashMap::new()));

    let p2p_chain = Arc::clone(&shared_chain);
    let p2p_mempool = Arc::clone(&mempool);
    let p2p_dex_pool = Arc::clone(&dex_pool);
    let p2p_peers = Arc::clone(&known_peers); 
    let p2p_active = Arc::clone(&active_peers);
    let port_clone = port.clone();
    let bind_ip_p2p = p2p_bind_ip.to_string(); 
    
    // LE SERVEUR P2P 
    tokio::spawn(async move {
        wattcoin_core::network::start_p2p_server(
            &bind_ip_p2p, &port_clone, p2p_chain, p2p_mempool, p2p_dex_pool, p2p_peers, p2p_active
        ).await;
    });
    
    let api_chain = Arc::clone(&shared_chain);
    let api_mempool = Arc::clone(&mempool);
    let api_dex_pool = Arc::clone(&dex_pool);
    let api_active_peers = Arc::clone(&active_peers);
    
    // 💡 ICI ON UTILISE DIRECTEMENT LES VARIABLES DU SCOPE PRINCIPAL
    let api_kyber_secret = node_kyber_secret.clone(); 
    let api_kyber_pub = node_kyber_pub.clone(); 
    
    tokio::spawn(async move { 
        wattcoin_core::api::start_api_server(
            api_port, api_bind_ip, api_mempool, api_chain, api_dex_pool, api_active_peers, api_kyber_secret, api_kyber_pub
        ).await; 
    });
    
    if let Some(target) = &peer_target {
        println!("🤝 Ouverture du tunnel P2P vers {}...", target);
        let target_clone = target.clone();
        let my_port = port.clone();
        let p2p_chain_handshake = Arc::clone(&shared_chain);
        let p2p_mempool_hs = Arc::clone(&mempool);
        let p2p_dex_hs = Arc::clone(&dex_pool);
        let p2p_peers_hs = Arc::clone(&known_peers);
        let p2p_active_hs = Arc::clone(&active_peers);
        
        tokio::spawn(async move {
            let mut address = if target_clone.contains(':') { 
                target_clone.clone() 
            } else { 
                format!("127.0.0.1:{}", target_clone) 
            };
            
            let mut consecutive_failures = 0;
            
            // LE CHIEN DE GARDE (Watchdog Auto-Reconnect)
            loop {
                let is_connected = {
                    let ap = p2p_active_hs.lock().unwrap();
                    let target_ip = address.split(':').next().unwrap_or("");
                    ap.keys().any(|k| k.starts_with(target_ip))
                };

                if !is_connected {
                    println!("🔓 Tentative de connexion P2P vers {}...", address);
                    
                    match tokio::net::TcpStream::connect(&address).await {
                        Ok(socket) => {
                            println!("✅ Connexion P2P réussie vers {} !", address);
                            consecutive_failures = 0; 
                            wattcoin_core::network::start_peer_connection(
                                socket, 
                                address.split(':').next().unwrap_or("127.0.0.1").to_string(), 
                                my_port.clone(), 
                                Arc::clone(&p2p_chain_handshake), 
                                Arc::clone(&p2p_mempool_hs), 
                                Arc::clone(&p2p_dex_hs), 
                                Arc::clone(&p2p_peers_hs), 
                                Arc::clone(&p2p_active_hs)
                            );
                        }
                        Err(e) => { 
                            println!("❌ Échec de connexion au réseau : {}", e); 
                            consecutive_failures += 1;
                
                            if consecutive_failures >= 3 {
                                println!("⚠️ [RELAIS] Isolement détecté. Recherche d'un nouveau Phare dans la DHT locale...");
                                
                                let mut new_target = None;
                                
                                {
                                    let chain = p2p_chain_handshake.lock().unwrap();
                                    if let Ok(dht_tree) = chain.db.open_tree("dht_nodes") {
                                        for result in dht_tree.iter() {
                                            if let Ok((_, value)) = result {
                                                if let Ok(record) = bincode::deserialize::<wattcoin_core::network::DhtRecord>(&value) {
                                                    if record.is_lighthouse && !record.ip_port.is_empty() && record.ip_port != address {
                                                        new_target = Some(record.ip_port);
                                                        break;
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                
                                if let Some(target) = new_target {
                                    println!("🔄 [RELAIS] Bascule vers un Phare de secours : {}", target);
                                    address = target;
                                    consecutive_failures = 0;
                                } else {
                                    let seed = wattcoin_core::SEED_NODES[0].to_string();
                                    if address != seed {
                                        println!("🔄 [RELAIS] DHT vide. Bascule vers le Seed Node racine : {}", seed);
                                        address = seed;
                                        consecutive_failures = 0;
                                    }
                                }
                            }
                            println!("⚠️ Nouvelle tentative automatique dans 10 secondes...");
                        }
                    }
                } else {
                    consecutive_failures = 0;
                }
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            }
        });
    }
    
    // ====================================================================
    // ANNONCE DHT : SIGNALEMENT DU NŒUD AU RÉSEAU (MINI-POW)
    // ====================================================================
    // On utilise la clé publique générée plus haut au lieu de la lire sur le disque !
    let dht_kyber_pub = node_kyber_pub.clone();
    let dht_chain = Arc::clone(&shared_chain);
    let dht_active_peers = Arc::clone(&active_peers);
    let dht_is_lighthouse = is_vps_mode; 
    let my_port_dht = port.clone();
    let is_live_dht = is_live_mode;

    tokio::spawn(async move {
        // 1. Découverte de l'IP Publique si on est un Phare (VPS)
        let mut dht_ip_port = String::new();
        if dht_is_lighthouse && is_live_dht {
            println!("🔍 [DHT] Découverte de l'IP publique pour le Phare...");
            let client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(5)).build().unwrap();
            if let Ok(res) = client.get("https://api.ipify.org").send().await {
                if let Ok(ip) = res.text().await {
                    dht_ip_port = format!("{}:{}", ip.trim(), my_port_dht);
                    println!("🌍 [DHT] IP Publique trouvée : {}", dht_ip_port);
                }
            }
        } else if dht_is_lighthouse {
            dht_ip_port = format!("127.0.0.1:{}", my_port_dht); // Mode local
        }
        // Si dht_is_lighthouse est faux (Mme odette toulemonde), dht_ip_port reste vide !

        // 2. Calcul du PoW isolé dans un thread bloquant (Zéro impact sur le nœud)
        tokio::task::spawn_blocking(move || {
            println!("⏳ [DHT] Génération de la preuve de travail anti-spam (Mini-PoW)...");
            let timestamp = chrono::Utc::now().timestamp();
            let mut nonce = 0u64;
            let mut pow_hash = String::new();

            let flags = randomx_rs::RandomXFlag::get_recommended_flags();
            
            // On récupère la graine de l'époque en cours
            let seed = {
                let chain = dht_chain.lock().unwrap();
                chain.get_epoch_seed(chain.current_height)
            };
            
            if let Ok(cache) = randomx_rs::RandomXCache::new(flags, seed.as_bytes()) {
                if let Ok(vm) = randomx_rs::RandomXVM::new(flags, Some(cache), None) {
                    loop {
                        let header_data = format!("{}{}{}{}", dht_kyber_pub, dht_ip_port, timestamp, nonce);
                        if let Ok(hash_bytes) = vm.calculate_hash(header_data.as_bytes()) {
                            // LA DIFFICULTÉ : Les 12 premiers bits à zéro (1 chance sur 4096)
                            if hash_bytes[0] == 0 && hash_bytes[1] < 16 {
                                pow_hash = hex::encode(&hash_bytes);
                                break;
                            }
                        }
                        nonce += 1;
                    }
                }
            }

            if !pow_hash.is_empty() {
                println!("✅ [DHT] Mini-PoW trouvé ! (Nonce: {})", nonce);
                let announcement = wattcoin_core::network::P2PMessage::NodeAnnouncement {
                    kyber_pubkey: dht_kyber_pub,
                    ip_port: dht_ip_port,
                    is_lighthouse: dht_is_lighthouse,
                    timestamp,
                    pow_hash,
                    nonce,
                };

                // On diffuse l'annonce à tous nos voisins
                if let Ok(payload) = bincode::serialize(&announcement) {
                    let length = (payload.len() as u32).to_be_bytes();
                    let mut framed = Vec::with_capacity(4 + payload.len());
                    framed.extend_from_slice(&length);
                    framed.extend_from_slice(&payload);

                    let peers = dht_active_peers.lock().unwrap().clone();
                    for (_, sender) in peers.iter() {
                        let _ = sender.try_send(framed.clone());
                    }
                }
            }
        });
    });

    if is_relay_mode {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
            let chain = shared_chain.lock().expect("Mutex empoisonné (panic précédent)");
            let _ = chain.db.flush(); // On s'assure juste que Sled a bien écrit sur le disque
        }
    } else {
        // 💡 On force le mineur à attendre la synchro initiale
        if peer_target.is_some() {
            println!("⏳ [SYNCHRONISATION] Pause de 15 secondes...");
            println!("⏳ Laissons le temps au tunnel Tor de s'établir et de télécharger l'historique du Relais.");
            tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
            println!("✅ [SYNCHRONISATION] Phase d'écoute terminée. Allumage des moteurs !");
        }

        // ====================================================================
        // 🛡️ PATCH ANTI-STARVATION : ISOLATION DU MINAGE
        // On prépare des clones de tous nos pointeurs intelligents (Arc) 
        // pour pouvoir les envoyer dans le thread de minage isolé.
        // ====================================================================
        let miner_chain = Arc::clone(&shared_chain);
        let miner_mempool = Arc::clone(&mempool);
        let miner_dex_pool = Arc::clone(&dex_pool);
        let miner_active_peers = Arc::clone(&active_peers);
        let miner_address_clone = miner_address.clone();
        let miner_port_clone = port.clone();

        // 🚀 On lance le minage lourd dans le pool de threads bloquants de Tokio.
        // Cela libère à 100% l'API Web et le serveur P2P qui tourneront sur les autres threads !
        tokio::task::spawn_blocking(move || {
            println!("\n⚙️  Initialisation du moteur RandomX...");
            let start_rx = std::time::Instant::now();

            let flags = RandomXFlag::get_recommended_flags();
            let mut current_epoch = 0;
            let mut seed_hash = miner_chain.lock().unwrap().get_epoch_seed(1);
            
            let mut cache = RandomXCache::new(flags, seed_hash.as_bytes()).unwrap();

            println!("⏳ Allocation du Dataset de 2 Go en RAM (Veuillez patienter...)");
            let mut dataset = RandomXDataset::new(flags, cache.clone(), 0).unwrap();
            let mut vm = RandomXVM::new(flags, Some(cache.clone()), Some(dataset.clone())).unwrap();
            println!("✅ RandomX prêt en {:.2?} !", start_rx.elapsed());

            println!("\n⛏️  Début de l'extraction pour l'adresse : {}...", miner_address_clone);
        
            let next_dataset: Arc<Mutex<Option<WarmUpContainer>>> = Arc::new(Mutex::new(None));
            let mut warming_up_epoch = current_epoch;
            
            // On garde la trace du Séquenceur actif pour pouvoir le tuer
            let mut current_sequencer_task: Option<tokio::task::JoinHandle<()>> = None;
        
            loop {
                // BOUCLIER ANTI-SPINLOCK
                let current_height = { miner_chain.lock().unwrap().current_height };
                let highest_known = wattcoin_core::network::HIGHEST_KNOWN_BLOCK.load(std::sync::atomic::Ordering::Relaxed);
                
                // 💡 CORRECTION : On vérifie si le réseau a déjà trouvé le PROCHAIN bloc (+1) !
                if highest_known >= current_height + 1 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    continue; 
                }

                // 1. GÉNÉRATION DES CLÉS L2 HORS DU MUTEX (ÉVITE LE GOULOT D'ÉTRANGLEMENT)
                let available_cores = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
                let num_threads = if available_cores > 2 { available_cores - 1 } else { 1 }; 
                let mut handles = Vec::new();
                let mut keys_left = 128;

                for i in 0..num_threads {
                    let chunk_size = if i == num_threads - 1 { keys_left } else { 128 / num_threads };
                    keys_left -= chunk_size;

                    handles.push(std::thread::spawn(move || {
                        let mut chunk_keys = Vec::with_capacity(chunk_size);
                        for _ in 0..chunk_size {
                            // 💡 CORRECTION DU KILL SWITCH WOTS+ (+1) !
                            if wattcoin_core::network::HIGHEST_KNOWN_BLOCK.load(std::sync::atomic::Ordering::Relaxed) >= current_height + 1 {
                                break; // On avorte la génération instantanément !
                            }
                            
                            // ON LAISSE RESPIRER LE RÉSEAU : 1ms de pause pour laisser passer les paquets TCP
                            std::thread::sleep(std::time::Duration::from_millis(1));
                            
                            // Génération WOTS+ (master_seed aléatoire, index 0)
                            let master_seed: [u8; 32] = rand::random();
                            chunk_keys.push(wots::Wots::generate_keypair(&master_seed, 0));
                        }
                        chunk_keys
                    }));
                }
                
                let mut pre_generated_l2_keys = Vec::with_capacity(128);
                for handle in handles {
                    let keys = handle.join().expect("Erreur critique thread WOTS+");
                    pre_generated_l2_keys.extend(keys);
                }

                // CORRECTION DU KILL SWITCH GLOBAL (+1) !
                if wattcoin_core::network::HIGHEST_KNOWN_BLOCK.load(std::sync::atomic::Ordering::Relaxed) >= current_height + 1 || pre_generated_l2_keys.len() < 128 {
                    continue; // On annule tout et on laisse la place au réseau !
                }

                // 0. LE MOTEUR DEX (FBA) ON-CHAIN - VERSION SÉCURISÉE
                let mut dex_settlement_tx = None;
                {
                    let p = miner_dex_pool.lock().unwrap();
                    let mut buys: Vec<_> = p.iter().filter(|o| o.order_type == "buy").cloned().collect();
                    let mut sells: Vec<_> = p.iter().filter(|o| o.order_type == "sell").cloned().collect();
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
                            clearing_price_sats = (buy.price_sats + sell.price_sats) / 2;
                            let matched_volume = std::cmp::min(buy.amount_flames, sell.amount_flames);
                            total_volume_flames += matched_volume;

                            let real_htlc_hash = buy.htlc_hash.clone().unwrap_or_else(|| "ERREUR_HASH_MANQUANT".to_string());

                            generated_swaps.push(wattcoin_core::transaction::SwapContract {
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
                            break;
                        }
                    }

                    if total_volume_flames > 0 {
                        println!("\n⚖️ [DEX] Matching réussi → {} WATT à {} Sats", 
                                 total_volume_flames as f64 / 1_000_000_000.0, clearing_price_sats);

                        dex_settlement_tx = Some(Transaction {
                            tx_type: TransactionType::DexSettlement { 
                                clearing_price_sats, 
                                total_volume_flames, 
                                swaps: generated_swaps 
                            },
                            inputs: vec![],
                            outputs: vec![],
                            fee: 0,
                            public_key: "DEX_SETTLEMENT_ON_CHAIN".to_string(), 
							wots_signature: None,
                        });
                    }
                }

                let (mut candidate_block, target, l2_keys) = {
                    let mut chain = miner_chain.lock().unwrap();
                    let mut pending_txs = miner_mempool.lock().unwrap().clone();
                    
                    if let Some(dex_tx) = dex_settlement_tx {
                        pending_txs.push(dex_tx);
                    }
                    
                    // On passe les clés pré-générées
                    chain.prepare_block_template(pending_txs, &miner_address_clone, pre_generated_l2_keys)
                };

                let target_epoch = (candidate_block.header.index - 1) / EPOCH_BLOCKS;
                if target_epoch > current_epoch {
                    println!("\n==========================================================");
                    println!("🔄 CHANGEMENT D'ÉPOQUE RANDOMX ! (Nouvelle Époque : {})", target_epoch);
                    println!("==========================================================");
                    current_epoch = target_epoch;
                    
                    seed_hash = miner_chain.lock().unwrap().get_epoch_seed(candidate_block.header.index);

                    let precalculated = next_dataset.lock().unwrap().take();
                    
                    if let Some(warm_data) = precalculated {
                        println!("⚡ [WARM-UP] Utilisation du Dataset précalculé en RAM ! Zéro temps d'arrêt pour le mineur.");
                        cache = warm_data.cache;
                        dataset = warm_data.dataset;
                        vm = RandomXVM::new(flags, Some(cache.clone()), Some(dataset.clone())).unwrap();
                    } else {
                        println!("⏳ Pas de cache prêt (serveur fraîchement démarré), calcul synchrone... (~30s)");
                        cache = RandomXCache::new(flags, seed_hash.as_bytes()).unwrap();
                        dataset = RandomXDataset::new(flags, cache.clone(), 0).unwrap();
                        vm = RandomXVM::new(flags, Some(cache.clone()), Some(dataset.clone())).unwrap();
                    }
                    println!("✅ Nouvelle Ère prête ! Le réseau est 100% sécurisé.");
                }

                let blocks_until_next = EPOCH_BLOCKS - ((candidate_block.header.index - 1) % EPOCH_BLOCKS);
                let next_epoch = current_epoch + 1;
                
                if blocks_until_next <= 10 && warming_up_epoch != next_epoch {
                    warming_up_epoch = next_epoch;
                    let next_seed = { miner_chain.lock().unwrap().get_epoch_seed(candidate_block.header.index + blocks_until_next + 1) };
                    let nd_clone = Arc::clone(&next_dataset);
                    
                    println!("\n🔥 [WARM-UP] Transition imminente ({} blocs). Début de la compilation en arrière-plan du Dataset {}...", blocks_until_next, next_epoch);
                    
                    tokio::task::spawn_blocking(move || {
                        let flags = RandomXFlag::get_recommended_flags();
                        if let Ok(warm_cache) = RandomXCache::new(flags, next_seed.as_bytes()) {
                            if let Ok(warm_dataset) = RandomXDataset::new(flags, warm_cache.clone(), 0) {
                                let container = WarmUpContainer { cache: warm_cache, dataset: warm_dataset };
                                *nd_clone.lock().unwrap() = Some(container);
                                println!("✅ [WARM-UP TERMINE] Dataset {} chargé en RAM. Prêt pour la bascule !", next_epoch);
                            }
                        }
                    });
                }

                let mut mined = false;
                let share_target = &target * 20u32; 
                let mut last_share_time = 0;
                
                loop {
					if candidate_block.header.nonce % 20 == 0 {
						// LECTURE DU KILL SWITCH (0 latence)
						if wattcoin_core::network::HIGHEST_KNOWN_BLOCK.load(std::sync::atomic::Ordering::Relaxed) >= candidate_block.header.index {
							println!("🛑 [ALERTE RAPIDE] Un bloc concurrent a été détecté ! Arrêt immédiat.");
							
							if let Some(task) = current_sequencer_task.take() {
								println!("🛑 [L2 SEQUENCER] Fin de règne (Nouveau bloc reçu du réseau).");
								task.abort();
							}
							break;
						}
						let chain = miner_chain.lock().unwrap();
                        if chain.current_height >= candidate_block.header.index {
                            println!("🛑 [ALERTE] Le réseau a trouvé le Bloc {} avant nous ! Annulation du minage.", candidate_block.header.index);
							
							if let Some(task) = current_sequencer_task.take() {
								println!("🛑 [L2 SEQUENCER] Fin de règne (Nouveau bloc reçu du réseau).");
								task.abort();
							}
                            break; 
                        }
                        
                        // On remplace le 'yield_now().await' par un mini-sleep natif
                        // Cela force ce thread intensif à respirer 1 ms pour le système d'exploitation.
                        std::thread::sleep(std::time::Duration::from_millis(1));
                    }

                    // Le PoW est désormais indissociable de l'adresse du mineur
                    let header_data = format!("{}{}{}{}{}{}{}", 
                        miner_address_clone,
						candidate_block.header.index, 
						candidate_block.header.timestamp, 
						candidate_block.header.previous_hash, 
						candidate_block.header.nonce,
						candidate_block.header.l2_root,
						candidate_block.header.tx_root // Verrouille l'intégrité des transactions !
					);

                    let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
                    candidate_block.header.hash = hex::encode(&hash_bytes);
                    let hash_value = num_bigint::BigUint::from_bytes_be(&hash_bytes);

                    if hash_value <= target {
                        mined = true;
                        break;
                    } 
                    else if hash_value <= share_target {
                        let now = chrono::Utc::now().timestamp();
                        if now - last_share_time > 5 {
                            last_share_time = now;
                            println!("💻 [P2POOL] Part de minage trouvée ! Partage avec le réseau...");
                            let share_tx = Transaction {
                                tx_type: TransactionType::MiningShare { 
                                    miner_address: miner_address_clone.clone(), 
                                    nonce: candidate_block.header.nonce, 
                                    hash: candidate_block.header.hash.clone(), 
                                    timestamp: candidate_block.header.timestamp 
                                },
                                inputs: vec![], outputs: vec![], fee: 0,
                                // On sépare par des |
								public_key: format!("{}|{}|{}", candidate_block.header.l2_root, candidate_block.header.tx_root, candidate_block.header.nonce), 
								wots_signature: None,
                            };
                            let mut pool = miner_mempool.lock().unwrap();
                            pool.push(share_tx.clone());
                            let tx_clone = share_tx.clone();
                            let peers_clone = Arc::clone(&miner_active_peers);
                            // On peut toujours appeler tokio::spawn depuis un spawn_blocking !
                            tokio::spawn(async move { wattcoin_core::network::broadcast_transaction(tx_clone, peers_clone).await; });
                        }
                    }
                    candidate_block.header.nonce += 1;
                }

                if mined {
                    let mut chain = miner_chain.lock().unwrap();
                    
                    if chain.current_height >= candidate_block.header.index {
                         println!("🗑️ [INFO] Hachage trouvé, mais la chaîne a été synchronisée entre temps. Bloc jeté.");
						 
						 if let Some(task) = current_sequencer_task.take() {
							println!("🛑 [L2 SEQUENCER] Fin de règne (Nouveau bloc reçu du réseau).");
							task.abort();
						}
						
                    } 
                    else if chain.current_height + 1 == candidate_block.header.index {
                        
                        let date_str = chrono::Local::now().format("%d-%m-%Y %H:%M:%S").to_string();
                        let nb_tx = candidate_block.transactions.len();
                        let mut total_fees = 0;
                        
                        for tx in candidate_block.transactions.iter().skip(1) { total_fees += tx.fee; }
                        
                        let l1_lottery_tax = total_fees / 100;
                        let l1_miner_fees = total_fees - l1_lottery_tax;

                        println!("\n====================================================================");
                        println!("🎉 NOUVEAU BLOC FORGÉ PAR LE MINEUR !");
                        println!("====================================================================");
                        println!("📦 Index du Bloc : {}", candidate_block.header.index);
                        println!("🔗 Hash          : {}", candidate_block.header.hash);
                        println!("🕒 Date et Heure : {}", date_str);
                        println!("📝 Transactions  : {} incluses (1 Coinbase + {} Publique/Swap/Lottery)", nb_tx, nb_tx - 1);
                        println!("💰 Frais perçus  : {} Flames", l1_miner_fees);
                        println!("====================================================================\n");
                        
                        for tx in &candidate_block.transactions {
							// On met à jour le prix en RAM si LE MINEUR L1 vient de miner un croisement DEX
							if let TransactionType::DexSettlement { clearing_price_sats, .. } = &tx.tx_type {
								wattcoin_core::api::LAST_PRICE_SATS.store(*clearing_price_sats, std::sync::atomic::Ordering::Relaxed);
							}

							if tx.tx_type != TransactionType::Coinbase {
								if let Some(sig) = &tx.wots_signature {
									chain.spent_key_images.insert(hex::encode(&sig.public_key));
								}
							}
						}

                        let _ = chain.push_block(&candidate_block);
                        chain.update_target(); 
                        
                        let l1_parent_hash = candidate_block.header.hash.clone();
                        let sequencer_keys = l2_keys.clone();
                        let mempool_seq = Arc::clone(&miner_mempool);
                        let active_peers_seq = Arc::clone(&miner_active_peers);
						
						let l2_pubkeys: Vec<String> = sequencer_keys.iter().map(|k| hex::encode(&k.1)).collect();
                        
                        // On prépare la blockchain et le fichier L2 pour l'état local
                        let chain_seq = Arc::clone(&miner_chain);

                        // RÉGICIDE : On tue brutalement l'ancien séquenceur s'il tourne encore
                        if let Some(task) = current_sequencer_task.take() {
                            println!("🛑 [L2 SEQUENCER] Fin de règne prématurée (Nouveau bloc L1 miné).");
                            task.abort(); // Coupe instantanément le thread asynchrone
                        }

                        // COURONNEMENT : On lance le nouveau et on garde son contrôle (JoinHandle)
                        let sequencer_handle = tokio::spawn(async move {
                            println!("\n⚡ [L2 SEQUENCER] Couronnement réussi ! Je suis le Séquenceur L2 pour les 2 prochaines minutes.");
                            use sha2::Digest; 
                            let mut already_sequenced = std::collections::HashSet::new();

                            // LECTURE DU VRAI COMPTEUR GLOBAL VIA SLED
                            let mut global_l2_index = 0;
                            {
                                let chain = chain_seq.lock().unwrap();
                                if let Ok(l2_tree) = chain.db.open_tree("l2_blocks") {
                                    if let Some(Ok((_, value))) = l2_tree.iter().rev().next() {
                                        if let Ok(last_mb) = bincode::deserialize::<wattcoin_core::block::MicroBlock>(&value) {
                                            global_l2_index = last_mb.micro_index;
                                        }
                                    }
                                }
                            }

                            for i in 0..128 {
                                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                                
                                let mut txs_to_sequence = Vec::new();
                                let mut expected_fees = 0u64; // On calcule les vrais frais
                                let mut current_mb_size = 1024; // 1 Ko de base pour l'en-tête

                                {
                                    let mp = mempool_seq.lock().unwrap();
                                    for tx in mp.iter() {
                                        let is_pure_l2 = !tx.outputs.is_empty() && tx.outputs.iter().all(|out| out.stealth_address.starts_with("L2_WATT_"));
                                        let tx_hash_hex = hex::encode(tx.hash_data());
                                        
										if is_pure_l2 && !already_sequenced.contains(&tx_hash_hex) {
                                            let tx_size = bincode::serialized_size(tx).unwrap_or(0) as usize;
                                            
                                            // LE BOUCLIER L2 : Limite stricte à 2 Mo par MicroBloc !
                                            if current_mb_size + tx_size > 2 * 1024 * 1024 {
                                                println!("⚠️ [L2] MicroBloc plein ! (2 Mo max). Fin du remplissage pour ce tour.");
                                                break;
                                            }

                                            current_mb_size += tx_size;
                                            expected_fees += tx.fee; // On additionne les vrais frais payés !
											txs_to_sequence.push(tx.clone());
											already_sequenced.insert(tx_hash_hex);
										}
                                    }
                                }

                                if txs_to_sequence.is_empty() { continue; }
                                
                                // On incrémente SEULEMENT parce qu'on a trouvé des transactions !
                                global_l2_index += 1; 
                                let true_tx_count = txs_to_sequence.len(); 
                                let keypair = &sequencer_keys[i];

                                // Répartition 99% Séquenceur / 1% Loto
                                let lottery_tax = expected_fees / 100;
                                let sequencer_reward = expected_fees - lottery_tax;

                                // 1. La part du Séquenceur
                                let mut coinbase_outputs = vec![
                                    wattcoin_core::transaction::TransactionOutput {
                                        stealth_address: format!("L2_WATT_{}", hex::encode(&keypair.1)),
                                        kyber_capsule: format!("MICRO_COINBASE_{}", global_l2_index), // 💡 Propre
                                        aes_vault: sequencer_reward.to_string(),
                                        lattice_commitment: wattcoin_core::lattice::LWECommitment::commit(sequencer_reward, &[0u64; wattcoin_core::lattice::LATTICE_COLS]),
										range_proof: String::new(),
                                    }
                                ];

                                // 2. La part de la Loterie (s'il y a des frais à taxer)
                                if lottery_tax > 0 {
                                    coinbase_outputs.push(wattcoin_core::transaction::TransactionOutput {
                                        stealth_address: "LOTTERY_RESERVE".to_string(),
                                        kyber_capsule: format!("L2_TAX_CAPSULE_{}", global_l2_index), // 💡 Propre
                                        aes_vault: lottery_tax.to_string(),
                                        lattice_commitment: wattcoin_core::lattice::LWECommitment::commit(lottery_tax, &[0u64; wattcoin_core::lattice::LATTICE_COLS]),
										range_proof: String::new(),
                                    });
                                }

                                let micro_coinbase = wattcoin_core::transaction::Transaction {
                                    tx_type: wattcoin_core::transaction::TransactionType::MicroCoinbase,
                                    inputs: vec![],
                                    outputs: coinbase_outputs,
                                    fee: 0,
                                    public_key: "MICRO_COINBASE".to_string(),
									wots_signature: None,
                                };

                                txs_to_sequence.insert(0, micro_coinbase);

                                println!("⚡ [L2 SEQUENCER] Signature du MicroBloc #{} ({} TXs, Frais encaissés: {} Flames)", 
                                          global_l2_index, txs_to_sequence.len() - 1, expected_fees);

								let mut micro_block = wattcoin_core::block::MicroBlock {
                                    l1_parent_hash: l1_parent_hash.clone(),
                                    micro_index: global_l2_index, // Le Compteur Global !
                                    key_index: i as u32,          // L'index pour la sécurité WOTS
                                    timestamp: chrono::Utc::now().timestamp(),
                                    transactions: txs_to_sequence,
                                    sequencer_pubkey: hex::encode(&keypair.1),
                                    sequencer_reward_address: "FEE_GOES_TO_NEXT_L1_MINER".to_string(), 
                                    sequencer_sig: wots::WotsSignature { index: 0, public_key: vec![], signature_bytes: vec![] },
                                    merkle_proof: l2_pubkeys.clone(), 
                                };

                                // On scelle cryptographiquement les transactions !
								let mut tx_hasher = sha2::Sha512::new();
								for tx in &micro_block.transactions {
									tx_hasher.update(&tx.hash_data());
								}
								let txs_hash = hex::encode(tx_hasher.finalize());

								// On hache TOUT pour la signature (incluant les transactions)
								let mb_data = format!("{}{}{}{}{}", 
									micro_block.l1_parent_hash, 
									micro_block.micro_index, 
									micro_block.key_index, 
									micro_block.timestamp, 
									txs_hash // Le contenu est maintenant verrouillé !
								);

								let mut hasher = sha2::Sha512::new();
								hasher.update(mb_data.as_bytes());
								let mut hash_arr = [0u8; 64];
								hash_arr.copy_from_slice(&hasher.finalize());

                                // On extrait les 32 premiers octets du hash SHA512 pour WOTS+
								let mut hash_arr_32 = [0u8; 32];
								hash_arr_32.copy_from_slice(&hash_arr[0..32]);

								// Note: Wots::sign prend en paramètre : 
								// (secret_key: &[[u8; 32]], index: u64, message_hash: &[u8; 32], public_key: &[u8])
								micro_block.sequencer_sig = wots::Wots::sign(
									&keypair.0, // <-- keypair.0 correspond à secret_key dans le tuple renvoyé par generate_keypair
									micro_block.micro_index,
									&hash_arr_32,
									&keypair.1  // <-- keypair.1 correspond à la clé publique
								);

                                wattcoin_core::network::broadcast_micro_block(micro_block.clone(), Arc::clone(&active_peers_seq)).await;
                                
                                println!("\n====================================================================");
                                println!("⚡ NOUVEAU MICRO-BLOC L2 SÉQUENCÉ !");
                                println!("====================================================================");
                                println!("📦 Micro-Index   : {}", micro_block.micro_index); // 💡 L'affichage est propre
                                println!("🔗 Parent L1     : {}", micro_block.l1_parent_hash);
                                println!("🕒 Date et Heure : {}", chrono::Local::now().format("%d-%m-%Y %H:%M:%S"));
                                println!("📝 Transactions  : {} incluses (Instantanées)", true_tx_count);
                                println!("💰 Frais perçus  : {} Flames", sequencer_reward);
                                println!("====================================================================\n");

                                // MISE À JOUR SÉCURISÉE DE L'ÉTAT LOCAL DU SÉQUENCEUR
                                // 1 & 2. Enregistrement direct dans Sled + Protection Anti-Double Dépense & Nettoyage Mempool
                                {
                                    let mut chain = chain_seq.lock().unwrap();
                                    
                                    // SAUVEGARDE L2 VIA SLED
                                    let _ = chain.push_microblock(&micro_block);
                                    
                                    for tx in &micro_block.transactions {
                                        if tx.tx_type != TransactionType::MicroCoinbase {
											if let Some(sig) = &tx.wots_signature {
												chain.spent_key_images.insert(hex::encode(&sig.public_key));
											}
										}
                                    }
                                    
                                    let mut mp = mempool_seq.lock().unwrap();
                                    mp.retain(|tx| !micro_block.transactions.iter().any(|m_tx| m_tx.hash_data() == tx.hash_data()));
                                }
                            }
                            println!("⚡ [L2 SEQUENCER] Mon règne est terminé. J'attends le prochain bloc L1...");
                        }); // Fin du tokio::spawn

                        // On enregistre le nouveau roi
                        current_sequencer_task = Some(sequencer_handle);

                        let block_clone = candidate_block.clone();
                        let my_port_clone = miner_port_clone.clone(); 
                        let active_clone = Arc::clone(&miner_active_peers);
                        
                        tokio::spawn(async move {
                            wattcoin_core::network::broadcast_mined_block(&my_port_clone, block_clone, active_clone).await;
                        });
                    }
                    
                    let mut mp = miner_mempool.lock().unwrap();
                    
                    let cutoff_time = {
                        if chain.current_height >= 1 {
                            chain.get_block_by_height(chain.current_height - 1).unwrap().header.timestamp
                        } else { 
                            0 
                        }
                    };

                    // L'astuce : on liste les HASHES des transactions fraîchement minées
					let mined_hashes: Vec<_> = candidate_block.transactions.iter().map(|tx| tx.hash_data()).collect();
					
					mp.retain(|tx| {
						// On compare les HASHES uniques
						let not_in_block = !mined_hashes.contains(&tx.hash_data());
						
						let is_valid_share = match &tx.tx_type {
							TransactionType::MiningShare { timestamp, .. } => *timestamp >= cutoff_time,
							_ => true
						};
						not_in_block && is_valid_share
					});
					
					{
						let now = chrono::Utc::now().timestamp();
						let mut dp = miner_dex_pool.lock().unwrap();
						
						// On déduit les montants qui ont été validés dans ce bloc
						for tx in &candidate_block.transactions {
							if let TransactionType::DexSettlement { swaps, .. } = &tx.tx_type {
								for swap in swaps {
									// Déduction côté Acheteur
									if let Some(buy) = dp.iter_mut().find(|o| o.order_type == "buy" && o.htlc_hash.as_ref() == Some(&swap.htlc_hash)) {
										buy.amount_flames = buy.amount_flames.saturating_sub(swap.watt_amount_flames);
									}
									// Déduction côté Vendeur
									if let Some(sell) = dp.iter_mut().find(|o| o.order_type == "sell" && o.watt_address == swap.seller_watt_address && o.amount_flames >= swap.watt_amount_flames) {
										sell.amount_flames = sell.amount_flames.saturating_sub(swap.watt_amount_flames);
									}
								}
							}
						}
						// On purge UNIQUEMENT les ordres vidés ou expirés !
						dp.retain(|o| o.amount_flames > 0 && o.expires_at > now);
						println!("🧹 [DEX] Bloc forgé : Dark Pool mis à jour (ordres restants: {}).", dp.len());
					}
					
                } // Le verrou `chain` est enfin relâché proprement ici !
            }
        });

        // Puisque le minage est parti dans un thread d'arrière-plan,
        // on demande à notre programme principal d'attendre indéfiniment sans s'éteindre.
        std::future::pending::<()>().await;
    }
}