use tokio::net::{TcpListener, TcpStream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicU64, Ordering};
use std::collections::HashMap;
use std::time::Duration;
use serde::{Serialize, Deserialize};
use rand::Rng;
use once_cell::sync::Lazy;
use crate::block::Block;
use crate::blockchain::Blockchain;
use crate::transaction::{Transaction, TransactionType};
use crate::api::{Order, SharedPool};
use crate::mixnet::OnionPacket;

// Le channel gère du binaire pur (Vec<u8>)
pub type ActivePeers = Arc<Mutex<HashMap<String, mpsc::Sender<Vec<u8>>>>>;
pub static HIGHEST_KNOWN_BLOCK: AtomicU64 = AtomicU64::new(0);
// BOUCLIER ANTI-DOS & BAN LIST
pub static BANNED_PEERS: Lazy<Mutex<HashMap<String, i64>>> = Lazy::new(|| Mutex::new(HashMap::new()));
// Ordres DEX clos (annulés OU entièrement exécutés) : id -> date d'oubli. Empêche leur résurrection via la synchro P2P
pub static CLOSED_ORDERS: Lazy<Mutex<HashMap<String, i64>>> = Lazy::new(|| Mutex::new(HashMap::new()));
// Parts de minage acceptées : hash de la part -> hauteur du tour vérifié (seuls le bloc en cours et le suivant sont gardés)
pub static SHARE_ROUNDS: Lazy<Mutex<HashMap<String, u64>>> = Lazy::new(|| Mutex::new(HashMap::new()));
// Mempool
pub const MAX_MEMPOOL_TXS: usize = 2000;
pub const MAX_DEX_ORDERS: usize = 1000;
// DEX
pub const ORDER_LIFETIME_SECS: i64 = 7200; // Un ordre vit 2 h
// Network
const MAX_PEERS: usize = 50;
const BAN_DURATION_SECS: i64 = 3600; // Banni pour 1 heure


#[derive(Serialize, Deserialize, Debug)]
pub enum P2PMessage {
    Handshake { genesis_hash: String, current_height: u64, sender_port: String },
    // AJOUT DU DERNIER INDEX L2 DANS LA REQUÊTE
    SyncRequest { locator_hashes: Vec<String>, last_l2_index: u64, sender_port: String },
    // AJOUT DES MICROBLOCS L2 DANS LA RÉPONSE
    SyncResponse { blocks: Vec<Block>, micro_blocks: Vec<crate::block::MicroBlock> },
    NewBlock { block: Block, sender_port: String }, 
    WhisperTransaction { tx: Transaction },    
    BroadcastTransaction { tx: Transaction },  
    BroadcastOrder { order: Order },
    GetMempool,
    MempoolSync { txs: Vec<Transaction> },
	BroadcastMicroBlock { micro_block: crate::block::MicroBlock },
	RelayOnion { packet: OnionPacket },
    NodeAnnouncement { 
        kyber_pubkey: String, 
        ip_port: String, 
        is_lighthouse: bool,
        timestamp: i64,
        pow_hash: String,     // Le hash RandomX
        nonce: u64,           // Le sel pour trouver le PoW
    },
    // Annulation d'un ordre DEX : on transmet le jeton, chaque nœud recalcule l'id
    CancelOrder { token: String },
}

// Le profil enregistré dans la base de données Sled
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DhtRecord {
    pub kyber_pubkey: String,
    pub ip_port: String,      // L'adresse IP et le port (vide si le nœud est caché)
    pub is_lighthouse: bool,  // Vrai si le nœud a passé le test TCP
    pub last_seen: i64,       // Timestamp de la dernière validation
}






// ====================================================================
// HYGIÈNE MEMPOOL & DEX : règles uniques (P2P, API, mineur)
// ====================================================================

// Clé HTLC d'un claim/refund (le hash du secret)
fn htlc_key(tx: &Transaction) -> Option<String> {
    use sha2::Digest;
    match &tx.tx_type {
        TransactionType::HTLCClaim { secret } => Some(hex::encode(sha2::Sha256::digest(&hex::decode(secret).unwrap_or_default()))),
        TransactionType::HTLCRefund { hash } => Some(hash.clone()),
        _ => None,
    }
}

// Clés consommées par une TX : ses UTXO + sa clé WOTS+
fn tx_spent_keys(tx: &Transaction) -> Vec<String> {
    let mut keys: Vec<String> = tx.inputs.iter().map(|i| i.utxo_id.clone()).collect();
    if let Some(sig) = &tx.wots_signature { keys.push(hex::encode(&sig.public_key)); }
    keys
}

// PORTE D'ENTRÉE UNIQUE de la mempool (hors parts de minage, vérifiées par RandomX)
pub fn check_tx_admission(chain: &Blockchain, pool: &[Transaction], tx: &Transaction) -> Result<(), String> {
    // 1. Les transactions système ne circulent jamais
    if matches!(tx.tx_type, TransactionType::Coinbase | TransactionType::MicroCoinbase
        | TransactionType::DexSettlement { .. } | TransactionType::LotteryPayout { .. }
        | TransactionType::MiningShare { .. }) {
        return Err("❌ Transaction système interdite en mempool".into());
    }
    // 2. Doublon exact
    let h = tx.hash_data();
    if pool.iter().any(|m| m.hash_data() == h) { return Err("✅ TX déjà en mempool".into()); }
    // 3. Mempool pleine
    if pool.len() >= MAX_MEMPOOL_TXS { return Err("❌ Réseau saturé".into()); }
    // 4. Maths + signature
    if !tx.is_valid() { return Err("❌ Math ou signature invalide".into()); }
    // 5. Frais minimum (mêmes règles que prepare_block_template)
    let is_pure_l2 = !tx.outputs.is_empty() && tx.outputs.iter().all(|o| o.stealth_address.starts_with("L2_WATT_"));
    if !matches!(tx.tx_type, TransactionType::HTLCClaim { .. } | TransactionType::HTLCRefund { .. }) {
        let is_l1_interop = matches!(tx.tx_type, TransactionType::L2Anchor { .. } | TransactionType::L2BridgeLock { .. }
            | TransactionType::L2Stake { .. } | TransactionType::L2Unstake { .. });
        let weight_kb = (bincode::serialized_size(tx).unwrap_or(0) as f64 / 1024.0).ceil() as u64;
        let min_fee = if is_pure_l2 && !is_l1_interop { std::cmp::max(100, weight_kb * 2) } else { std::cmp::max(1000, weight_kb * 20) };
        if tx.fee < min_fee { return Err(format!("❌ Frais insuffisants ({} < {} Flames)", tx.fee, min_fee)); }
    }
    // 6. UTXO / clé WOTS+ : pas de doublon interne, pas dépensés on-chain, pas réservés en mempool
    let keys = tx_spent_keys(tx);
    let mut uniq: std::collections::HashSet<String> = std::collections::HashSet::new();
    if !keys.iter().all(|k| uniq.insert(k.clone())) { return Err("❌ Input dupliqué dans la transaction".into()); }
    if keys.iter().any(|k| chain.spent_key_images.contains(k)) { return Err("❌ UTXO ou clé WOTS+ déjà dépensé on-chain".into()); }
    for m in pool {
        if tx_spent_keys(m).iter().any(|k| uniq.contains(k)) { return Err("❌ UTXO déjà en cours de dépense dans la mempool".into()); }
    }
    // 7. Existence et montant réel des inputs L1 (le L2 est géré par le séquenceur)
    if !is_pure_l2 {
        for input in &tx.inputs {
            match chain.find_utxo(&input.utxo_id) {
                None => return Err(format!("❌ UTXO fantôme ({})", input.utxo_id)),
                Some(u) if u.amount != input.amount => return Err("❌ Montant d'input falsifié".into()),
                _ => {}
            }
        }
    }
    // 8. HTLC : règles du consensus + un seul règlement par hash en mempool
    if let Some(k) = htlc_key(tx) {
        chain.check_htlc_tx(tx, chain.current_height + 1, &mut std::collections::HashSet::new())?;
        if pool.iter().any(|m| htlc_key(m).as_ref() == Some(&k)) { return Err("❌ HTLC : règlement déjà en mempool".into()); }
    }
    Ok(())
}

// PURGE UNIQUE : à appeler après chaque changement de la chaîne (bloc reçu, bloc miné, synchro, fork)
pub fn purge_mempool(chain: &Blockchain, pool: &mut Vec<Transaction>) {
    let tip = chain.current_height;
    let before = pool.len();
    let mut seen = std::collections::HashSet::new();
    let mut reserved: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut htlc_reserved = std::collections::HashSet::new();
    let mut rounds = SHARE_ROUNDS.lock().unwrap();
    // Parts : on ne garde que le tour du bloc en cours (tip, incluses dans tip+1) et le suivant (tip+1)
    rounds.retain(|_, h| *h >= tip);

    pool.retain(|tx| {
        // 1. Doublon exact
        if !seen.insert(tx.hash_data()) { return false; }
        match &tx.tx_type {
            // 2. Jamais de transaction système en mempool
            TransactionType::Coinbase | TransactionType::MicroCoinbase
            | TransactionType::DexSettlement { .. } | TransactionType::LotteryPayout { .. } => return false,
            // 3. Parts de minage : uniquement les tours vérifiés et encore utiles
            TransactionType::MiningShare { hash, .. } => return rounds.contains_key(hash),
            // 4. HTLC : déjà réglé, expiré ou en double → dehors
            TransactionType::HTLCClaim { .. } | TransactionType::HTLCRefund { .. } => {
                if chain.check_htlc_tx(tx, tip + 1, &mut std::collections::HashSet::new()).is_err() { return false; }
                if let Some(k) = htlc_key(tx) { if !htlc_reserved.insert(k) { return false; } }
            }
            _ => {}
        }
        // 5. UTXO / clé WOTS+ : minée, en conflit avec le bloc, ou déjà réservée par une TX plus ancienne
        let keys = tx_spent_keys(tx);
        let mut uniq: std::collections::HashSet<String> = std::collections::HashSet::new();
        if !keys.iter().all(|k| uniq.insert(k.clone())) { return false; }
        if keys.iter().any(|k| chain.spent_key_images.contains(k) || reserved.contains(k)) { return false; }
        reserved.extend(keys);
        true
    });

    let removed = before - pool.len();
    if removed > 0 {
        println!("🧹 [MEMPOOL] {} transaction(s) purgée(s) (minées, conflits, périmées). Restantes : {}", removed, pool.len());
    }
}

// Ordre clos (annulé ou exécuté) : mémorisé 2 h, puis oublié automatiquement
pub fn close_order(id: &str) -> bool {
    let now = chrono::Utc::now().timestamp();
    let mut closed = CLOSED_ORDERS.lock().unwrap();
    closed.retain(|_, until| *until > now);
    closed.insert(id.to_string(), now + ORDER_LIFETIME_SECS + 300).is_none()
}

pub fn is_order_closed(id: &str) -> bool {
    let now = chrono::Utc::now().timestamp();
    CLOSED_ORDERS.lock().unwrap().get(id).map_or(false, |until| *until > now)
}

// Contrôle d'un ordre DEX (API et P2P)
pub fn check_order(o: &Order) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    if o.order_type != "buy" && o.order_type != "sell" { return Err("❌ Type d'ordre inconnu".into()); }
    if o.amount_flames == 0 || o.price_sats == 0 { return Err("❌ Montant ou prix nul".into()); }
    if o.expires_at <= now { return Err("❌ Ordre expiré".into()); }
    if o.expires_at > now + ORDER_LIFETIME_SECS + 300 { return Err("❌ Durée de vie supérieure à 2 h".into()); }
    if o.order_type == "buy" && o.htlc_hash.as_ref().map_or(true, |h| h.len() != 64) { return Err("❌ Achat sans HTLC hash valide".into()); }
    if is_order_closed(&o.id) { return Err("❌ Ordre annulé ou déjà exécuté".into()); }
    Ok(())
}

// Applique les swaps d'un bloc au Dark Pool (bloc reçu, bloc miné, synchro)
pub fn apply_block_to_dex(dp: &mut Vec<Order>, block: &Block) {
    for tx in &block.transactions {
        if let TransactionType::DexSettlement { swaps, .. } = &tx.tx_type {
            for swap in swaps {
                // Achat : un hash HTLC ne sert qu'à UN swap → l'ordre d'achat est clos
                if let Some(buy) = dp.iter_mut().find(|o| o.order_type == "buy" && o.htlc_hash.as_ref() == Some(&swap.htlc_hash)) {
                    buy.amount_flames = 0;
                }
                // Vente : on déduit le volume exécuté
                if let Some(sell) = dp.iter_mut().find(|o| o.order_type == "sell" && o.watt_address == swap.seller_watt_address && o.amount_flames >= swap.watt_amount_flames) {
                    sell.amount_flames -= swap.watt_amount_flames;
                }
            }
        }
    }
    let now = chrono::Utc::now().timestamp();
    dp.retain(|o| {
        if o.amount_flames == 0 { close_order(&o.id); return false; } // exécuté → ne ressuscitera pas
        o.expires_at > now
    });
}

// Synchro L1 (rattrapage ou fork) + remise en ordre de la mempool et du Dark Pool
fn sync_blocks_with_pools(chain: &mut Blockchain, blocks: &[Block], mempool: &Arc<Mutex<Vec<Transaction>>>, dex_pool: &SharedPool) -> bool {
    // Avant la synchro : nos blocs déjà identiques (connus) et ceux qui vont être remplacés (orphelins)
    let incoming: std::collections::HashSet<String> = blocks.iter().map(|b| b.header.hash.clone()).collect();
    let mut known = std::collections::HashSet::new();
    let mut orphans = Vec::new();
    if let Some(first) = blocks.first() {
        for i in first.header.index..=chain.current_height {
            if let Some(b) = chain.get_block_by_height(i) {
                if incoming.contains(&b.header.hash) { known.insert(b.header.hash.clone()); } else { orphans.push(b); }
            }
        }
    }

    if !chain.resolve_partial_fork(blocks.to_vec()) { return false; }

    {
        let mut mp = mempool.lock().unwrap();
        // 1. Les TX des blocs orphelins retournent en mempool (si toujours valides sur la nouvelle chaîne)
        let mut reinjected = 0;
        for b in &orphans {
            for t in &b.transactions {
                if check_tx_admission(chain, &mp, t).is_ok() { mp.push(t.clone()); reinjected += 1; }
            }
        }
        if reinjected > 0 { println!("♻️ [FORK] {} transaction(s) des blocs orphelins remises en mempool.", reinjected); }
        // 2. Purge générale
        purge_mempool(chain, &mut mp);
    }

    // 3. DEX : uniquement les blocs réellement nouveaux (pas de double déduction)
    let mut dp = dex_pool.lock().unwrap();
    for b in blocks.iter().filter(|b| !known.contains(&b.header.hash)) {
        apply_block_to_dex(&mut dp, b);
    }
    true
}

// Lecture avec préfixe de taille (TCP Framing)
async fn read_p2p_message<R: AsyncReadExt + std::marker::Unpin>(reader: &mut R, peer_ip: &str) -> Option<P2PMessage> {
    let mut len_buf = [0u8; 4];
    
    // 1. On lit exactement 4 octets pour connaître la taille
    if reader.read_exact(&mut len_buf).await.is_err() { return None; }
    let length = u32::from_be_bytes(len_buf) as usize;
    
    // 2. Limite stricte à 33 Mo (32 Mo de bloc + 1 Mo de marge Bincode) = 34_603_008 octets
    const MAX_MESSAGE_SIZE: usize = 34_603_008; 
    
    if length > MAX_MESSAGE_SIZE {
        println!("🚨 [SÉCURITÉ] Flux TCP ignoré : Message binaire trop volumineux ({} octets).", length);
        ban_peer(peer_ip, "Spam Mémoire (Payload géant)");
        return None; // Coupe la connexion instantanément
    }
    
    // 3. On lit exactement le reste du message
    let mut payload = vec![0u8; length];
    if reader.read_exact(&mut payload).await.is_err() { return None; }
    
    bincode::deserialize(&payload).ok()
}

// Écriture Binaire avec préfixe
async fn send_message_to_channel(sender: &mpsc::Sender<Vec<u8>>, message: P2PMessage) {
    if let Ok(payload) = bincode::serialize(&message) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);
        let _ = sender.send(framed).await;
    }
}

pub async fn start_p2p_server(host_ip: &str, port: &str, blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool, known_peers: crate::SharedPeers, active_peers: ActivePeers) {
    let address = format!("{}:{}", host_ip, port);
    let listener = TcpListener::bind(&address).await.unwrap();
    println!("📡 Serveur P2P (Tunnels Persistants) à l'écoute sur TCP/{}...", port);
    
    let my_port = port.to_string(); 

    loop {
        let (socket, peer_addr) = listener.accept().await.unwrap();
        let peer_ip = peer_addr.ip().to_string();
        
        // --- BOUCLIER ANTI-DOS : LIMITES ET BAN LIST ---
        let current_time = chrono::Utc::now().timestamp();
        
        let is_banned = {
            let mut bans = BANNED_PEERS.lock().unwrap();
            bans.retain(|_, &mut unban_time| current_time < unban_time); // Purge des vieux bans
            bans.contains_key(&peer_ip)
        };

        if is_banned {
            println!("🚫 Rejet de la connexion : IP {} est actuellement bannie.", peer_ip);
            continue; // On ferme la socket sans répondre
        }

        let current_peers = active_peers.lock().unwrap().len();
        if current_peers >= MAX_PEERS {
            println!("⚠️ Nœud saturé ({} peers max). Rejet de {}.", MAX_PEERS, peer_ip);
            continue;
        }
        // -----------------------------------------------

        let now = chrono::Local::now().format("%d-%m-%Y %H:%M:%S");
        println!("🤝 [{}] Nouvelle connexion P2P entrante depuis {} !", now, peer_ip);
        
        start_peer_connection(
            socket, peer_ip, my_port.clone(), 
            Arc::clone(&blockchain), Arc::clone(&mempool), Arc::clone(&dex_pool), 
            Arc::clone(&known_peers), Arc::clone(&active_peers)
        );
    }
}

pub fn start_peer_connection(
    socket: TcpStream, peer_ip: String, my_port: String,
    blockchain: Arc<Mutex<Blockchain>>, mempool: Arc<Mutex<Vec<Transaction>>>, dex_pool: SharedPool,
    known_peers: crate::SharedPeers, active_peers: ActivePeers
) {
    let (mut read_half, mut write_half) = socket.into_split();
    // Le channel transite du Vec<u8> !
    let (tx, mut rx) = mpsc::channel::<Vec<u8>>(10_000);

    let random_id: u32 = rand::random();
    let temp_peer_id = format!("{}:incoming_{}", peer_ip, random_id);
    active_peers.lock().unwrap().insert(temp_peer_id.clone(), tx.clone());

    tokio::spawn(async move {
        while let Some(msg_bytes) = rx.recv().await {
            if write_half.write_all(&msg_bytes).await.is_err() { break; }
        }
    });

    tokio::spawn(async move {
        let mut actual_peer_id = temp_peer_id.clone();

        // Le "Hello" immédiat ! Dès qu'on se connecte, on annonce notre hauteur.
        let (my_height, my_genesis) = {
            let chain = blockchain.lock().unwrap();
            (chain.current_height + 1, chain.get_block_by_height(0).unwrap().header.hash.clone())
        };
        send_message_to_channel(&tx, P2PMessage::Handshake { 
            genesis_hash: my_genesis, 
            current_height: my_height, 
            sender_port: my_port.clone() 
        }).await;

        // La boucle d'écoute existante...
        // On lit directement sur read_half
        while let Some(message) = read_p2p_message(&mut read_half, &peer_ip).await {
            match message {
                P2PMessage::Handshake { genesis_hash, current_height, sender_port } => {
                    actual_peer_id = format!("{}:{}_{}", peer_ip, sender_port, random_id);
                    known_peers.lock().unwrap().insert(actual_peer_id.clone());
                    
                    {
                        let mut ap = active_peers.lock().unwrap();
                        if let Some(sender) = ap.remove(&temp_peer_id) {
                            ap.insert(actual_peer_id.clone(), sender);
                        }
                    } 

                    let (is_behind, i_am_ahead, my_height, genesis_valid, my_l2_index, locator_hashes) = {
                        let chain = blockchain.lock().unwrap(); 
                        let my_h = chain.current_height + 1;
                        let genesis_ok = genesis_hash == chain.get_block_by_height(0).unwrap().header.hash;
                        
                        // 1. Récupérer notre index L2 actuel
                        let mut l2_idx = 0;
                        if let Ok(l2_tree) = chain.db.open_tree("l2_blocks") {
                            if let Some(Ok((_, value))) = l2_tree.iter().rev().next() {
                                if let Ok(last_mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                                    l2_idx = last_mb.micro_index;
                                }
                            }
                        }

                        // 2. Générer les locators L1
                        let mut locators = Vec::new();
                        let len = my_h as usize;
                        if len > 0 {
                            locators.push(chain.get_block_by_height((len - 1) as u64).unwrap().header.hash.clone());
                            if len > 1 { locators.push(chain.get_block_by_height((len - 2) as u64).unwrap().header.hash.clone()); }
                            let mut idx = len.saturating_sub(2).saturating_sub(5);
                            while idx > 0 && locators.len() < 10 {
                                locators.push(chain.get_block_by_height(idx as u64).unwrap().header.hash.clone());
                                idx = idx.saturating_sub(5);
                            }
                            if locators.last() != Some(&chain.get_block_by_height(0).unwrap().header.hash) {
                                locators.push(chain.get_block_by_height(0).unwrap().header.hash.clone()); 
                            }
                        }
                        
                        (current_height > my_h, my_h > current_height, my_h, genesis_ok, l2_idx, locators)
                    }; 

                    if !genesis_valid { break; }

                    // On demande une synchro si le L1 est en retard, OU si le L1 est à la même hauteur (pour vérifier le L2)
                    if is_behind || current_height == my_height {
                        send_message_to_channel(&tx, P2PMessage::SyncRequest { locator_hashes, last_l2_index: my_l2_index, sender_port: my_port.clone() }).await;
                    } else if i_am_ahead {
                        send_message_to_channel(&tx, P2PMessage::Handshake { genesis_hash, current_height: my_height, sender_port: my_port.clone() }).await;
                    } else {
						// Nous sommes à la même hauteur, on demande directement la mempool !
						send_message_to_channel(&tx, P2PMessage::GetMempool).await;
					}
                },

                P2PMessage::SyncRequest { locator_hashes, last_l2_index, sender_port: _ } => {
                    let data_to_send = {
                        let chain = blockchain.lock().unwrap();
                        let mut found_idx = 0; 
                        
                        // RECHERCHE DYNAMIQUE DE L'ANCÊTRE
                        for locator in locator_hashes {
                            let mut found = false;
                            for i in (0..=chain.current_height).rev() {
                                if chain.get_block_by_height(i).unwrap().header.hash == locator {
                                    found_idx = i as usize;
                                    found = true; break;
                                }
                            }
                            if found { break; }
                        }
                        
                        let mut blocks = Vec::new();
                        if (found_idx as u64) < chain.current_height {
                            for i in (found_idx as u64 + 1)..=chain.current_height {
                                if let Some(b) = chain.get_block_by_height(i) {
                                    blocks.push(b);
                                }
                            }
                        }

                        // RECHERCHE DES MICROBLOCS L2 MANQUANTS
                        let mut m_blocks = Vec::new();
                        if let Ok(l2_tree) = chain.db.open_tree("l2_blocks") {
                            for item in l2_tree.iter() {
                                if let Ok((_, value)) = item {
                                    if let Ok(mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                                        if mb.micro_index > last_l2_index {
                                            m_blocks.push(mb);
                                        }
                                    }
                                }
                            }
                        }

                        if !blocks.is_empty() || !m_blocks.is_empty() {
                            Some((blocks, m_blocks))
                        } else {
                            None
                        }
                    }; 

					// Extrait les vecteurs, ou crée des vecteurs vides si data_to_send est None
					let (blocks_to_send, micro_blocks_to_send) = data_to_send.unwrap_or_default();

					// On répond TOUJOURS pour ne pas bloquer la machine à états de l'autre nœud
					send_message_to_channel(&tx, P2PMessage::SyncResponse { 
						blocks: blocks_to_send, 
						micro_blocks: micro_blocks_to_send 
					}).await;
                },
                
                P2PMessage::SyncResponse { blocks, micro_blocks } => {
                    if blocks.is_empty() && micro_blocks.is_empty() {
                        println!("✅ [SYNC] Nœud parfaitement à jour (L1 et L2). Récupération de la Mempool...");
                        send_message_to_channel(&tx, P2PMessage::GetMempool).await;
                        continue;
                    }
                    
                    let mut needs_sync_request = false;
                    let mut locators = Vec::new();

                    // ===================================
                    // 1. SYNCHRONISATION DU L1
                    // ===================================
                    if !blocks.is_empty() {
                        let incoming_last = blocks.last().unwrap();
                        
                        let mut chain = blockchain.lock().unwrap(); 
                        let current_height = chain.current_height + 1;

                        if incoming_last.header.index < current_height {
                            let our_hash = &chain.get_block_by_height(incoming_last.header.index).unwrap().header.hash;
                            if our_hash == &incoming_last.header.hash {
                                // On est déjà à jour sur ce bloc L1
                            } else {
                                // Cas de fork
                                println!("📥 [SYNC] Bloc téléchargé ! (Index {})", incoming_last.header.index);
                                if sync_blocks_with_pools(&mut chain, &blocks, &mempool, &dex_pool) {
                                    println!("✅ [SYNC] Rattrapage réussi ! La blockchain locale est à jour (Taille: {}).", chain.current_height + 1);
                                    
                                    let now = chrono::Local::now().format("%d-%m-%Y %H:%M:%S");
                                    let tx_count = incoming_last.transactions.len();
                                    let tx_detail = if tx_count == 1 { 
                                        "1 Coinbase".to_string() 
                                    } else { 
                                        format!("1 Coinbase + {} Publique/Swap/Loto", tx_count - 1) 
                                    };
                                    println!("\n====================================================================");
                                    println!("🔄 [SYNC] BLOC {} RATTRAPÉ ET REDIFFUSÉ !", incoming_last.header.index);
                                    println!("🕒 Synchronisé le : {}", now);
                                    println!("🔗 Hash           : {}", incoming_last.header.hash);
                                    println!("📝 Contenu        : {} transactions incluses ({})", tx_count, tx_detail);
                                    println!("====================================================================");
                                    
                                    let env = P2PMessage::NewBlock { 
                                        block: incoming_last.clone(), 
                                        sender_port: my_port.clone() 
                                    };
                                    if let Ok(payload) = bincode::serialize(&env) {
                                        let length = (payload.len() as u32).to_be_bytes();
                                        let mut framed = Vec::with_capacity(4 + payload.len());
                                        framed.extend_from_slice(&length);
                                        framed.extend_from_slice(&payload);
                                        let ap = active_peers.lock().unwrap().clone();
                                        for (peer_id, sender) in ap.iter() {
                                            if peer_id != &actual_peer_id {
                                                let _ = sender.try_send(framed.clone()); 
                                            }
                                        }
                                    }
                                    needs_sync_request = true;
                                    locators = vec![incoming_last.header.hash.clone()];
                                } else {
                                    println!("❌ [SYNC] Échec de la fusion !");
                                }
                            }
                        } else {
                            println!("📥 [SYNC] Bloc téléchargé ! (Index {})", incoming_last.header.index);
                            
                            if sync_blocks_with_pools(&mut chain, &blocks, &mempool, &dex_pool) { 
                                println!("✅ [SYNC] Rattrapage réussi ! La blockchain locale est à jour (Taille: {}).", chain.current_height + 1);

                                // On relaie la bonne nouvelle au reste du réseau !
                                if let Some(last_block) = blocks.last() {
                                    
                                    let now = chrono::Local::now().format("%d-%m-%Y %H:%M:%S");
                                    let tx_count = last_block.transactions.len();
                                    let tx_detail = if tx_count == 1 { 
                                        "1 Coinbase".to_string() 
                                    } else { 
                                        format!("1 Coinbase + {} Publique/Swap/Loto", tx_count - 1) 
                                    };

                                    println!("\n====================================================================");
                                    println!("🔄 [SYNC] BLOC {} RATTRAPÉ ET REDIFFUSÉ !", last_block.header.index);
                                    println!("🕒 Synchronisé le : {}", now);
                                    println!("🔗 Hash           : {}", last_block.header.hash);
                                    println!("📝 Contenu        : {} transactions incluses ({})", tx_count, tx_detail);
                                    println!("====================================================================");

                                    let env = P2PMessage::NewBlock { 
                                        block: last_block.clone(), 
                                        sender_port: my_port.clone() 
                                    };
                                    if let Ok(payload) = bincode::serialize(&env) {
                                        let length = (payload.len() as u32).to_be_bytes();
                                        let mut framed = Vec::with_capacity(4 + payload.len());
                                        framed.extend_from_slice(&length);
                                        framed.extend_from_slice(&payload);

                                        let ap = active_peers.lock().unwrap().clone();
                                        for (peer_id, sender) in ap.iter() {
                                            if peer_id != &actual_peer_id {
                                                let _ = sender.try_send(framed.clone()); 
                                            }
                                        }
                                    }
                                }

                                needs_sync_request = true;
                                locators = vec![incoming_last.header.hash.clone()];

                            } else {
                                println!("❌ [SYNC] Échec de la fusion !");
                            }
                        }
                    }

                    // ===================================
                    // 2. SYNCHRONISATION DU L2
                    // ===================================
                    if !micro_blocks.is_empty() {
                        let mut chain = blockchain.lock().unwrap();
                        let mut added_l2 = 0;
                        
                        for mb in micro_blocks {
                            // Le tribunal L2 s'occupe de valider l'ancre parent L1 et la signature
                            if let Ok(_) = chain.validate_and_add_microblock(mb) {
                                added_l2 += 1;
                            }
                        }
                        if added_l2 > 0 {
                            println!("✅ [SYNC] {} MicroBlocs L2 synchronisés avec succès via P2P !", added_l2);
                        }
                    }

                    // ===================================
                    // 3. CONTINUITÉ
                    // ===================================
                    if needs_sync_request {
                        let my_l2_index = {
                            let chain = blockchain.lock().unwrap();
                            let mut idx = 0;
                            if let Ok(l2_tree) = chain.db.open_tree("l2_blocks") {
                                if let Some(Ok((_, value))) = l2_tree.iter().rev().next() {
                                    if let Ok(last_mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                                        idx = last_mb.micro_index;
                                    }
                                }
                            }
                            idx
                        };
                        send_message_to_channel(&tx, P2PMessage::SyncRequest { locator_hashes: locators, last_l2_index: my_l2_index, sender_port: my_port.clone() }).await;
                    } else {
                        // La blockchain est totalement synchronisée (L1 et L2 à jour), on aspire la mempool !
                        send_message_to_channel(&tx, P2PMessage::GetMempool).await;
                    }
                },

                P2PMessage::NewBlock { block, sender_port } => {
					// DÉCLENCHEMENT DU KILL SWITCH
					crate::network::HIGHEST_KNOWN_BLOCK.fetch_max(block.header.index, Ordering::Relaxed);
                    // Clones pour envoyer dans le thread d'arrière-plan
                    let bc_clone = Arc::clone(&blockchain);
                    let block_clone = block.clone();
                    let my_port_clone = my_port.clone();
                    let tx_clone = tx.clone();
                    let mempool_clone = Arc::clone(&mempool);
                    let dex_pool_clone = Arc::clone(&dex_pool);
                    let active_peers_clone = Arc::clone(&active_peers);
                    let actual_peer_id_clone = actual_peer_id.clone();
					let peer_ip_task = peer_ip.clone();

                    // On libère IMMÉDIATEMENT le port TCP (le réseau respire)
                    tokio::spawn(async move {
                        // BOUCLIER ANTI-DOS : VÉRIFICATION PoW *AVANT* LE RESTE ET *HORS MUTEX*
                        let (seed, target) = {
                            let chain = bc_clone.lock().unwrap();
                            (chain.get_epoch_seed(block_clone.header.index), chain.target.clone())
                        };

                        let is_pow_valid = tokio::task::spawn_blocking({
                            let b_clone = block_clone.clone();
                            let s = seed.clone();
                            move || {
                                let flags = randomx_rs::RandomXFlag::get_recommended_flags();
                                if let Ok(cache) = randomx_rs::RandomXCache::new(flags, s.as_bytes()) {
                                    if let Ok(vm) = randomx_rs::RandomXVM::new(flags, Some(cache), None) {
                                        let miner_address = b_clone.transactions.get(0)
                                            .and_then(|tx| tx.outputs.get(0))
                                            .map(|out| out.stealth_address.replace("COINBASE_", ""))
                                            .unwrap_or_default();

                                        let header_data = format!("{}{}{}{}{}{}{}", 
                                            miner_address, b_clone.header.index, b_clone.header.timestamp, 
                                            b_clone.header.previous_hash, b_clone.header.nonce,
                                            b_clone.header.l2_root, b_clone.header.tx_root 
                                        );

                                        if let Ok(hash_bytes) = vm.calculate_hash(header_data.as_bytes()) {
                                            let hash_hex = hex::encode(&hash_bytes);
                                            let hash_bigint = num_bigint::BigUint::parse_bytes(hash_hex.as_bytes(), 16).unwrap_or_default();
                                            return hash_hex == b_clone.header.hash && hash_bigint <= target;
                                        }
                                    }
                                }
                                false
                            }
                        }).await.unwrap();

                        if !is_pow_valid {
                            println!("❌ [SÉCURITÉ] Bloc rejeté : Preuve de Travail (PoW) invalide ! Spam détecté.");
                            crate::network::ban_peer(&peer_ip_task, "Faux PoW (Spam CPU)");
                            return; 
                        }

                        // BOUCLIER ZKP : VÉRIFICATION DES TX SEULEMENT SI LE POW EST BON !
                        let mut all_math_valid = true;
                        for tx in &block_clone.transactions {
                            if tx.tx_type != TransactionType::Coinbase && tx.tx_type != TransactionType::MicroCoinbase {
                                if !tx.is_valid() {
                                    all_math_valid = false;
                                    break;
                                }
                            }
                        }
                        
                        if !all_math_valid {
                            println!("❌ [SÉCURITÉ] Bloc frauduleux ! Cryptographie invalide (Rejeté).");
                            crate::network::ban_peer(&peer_ip_task, "Cryptographie falsifiée");
                            return; 
                        }

                        // INSERTION SÉCURISÉE (Le bloc est 100% sûr, on peut verrouiller)
                        let bc_clone_blocking = Arc::clone(&bc_clone); 
                        let validation_result = tokio::task::spawn_blocking(move || {
							let mut chain = bc_clone_blocking.lock().unwrap();
							let current_height = chain.current_height + 1;
							
							// Anti-doublon ultra-rapide avant la grosse validation
							if block_clone.header.index < current_height {
								let our_hash = &chain.get_block_by_height(block_clone.header.index).unwrap().header.hash;
								if our_hash == &block_clone.header.hash {
									return Ok(false); // FAUX : Bloc déjà connu, on arrête les frais.
								}
							}

							if let Err(_) = chain.validate_and_add_external_block(block_clone) {
                                // Préparation des locators pour la synchro en cas de rejet
                                let mut locators = Vec::new();
                                let len = (chain.current_height + 1) as usize;
                                if len > 0 {
                                    locators.push(chain.get_block_by_height((len - 1) as u64).unwrap().header.hash.clone());
                                    if len > 1 { locators.push(chain.get_block_by_height((len - 2) as u64).unwrap().header.hash.clone()); }
                                    let mut idx = len.saturating_sub(2).saturating_sub(5);
                                    while idx > 0 && locators.len() < 10 {
                                        locators.push(chain.get_block_by_height(idx as u64).unwrap().header.hash.clone());
                                        idx = idx.saturating_sub(5);
                                    }
                                    if locators.last() != Some(&chain.get_block_by_height(0).unwrap().header.hash) {
                                        locators.push(chain.get_block_by_height(0).unwrap().header.hash.clone()); 
                                    }
                                }
                                Err((chain.get_block_by_height(0).unwrap().header.hash.clone(), len as u64, locators))
                            } else {
								Ok(true) // VRAI : C'est un vrai nouveau bloc validé !
							}
                        }).await.unwrap();

                        // 4. Gestion du résultat réseau
                        match validation_result {
                            Err((_, my_height, locator_hashes)) => {
                                // SI LE BLOC EST INVALIDE, ON RELÂCHE LE KILL SWITCH POUR LE MINEUR !
                                crate::network::HIGHEST_KNOWN_BLOCK.store(my_height.saturating_sub(1), Ordering::Relaxed);
                                
                                let my_l2_index = {
                                    let chain = bc_clone.lock().unwrap();
                                    let mut idx = 0;
                                    if let Ok(l2_tree) = chain.db.open_tree("l2_blocks") {
                                        if let Some(Ok((_, value))) = l2_tree.iter().rev().next() {
                                            if let Ok(last_mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                                                idx = last_mb.micro_index;
                                            }
                                        }
                                    }
                                    idx
                                };

                                send_message_to_channel(&tx_clone, P2PMessage::SyncRequest { locator_hashes, last_l2_index: my_l2_index, sender_port: my_port_clone.clone() }).await;
                            },
                            Ok(is_new) => {
								// BOUCLIER ANTI-TEMPÊTE
								if !is_new {
									return; // On coupe la propagation instantanément !
								}
								
								let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                                let tx_count = block.transactions.len();
                                let tx_detail = if tx_count == 1 { "1 Coinbase".to_string() } else { format!("1 Coinbase + {} Publique/Swap", tx_count - 1) };

                                println!("\n====================================================================");
                                println!("🌍 [RÉSEAU] NOUVEAU BLOC {} REÇU VIA P2P ! (Source: {})", block.header.index, sender_port);
                                println!("🕒 Reçu le : {}", now);
                                println!("🔗 Hash    : {}", block.header.hash);
                                println!("📝 Contenu : {} transactions incluses ({})", tx_count, tx_detail);
                                println!("====================================================================");
								println!("✅ Bloc {} validé et ajouté à la chaîne locale.", block.header.index);
                                
                                // PURGE UNIQUE : minées, conflits, HTLC réglés/expirés, parts périmées
                                {
                                    let chain = bc_clone.lock().unwrap();
                                    let mut mp = mempool_clone.lock().unwrap();
                                    crate::network::purge_mempool(&chain, &mut mp);
                                }
                                
                                {
									let mut dp = dex_pool_clone.lock().unwrap();
									crate::network::apply_block_to_dex(&mut dp, &block);
									println!("🧹 [DEX] Bloc reçu : Dark Pool synchronisé (ordres restants: {}).", dp.len());
								}
                                
                                let env = P2PMessage::NewBlock { block: block.clone(), sender_port: my_port_clone };
								if let Ok(payload) = bincode::serialize(&env) {
									let length = (payload.len() as u32).to_be_bytes();
									let mut framed = Vec::with_capacity(4 + payload.len());
									framed.extend_from_slice(&length);
									framed.extend_from_slice(&payload);

									let ap = active_peers_clone.lock().unwrap().clone();
									for (peer_id, sender) in ap.iter() {
										// Utilisation des bonnes variables du scope
										if peer_id != &actual_peer_id_clone {
											let _ = sender.try_send(framed.clone());
										}
									}
								}
                            }
                        }
                    });
                },

                P2PMessage::WhisperTransaction { tx: in_tx } => {
					// SÉCURITÉ P2P : On drop les Coinbase volantes !
					if in_tx.tx_type == TransactionType::Coinbase || in_tx.tx_type == TransactionType::MicroCoinbase {
						println!("🚨 [SÉCURITÉ] Drop d'une transaction Coinbase ou MicroCoinbase illégale reçue via P2P.");
						continue; 
					}
                    let mut rng = rand::thread_rng();
                    if rng.gen_range(1..=10) <= 2 {
                        let chain = blockchain.lock().unwrap();
                        let mut pool = mempool.lock().unwrap();
                        if check_tx_admission(&chain, &pool, &in_tx).is_ok() {
                            pool.push(in_tx);
                        }
                    }
                },

                P2PMessage::BroadcastTransaction { tx: in_tx } => {
					// 1. REJET DES TRANSACTIONS SYSTÈMES EN P2P
					if matches!(in_tx.tx_type, TransactionType::Coinbase | TransactionType::MicroCoinbase | TransactionType::DexSettlement { .. } | TransactionType::LotteryPayout { .. }) {
						println!("🚨 [SÉCURITÉ] Tentative d'injection d'une transaction système via P2P. Bloquée.");
						continue; 
					}

					// 2. LE BOUCLIER QUALITATIF (P2Pool Mining Share)
					if let TransactionType::MiningShare { miner_address, nonce, hash, timestamp, .. } = in_tx.tx_type.clone() {
						
						let (target, current_height, previous_hash, seed) = {
							let chain = blockchain.lock().unwrap();
							let height = chain.current_height + 1;
							let prev_hash = if height > 0 { chain.get_last_block().header.hash.clone() } else { String::new() };
							(chain.target.clone(), height, prev_hash, chain.get_epoch_seed(height))
						};

						// KILL SWITCH 1 : Si la part appartient à un vieux bloc, on la jette sans calcul !
						let highest_known = crate::network::HIGHEST_KNOWN_BLOCK.load(Ordering::Relaxed);
						if current_height < highest_known {
							continue; 
						}

						// Filtre Mathématique ultra-rapide (Instant Kill)
						let hash_bigint = num_bigint::BigUint::parse_bytes(hash.as_bytes(), 16).unwrap_or_default();
						if hash_bigint > (&target * 20u32) {
							continue;
						}

						// Filtre Anti-Exhaustion CPU
						{
							let pool = mempool.lock().unwrap();
							if pool.iter().filter(|t| matches!(t.tx_type, TransactionType::MiningShare { .. })).count() > 500 {
								continue; 
							}
						}

						let tx_clone = in_tx.clone();
						let mp_clone_bg = Arc::clone(&mempool);
						let ap_clone_bg = Arc::clone(&active_peers);
						let actual_peer_id_clone = actual_peer_id.clone();

						tokio::task::spawn_blocking(move || {
							// KILL SWITCH 2 : Juste avant de lancer le hachage lourd, on revérifie si le bloc a changé !
							if crate::network::HIGHEST_KNOWN_BLOCK.load(Ordering::Relaxed) > current_height {
								return; // On avorte la tâche pour libérer le CPU
							}

							let parts: Vec<&str> = tx_clone.public_key.split('|').collect();
							let l2_root = parts.get(0).cloned().unwrap_or("");
							let tx_root = parts.get(1).cloned().unwrap_or("");
							
							let header_data = format!("{}{}{}{}{}{}{}", miner_address, current_height, timestamp, previous_hash, nonce, l2_root, tx_root);
							
							let flags = randomx_rs::RandomXFlag::get_recommended_flags();
							if let Ok(cache) = randomx_rs::RandomXCache::new(flags, seed.as_bytes()) {
								if let Ok(vm) = randomx_rs::RandomXVM::new(flags, Some(cache), None) {
									if let Ok(hash_bytes) = vm.calculate_hash(header_data.as_bytes()) {
										if hex::encode(&hash_bytes) == *hash {
											let mut pool = mp_clone_bg.lock().unwrap();
											let tx_hash = in_tx.hash_data();
											if !pool.iter().any(|t| t.hash_data() == tx_hash) {
												println!("⛏️ [P2POOL] Nouvelle part de minage relayée !");
												let tx_to_propagate = in_tx.clone();
												pool.push(in_tx);
												// On mémorise le tour vérifié : la purge ne garde que le bloc en cours et le suivant
												crate::network::SHARE_ROUNDS.lock().unwrap().insert(hash.clone(), current_height);

												let envelope = P2PMessage::BroadcastTransaction { tx: tx_to_propagate };
												if let Ok(payload) = bincode::serialize(&envelope) {
													let length = (payload.len() as u32).to_be_bytes();
													let mut framed = Vec::with_capacity(4 + payload.len());
													framed.extend_from_slice(&length);
													framed.extend_from_slice(&payload);
													
													let ap = ap_clone_bg.lock().unwrap().clone();
													for (peer_id, sender) in ap.iter() {
														if peer_id != &actual_peer_id_clone {
															let _ = sender.try_send(framed.clone());
														}
													}
												}
											}
										}
									}
								}
							}
						});
						
						continue;
					}

					// 3. TRAITEMENT CLASSIQUE POUR LES AUTRES TRANSACTIONS (HTLC, Envoi Classique...)
					let admitted = {
						let chain = blockchain.lock().unwrap();
						let mut pool = mempool.lock().unwrap();
						match check_tx_admission(&chain, &pool, &in_tx) {
							Ok(()) => { pool.push(in_tx.clone()); true }
							Err(e) => {
								if !e.starts_with("✅") { println!("⛔ [P2P] TX refusée : {}", e); }
								false
							}
						}
					};
					if admitted {
						println!("📥 [MEMPOOL] Nouvelle TX reçue via P2P !"); 
						let envelope = P2PMessage::BroadcastTransaction { tx: in_tx };
						if let Ok(payload) = bincode::serialize(&envelope) {
							let length = (payload.len() as u32).to_be_bytes();
							let mut framed = Vec::with_capacity(4 + payload.len());
							framed.extend_from_slice(&length);
							framed.extend_from_slice(&payload);

							let ap = active_peers.lock().unwrap().clone();
							for (peer_id, sender) in ap.iter() {
								if peer_id != &actual_peer_id {
									let _ = sender.try_send(framed.clone());
								}
							}
						}
					}
				},

                P2PMessage::GetMempool => {
                    // Les parts de minage ne sont pas envoyées : le receveur ne pourrait pas vérifier leur tour
                    let pool: Vec<Transaction> = mempool.lock().unwrap().iter()
                        .filter(|t| !matches!(t.tx_type, TransactionType::MiningShare { .. })).cloned().collect();
                    send_message_to_channel(&tx, P2PMessage::MempoolSync { txs: pool }).await;
                    // DEX : on transmet aussi les ordres actifs (sinon un nœud qui arrive ne les voit jamais)
                    let now = chrono::Utc::now().timestamp();
                    let orders: Vec<Order> = dex_pool.lock().unwrap().iter()
                        .filter(|o| o.expires_at > now).cloned().collect();
                    for order in orders {
                        send_message_to_channel(&tx, P2PMessage::BroadcastOrder { order }).await;
                    }
                },

                P2PMessage::MempoolSync { txs } => {
                    // Ordre strict (Chain -> Mempool) pour éviter le deadlock !
                    let chain = blockchain.lock().unwrap(); 
                    let mut local_mp = mempool.lock().unwrap(); 
                    let mut added = 0;
                    for t in txs {
                        // Même porte d'entrée que le reste (parts de minage refusées : tour invérifiable ici)
                        if check_tx_admission(&chain, &local_mp, &t).is_ok() {
                            local_mp.push(t);
                            added += 1;
                        }
                    }
                    if added > 0 { println!("📥 [PULL] {} transaction(s) aspirée(s) !", added); }
                },

				P2PMessage::CancelOrder { token } => {
                    use sha2::Digest;
                    let id = hex::encode(sha2::Sha256::digest(&hex::decode(&token).unwrap_or_default()));
                    // Déjà connue → on s'arrête (anti-boucle)
                    if !close_order(&id) { continue; }
                    dex_pool.lock().unwrap().retain(|o| o.id != id);
                    println!("🗑️ [P2P DEX] Ordre annulé par le réseau : {}...", &id[..16]);
                    let envelope = P2PMessage::CancelOrder { token };
                    if let Ok(payload) = bincode::serialize(&envelope) {
                        let length = (payload.len() as u32).to_be_bytes();
                        let mut framed = Vec::with_capacity(4 + payload.len());
                        framed.extend_from_slice(&length);
                        framed.extend_from_slice(&payload);
                        let ap = active_peers.lock().unwrap().clone();
                        for (peer_id, sender) in ap.iter() {
                            if peer_id != &actual_peer_id {
                                let _ = sender.try_send(framed.clone());
                            }
                        }
                    }
                },

                P2PMessage::BroadcastOrder { order } => {
                    // Ordre invalide, expiré, > 2 h, annulé ou déjà exécuté → ignoré
                    if check_order(&order).is_err() { continue; }
                    // ANTI DOUBLE-MATCH : un achat dont le hash a déjà un swap on-chain est périmé
                    if let Some(h) = &order.htlc_hash {
                        if blockchain.lock().unwrap().get_htlc_state(h).swap.is_some() { continue; }
                    }
                    let is_new = {
                        let mut pool = dex_pool.lock().unwrap();
                        if pool.len() >= MAX_DEX_ORDERS || pool.iter().any(|o| o.id == order.id) {
                            false
                        } else {
							println!("🌊 [P2P DEX] Ordre reçu du réseau : {} {} FLAME ( {} WATT)", order.order_type, order.amount_flames, order.amount_flames as f64 / 1_000_000_000.0);
							pool.push(order.clone());
                            true
                        }
                    };
                    // RELAIS : on propage aux autres pairs (le dédoublonnage par id empêche les boucles)
                    if is_new {
                        let envelope = P2PMessage::BroadcastOrder { order };
                        if let Ok(payload) = bincode::serialize(&envelope) {
                            let length = (payload.len() as u32).to_be_bytes();
                            let mut framed = Vec::with_capacity(4 + payload.len());
                            framed.extend_from_slice(&length);
                            framed.extend_from_slice(&payload);
                            let ap = active_peers.lock().unwrap().clone();
                            for (peer_id, sender) in ap.iter() {
                                if peer_id != &actual_peer_id {
                                    let _ = sender.try_send(framed.clone());
                                }
                            }
                        }
                    }
                },
				
				P2PMessage::BroadcastMicroBlock { micro_block } => {
                    let bc_clone = Arc::clone(&blockchain);
                    let mp_clone = Arc::clone(&mempool);
                    let ap_clone = Arc::clone(&active_peers);
                    let actual_peer_id_clone = actual_peer_id.clone();

                    tokio::spawn(async move {
                        // 1. BOUCLIER DE LATENCE : On attend l'arrivée du parent L1 si besoin (Max 60 sec)
                        let mut parent_l1_ready = false;
                        for _ in 0..120 {
                            {
                                let chain = bc_clone.lock().unwrap();
                                for i in (0..=chain.current_height).rev().take(10) {
                                    if let Some(b) = chain.get_block_by_height(i) {
                                        if b.header.hash == micro_block.l1_parent_hash {
                                            parent_l1_ready = true;
                                            break;
                                        }
                                    }
                                }
                            }
                            if parent_l1_ready { break; }
                            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                        }

                        if !parent_l1_ready {
                            println!("⚠️ [L2] Microbloc orphelin rejeté (Parent L1 '{}' inconnu).", micro_block.l1_parent_hash);
                            return;
                        }

                        // 2. LE TRIBUNAL CONSENSUS L2 (Dans un thread dédié pour ne pas bloquer le P2P)
                        let mb_clone = micro_block.clone();
                        let validation_result = tokio::task::spawn_blocking(move || {
                            let mut chain = bc_clone.lock().unwrap();
                            chain.validate_and_add_microblock(mb_clone)
                        }).await.unwrap();

                        match validation_result {
							Ok(()) => {
								{
									let mut mp = mp_clone.lock().unwrap();
									mp.retain(|tx| !micro_block.transactions.iter().any(|m_tx| m_tx.hash_data() == tx.hash_data()));
								}
								
								let envelope = P2PMessage::BroadcastMicroBlock { micro_block: micro_block.clone() };
								if let Ok(payload) = bincode::serialize(&envelope) {
									let length = (payload.len() as u32).to_be_bytes();
									let mut framed = Vec::with_capacity(4 + payload.len());
									framed.extend_from_slice(&length);
									framed.extend_from_slice(&payload);

									let ap = ap_clone.lock().unwrap().clone();
									for (peer_id, sender) in ap.iter() {
										if peer_id != &actual_peer_id_clone { 
											let _ = sender.try_send(framed.clone()); 
										}
									}
								}
							},
                            Err(e) => {
                                // Le Tribunal L2 a parlé. Le bloc est une fraude.
                                println!("{}", e); 
                            }
                        }
                    });
                },
				
				P2PMessage::RelayOnion { packet } => {
                    // Le code du Nœud Relais
                    let my_secret = "HEX_SECRET_KYBER_DU_NOEUD"; // Plus tard on chargera la vraie clé
                    match packet.peel(my_secret) {
                        Ok(hop_payload) => {
                            if hop_payload.next_hop_address.is_empty() {
                                println!("🎯 [MIXNET] Destination finale atteinte. Traitement de la requête.");
                            } else {
                                println!("🧅 [MIXNET] Couche épluchée. Transfert aveugle vers : {}", hop_payload.next_hop_address);
                                
                                // Décodage Binaire
                                if let Ok(next_packet) = bincode::deserialize::<OnionPacket>(&hop_payload.inner_data) {
                                    let target_ip = hop_payload.next_hop_address.clone();
                                    tokio::spawn(async move {
                                        if let Ok(mut stream) = tokio::net::TcpStream::connect(&target_ip).await {
                                            use tokio::io::AsyncWriteExt;
                                            let envelope = P2PMessage::RelayOnion { packet: next_packet };
                                            let mut json_str = serde_json::to_string(&envelope).unwrap();
                                            json_str.push('\n');
                                            let _ = stream.write_all(json_str.as_bytes()).await;
                                        }
                                    });
                                }
                            }
                        },
                        Err(e) => { println!("❌ [MIXNET] Rejet du paquet en oignon : {}", e); }
                    }
                },
				
				P2PMessage::NodeAnnouncement { kyber_pubkey, ip_port, is_lighthouse, timestamp, pow_hash, nonce } => {
					// A. Filtre temporel (Anti-Rejeu) : On refuse les annonces qui ont plus de 2 heures
					// On ne les "périme" pas de la base une fois entrées, c'est juste pour l'admission.
					let now = chrono::Utc::now().timestamp();
					if timestamp < now - 7200 || timestamp > now + 3600 {
						println!("🚨 [DHT] Annonce ignorée (Timestamp invalide/rejeu).");
						continue;
					}

					// B. Validation du PoW RandomX (Mode Light, 0 RAM)
					let header_data = format!("{}{}{}{}", kyber_pubkey, ip_port, timestamp, nonce);
					let flags = randomx_rs::RandomXFlag::get_recommended_flags();
					
					let is_pow_valid = {
						let chain = blockchain.lock().unwrap();
						let seed = chain.get_epoch_seed(chain.current_height);
						
						if let Ok(cache) = randomx_rs::RandomXCache::new(flags, seed.as_bytes()) {
							if let Ok(vm) = randomx_rs::RandomXVM::new(flags, Some(cache), None) {
								if let Ok(hash_bytes) = vm.calculate_hash(header_data.as_bytes()) {
									let calculated_hash = hex::encode(&hash_bytes);
									// On vérifie que le hash correspond ET qu'il respecte la difficulté (les 12 premiers bits à zéro) !
									calculated_hash == pow_hash && hash_bytes[0] == 0 && hash_bytes[1] < 16
								} else { false }
							} else { false }
						} else { false }
					};

					if !is_pow_valid {
						println!("🚨 [DHT] Annonce ignorée (PoW Invalide). Spam détecté !");
						continue;
					}

					// C. Validation TCP Asynchrone & Écriture dans Sled
					// On clone les variables nécessaires pour le thread d'arrière-plan
					let kp_clone = kyber_pubkey.clone();
					let ip_clone = ip_port.clone();
					let chain_arc = Arc::clone(&blockchain); // On utilise la db de la blockchain
					
					tokio::spawn(async move {
						let mut is_valid = true;

						// Si le nœud se déclare "Phare", on le teste IMMÉDIATEMENT
						if is_lighthouse {
							// Timeout ultra court (2 secondes) pour ne pas engorger le réseau
							match tokio::time::timeout(Duration::from_secs(2), TcpStream::connect(&ip_clone)).await {
								Ok(Ok(_stream)) => {
									println!("📡 [DHT] Ping réussi ! Nœud Phare validé : {}", ip_clone);
									// (La connexion se coupe toute seule à la fin du bloc)
								}
								_ => {
									println!("🚨 [DHT] Échec Ping TCP sur {}. Le nœud ment sur son statut de Phare.", ip_clone);
									is_valid = false;
								}
							}
						} else {
							println!("📡 [DHT] Annonce de Nœud Caché valide reçue : {}", kp_clone);
						}

						// Si tout est bon, on l'écrit de manière persistante
						if is_valid {
							let record = DhtRecord {
								kyber_pubkey: kp_clone.clone(),
								ip_port: ip_clone,
								is_lighthouse,
								last_seen: chrono::Utc::now().timestamp(),
							};

							// On récupère l'instance Sled depuis l'Arc de la Blockchain
							let db = { chain_arc.lock().unwrap().db.clone() }; 
							if let Ok(dht_tree) = db.open_tree("dht_nodes") {
								if let Ok(bincode_data) = bincode::serialize(&record) {
									let _ = dht_tree.insert(kp_clone.as_bytes(), bincode_data);
									let _ = dht_tree.flush(); // Force l'écriture sur le disque
									println!("💾 [DHT] Identité {} sauvegardée sur le disque.", kp_clone);
								}
							}
						}
					});
				},
                
            }
        }
        
        println!("🔌 [P2P] Connexion perdue avec {}.", actual_peer_id);
        active_peers.lock().unwrap().remove(&actual_peer_id);
    });
}

pub async fn broadcast_mined_block(my_port: &str, block: Block, active_peers: ActivePeers) {
    let envelope = P2PMessage::NewBlock { block, sender_port: my_port.to_string() };
    if let Ok(payload) = bincode::serialize(&envelope) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);

        let peers = active_peers.lock().unwrap().clone();
        for (_peer_id, sender) in peers.iter() {
            let _ = sender.try_send(framed.clone());
        }
    }
}

pub async fn broadcast_transaction(tx: Transaction, active_peers: ActivePeers) {
    let envelope = P2PMessage::BroadcastTransaction { tx };
    if let Ok(payload) = bincode::serialize(&envelope) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);

        let peers = active_peers.lock().unwrap().clone();
        for (_peer_id, sender) in peers.iter() {
            let _ = sender.try_send(framed.clone());
        }
    }
}

pub async fn broadcast_order(order: Order, active_peers: ActivePeers) {
    let envelope = P2PMessage::BroadcastOrder { order };
    if let Ok(payload) = bincode::serialize(&envelope) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);

        let peers = active_peers.lock().unwrap().clone();
        for (_peer_id, sender) in peers.iter() {
            let _ = sender.try_send(framed.clone());
        }
    }
}

pub async fn broadcast_cancel_order(token: String, active_peers: ActivePeers) {
    let envelope = P2PMessage::CancelOrder { token };
    if let Ok(payload) = bincode::serialize(&envelope) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);

        let peers = active_peers.lock().unwrap().clone();
        for (_peer_id, sender) in peers.iter() {
            let _ = sender.try_send(framed.clone());
        }
    }
}

pub async fn broadcast_micro_block(micro_block: crate::block::MicroBlock, active_peers: ActivePeers) {
    let envelope = P2PMessage::BroadcastMicroBlock { micro_block };
    if let Ok(payload) = bincode::serialize(&envelope) {
        let length = (payload.len() as u32).to_be_bytes();
        let mut framed = Vec::with_capacity(4 + payload.len());
        framed.extend_from_slice(&length);
        framed.extend_from_slice(&payload);

        let peers = active_peers.lock().unwrap().clone();
        for (_peer_id, sender) in peers.iter() {
            let _ = sender.try_send(framed.clone());
        }
    }
}

pub fn setup_upnp(port: u16) {
    std::thread::spawn(move || {
        println!("🔌 [UPnP] Tentative de communication avec la box internet...");
        match igd::search_gateway(Default::default()) {
            Ok(gateway) => {
                // Astuce pour trouver notre propre IP locale sur le réseau
                let local_addr = match std::net::UdpSocket::bind("0.0.0.0:0") {
                    Ok(s) => {
                        if s.connect("8.8.8.8:53").is_ok() {
                            s.local_addr().ok().map(|a| a.ip())
                        } else { None }
                    },
                    Err(_) => None,
                };

                if let Some(std::net::IpAddr::V4(ipv4)) = local_addr {
                    let local_socket = std::net::SocketAddrV4::new(ipv4, port);
                    match gateway.add_port(igd::PortMappingProtocol::TCP, port, local_socket, 0, "Wattcoin Node") {
                        Ok(_) => println!("✅ [UPnP] Port TCP/{} ouvert automatiquement sur la box ! Votre nœud est joignable de l'extérieur.", port),
                        Err(e) => println!("⚠️ [UPnP] La box a refusé d'ouvrir le port (Erreur: {:?}). Les autres mineurs ne pourront pas initier la connexion vers vous.", e),
                    }
                }
            },
            Err(e) => println!("⚠️ [UPnP] Routeur introuvable ou UPnP désactivé ({:?}).", e),
        }
    });
}

pub fn ban_peer(ip: &str, reason: &str) {
    println!("🛑 [SÉCURITÉ] IP bannie ({}): {}", ip, reason);
    let unban_time = chrono::Utc::now().timestamp() + BAN_DURATION_SECS;
    BANNED_PEERS.lock().unwrap().insert(ip.to_string(), unban_time);
}