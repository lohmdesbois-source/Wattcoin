use crate::block::{Block, BlockHeader};
use crate::transaction::{Transaction, TransactionType, TransactionOutput};
use num_bigint::BigUint;
use std::collections::HashSet;
use randomx_rs::{RandomXFlag, RandomXCache, RandomXVM};
use crate::WattError;
use sha2::Digest;
// SLED : Import de la base de données
use sled::Db;

const FLAME: u64 = 1_000_000_000;
const MAX_BLOCK_SIZE_BYTES: usize = 32 * 1024 * 1024; // 32 Mo maximum par bloc !
const MAX_BLOCK_L2_SIZE_BYTES: usize = 2 * 1024 * 1024; // 2 Mo maximum par microbloc !
const MATURITY_BLOCKS: u64 = 12; // 12 Prod
const EXPECTED_BLOCK_TIME: u64 = 120;    // 2 mins (120 s)
const INITIAL_REWARD: u64 = 15 * FLAME; // 15 Watts
const TAIL_EMISSION: u64 = 600_000_000; // 0.6 Watts
const EMISSION_DECAY_SHIFT: u32 = 18;   // Ajusté pour ~21 ans
const INITIAL_DIFFICULTY_SHIFT: u32 = 12;
pub const LOTTERY_TIME_BLOCK: u64 = 10; // 720 blocks pour un jour
pub const EPOCH_BLOCKS: u64 = 255;  // toutes les 8H30 (8,5 Heures = 255 blocks)
const MONTANT_STAKE: u64 = 100; // 10 000 Pour la prod (840 $)
const FENETRE_DIFFICULTY: usize = 17; // 720 Pour la prod (un jour)

pub struct Blockchain {
    pub db: Db,
    pub current_height: u64, 
    pub target: BigUint, 
    pub spent_key_images: HashSet<String>, 
}

impl Blockchain {
    pub fn new(db_path: &str) -> Result<Self, WattError> {
        let db = sled::open(db_path).map_err(|e| WattError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        let max_target = BigUint::from_bytes_be(&[0xFF; 32]);
        let initial_target = &max_target >> INITIAL_DIFFICULTY_SHIFT;

        let mut blockchain = Blockchain {
            db,
            current_height: 0,
            target: initial_target,
            spent_key_images: HashSet::new(),
        };

        if blockchain.get_block_by_height(0).is_none() {
            println!("🌱 Initialisation du Genesis Block dans Sled.");
            let genesis = Block::genesis();
            blockchain.push_block(&genesis)?;
        } else {
            blockchain.current_height = blockchain.get_last_height();
            println!("💾 HISTORIQUE CHARGÉ DEPUIS SLED : {} blocs retrouvés.", blockchain.current_height + 1);
            blockchain.rebuild_spent_cache();
            blockchain.recalculate_target_from_scratch();
        }

        Ok(blockchain)
    }

    fn get_last_height(&self) -> u64 {
        if let Some(Ok((key, _))) = self.db.iter().rev().next() {
            let mut bytes = [0u8; 8];
            bytes.copy_from_slice(&key);
            return u64::from_be_bytes(bytes);
        }
        0 
    }

    pub fn get_block_by_height(&self, height: u64) -> Option<Block> {
        let key = height.to_be_bytes();
        match self.db.get(&key) {
            Ok(Some(ivec)) => bincode::deserialize(&ivec).ok(), 
            _ => None,
        }
    }

    pub fn get_last_block(&self) -> Block {
        self.get_block_by_height(self.current_height).expect("La blockchain est vide (Même pas de Genesis !)")
    }

    pub fn push_block(&mut self, block: &Block) -> Result<(), WattError> {
        let key = block.header.index.to_be_bytes();
        let value = bincode::serialize(block).map_err(|e| WattError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        self.db.insert(&key, value).map_err(|e| WattError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        self.db.flush().map_err(|e| WattError::Io(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        
        self.current_height = block.header.index;
        Ok(())
    }
    
    // ==========================================
    // Sauvegarde L2 propulsée par Sled
    // ==========================================
    pub fn push_microblock(&self, micro_block: &crate::block::MicroBlock) -> Result<(), WattError> {
        let l2_tree = self.db.open_tree("l2_blocks").unwrap();
        let key = micro_block.micro_index.to_be_bytes();
        
        // Anti-doublon direct via Sled (Ultra rapide)
        if !l2_tree.contains_key(&key).unwrap_or(false) {
            let value = bincode::serialize(micro_block).unwrap();
            l2_tree.insert(&key, value).unwrap();
            self.db.flush().unwrap();
        }
        Ok(())
    }

	fn rebuild_spent_cache(&mut self) {
        self.spent_key_images.clear();
        
        // Rechargement des dépenses L1
        for result in self.db.iter() {
            if let Ok((_, value)) = result {
                if let Ok(block) = bincode::deserialize::<Block>(&value) {
                    for tx in &block.transactions {
                        for input in &tx.inputs {
                            self.spent_key_images.insert(input.utxo_id.clone()); 
                        }
                        if tx.tx_type != TransactionType::Coinbase {
                            if let Some(sig) = &tx.wots_signature {
                                self.spent_key_images.insert(hex::encode(&sig.public_key));
                            }
                        }
                    }
                }
            }
        }
        
        // Rechargement des MicroBlocs L2 (Anti Double-Dépense absolue)
        if let Ok(l2_tree) = self.db.open_tree("l2_blocks") {
            for result in l2_tree.iter() {
                if let Ok((_, value)) = result {
                    if let Ok(mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                        for tx in &mb.transactions {
                            for input in &tx.inputs {
                                self.spent_key_images.insert(input.utxo_id.clone());
                            }
                            if tx.tx_type != TransactionType::MicroCoinbase {
                                if let Some(sig) = &tx.wots_signature {
                                    self.spent_key_images.insert(hex::encode(&sig.public_key));
                                }
                            }
                        }
                    }
                }
            }
        }
        println!("💾 [CACHE] Table des doubles-dépenses (UTXO/WOTS) reconstruite.");
    }
    
    pub fn get_epoch_seed(&self, height: u64) -> String {
		if height <= EPOCH_BLOCKS {
			return self.get_block_by_height(0).unwrap().header.hash.clone(); 
		}
		let epoch = (height - 1) / EPOCH_BLOCKS;
		let target_block = (epoch * EPOCH_BLOCKS).saturating_sub(11);
		
		if target_block <= self.current_height {
			self.get_block_by_height(target_block).unwrap().header.hash.clone()
		} else {
			self.get_block_by_height(0).unwrap().header.hash.clone()
		}
	}
	
    pub fn get_next_base_reward(prev_base_reward: u64) -> u64 {
        let decay = prev_base_reward >> EMISSION_DECAY_SHIFT;
        let expected = prev_base_reward.saturating_sub(decay);
        
        if expected < TAIL_EMISSION {
            TAIL_EMISSION
        } else {
            expected
        }
    }
	
    pub fn get_total_supply(&self) -> u64 {
        let mut supply = 0;
        
        for result in self.db.iter() {
            if let Ok((_, value)) = result {
                if let Ok(block) = bincode::deserialize::<Block>(&value) {
                    let mut block_fees = 0;
                    
                    for tx in &block.transactions {
                        if tx.tx_type != TransactionType::Coinbase {
                            block_fees += tx.fee;
                        }
                    }
                    
                    for tx in &block.transactions {
                        if tx.tx_type == TransactionType::Coinbase {
                            let mut coinbase_total = 0;
                            
                            for out in &tx.outputs {
                                if let Ok(val) = out.aes_vault.parse::<u64>() {
                                    coinbase_total += val;
                                }
                            }
                            supply += coinbase_total.saturating_sub(block_fees);
                        }
                    }
                }
            }
        }
        supply
    }

    // Mise à jour pour lire l'arbre Sled
    pub fn get_jackpot_info(&self, _target_height: u64) -> (u64, Vec<(String, String)>) {
        let mut tickets = Vec::new();
        let mut pot = 0u64;

        let mut valid_l1_hashes = std::collections::HashSet::new();

        'block_loop: for result in self.db.iter().rev() {
            if let Ok((_, value)) = result {
                if let Ok(block) = bincode::deserialize::<Block>(&value) {
                    valid_l1_hashes.insert(block.header.hash.clone()); 
                    
                    for tx in &block.transactions {
                        if let TransactionType::HTLCLottery { player_pubkey, .. } = &tx.tx_type {
                            if !tx.outputs.is_empty() {
                                let ticket_id = tx.outputs[0].kyber_capsule.clone();
                                tickets.push((ticket_id, player_pubkey.clone()));
                            }
                        }
                        
                        if tx.tx_type == TransactionType::Coinbase || matches!(tx.tx_type, TransactionType::HTLCLottery { .. }) {
                            for out in &tx.outputs {
                                if out.stealth_address == "LOTTERY_RESERVE" {
                                    pot += out.aes_vault.parse::<u64>().unwrap_or(0);
                                }
                            }
                        }
                        
                        if let TransactionType::LotteryPayout { .. } = &tx.tx_type {
                            break 'block_loop; 
                        }
                    }
                }
            }
        }

        // L2 Aspiré depuis l'arbre Sled
        if let Ok(l2_tree) = self.db.open_tree("l2_blocks") {
            for result in l2_tree.iter() {
                if let Ok((_, value)) = result {
                    if let Ok(mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                        if valid_l1_hashes.contains(&mb.l1_parent_hash) {
                            for tx in &mb.transactions {
                                if tx.tx_type == TransactionType::MicroCoinbase {
                                    for out in &tx.outputs {
                                        if out.stealth_address == "LOTTERY_RESERVE" {
                                            pot += out.aes_vault.parse::<u64>().unwrap_or(0);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        tickets.sort_by(|a, b| a.0.cmp(&b.0));
        (pot, tickets)
    }

    pub fn get_current_jackpot(&self) -> (u64, Vec<(String, String)>) {
		let current_height = self.current_height + 1; 
		let next_draw = current_height + (LOTTERY_TIME_BLOCK - (current_height % LOTTERY_TIME_BLOCK));
		
		self.get_jackpot_info(next_draw)
	}

    pub fn prepare_block_template(&mut self, mut transactions: Vec<Transaction>, miner_address: &str, l2_keys: Vec<(Vec<[u8; 32]>, Vec<u8>)>) -> (Block, BigUint, Vec<(Vec<[u8; 32]>, Vec<u8>)>) {
        let current_height = self.current_height + 1; 
        println!("\n⏳ Préparation du Bloc {}...", current_height);

        // ====================================================================
        // LE MARCHÉ DES FRAIS (FEE MARKET) - ANTI-SPAM ET MAXIMISATION DES GAINS
        // ====================================================================
        transactions.sort_by(|a, b| {
            let get_score = |tx: &Transaction| -> f64 {
                let is_feeless = matches!(tx.tx_type, 
                    TransactionType::Coinbase | TransactionType::MicroCoinbase |
                    TransactionType::MiningShare { .. } | TransactionType::DexSettlement { .. } |
                    TransactionType::LotteryPayout { .. } | TransactionType::HTLCClaim { .. } | TransactionType::HTLCRefund { .. }
                );
                
                if is_feeless {
                    return std::f64::MAX; // Les transactions du consensus passent en priorité absolue
                }
                
                let weight = bincode::serialized_size(tx).unwrap_or(1) as f64;
                (tx.fee as f64) / weight // Rentabilité : Flames par Octet
            };

            let score_a = get_score(a);
            let score_b = get_score(b);
            
            // Tri décroissant : Le score le plus élevé (b) passe avant le plus faible (a)
            score_b.partial_cmp(&score_a).unwrap_or(std::cmp::Ordering::Equal)
        });
        // ====================================================================

        let mut valid_transactions = Vec::new();
        let mut l1_total_fees = 0;
        let mut temp_spent_images = self.spent_key_images.clone(); 
        
        // On garde 1 Mo de marge pour la Coinbase et les en-têtes
        let mut current_block_size = 1024 * 1024; 
		
        let mut immature_pubkeys = std::collections::HashSet::new();
        let scan_limit = current_height.saturating_sub(MATURITY_BLOCKS);
        
        for i in (0..=self.current_height).rev() {
            if i <= scan_limit { break; }
            if let Some(block) = self.get_block_by_height(i) {
                for past_tx in &block.transactions {
                    if matches!(past_tx.tx_type, TransactionType::Coinbase | TransactionType::LotteryPayout { .. }) {
                        for out in &past_tx.outputs {
                            if out.stealth_address.starts_with("COINBASE_") {
                                immature_pubkeys.insert(out.stealth_address.replace("COINBASE_", ""));
                            } else if out.stealth_address.starts_with("JACKPOT_") {
                                immature_pubkeys.insert(out.stealth_address.replace("JACKPOT_", ""));
                            }
                        }
                    }
                }
            }
        }

        for tx in &transactions {
			// On ignore les parts ici, on les traitera proprement après
			if let TransactionType::MiningShare { .. } = &tx.tx_type {
				continue; 
			}
			
			if true {
                
                let is_pure_l2 = !tx.outputs.is_empty() && tx.outputs.iter().all(|out| out.stealth_address.starts_with("L2_WATT_"));
                if is_pure_l2 && tx.tx_type != TransactionType::MicroCoinbase {
                    continue; 
                }

                // ====================================================================
                // LE TRIBUNAL ÉCONOMIQUE DE PRÉPARATION (Consensus Level)
                // Empêche le mineur d'inclure des transactions gratuites spammantes
                // ====================================================================
                let is_l1_interop = matches!(tx.tx_type, 
                    TransactionType::L2Anchor { .. } | TransactionType::L2BridgeLock { .. } |
                    TransactionType::L2Stake { .. } | TransactionType::L2Unstake { .. }
                );
                let is_feeless = matches!(tx.tx_type, 
                    TransactionType::Coinbase | TransactionType::MicroCoinbase |
                    TransactionType::MiningShare { .. } | TransactionType::DexSettlement { .. } |
                    TransactionType::LotteryPayout { .. } | TransactionType::HTLCClaim { .. } | TransactionType::HTLCRefund { .. }
                );

                if !is_feeless {
                    let tx_weight_bytes = bincode::serialized_size(tx).unwrap_or(0) as usize;
                    let weight_kb = (tx_weight_bytes as f64 / 1024.0).ceil() as u64;
                    let min_fee = if is_pure_l2 && !is_l1_interop {
                        std::cmp::max(100, weight_kb * 2)
                    } else {
                        std::cmp::max(1000, weight_kb * 20)
                    };

                    if tx.fee < min_fee {
                        println!("⛔ Rejet Mempool (Consensus) : Frais insuffisants ({} Flames < {} Flames requis)", tx.fee, min_fee);
                        continue; // On ne l'inclut pas dans le bloc !
                    }
                }
                // ====================================================================
				
                let mut immature = false;
                if tx.tx_type != TransactionType::Coinbase {
                    if let Some(sig) = &tx.wots_signature {
						let pubkey_hex = hex::encode(&sig.public_key);
						if immature_pubkeys.contains(&pubkey_hex) {
							immature = true;
						}
					}
                }
                if immature { 
                    println!("⛔ Rejet : Tentative de dépense d'une récompense immature (Coinbase/Loto < {} blocs) !", MATURITY_BLOCKS);
                    continue; 
                }

                if matches!(tx.tx_type, TransactionType::LotteryPayout { .. }) {
                    valid_transactions.push(tx.clone()); continue;
                }
				
                if let TransactionType::HTLCRefund { hash } = &tx.tx_type {
                    let mut timeout = 0;
                    let mut lock_found = false;
                    
                    for i in (0..=self.current_height).rev() {
                        if let Some(b) = self.get_block_by_height(i) {
                            for past_tx in &b.transactions {
                                if let TransactionType::HTLCLock { hash: lock_hash, timeout_block } = &past_tx.tx_type {
                                    if lock_hash == hash {
                                        timeout = *timeout_block;
                                        lock_found = true;
                                        break;
                                    }
                                }
                            }
                            if lock_found { break; }
                        }
                    }
                    
                    if !lock_found {
                        println!("⛔ HTLCRefund : Contrat d'origine introuvable !");
                        continue; 
                    }
                    if current_height < timeout {
                        println!("⛔ HTLCRefund : Délai temporel non expiré (Actuel: {} < Requis: {}).", current_height, timeout);
                        continue; 
                    }
                }
				
				if let TransactionType::L2Stake { l2_name, .. } = &tx.tx_type {
					if tx.outputs.is_empty() {
						println!("⛔ Rejet : Un L2Stake doit contenir un output de verrouillage !");
						continue; 
					}
					let stake_amount: u64 = tx.outputs[0].aes_vault.parse().unwrap_or(0);
					let required_stake = MONTANT_STAKE * FLAME;
					
					if stake_amount < required_stake {
						println!("⛔ Rejet : Le staking pour '{}' est insuffisant (Requis: {} WATT) !", l2_name, MONTANT_STAKE);
						continue;
					}
					if !tx.outputs[0].stealth_address.starts_with("L2_STAKE_") {
						println!("⛔ Rejet : L'adresse de destination du Staking est invalide !");
						continue;
					}

					let mut is_valid_math = true;
					for (i, &val) in tx.outputs[0].lattice_commitment.t_vector.iter().enumerate() {
						let expected = if i == 0 { stake_amount } else { 0 };
						let diff = val.wrapping_sub(expected);
						if diff > 24 && diff < u64::MAX.wrapping_sub(24) {
							is_valid_math = false; break;
						}
					}
					
					if !is_valid_math {
						println!("⛔ Rejet : Fraude mathématique ! L'engagement Lattice ne correspond pas au montant déclaré.");
						continue;
					}
				}
				
				if let TransactionType::L2BridgeLock { l2_target_name, .. } = &tx.tx_type {
					if tx.outputs.is_empty() {
						println!("⛔ Rejet : Un L2BridgeLock doit contenir un output de verrouillage !");
						continue; 
					}

					let official_bridge_address = format!("BRIDGE_L2_{}", l2_target_name.to_uppercase());
					
					if tx.outputs[0].stealth_address != official_bridge_address {
						println!("⛔ Rejet : Les fonds doivent être envoyés au contrat L2 strict : {}", official_bridge_address);
						continue;
					}

					let bridge_amount: u64 = tx.outputs[0].aes_vault.parse().unwrap_or(0);
					
					if bridge_amount == 0 {
						println!("⛔ Rejet : Le montant du bridge est invalide ou nul !");
						continue;
					}

					let mut is_valid_math = true;
					for (i, &val) in tx.outputs[0].lattice_commitment.t_vector.iter().enumerate() {
						let expected = if i == 0 { bridge_amount } else { 0 };
						let diff = val.wrapping_sub(expected);
						if diff > 24 && diff < u64::MAX.wrapping_sub(24) {
							is_valid_math = false; break;
						}
					}
					
					if !is_valid_math {
						println!("⛔ Rejet : Fraude mathématique ! L'engagement Lattice du Bridge ne correspond pas au montant déclaré.");
						continue; 
					}

					println!("🌉 [BRIDGE L2] {} Flames verrouillés publiquement pour le réseau {}", bridge_amount, l2_target_name);
				}

				let mut double_spend = false;
                let mut tx_inputs_valid = true;

                if tx.tx_type != TransactionType::Coinbase {
                    for input in &tx.inputs {
                        if temp_spent_images.contains(&input.utxo_id) {
                            double_spend = true; break;
                        }
                        // VÉRIFICATION D'EXISTENCE ET DE PROPRIÉTÉ
                        if let Some(utxo) = self.find_utxo(&input.utxo_id) {
                            let owner = utxo.stealth_address.replace("COINBASE_", "");
                            if owner != tx.public_key && !owner.contains(&tx.public_key) {
                                println!("⛔ Rejet : Usurpation d'UTXO. Le signataire n'est pas le propriétaire.");
                                tx_inputs_valid = false; break;
                            }
                        } else {
                            println!("⛔ Rejet : UTXO fantôme ({})", input.utxo_id);
                            tx_inputs_valid = false; break;
                        }
                    }
                }

                if !double_spend && tx_inputs_valid {
                    // LE BOUCLIER DE TAILLE DU MINEUR
                    let tx_size = bincode::serialized_size(tx).unwrap_or(0) as usize;
                    if current_block_size + tx_size > MAX_BLOCK_SIZE_BYTES {
                        println!("⚠️ [MEMPOOL] Bloc plein ! (Limite de 32 Mo atteinte). Les transactions restantes attendront le prochain bloc.");
                        break; // On arrête de remplir le bloc !
                    }

                    current_block_size += tx_size; // On met à jour le poids
					//println!("⚠️ [POIDS] Le poids du bloc est de {} Ko.", current_block_size/1_024);
                    l1_total_fees += tx.fee; 
                    valid_transactions.push(tx.clone()); 
                    if let Some(sig) = &tx.wots_signature {
						temp_spent_images.insert(hex::encode(&sig.public_key));
					}
					// Enregistrement des UTXOs dépensés
                    for input in &tx.inputs {
                        temp_spent_images.insert(input.utxo_id.clone());
                    }
                }
            }
        }

        let previous_block = self.get_last_block();
        
        let mut new_timestamp = chrono::Utc::now().timestamp();
        if new_timestamp <= previous_block.header.timestamp {
            new_timestamp = previous_block.header.timestamp + 1;
        }
        
        let mut time_taken = new_timestamp - previous_block.header.timestamp;
        if time_taken <= 0 { time_taken = 1; }
        
        let max_target = BigUint::from_bytes_be(&[0xFF; 32]);
        let initial_target = &max_target >> INITIAL_DIFFICULTY_SHIFT; 

        let difficulty_x100 = (&initial_target * 100u64) / &self.target;
        let diff_int = &difficulty_x100 / 100u64;
        let diff_dec = &difficulty_x100 % 100u64;

        if current_height > 1 { println!("⚙️  Dernier bloc miné en {}s", time_taken); }
        println!("🎯 Difficulté cible : {}.{:02}x", diff_int, diff_dec);

        let mut expected_subsidy = INITIAL_REWARD;
        for _ in 0..current_height {
            expected_subsidy = Blockchain::get_next_base_reward(expected_subsidy);
        }

        let mut allowed_subsidy = expected_subsidy;
		
        if allowed_subsidy < TAIL_EMISSION { allowed_subsidy = TAIL_EMISSION; }
        println!("📉 Émission monétaire : {:.9} Watts", (allowed_subsidy as f64) / (FLAME as f64));

        let mut slashed_for_jackpot = 0;

        if current_height > 17 && time_taken < 30 {
            let time_penalty_ratio = time_taken as f64 / 30.0;
            
            let penalty_subsidy = (allowed_subsidy as f64 * time_penalty_ratio) as u64;
            slashed_for_jackpot = allowed_subsidy.saturating_sub(penalty_subsidy);
            allowed_subsidy = penalty_subsidy; 
            
            println!("🚨 [ANTI-FARM] Hashrate extrême détecté ! (Bloc trouvé en {}s).", time_taken);
            println!("🎰 [ROBIN DES BOIS] Pénalité appliquée : {} WATT confisqués et envoyés au Jackpot L1 !", slashed_for_jackpot as f64 / 1_000_000_000.0);
        }

        let l1_lottery_tax = l1_total_fees / 100;
        let l1_miner_fees = l1_total_fees - l1_lottery_tax;
        let total_lottery_tax = l1_lottery_tax + slashed_for_jackpot; 

        
        println!("📉 Frais du mineur L1 : {:.9} Watts", (l1_miner_fees as f64) / (FLAME as f64));
		
        let mut coinbase_outputs = Vec::new();
        let mut valid_shares = Vec::new();

        let mut has_shares = false;
        for tx in &transactions {
            if matches!(tx.tx_type, TransactionType::MiningShare { .. }) {
                has_shares = true;
                break;
            }
        }

        if has_shares {
            let share_height = current_height.saturating_sub(1); 
			let share_prev_hash = previous_block.header.previous_hash.clone();
            let share_seed = self.get_epoch_seed(share_height);

            let flags = randomx_rs::RandomXFlag::get_recommended_flags();
            let cache = randomx_rs::RandomXCache::new(flags, share_seed.as_bytes()).unwrap();
            let vm = randomx_rs::RandomXVM::new(flags, Some(cache), None).unwrap();

            for tx in &transactions {
                if let TransactionType::MiningShare { miner_address: share_miner, nonce, hash, timestamp, .. } = &tx.tx_type {
                    let parts: Vec<&str> = tx.public_key.split('|').collect();
                    let l2_root = parts.get(0).cloned().unwrap_or("");
                    let tx_root = parts.get(1).cloned().unwrap_or("");
                    
                    let header_data = format!("{}{}{}{}{}{}{}", share_miner, share_height, timestamp, share_prev_hash, nonce, l2_root, tx_root);
                    
                    if let Ok(hash_bytes) = vm.calculate_hash(header_data.as_bytes()) {
                        if hex::encode(&hash_bytes) == *hash {
                            if valid_shares.len() < 50 { valid_shares.push(tx.clone()); }
                        }
                    }
                }
            }
        }

        if !valid_shares.is_empty() {
            let base_community_reward = allowed_subsidy * 80 / 100; 
            let share_reward = base_community_reward / valid_shares.len() as u64; 
            let exact_community_reward = share_reward * valid_shares.len() as u64; 
            let final_finder_reward = (allowed_subsidy - exact_community_reward) + l1_miner_fees;

            println!("🤝 [P2POOL] Répartition :\nMineur (Base {:.9}  + Frais {:.9}  = {:.9} Watts).\nCommunauté ({:.9} Watts).", 
                ((allowed_subsidy - exact_community_reward)  as f64) / (FLAME as f64),
				(l1_miner_fees as f64) / (FLAME as f64),
				(final_finder_reward as f64) / (FLAME as f64),
                (exact_community_reward as f64) / (FLAME as f64)
            );
            println!("🤝 [P2POOL] Il y a {} parts de minages de : {:.9} Watts", 
                valid_shares.len(), 
                (share_reward as f64) / (FLAME as f64)
            );

            coinbase_outputs.push(crate::transaction::TransactionOutput {
                stealth_address: format!("COINBASE_{}", miner_address), 
                kyber_capsule: format!("COINBASE_CAPSULE_{}", current_height),
                aes_vault: final_finder_reward.to_string(), 
                lattice_commitment: crate::lattice::LWECommitment::commit(final_finder_reward, &[0u64; crate::lattice::LATTICE_COLS]),
				range_proof: String::new(),
            });

			let mut aggregated_shares: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
			
			for share_tx in valid_shares.iter() {
				if let TransactionType::MiningShare { miner_address: share_addr, .. } = &share_tx.tx_type {
					*aggregated_shares.entry(share_addr.clone()).or_insert(0) += share_reward;
				}
			}

			for (i, (share_addr, total_reward)) in aggregated_shares.into_iter().enumerate() {
				coinbase_outputs.push(crate::transaction::TransactionOutput {
					stealth_address: format!("COINBASE_{}", share_addr), 
					kyber_capsule: format!("SHARE_CAPSULE_{}_{}", current_height, i),
					aes_vault: total_reward.to_string(), 
					lattice_commitment: crate::lattice::LWECommitment::commit(total_reward, &[0u64; crate::lattice::LATTICE_COLS]),
					range_proof: String::new(),
				});
			}
			
			// On pousse uniquement les parts qui ont réussi le test (Max 50)
			for share_tx in &valid_shares {
				valid_transactions.push(share_tx.clone());
			}
        } else {
            let total_solo_reward = allowed_subsidy + l1_miner_fees;
            coinbase_outputs.push(crate::transaction::TransactionOutput {
                stealth_address: format!("COINBASE_{}", miner_address), 
                kyber_capsule: format!("COINBASE_CAPSULE_{}", current_height),
                aes_vault: total_solo_reward.to_string(), 
                lattice_commitment: crate::lattice::LWECommitment::commit(total_solo_reward, &[0u64; crate::lattice::LATTICE_COLS]),
				range_proof: String::new(),
            });
        }

        if total_lottery_tax > 0 {
            coinbase_outputs.push(crate::transaction::TransactionOutput {
                stealth_address: "LOTTERY_RESERVE".to_string(), 
                kyber_capsule: format!("TAX_CAPSULE_{}", current_height),
                aes_vault: total_lottery_tax.to_string(), 
                lattice_commitment: crate::lattice::LWECommitment::commit(total_lottery_tax, &[0u64; crate::lattice::LATTICE_COLS]),
				range_proof: String::new(),
            });
        }
		
        if current_height % LOTTERY_TIME_BLOCK == 0 && current_height > 0 {
            let (mut jackpot_amount, mut tickets) = self.get_jackpot_info(current_height);
            
            for tx in &valid_transactions {
                if let TransactionType::HTLCLottery { target_block, player_pubkey } = &tx.tx_type {
                    if *target_block == current_height && !tx.outputs.is_empty() {
                        let ticket_id = tx.outputs[0].kyber_capsule.clone();
                        tickets.push((ticket_id, player_pubkey.clone()));
                        
                        for out in &tx.outputs {
                            if out.stealth_address == "LOTTERY_RESERVE" {
                                jackpot_amount += out.aes_vault.parse::<u64>().unwrap_or(0);
                            }
                        }
                    }
                }
            }

            tickets.sort_by(|a, b| a.0.cmp(&b.0));
            
            if !tickets.is_empty() {
                
                let last_block_hash = &previous_block.header.hash;
                
                let mut lowest_score = [0xFFu8; 32];
                let mut winner_pubkey = String::new();
                let mut winner_ticket_id = String::new();

                for ticket in &tickets {
                    let mut vrf_hasher = sha2::Sha256::new();
                    vrf_hasher.update(last_block_hash.as_bytes());
                    vrf_hasher.update(b"LOTTERY"); 
                    vrf_hasher.update(ticket.0.as_bytes()); // ID de la capsule
                    vrf_hasher.update(ticket.1.as_bytes()); // Pubkey
                    
                    let mut vrf_hash = [0u8; 32];
                    vrf_hash.copy_from_slice(&vrf_hasher.finalize());
                    
                    if vrf_hash < lowest_score {
                        lowest_score = vrf_hash;
                        winner_ticket_id = ticket.0.clone();
                        winner_pubkey = ticket.1.clone();
                    }
                }

                println!("🎰 [LOTO VRF] Le ticket {} remporte le Jackpot de {} Flames !", 
                         winner_ticket_id, jackpot_amount);

                let payout_output = crate::transaction::TransactionOutput {
                    stealth_address: format!("JACKPOT_{}", winner_pubkey),
                    kyber_capsule: format!("JACKPOT_PAYOUT_{}", current_height),
                    aes_vault: jackpot_amount.to_string(),
                    lattice_commitment: crate::lattice::LWECommitment::commit(jackpot_amount, &[0u64; crate::lattice::LATTICE_COLS]),
					range_proof: String::new(),
                };

                let lottery_payout_tx = Transaction {
                    tx_type: TransactionType::LotteryPayout { 
                        target_block: current_height, 
                        winner_pubkey 
                    },
                    inputs: vec![],
                    outputs: vec![payout_output],
                    fee: 0,
                    public_key: "LOTTERY_PAYOUT".to_string(), wots_signature: None,
                };

                valid_transactions.push(lottery_payout_tx);
                println!("💸 LotteryPayout ajouté au template (montant : {} Flames)", jackpot_amount);
            }
        }

        let coinbase_tx = Transaction {
            tx_type: TransactionType::Coinbase,
            inputs: vec![],
            outputs: coinbase_outputs,
            fee: 0,
            public_key: "COINBASE_SIG".to_string(), 
			wots_signature: None,
        };
        valid_transactions.insert(0, coinbase_tx);
		
        let mut l2_pubkeys = Vec::with_capacity(128);
        for k in &l2_keys {
            l2_pubkeys.push(hex::encode(&k.1)); 
        }

        let l2_root = if l2_pubkeys.is_empty() {
            "NO_L2_ROOT_FOR_THIS_BLOCK".to_string()
        } else {
            let mut current_root = l2_pubkeys[0].clone();
            for i in 1..l2_pubkeys.len() {
                let mut hasher = sha2::Sha512::new();
                hasher.update(current_root.as_bytes());
                hasher.update(l2_pubkeys[i].as_bytes());
                current_root = hex::encode(hasher.finalize());
            }
            current_root
        };

        let new_header = BlockHeader {
            index: current_height,
            timestamp: new_timestamp, 
            previous_hash: previous_block.header.hash.clone(),
            hash: String::new(),
            nonce: 0,
            target_hex: format!("{:0>64}", self.target.to_str_radix(16)),
            l2_root, 
            tx_root: String::new(), 
        };

        let mut block = Block { header: new_header, transactions: valid_transactions };
		
        block.header.tx_root = block.calculate_tx_root();
		
        (block, self.target.clone(), l2_keys) 
    }
    
    pub fn resolve_fork(&mut self, new_chain: Vec<Block>) -> bool {
        self.resolve_partial_fork(new_chain)
    }
    
    pub fn resolve_partial_fork(&mut self, new_blocks: Vec<Block>) -> bool {
        if new_blocks.is_empty() {
            println!("⚠️ [FORK] Lot de blocs vide reçu, ignoré.");
            return false;
        }

        let start_index = new_blocks[0].header.index;

        // 1. VÉRIFICATIONS STRUCTURELLES GLOBALES DE LA BRANCHE (Continuité)
        for i in 1..new_blocks.len() {
            if new_blocks[i].header.index != new_blocks[i-1].header.index + 1 {
                println!("❌ [FORK] Index non séquentiels ({})", new_blocks[i].header.index);
                return false;
            }
            if new_blocks[i].header.previous_hash != new_blocks[i-1].header.hash {
                println!("❌ [FORK] Rupture de la chaîne dans la branche reçue.");
                return false;
            }
            if new_blocks[i].header.timestamp <= new_blocks[i-1].header.timestamp {
                println!("❌ [FORK] Le temps stagne ou recule dans la branche.");
                return false;
            }
        }

        // 2. TROUVER L'ANCÊTRE
        if start_index > self.current_height + 1 {
            println!("❌ [FORK] Index trop grand ({} > {})", start_index, self.current_height + 1);
            return false;
        }

        let mut ancestor_index = start_index.saturating_sub(1);
        let mut found_ancestor = false;

        if start_index == 0 {
            found_ancestor = true;
        } else {
            while ancestor_index > 0 && ancestor_index <= self.current_height {
                if let Some(b) = self.get_block_by_height(ancestor_index) {
                    if b.header.hash == new_blocks[0].header.previous_hash {
                        found_ancestor = true;
                        break;
                    }
                }
                ancestor_index = ancestor_index.saturating_sub(1);
            }
            if !found_ancestor {
                if let Some(genesis_block) = self.get_block_by_height(0) {
					if genesis_block.header.hash == new_blocks[0].header.previous_hash { found_ancestor = true; }
				}
            }
        }

        if !found_ancestor && start_index != 0 {
            println!("❌ [FORK] Impossible de trouver un ancêtre commun.");
            return false;
        }

        // Création de la chaîne théorique
        let mut theoretical_chain = Vec::new();
        for i in 0..=ancestor_index {
            if let Some(b) = self.get_block_by_height(i) {
                theoretical_chain.push(b);
            }
        }
        let mut last_verified_timestamp = theoretical_chain.last().map(|b| b.header.timestamp).unwrap_or(0);
        theoretical_chain.extend(new_blocks.clone());

        // 3. VÉRIFICATION SÉCURISÉE DES HEADERS (RandomX, TX Root, Temps)
        let get_theoretical_seed = |height: u64, t_chain: &[Block]| -> String {
            if height <= EPOCH_BLOCKS { return t_chain[0].header.hash.clone(); }
            let epoch = (height - 1) / EPOCH_BLOCKS;
            let target_block = (epoch * EPOCH_BLOCKS).saturating_sub(11);
            if (target_block as usize) < t_chain.len() { t_chain[target_block as usize].header.hash.clone() } 
            else { t_chain[0].header.hash.clone() }
        };
        
        let current_time = chrono::Utc::now().timestamp();
        let max_future_tolerance = 7200; 
        let flags = RandomXFlag::get_recommended_flags();
        let mut current_seed = get_theoretical_seed(new_blocks[0].header.index, &theoretical_chain);
        let mut cache = RandomXCache::new(flags, current_seed.as_bytes()).unwrap();
        let mut vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap(); 
        
        for block in &new_blocks {
            if block.header.timestamp > current_time + max_future_tolerance {
                println!("❌ [FORK] Bloc {} trop loin dans le futur !", block.header.index);
                return false;
            }
            if block.header.timestamp <= last_verified_timestamp {
                println!("❌ [FORK] Temps recule au bloc {}.", block.header.index);
                return false;
            }
            last_verified_timestamp = block.header.timestamp;
            if block.header.tx_root != block.calculate_tx_root() { 
                println!("❌ [FORK] Racine de Merkle invalide au bloc {}.", block.header.index);
                return false; 
            }

            let needed_seed = get_theoretical_seed(block.header.index, &theoretical_chain);
            if needed_seed != current_seed {
                current_seed = needed_seed;
                cache = RandomXCache::new(flags, current_seed.as_bytes()).unwrap();
                vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap();
            }

            let header_data = format!("{}{}{}{}{}{}", 
				block.header.index, 
				block.header.timestamp, 
				block.header.previous_hash, 
				block.header.nonce, 
				block.header.l2_root, 
				block.header.tx_root 
			);
            let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
            if hex::encode(&hash_bytes) != block.header.hash { 
                println!("❌ [FORK] Hash RandomX falsifié au bloc {}.", block.header.index);
                return false; 
            }
        }

        // 4. COMPARAISON DU TRAVAIL ET VALIDATION DU TARGET
        let mut old_chain = Vec::new();
        for i in 0..=self.current_height {
            if let Some(b) = self.get_block_by_height(i) {
                old_chain.push(b);
            }
        }

        let (my_work, _) = Blockchain::calculate_total_work(&old_chain);
        let (mut new_work, is_pow_valid) = Blockchain::calculate_total_work(&theoretical_chain);

        if !is_pow_valid {
            println!("❌ [FORK] Difficulté non respectée (Hash > Target) dans la branche.");
            return false;
        }

        let reorg_depth = self.current_height.saturating_sub(ancestor_index);
        if reorg_depth > 10 {
            let penalty_shift = std::cmp::min((reorg_depth - 10) as u32, 256); 
            println!("🛡️ [MESS] Réorganisation profonde ({}). Poids divisé par 2^{}", reorg_depth, penalty_shift);
            new_work = new_work >> penalty_shift;
        }

        if new_work <= my_work && self.current_height > 0 {
            println!("❌ [FORK] La chaîne n'est pas assez lourde.");
            return false;
        }

        println!("✅ [FORK] Poids supérieur validé. Lancement de la validation stricte des transactions...");

        // 5. BACKUP POUR ROLLBACK SÉCURISÉ
        let mut backup_blocks = Vec::new();
        for i in (ancestor_index + 1)..=self.current_height {
            if let Some(b) = self.get_block_by_height(i) {
                backup_blocks.push(b);
            }
            let _ = self.db.remove(&i.to_be_bytes());
        }
        
        self.current_height = ancestor_index;
        self.recalculate_target_from_scratch();
        self.rebuild_spent_cache();

        // 6. VALIDATION STRICTE via validate_and_add_external_block
        let mut success = true;
        for block in &new_blocks {
            if let Err(e) = self.validate_and_add_external_block(block.clone()) {
                println!("❌ [FORK] Échec validation du bloc {}: {}", block.header.index, e);
                success = false;
                break;
            }
        }

        // 7. VERDICT
        if success {
            println!("✅ [FORK] Réorganisation réussie ! Nouvelle hauteur: {}", self.current_height);
            return true;
        } else {
            println!("🔄 [FORK] Fraude détectée dans le corps des blocs. Rollback et restauration de l'ancienne chaîne...");
            
            // Destruction des blocs corrompus
            for i in (ancestor_index + 1)..=self.current_height {
                let _ = self.db.remove(&i.to_be_bytes());
            }
            self.current_height = ancestor_index;
            
            // Restauration en force
            for b in backup_blocks {
                let _ = self.push_block(&b); 
            }
            self.recalculate_target_from_scratch();
            self.rebuild_spent_cache();
            return false;
        }
    }
    
	pub fn validate_and_add_external_block(&mut self, block: Block) -> Result<(), String> {
        // LE COUPERET DU POIDS : Rejet immédiat si c'est trop lourd !
        let block_size = bincode::serialized_size(&block).unwrap_or(0) as usize;
		println!("🏋️ [POIDS] Le poids du bloc dans validate est de {} Ko.", block_size/ 1_024);
        if block_size > MAX_BLOCK_SIZE_BYTES {
            return Err(format!("❌ FRAUDE : Le bloc dépasse la limite de consensus stricte ({} Mo > 32 Mo) !", block_size / 1_024 / 1_024));
        }

		let last_block = self.get_last_block();
		
		let current_time = chrono::Utc::now().timestamp();
		let max_future_tolerance = 7200; 
		
		if block.header.timestamp > current_time + max_future_tolerance {
			return Err(format!("❌ FRAUDE TEMPORELLE : Ce bloc vient du futur ! (Timestamp: {}, Actuel: {})", block.header.timestamp, current_time));
		}
		
		if block.header.timestamp <= last_block.header.timestamp {
			return Err("❌ FRAUDE TEMPORELLE : Le temps ne peut pas reculer ou stagner par rapport au bloc précédent.".to_string());
		}

		if block.header.index != last_block.header.index + 1 { 
			return Err("Index de bloc invalide.".to_string()); 
		}
		if block.header.previous_hash != last_block.header.hash { 
			return Err("Rupture de la chaîne.".to_string()); 
		}
		
		if block.header.tx_root != block.calculate_tx_root() {
			return Err("❌ FRAUDE : La racine de Merkle (tx_root) est invalide ou falsifiée !".to_string());
		}

		let flags = randomx_rs::RandomXFlag::get_recommended_flags();
		let seed = self.get_epoch_seed(block.header.index);
		let cache = randomx_rs::RandomXCache::new(flags, seed.as_bytes()).map_err(|_| "Erreur Cache")?;
		let vm = randomx_rs::RandomXVM::new(flags, Some(cache.clone()), None).map_err(|_| "Erreur VM")?;

		// Extraction de l'adresse du mineur depuis la première TX (Coinbase)
        let miner_address = block.transactions.get(0)
            .and_then(|tx| tx.outputs.get(0))
            .map(|out| out.stealth_address.replace("COINBASE_", ""))
            .unwrap_or_default();

		let header_data = format!("{}{}{}{}{}{}{}", 
			miner_address,
			block.header.index, 
			block.header.timestamp, 
			block.header.previous_hash, 
			block.header.nonce,
			block.header.l2_root,
			block.header.tx_root 
		);
		
		let hash_bytes = vm.calculate_hash(header_data.as_bytes()).map_err(|_| "Erreur calcul")?;
		
		if block.header.hash != hex::encode(&hash_bytes) { 
			return Err("Hash frauduleux.".to_string()); 
		}

		let hash_bigint = num_bigint::BigUint::parse_bytes(block.header.hash.as_bytes(), 16).unwrap_or_default();
		if hash_bigint > self.target { 
			return Err("Preuve de travail insuffisante.".to_string()); 
		}

		let mut coinbase_count = 0;
		let mut total_block_fees = 0u64;
		let mut block_key_images = HashSet::new();
		let current_height = block.header.index;
		
		// Accumulateur global pour TOUTE l'impression monétaire du bloc
        let mut total_coinbase_output = 0u64;
		
		let mut immature_pubkeys = std::collections::HashSet::new();
		let scan_limit = current_height.saturating_sub(MATURITY_BLOCKS);
		
		for i in (0..=self.current_height).rev() {
			if i <= scan_limit { break; }
            if let Some(b) = self.get_block_by_height(i) {
                for past_tx in &b.transactions {
                    if matches!(past_tx.tx_type, TransactionType::Coinbase | TransactionType::MicroCoinbase | TransactionType::LotteryPayout { .. }) {
                        for out in &past_tx.outputs {
                            if out.stealth_address.starts_with("COINBASE_") {
                                immature_pubkeys.insert(out.stealth_address.replace("COINBASE_", ""));
                            } else if out.stealth_address.starts_with("JACKPOT_") {
                                immature_pubkeys.insert(out.stealth_address.replace("JACKPOT_", ""));
                            } else if out.kyber_capsule.starts_with("MICRO_COINBASE_") && out.stealth_address.starts_with("L2_WATT_") {
                                immature_pubkeys.insert(out.stealth_address.replace("L2_WATT_", ""));
                            }
                        }
                    }
                }
            }
		}
		
        let mut expected_subsidy = INITIAL_REWARD;
        for _ in 0..current_height {
            expected_subsidy = Blockchain::get_next_base_reward(expected_subsidy);
        }
		
        let mut share_count = 0;
        let mut share_hashes = HashSet::new();

        let share_height = current_height.saturating_sub(1);
		let share_prev_hash = last_block.header.previous_hash.clone();
        let share_seed = self.get_epoch_seed(share_height);
        
        let share_vm = if seed == share_seed {
            randomx_rs::RandomXVM::new(flags, Some(cache.clone()), None).map_err(|_| "Erreur VM Part")?
        } else {
            let new_share_cache = randomx_rs::RandomXCache::new(flags, share_seed.as_bytes()).map_err(|_| "Erreur Cache Part")?;
            randomx_rs::RandomXVM::new(flags, Some(new_share_cache), None).map_err(|_| "Erreur VM Part")?
        };

        for tx in &block.transactions {
			if tx.tx_type == TransactionType::Coinbase {
				coinbase_count += 1;
				// On additionne ABSOLUMENT TOUS les outputs de la Coinbase !
                for out in &tx.outputs {
                    total_coinbase_output += out.aes_vault.parse::<u64>().unwrap_or(u64::MAX);
                }
				continue;
			}
			
			if tx.tx_type == TransactionType::MicroCoinbase {
				return Err("❌ FRAUDE : Présence d'une MicroCoinbase L2 dans un bloc L1 !".to_string());
			}

			if let TransactionType::DexSettlement { clearing_price_sats, .. } = &tx.tx_type {
                crate::api::LAST_PRICE_SATS.store(*clearing_price_sats, std::sync::atomic::Ordering::Relaxed);
                // On a supprimé le 'continue;' ici. La TX doit traverser les vérifications de base !
            }

			if tx.tx_type != TransactionType::Coinbase {
                for input in &tx.inputs {
                    if self.spent_key_images.contains(&input.utxo_id) || !block_key_images.insert(input.utxo_id.clone()) {
                        return Err(format!("Double-dépense détectée sur l'UTXO : {}", input.utxo_id));
                    }
                    
                    let utxo = self.find_utxo(&input.utxo_id).ok_or(format!("UTXO fantôme inventé : {}", input.utxo_id))?;
                    
                    let owner = utxo.stealth_address.replace("COINBASE_", "");
                    if owner != tx.public_key && !owner.contains(&tx.public_key) {
                        return Err("Usurpation d'identité : La signature WOTS+ ne correspond pas à l'UTXO".into());
                    }
                }
            }

            // ====================================================================
            // TRIBUNAL ÉCONOMIQUE L1/L2 (RÈGLE DE CONSENSUS)
            // ====================================================================
            let is_pure_l2 = !tx.outputs.is_empty() && tx.outputs.iter().all(|out| out.stealth_address.starts_with("L2_WATT_"));
            let is_l1_interop = matches!(tx.tx_type, 
                TransactionType::L2Anchor { .. } | TransactionType::L2BridgeLock { .. } |
                TransactionType::L2Stake { .. } | TransactionType::L2Unstake { .. }
            );
            let is_feeless = matches!(tx.tx_type, 
                TransactionType::Coinbase | TransactionType::MicroCoinbase |
                TransactionType::MiningShare { .. } | TransactionType::DexSettlement { .. } |
                TransactionType::LotteryPayout { .. } | TransactionType::HTLCClaim { .. } | TransactionType::HTLCRefund { .. }
            );

            if !is_feeless {
                let tx_weight_bytes = bincode::serialized_size(tx).unwrap_or(0) as usize;
                let weight_kb = (tx_weight_bytes as f64 / 1024.0).ceil() as u64;
                let min_fee = if is_pure_l2 && !is_l1_interop {
                    std::cmp::max(100, weight_kb * 2)
                } else {
                    std::cmp::max(1000, weight_kb * 20)
                };

                if tx.fee < min_fee {
                    return Err(format!("❌ FRAUDE ÉCONOMIQUE : Une transaction ne paie pas le minimum syndical au poids (Frais payés: {}, Requis: {}). Bloc rejeté !", tx.fee, min_fee));
                }
            }
            // ====================================================================
			
            if let TransactionType::MiningShare { miner_address: share_miner, nonce, hash, timestamp, .. } = &tx.tx_type {
            
				// Limite stricte et Dédoublonnage au niveau du consensus
				share_count += 1;
				if share_count > 50 { 
					return Err("❌ FRAUDE : Trop de parts de minage dans ce bloc (Max 50).".into()); 
				}
				if !share_hashes.insert(hash.clone()) { 
					return Err("❌ FRAUDE : Part de minage dupliquée dans le bloc.".into()); 
				}

				let parts: Vec<&str> = tx.public_key.split('|').collect();
				let l2_root = parts.get(0).cloned().unwrap_or("");
				let tx_root = parts.get(1).cloned().unwrap_or("");
				
				// L'adresse du mineur verrouille l'intégrité de la part
				let header_data = format!("{}{}{}{}{}{}{}", share_miner, share_height, timestamp, share_prev_hash, nonce, l2_root, tx_root);
				
				let hash_bytes = share_vm.calculate_hash(header_data.as_bytes()).map_err(|_| "Erreur VM P2Pool")?;
				
				if hex::encode(&hash_bytes) != *hash { 
					return Err("❌ MiningShare: Hash falsifié, identité volée ou corrompu !".into()); 
				}
				
				let hash_bigint = num_bigint::BigUint::parse_bytes(hash.as_bytes(), 16).unwrap_or_default();
				if hash_bigint > (&self.target * 20u32) { 
					return Err("❌ MiningShare: Preuve de travail insuffisante !".into()); 
				}
			}

			if let Some(sig) = &tx.wots_signature {
				let ki = hex::encode(&sig.public_key);
				if self.spent_key_images.contains(&ki) || !block_key_images.insert(ki) {
					return Err("Tentative de double-dépense détectée !".to_string());
				}
			}
			
			// RÈGLE STRICTE 2 : Vérification du HTLCClaim sur la chaîne
            if let TransactionType::HTLCClaim { secret } = &tx.tx_type {
                let secret_bytes = hex::decode(secret).unwrap_or_default();
                let hash_to_find = hex::encode(sha2::Sha256::digest(&secret_bytes));

                let mut buyer_addr = None;
                let mut expected_amount = 0;
                let mut lock_exists = false;

                // On fouille l'historique pour retrouver le contrat original
                for i in 0..=self.current_height {
                    if let Some(b) = self.get_block_by_height(i) {
                        for past_tx in &b.transactions {
                            if let TransactionType::HTLCLock { hash: lock_hash, .. } = &past_tx.tx_type {
                                if lock_hash == &hash_to_find { lock_exists = true; }
                            }
                            if let TransactionType::DexSettlement { swaps, .. } = &past_tx.tx_type {
                                for swap in swaps {
                                    if swap.htlc_hash == hash_to_find {
                                        buyer_addr = Some(swap.buyer_watt_address.clone());
                                        expected_amount = swap.watt_amount_flames;
                                    }
                                }
                            }
                        }
                    }
                }

                if !lock_exists || buyer_addr.is_none() { return Err("❌ FRAUDE : HTLCClaim sur un contrat inexistant.".to_string()); }
                if tx.outputs.len() != 1 { return Err("❌ FRAUDE : HTLCClaim doit avoir exactement 1 output.".to_string()); }
                if tx.outputs[0].aes_vault.parse::<u64>().unwrap_or(0) != expected_amount { return Err("❌ FRAUDE : HTLCClaim tente de voler un montant incorrect.".to_string()); }
                if tx.outputs[0].stealth_address != buyer_addr.unwrap() { return Err("❌ FRAUDE : HTLCClaim redirige les fonds vers la mauvaise adresse.".to_string()); }
            }

            // RÈGLE STRICTE 3 : Vérification mathématique de la Loterie
            if let TransactionType::LotteryPayout { target_block, winner_pubkey } = &tx.tx_type {
                if *target_block != current_height { return Err("❌ FRAUDE : LotteryPayout cible le mauvais bloc.".to_string()); }
                
                let (expected_jackpot, _) = self.get_jackpot_info(current_height);
                if tx.outputs.len() != 1 { return Err("❌ FRAUDE : LotteryPayout doit avoir 1 output.".to_string()); }
                if tx.outputs[0].aes_vault.parse::<u64>().unwrap_or(0) != expected_jackpot { return Err(format!("❌ FRAUDE : Montant du Jackpot falsifié (Attendu: {}).", expected_jackpot)); }
                if tx.outputs[0].stealth_address != format!("JACKPOT_{}", winner_pubkey) { return Err("❌ FRAUDE : Le Jackpot n'est pas envoyé au gagnant légitime.".to_string()); }
            }

            if let TransactionType::HTLCRefund { hash } = &tx.tx_type {
                let mut timeout = 0;
                let mut lock_found = false;
                
                for i in (0..=self.current_height).rev() {
                    if let Some(b) = self.get_block_by_height(i) {
                        for past_tx in &b.transactions {
                            if let TransactionType::HTLCLock { hash: lock_hash, timeout_block } = &past_tx.tx_type {
                                if lock_hash == hash {
                                    timeout = *timeout_block;
                                    lock_found = true;
                                    break;
                                }
                            }
                        }
                        if lock_found { break; }
                    }
                }
                
                if !lock_found || current_height < timeout {
                    return Err(format!("❌ FRAUDE : HTLCRefund invalide ou délai non expiré ! (Actuel: {}, Timeout: {})", current_height, timeout));
                }
            }
			
			if let TransactionType::L2Stake { l2_name, .. } = &tx.tx_type {
				if tx.outputs.is_empty() {
					return Err("❌ FRAUDE : Un L2Stake doit contenir un output de verrouillage !".into());
				}
				let stake_amount: u64 = tx.outputs[0].aes_vault.parse().unwrap_or(0);
				let required_stake = MONTANT_STAKE * FLAME;
				
				if stake_amount < required_stake {
					return Err(format!("❌ FRAUDE : Le staking pour '{}' est insuffisant (Requis: {} WATT) !", l2_name, MONTANT_STAKE));
				}
				if !tx.outputs[0].stealth_address.starts_with("L2_STAKE_") {
					return Err("❌ FRAUDE : L'adresse de destination du Staking est invalide !".into());
				}

				let mut is_valid_math = true;
				for (i, &val) in tx.outputs[0].lattice_commitment.t_vector.iter().enumerate() {
					let expected = if i == 0 { stake_amount } else { 0 };
					let diff = val.wrapping_sub(expected);
					if diff > 24 && diff < u64::MAX.wrapping_sub(24) {
						is_valid_math = false; break;
					}
				}
				
				if !is_valid_math {
					return Err("❌ FRAUDE : Fraude mathématique ! L'engagement Lattice ne correspond pas au montant déclaré.".into());
				}
			}
			
			if let TransactionType::L2Unstake { l2_name } = &tx.tx_type {
				let mut is_legit_staker = false;
				
				// On fouille l'historique pour vérifier que ce signataire a bien misé des fonds sur ce L2
				for i in 0..=self.current_height {
					if let Some(b) = self.get_block_by_height(i) {
						for past_tx in &b.transactions {
							if let TransactionType::L2Stake { l2_name: staked_name, sequencer_pubkey } = &past_tx.tx_type {
								// On vérifie que la clé publique de la transaction correspond à celle d'un vrai Séquenceur
								if staked_name == l2_name && sequencer_pubkey == &tx.public_key {
									is_legit_staker = true;
								}
							}
						}
					}
				}
				
				if !is_legit_staker {
					return Err(format!("❌ FRAUDE L2 : La clé {} n'a pas l'autorité pour Unstake le réseau {} (Aucun stake trouvé) !", tx.public_key, l2_name));
				}
			}
			
			if let TransactionType::L2BridgeLock { l2_target_name, .. } = &tx.tx_type {
				if tx.outputs.is_empty() {
					return Err("⛔ Rejet : Un L2BridgeLock doit contenir un output de verrouillage !".into());
				}

				let official_bridge_address = format!("BRIDGE_L2_{}", l2_target_name.to_uppercase());
				if tx.outputs[0].stealth_address != official_bridge_address {
					return Err(format!("⛔ Rejet : Les fonds doivent être envoyés au contrat L2 strict : {}", official_bridge_address));
				}

				let bridge_amount: u64 = tx.outputs[0].aes_vault.parse().unwrap_or(0);
				if bridge_amount == 0 {
					return Err("⛔ Rejet : Le montant du bridge est invalide ou nul !".into());
				}

				let mut is_valid_math = true;
				for (i, &val) in tx.outputs[0].lattice_commitment.t_vector.iter().enumerate() {
					let expected = if i == 0 { bridge_amount } else { 0 };
					let diff = val.wrapping_sub(expected);
					if diff > 24 && diff < u64::MAX.wrapping_sub(24) {
						is_valid_math = false; break;
					}
				}
				
				if !is_valid_math {
					return Err("⛔ Rejet : Fraude mathématique ! L'engagement Lattice du Bridge ne correspond pas au montant déclaré.".into());
				}

				println!("🌉 [BRIDGE L2] {} Flames verrouillés publiquement pour le réseau {}", bridge_amount, l2_target_name);
			}
			
            if let TransactionType::L2Anchor { l2_name, state_root, sequencer_signature, .. } = &tx.tx_type {
                let mut active_sequencers: std::collections::HashSet<String> = std::collections::HashSet::new();
                
                for result in self.db.iter() {
                    if let Ok((_, value)) = result {
                        if let Ok(b) = bincode::deserialize::<Block>(&value) {
                            for past_tx in &b.transactions {
                                if let TransactionType::L2Stake { l2_name: staked_name, sequencer_pubkey } = &past_tx.tx_type {
                                    if staked_name == l2_name { active_sequencers.insert(sequencer_pubkey.clone()); }
                                }
                                if let TransactionType::L2Unstake { l2_name: unstaked_name } = &past_tx.tx_type {
                                    if unstaked_name == l2_name { active_sequencers.clear(); }
                                }
                            }
                        }
                    }
                }

                if active_sequencers.is_empty() {
                    return Err(format!("❌ FRAUDE : La L2 '{}' n'a aucun staker actif !", l2_name));
                }

                let mut candidates: Vec<String> = active_sequencers.into_iter().collect();
                candidates.sort(); 

                // VRF V4 : On hash la racine L1 avec l'identité de chaque Séquenceur 
                // pour créer un score imprévisible (Similaire au consensus Algorand)
                let last_block_hash = &self.get_last_block().header.hash;
                let mut legit_sequencer = String::new();
                let mut lowest_score = [0xFFu8; 32]; // On cherche le plus petit hash

                for candidate in &candidates {
                    let mut vrf_hasher = sha2::Sha256::new();
                    vrf_hasher.update(last_block_hash.as_bytes());
                    vrf_hasher.update(l2_name.as_bytes());
                    vrf_hasher.update(candidate.as_bytes()); // Le mineur L1 ne peut pas manipuler ça facilement
                    
                    let mut vrf_hash = [0u8; 32];
                    vrf_hash.copy_from_slice(&vrf_hasher.finalize());
                    
                    if vrf_hash < lowest_score {
                        lowest_score = vrf_hash;
                        legit_sequencer = candidate.clone();
                    }
                }

                if let Ok(sig) = serde_json::from_str::<wots::WotsSignature>(sequencer_signature) {
                    let mut hasher = sha2::Sha512::new();
                    hasher.update(state_root.as_bytes());
                    let mut hash_array = [0u8; 64];
                    hash_array.copy_from_slice(&hasher.finalize());
                    
                    let mut hash_arr_32 = [0u8; 32];
                    hash_arr_32.copy_from_slice(&hash_array[0..32]);
                    
                    if hex::encode(&sig.public_key) != *legit_sequencer {
                        return Err(format!("❌ FRAUDE VRF : La clé publique de la signature ne correspond pas au gagnant !"));
                    }

                    if !wots::Wots::verify(&sig, &hash_arr_32) {
                        return Err(format!("❌ FRAUDE VRF : Signature WOTS+ invalide ! Le Séquenceur a soumis un faux bloc."));
                    }
                } else {
                    return Err(format!("❌ Signature WOTS+ illisible pour la L2 '{}'", l2_name));
                }
                
                println!("🔗 [INTEROPÉRABILITÉ] État '{}' ancré par le Séquenceur VRF légitime ! (Root: {})", l2_name, state_root);
            }

			total_block_fees += tx.fee;
		}

		if coinbase_count != 1 { 
			return Err("Un bloc doit contenir exactement une Coinbase.".to_string()); 
		}

		// LE VERDICT FINAL DE L'IMPRESSION MONÉTAIRE
        if total_coinbase_output > (expected_subsidy + total_block_fees) {
            return Err(format!("❌ FRAUDE : Inflation illégale dans la Coinbase ! Attendu: {}, Reçu: {}", 
                expected_subsidy + total_block_fees, total_coinbase_output));
        }

        for ki in block_key_images { self.spent_key_images.insert(ki); }
        
        let mut final_block = block;
        if final_block.header.target_hex.is_empty() {
            final_block.header.target_hex = format!("{:0>64}", self.target.to_str_radix(16));
        }
        
        let _ = self.push_block(&final_block);
        self.update_target();

        println!("✅ Bloc {} validé. Masse monétaire intègre.", current_height);
        Ok(())
	}
	
	// ====================================================================
    // LE TRIBUNAL CONSENSUS L2 (MicroBlocs)
    // ====================================================================
    pub fn validate_and_add_microblock(&mut self, micro_block: crate::block::MicroBlock) -> Result<(), String> {
        let l2_tree = self.db.open_tree("l2_blocks").map_err(|e| e.to_string())?;
        let key = micro_block.micro_index.to_be_bytes();
        
        // 0. ANTI-DOUBLON SILENCIEUX (Si on l'a déjà, on l'ignore)
        if l2_tree.contains_key(&key).unwrap_or(false) {
            return Ok(()); 
        }

        // 1. LE COUPERET DU POIDS L2 : 2 Mo Maximum !
        let mb_size = bincode::serialized_size(&micro_block).unwrap_or(0) as usize;
		println!("🏋️ [POIDS] Le poids du microbloc dans validate est de {} Ko.", mb_size/ 1_024);
        if mb_size > MAX_BLOCK_L2_SIZE_BYTES {
            return Err(format!("❌ FRAUDE L2 : Le MicroBloc dépasse la limite stricte de 2 Mo ({} Ko)", mb_size / 1024));
        }

        // 2. VÉRIFICATION DE LA SÉQUENCE STRICTE ET DE L'USAGE UNIQUE WOTS+
        if let Some(Ok((_, value))) = l2_tree.iter().rev().next() {
            if let Ok(last_mb) = bincode::deserialize::<crate::block::MicroBlock>(&value) {
                // Règle 2.A : Le micro_index doit suivre parfaitement
                if micro_block.micro_index != last_mb.micro_index + 1 {
                    return Err(format!("❌ FRAUDE L2 : Désynchronisation de l'index global (Attendu: {}, Reçu: {}) !", last_mb.micro_index + 1, micro_block.micro_index));
                }
                
                // Règle 2.B : Le key_index ne peut jamais être réutilisé (One-Time Signature)
                if micro_block.l1_parent_hash == last_mb.l1_parent_hash {
                    if micro_block.key_index != last_mb.key_index + 1 {
                        return Err(format!("❌ FRAUDE L2 : Réutilisation ou saut de clé WOTS+ détecté (Attendu: {}, Reçu: {}) !", last_mb.key_index + 1, micro_block.key_index));
                    }
                } else {
                    if micro_block.key_index != 0 {
                        return Err("❌ FRAUDE L2 : Le premier MicroBloc d'un nouveau Séquenceur doit utiliser le key_index 0 !".into());
                    }
                }
            }
        } else {
            // Tout premier MicroBloc absolu du réseau
            if micro_block.key_index != 0 {
                 return Err("❌ FRAUDE L2 : Le tout premier MicroBloc du réseau doit commencer à l'index de clé 0.".into());
            }
        }

        // 3. VÉRIFICATION DE L'ARBRE DE MERKLE (128 CLÉS OBLIGATOIRES)
        if micro_block.merkle_proof.len() != 128 {
            return Err(format!("❌ FRAUDE L2 : Le Séquenceur a fourni {} clés au lieu des 128 clés WOTS+ requises !", micro_block.merkle_proof.len()));
        }

        // 4. VÉRIFICATION DE L'ANCRAGE AU PARENT L1
        let mut parent_l1_block = None;
        for i in (0..=self.current_height).rev().take(10) {
            if let Some(b) = self.get_block_by_height(i) {
                if b.header.hash == micro_block.l1_parent_hash {
                    parent_l1_block = Some(b);
                    break;
                }
            }
        }
        let parent_block = parent_l1_block.ok_or("❌ L2 : Parent L1 introuvable ou trop vieux.")?;
        
        let mut calculated_root = micro_block.merkle_proof[0].clone();
        for i in 1..128 {
            let mut hasher = sha2::Sha512::new();
            hasher.update(calculated_root.as_bytes());
            hasher.update(micro_block.merkle_proof[i].as_bytes());
            calculated_root = hex::encode(hasher.finalize());
        }
        
        if calculated_root != parent_block.header.l2_root {
            return Err("❌ FRAUDE L2 : L'arbre de Merkle fourni a été falsifié (Ne correspond pas à la racine L1) !".into());
        }

        // Empêcher le crash "Index out of bounds"
        if micro_block.key_index >= 128 {
            return Err(format!("❌ FRAUDE L2 : key_index hors limite ({})", micro_block.key_index));
        }

        if micro_block.merkle_proof[micro_block.key_index as usize] != micro_block.sequencer_pubkey {
            return Err("❌ FRAUDE L2 : La clé publique du Séquenceur n'appartient pas à l'arbre validé !".into());
        }

        // 5. BLINDAGE DE L'USURPATION : La clé qui signe DOIT être la clé déclarée !
        let sig_pubkey_hex = hex::encode(&micro_block.sequencer_sig.public_key);
        if sig_pubkey_hex != micro_block.sequencer_pubkey {
            return Err("❌ FRAUDE L2 : La clé de la signature WOTS+ ne correspond pas à la clé Merkle déclarée (Usurpation) !".into());
        }

        // 6. SIGNATURE POST-QUANTIQUE WOTS+ DU MICROBLOC
        let mut tx_hasher = sha2::Sha512::new();
        for tx in &micro_block.transactions {
            tx_hasher.update(&tx.hash_data());
        }
        let txs_hash = hex::encode(tx_hasher.finalize());

        let mb_data = format!("{}{}{}{}{}", 
            micro_block.l1_parent_hash, 
            micro_block.micro_index, 
            micro_block.key_index, 
            micro_block.timestamp, 
            txs_hash 
        );

        let mut hasher = sha2::Sha512::new();
        hasher.update(mb_data.as_bytes());
        let mut hash_arr = [0u8; 64];
        hash_arr.copy_from_slice(&hasher.finalize());
        let mut hash_arr_32 = [0u8; 32];
        hash_arr_32.copy_from_slice(&hash_arr[0..32]);

        if !wots::Wots::verify(&micro_block.sequencer_sig, &hash_arr_32) {
            return Err("❌ FRAUDE L2 : Signature WOTS+ du MicroBloc invalide ou transactions altérées !".into());
        }

        // 7. STRUCTURE ET FRAIS DYNAMIQUES DES TXs INTERNES (2 Flames / Ko)
        if micro_block.transactions.is_empty() || micro_block.transactions[0].tx_type != TransactionType::MicroCoinbase {
            return Err("❌ FRAUDE L2 : La première transaction doit être la MicroCoinbase.".into());
        }

        let mut expected_fees = 0u64;
        let mut temp_spent = std::collections::HashSet::new();

        for tx in micro_block.transactions.iter().skip(1) {
            if !tx.is_valid() {
                return Err("❌ FRAUDE L2 : Transaction interne invalide (Maths Lattice ou Signature).".into());
            }

            let is_pure_l2 = !tx.outputs.is_empty() && tx.outputs.iter().all(|out| out.stealth_address.starts_with("L2_WATT_"));
            if !is_pure_l2 {
                return Err("❌ FRAUDE L2 : Un MicroBloc ne peut contenir que des transactions L2_WATT_ pures.".into());
            }

            let tx_weight_bytes = bincode::serialized_size(tx).unwrap_or(0) as usize;
            let weight_kb = (tx_weight_bytes as f64 / 1024.0).ceil() as u64;
            let min_fee = std::cmp::max(100, weight_kb * 2); 

            if tx.fee < min_fee {
                return Err(format!("❌ FRAUDE L2 : Transaction sous-payée incluse par le Séquenceur (Frais: {}, Requis au poids: {}).", tx.fee, min_fee));
            }

            expected_fees += tx.fee;

            if let Some(sig) = &tx.wots_signature {
                let ki = hex::encode(&sig.public_key);
                if self.spent_key_images.contains(&ki) || temp_spent.contains(&ki) {
                    return Err("❌ FRAUDE L2 : Double dépense UTXO détectée dans le MicroBloc !".into());
                }
                temp_spent.insert(ki);
            }
        }

        // 8. VÉRIFICATION DE LA MICRO-COINBASE (Pas de planche à billets !)
        // Vérifier que l'output existe avant de le lire !
        if micro_block.transactions[0].outputs.is_empty() {
            return Err("❌ FRAUDE L2 : La MicroCoinbase ne contient aucun output !".into());
        }

        let actual_fees: u64 = micro_block.transactions[0].outputs[0].aes_vault.parse().unwrap_or(u64::MAX);
        if actual_fees > expected_fees {
            return Err(format!("❌ FRAUDE L2 : Le Séquenceur a imprimé {} Flames au lieu des {} collectés !", actual_fees, expected_fees));
        }

        // 9. SAUVEGARDE SLED DÉFINITIVE
        for ki in temp_spent {
            self.spent_key_images.insert(ki);
        }

        let value = bincode::serialize(&micro_block).unwrap();
        l2_tree.insert(&key, value).map_err(|e| e.to_string())?;
        self.db.flush().map_err(|e| e.to_string())?;

        println!("⚡ [L2 TRIBUNAL] MicroBloc {}/128 validé ! (Taille: {} Ko, Frais légitimes: {} Flames)", 
                 micro_block.micro_index, mb_size / 1024, expected_fees);

        Ok(())
    }
    
    pub fn update_target(&mut self) {
        let current_len = self.current_height + 1; 
        if current_len < 2 { return; }

        let window_size = FENETRE_DIFFICULTY;
        
        let start_idx = if current_len > window_size as u64 { 
            current_len - window_size as u64 
        } else { 
            0 
        };
        
        let mut total_time = 0;
        let mut num_blocks = 0;
        
        for i in (start_idx + 1)..current_len {
            let prev = self.get_block_by_height(i - 1).unwrap();
            let curr = self.get_block_by_height(i).unwrap();
            let mut time_taken = curr.header.timestamp - prev.header.timestamp;
            
            if time_taken > (EXPECTED_BLOCK_TIME * 3) as i64 { time_taken = (EXPECTED_BLOCK_TIME * 3) as i64; }
            if time_taken <= 0 { time_taken = 1; } 
            
            total_time += time_taken as u64;
            num_blocks += 1;
        }
        
        if num_blocks == 0 { return; }
        let avg_time = total_time / num_blocks;

        let max_target = num_bigint::BigUint::from_bytes_be(&[0xFF; 32]);
        let dampening = 3; 
        let damped_time = (avg_time + (EXPECTED_BLOCK_TIME * (dampening - 1))) / dampening;
        
        self.target = &self.target * damped_time / EXPECTED_BLOCK_TIME;
        if self.target > max_target { self.target = max_target; }
    }
    
    pub fn recalculate_target_from_scratch(&mut self) {
        let max_target = num_bigint::BigUint::from_bytes_be(&[0xFF; 32]);
        let mut current_target = &max_target >> INITIAL_DIFFICULTY_SHIFT; 
        let window_size = 17;
        
        for i in 2..=(self.current_height + 1) { 
            let start_idx = if i > window_size { i - window_size } else { 0 };
            let mut total_time = 0;
            let mut num_blocks = 0;
            
            for j in (start_idx + 1)..i {
                let prev = self.get_block_by_height(j - 1).unwrap();
                let curr = self.get_block_by_height(j).unwrap();
                let mut time_taken = curr.header.timestamp - prev.header.timestamp;
                if time_taken > (EXPECTED_BLOCK_TIME * 3) as i64 { time_taken = (EXPECTED_BLOCK_TIME * 3) as i64; }
                if time_taken <= 0 { time_taken = 1; } 
                total_time += time_taken as u64;
                num_blocks += 1;
            }
            
            if num_blocks > 0 {
                let avg_time = total_time / num_blocks;
                let dampening = 3; 
                let damped_time = (avg_time + (EXPECTED_BLOCK_TIME * (dampening - 1))) / dampening;
                current_target = &current_target * damped_time / EXPECTED_BLOCK_TIME;
                if current_target > max_target { current_target = max_target.clone(); }
            }
        }
        self.target = current_target;
    }
    
    pub fn calculate_total_work(chain_to_measure: &[Block]) -> (BigUint, bool) {
        let max_target = num_bigint::BigUint::from_bytes_be(&[0xFF; 32]);
        let mut current_target = &max_target >> INITIAL_DIFFICULTY_SHIFT;
        let mut total_work = num_bigint::BigUint::from(0u32);
        let window_size = FENETRE_DIFFICULTY;
        
        let mut is_pow_valid = true;

        for i in 0..chain_to_measure.len() {
            if i >= 2 {
                let start_idx = if i > window_size { i - window_size } else { 0 };
                let mut total_time = 0;
                let mut num_blocks = 0;
                
                for j in (start_idx + 1)..i {
                    let prev = &chain_to_measure[j - 1];
                    let curr = &chain_to_measure[j];
                    let mut time_taken = curr.header.timestamp - prev.header.timestamp;
                    if time_taken > (EXPECTED_BLOCK_TIME * 3) as i64 { time_taken = (EXPECTED_BLOCK_TIME * 3) as i64; }
                    if time_taken <= 0 { time_taken = 1; } 
                    total_time += time_taken as u64;
                    num_blocks += 1;
                }
                
                if num_blocks > 0 {
                    let avg_time = total_time / num_blocks;
                    let dampening = 3; 
                    let damped_time = (avg_time + (EXPECTED_BLOCK_TIME * (dampening - 1))) / dampening;
                    current_target = &current_target * damped_time / EXPECTED_BLOCK_TIME;
                    if current_target > max_target { current_target = max_target.clone(); }
                }
            }
            
            // VERIFICATION STRICTE DU POW
            let hash_val = num_bigint::BigUint::parse_bytes(chain_to_measure[i].header.hash.as_bytes(), 16).unwrap_or_else(|| max_target.clone());
            if hash_val > current_target {
                is_pow_valid = false;
            }

            total_work += &max_target / &current_target;
        }
        (total_work, is_pow_valid)
    }
	
	pub fn find_utxo(&self, utxo_id: &str) -> Option<TransactionOutput> {
        for i in (0..=self.current_height).rev() {
            if let Some(block) = self.get_block_by_height(i) {
                for tx in &block.transactions {
                    for out in &tx.outputs {
                        if out.kyber_capsule == utxo_id {
                            return Some(out.clone());
                        }
                    }
                }
            }
        }
        None
    }
}