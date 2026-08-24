use crate::block::{Block, BlockHeader};
use crate::transaction::{Transaction, TransactionType};
use std::fs;
use num_bigint::BigUint;
use std::collections::HashSet;
use randomx_rs::{RandomXFlag, RandomXCache, RandomXVM};
use crate::WattError;
use sha2::Digest;  

const FLAME: u64 = 1_000_000_000;
const MATURITY_BLOCKS: u64 = 3; // 12 Prod
const EXPECTED_BLOCK_TIME: u64 = 120;    // 2 mins (120 s)
const INITIAL_REWARD: u64 = 15 * FLAME; // 15 Watts
const TAIL_EMISSION: u64 = 600_000_000; // 0.6 Watts
const EMISSION_DECAY_SHIFT: u32 = 18;   // Ajusté pour ~21 ans
const INITIAL_DIFFICULTY_SHIFT: u32 = 12;
pub const LOTTERY_TIME_BLOCK: u64 = 10; // 720 blocks pour un jour
// Changement de Dataset tous les 255 blocs pour tuer les ASICs !
pub const EPOCH_BLOCKS: u64 = 255;  // toutes les 8H30 (8,5 Heures = 255 blocks)
const MONTANT_STAKE: u64 = 100; // 10 000 Pour la prod (840 $)


pub struct Blockchain {
    pub chain: Vec<Block>,
    pub target: BigUint, 
    pub spent_key_images: HashSet<String>, 
}

impl Blockchain {
    pub fn new() -> Self {
        let max_target = BigUint::from_bytes_be(&[0xFF; 32]);
        let initial_target = &max_target >> INITIAL_DIFFICULTY_SHIFT;

        let mut blockchain = Blockchain {
            chain: Vec::new(),
            target: initial_target,
            spent_key_images: HashSet::new(),
        };
        blockchain.chain.push(Block::genesis());
        blockchain
    }
    
    // 💡 Trouve la graine RandomX appropriée pour une hauteur de bloc donnée
    pub fn get_epoch_seed(&self, height: u64) -> String {
		if height <= EPOCH_BLOCKS {
			return self.chain[0].header.hash.clone(); // Ère 0 : On utilise le Genesis
		}
		let epoch = (height - 1) / EPOCH_BLOCKS;
		// 💡 FIX : On recule de 11 blocs dans l'époque précédente pour être SÛR 
		// que le bloc est déjà miné par tout le monde lors du warm-up !
		let target_block = (epoch * EPOCH_BLOCKS).saturating_sub(11);
		
		if (target_block as usize) < self.chain.len() {
			self.chain[target_block as usize].header.hash.clone()
		} else {
			self.chain[0].header.hash.clone() // Fallback sécurité
		}
	}

    pub fn load_from_disk(path: &str) -> Result<Self, WattError> {
        let path_obj = std::path::Path::new(path);
        if !path_obj.exists() {
            println!("🌱 Aucune blockchain locale trouvée, initialisation du Genesis Block.");
            let new_chain = Blockchain::new();
            new_chain.save_to_disk(path);
            return Ok(new_chain);
        }

        let data = fs::read_to_string(path)?;
        let mut chain: Vec<Block> = serde_json::from_str(&data)?;
		
		// 💡 Migration automatique des anciennes chaînes (target_hex manquant)
		let mut migrated = false;
		for block in &mut chain {
			if block.header.target_hex.is_empty() || block.header.target_hex == "0" {
				// On recalcule avec le target de la blockchain au moment du load
				let max_target = BigUint::from_bytes_be(&[0xFF; 32]);
				let initial = max_target >> 12_u32;
				block.header.target_hex = format!("{:0>64}", initial.to_str_radix(16));
				migrated = true;
			}
		}
		if migrated {
			println!("🔄 Migration automatique : target_hex ajouté aux anciens blocs.");
		}
		
        println!("💾 HISTORIQUE CHARGÉ : {} blocs retrouvés.", chain.len());
        
        let max_target = BigUint::from_bytes_be(&[0xFF; 32]);
        let mut spent_key_images = HashSet::new();
        
        for block in &chain {
            for tx in &block.transactions {
                if tx.tx_type != TransactionType::Coinbase {
                    for input in &tx.inputs {
                        spent_key_images.insert(input.mpc_ring.key_image.clone());
                    }
                }
            }
        }

        let mut blockchain = Blockchain {
            chain,
            target: max_target >> INITIAL_DIFFICULTY_SHIFT, 
            spent_key_images,
        };
        
        blockchain.recalculate_target_from_scratch();
        Ok(blockchain)
    }
    
    pub fn save_to_disk(&self, filename: &str) {
        let json = serde_json::to_string_pretty(&self.chain).unwrap();
        fs::write(filename, json).expect("Impossible d'écrire sur le disque !");
        println!("💾 Blockchain sauvegardée en toute sécurité dans '{}'.", filename);
    }
	
    pub fn save_microblock_to_disk(filename: &str, micro_block: &crate::block::MicroBlock) {
        // Lecture de la L2 chain existante
        let mut l2_chain: Vec<crate::block::MicroBlock> = std::fs::read_to_string(filename)
            .ok()
            .and_then(|data| serde_json::from_str(&data).ok())
            .unwrap_or_else(Vec::new);
        
        // Sécurité KISS : On vérifie qu'on n'enregistre pas un doublon
        let is_duplicate = l2_chain.iter().any(|mb| 
            mb.l1_parent_hash == micro_block.l1_parent_hash && mb.micro_index == micro_block.micro_index
        );

        if !is_duplicate {
            l2_chain.push(micro_block.clone());
            let json = serde_json::to_string_pretty(&l2_chain).unwrap();
            std::fs::write(filename, json).unwrap_or_else(|_| println!("⚠️ Échec d'écriture L2"));
        }
    }
	
	/// OPTIMISATION PRO (O(1)) : Calcule la récompense directement 
    /// à partir de la récompense de base du bloc précédent.
    pub fn get_next_base_reward(prev_base_reward: u64) -> u64 {
        let decay = prev_base_reward >> EMISSION_DECAY_SHIFT;
        let expected = prev_base_reward.saturating_sub(decay);
        
        if expected < TAIL_EMISSION {
            TAIL_EMISSION
        } else {
            expected
        }
    }
	
    // Calcul de la Supply Totale (Précision Absolue)
    pub fn get_total_supply(&self) -> u64 {
        let mut supply = 0;
        
        for block in &self.chain {
            let mut block_fees = 0;
            
            // 1. On calcule le total des frais payés dans ce bloc (monnaie recyclée)
            for tx in &block.transactions {
                if tx.tx_type != TransactionType::Coinbase {
                    block_fees += tx.fee;
                }
            }
            
            // 2. On fait la somme de TOUTES les parts de la Coinbase 
            for tx in &block.transactions {
                if tx.tx_type == TransactionType::Coinbase {
                    let mut coinbase_total = 0;
                    
                    // On additionne le Validateur (20%) + Les Mineurs (80%) + La Loterie (Robin des Bois + Taxes)
                    for out in &tx.outputs {
                        if let Ok(val) = out.aes_vault.parse::<u64>() {
                            coinbase_total += val;
                        }
                    }
                    
                    // 3. La monnaie NOUVELLEMENT CRÉÉE = (Total distribué) - (Frais recyclés)
                    supply += coinbase_total.saturating_sub(block_fees);
                }
            }
        }
        supply
    }

    // On ajoute le paramètre `l2_db_path` pour faire le pont avec le disque
    pub fn get_jackpot_info(&self, _target_height: u64, l2_db_path: Option<&str>) -> (u64, Vec<(String, String)>) {
        let mut tickets = Vec::new();
        let mut pot = 0u64;

        // On garde une trace des blocs L1 "récents" (post-tirage) pour filtrer le L2
        let mut valid_l1_hashes = std::collections::HashSet::new();

        // 1. Lecture robuste du L1 (KISS)
        'block_loop: for block in self.chain.iter().rev() {
            // On enregistre ce bloc dans la liste blanche
            valid_l1_hashes.insert(block.header.hash.clone()); 
            
            for tx in &block.transactions {
                // On prend TOUS les tickets trouvés
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
                
                // LE FREIN : Dès qu'on tombe sur le PRÉCÉDENT tirage, on arrête de remonter le temps !
                if let TransactionType::LotteryPayout { .. } = &tx.tx_type {
                    break 'block_loop; 
                }
            }
        }

        // 2. Aspiration L2 (avec le filtre chronologique)
        if let Some(path) = l2_db_path {
            if let Ok(data) = std::fs::read_to_string(path) {
                if let Ok(l2_chain) = serde_json::from_str::<Vec<crate::block::MicroBlock>>(&data) {
                    for mb in l2_chain {
                        // LE FILTRE EST ICI : On ignore les micro-blocs ancrés à un vieux bloc L1 (déjà purgé)
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

        // On trie pour garantir un gagnant déterministe
        tickets.sort_by(|a, b| a.0.cmp(&b.0));
        (pot, tickets)
    }

    pub fn get_current_jackpot(&self, l2_db_path: Option<&str>) -> (u64, Vec<(String, String)>) {
		let current_height = self.chain.len() as u64;
		let next_draw = current_height + (LOTTERY_TIME_BLOCK - (current_height % LOTTERY_TIME_BLOCK));
		
		self.get_jackpot_info(next_draw, l2_db_path)
	}

    pub fn prepare_block_template(&mut self, transactions: Vec<Transaction>, miner_address: &str, l2_db_path: Option<&str>, l2_keys: Vec<crate::wots::WotsKeyPair>) -> (Block, BigUint, Vec<crate::wots::WotsKeyPair>) {
        let current_height = self.chain.len() as u64;
        println!("\n⏳ Préparation du Bloc {}...", current_height);

        let mut valid_transactions = Vec::new();
        let mut l1_total_fees = 0;
        let mut temp_spent_images = self.spent_key_images.clone(); 
		
		// ====================================================================
        // BOUCLIER DE MATURITÉ NODE-SIDE (Infaillible)
        // Le Nœud calcule lui-même les clés immatures en scannant les derniers blocs.
        // On ne fait plus confiance au Wallet !
        // ====================================================================
        let mut immature_pubkeys = std::collections::HashSet::new();
        let scan_limit = current_height.saturating_sub(MATURITY_BLOCKS);
        
        for block in self.chain.iter().rev() {
            if block.header.index <= scan_limit { break; }
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

        // ====================================================================
        // TRAITEMENT DU MEMPOOL CLASSIQUE (L1 + L2)
        // ====================================================================
        for tx in &transactions {
            if tx.is_valid() {
                
                // BOUCLIER : On laisse les TX pures L2 au Séquenceur !
                let is_pure_l2 = !tx.outputs.is_empty() && tx.outputs.iter().all(|out| out.stealth_address.starts_with("L2_WATT_"));
                if is_pure_l2 && tx.tx_type != TransactionType::MicroCoinbase {
                    continue; // Le L1 l'ignore, elle reste dans le mempool pour le L2
                }
				
                let mut immature = false;
                if tx.tx_type != TransactionType::Coinbase {
                    for input in &tx.inputs {
                        for decoy in &input.mpc_ring.ring_decoys {
                            if immature_pubkeys.contains(decoy) {
                                immature = true;
                                break;
                            }
                        }
                        if immature { break; }
                    }
                }
                if immature { 
                    println!("⛔ Rejet : Tentative de dépense d'une récompense immature (Coinbase/Loto < {} blocs) !", MATURITY_BLOCKS);
                    continue; 
                }

                if matches!(tx.tx_type, TransactionType::LotteryPayout { .. }) {
                    valid_transactions.push(tx.clone()); continue;
                }
				
				// BOUCLIER HTLC
                if let TransactionType::HTLCRefund { hash } = &tx.tx_type {
                    let mut timeout = 0;
                    let mut lock_found = false;
                    
                    for b in self.chain.iter().rev() {
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
                    
                    if !lock_found {
                        println!("⛔ HTLCRefund : Contrat d'origine introuvable !");
                        continue; // On rejette
                    }
                    if current_height < timeout {
                        println!("⛔ HTLCRefund : Délai temporel non expiré (Actuel: {} < Requis: {}).", current_height, timeout);
                        continue; // On rejette
                    }
                }
				
				// BOUCLIER MINEUR L1 : Vérification stricte du Staking L2
				if let TransactionType::L2Stake { l2_name, .. } = &tx.tx_type {
					if tx.outputs.is_empty() {
						println!("⛔ Rejet : Un L2Stake doit contenir un output de verrouillage !");
						continue; // Utilise 'return Err(...)' dans validate_and_add_external_block
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

					// VÉRIFICATION HOMOMORPHE ABSOLUE (Analyse du bruit post-quantique)
					// Comme le Blinding Factor est nul, la valeur est juste (montant + bruit).
					// On vérifie que la déviation ne dépasse pas l'amplitude théorique du bruit CBD (24 max).
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
				
				// On vérifie la transaction du bridge
				if let TransactionType::L2BridgeLock { l2_target_name, .. } = &tx.tx_type {
					if tx.outputs.is_empty() {
						println!("⛔ Rejet : Un L2BridgeLock doit contenir un output de verrouillage !");
						continue; // 💡 REMPLACE PAR `return Err(...)` dans validate_and_add_external_block
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

					// LE BOUCLIER LATTICE POUR LE BRIDGE
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
						continue; // 💡 REMPLACE PAR `return Err(...)` dans validate_and_add_external_block
					}

					println!("🌉 [BRIDGE L2] {} Flames verrouillés publiquement pour le réseau {}", bridge_amount, l2_target_name);
				}

                // ANTI-DOUBLE DÉPENSE
				let mut double_spend = false;
                if tx.tx_type != TransactionType::Coinbase {
                    for input in &tx.inputs {
                        if temp_spent_images.contains(&input.mpc_ring.key_image) {
                            double_spend = true; break;
                        }
                    }
                }
                
                // VALIDATION FINALE : On ajoute la transaction au bloc
				if !double_spend {
                    l1_total_fees += tx.fee; 
                    valid_transactions.push(tx.clone()); 
                    for input in &tx.inputs { temp_spent_images.insert(input.mpc_ring.key_image.clone()); }
                }
            }
        }

        let previous_block = self.chain.last().unwrap();
        
        // Tolérance de synchronisation d'horloge
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

        // CALCUL MATHÉMATIQUE STRICT DE L'ÉMISSION (Indépendant des UTXOs)
        let mut expected_subsidy = INITIAL_REWARD;
        for _ in 0..current_height {
            expected_subsidy = Blockchain::get_next_base_reward(expected_subsidy);
        }

        let mut allowed_subsidy = expected_subsidy;
		
        // 1. La récompense de base (Subvention pure)
        if allowed_subsidy < TAIL_EMISSION { allowed_subsidy = TAIL_EMISSION; }
        println!("📉 Émission monétaire : {:.9} Watts", (allowed_subsidy as f64) / (FLAME as f64));

        // BOUCLIER ANTI-FERMES DE MINAGE ("L'Effet Robin des Bois")
        let mut slashed_for_jackpot = 0;

        // 2. On punit le mineur en tapant dans sa subvention ACTUELLE (même si c'est la Tail Emission)
        if current_height > 17 && time_taken < 30 {
            let time_penalty_ratio = time_taken as f64 / 30.0;
            
            // On calcule le slash sur `allowed_subsidy` !
            let penalty_subsidy = (allowed_subsidy as f64 * time_penalty_ratio) as u64;
            slashed_for_jackpot = allowed_subsidy.saturating_sub(penalty_subsidy);
            allowed_subsidy = penalty_subsidy; // La nouvelle limite autorisée
            
            println!("🚨 [ANTI-FARM] Hashrate extrême détecté ! (Bloc trouvé en {}s).", time_taken);
            println!("🎰 [ROBIN DES BOIS] Pénalité appliquée : {} WATT confisqués et envoyés au Jackpot L1 !", slashed_for_jackpot as f64 / 1_000_000_000.0);
        }

        // Note : expected_subsidy intègre DÉJÀ la vérification du TAIL_EMISSION
        // grâce à notre fonction get_next_base_reward() !
        // Taxe loterie TOUJOURS collectée (1% des frais)
        // On ne la conditionne plus → le pot peut s'accumuler normalement
        // DISTRIBUTION DES FRAIS ET DE LA LOTERIE
		// Les taxes de base + l'argent confisqué aux gros mineurs vont dans la Loterie !
        let l1_lottery_tax = l1_total_fees / 100;
        let l1_miner_fees = l1_total_fees - l1_lottery_tax;
        let total_lottery_tax = l1_lottery_tax + slashed_for_jackpot; 

        
        println!("📉 Frais du mineur L1 : {:.9} Watts", (l1_miner_fees as f64) / (FLAME as f64));
		
		// RÉPARTITION ÉQUITABLE (80/20) P2POOL NATIF SUR LA SUBVENTION UNIQUEMENT
        let mut coinbase_outputs = Vec::new();
        let mut valid_shares = Vec::new();

        // OPTIMISATION ZÉRO-DAY : On vérifie s'il y a des parts avant de charger RandomX !
        let mut has_shares = false;
        for tx in &transactions {
            if matches!(tx.tx_type, TransactionType::MiningShare { .. }) {
                has_shares = true;
                break;
            }
        }

        // On n'allume la lourde Machine Virtuelle QUE si c'est nécessaire
        if has_shares {
            // On initialise une VM légère pour valider les parts !
            let share_height = current_height.saturating_sub(1);
            let share_prev_hash = if self.chain.len() >= 2 {
                self.chain[self.chain.len() - 2].header.hash.clone()
            } else {
                self.chain[0].header.hash.clone()
            };
            let share_seed = self.get_epoch_seed(share_height);

            let flags = randomx_rs::RandomXFlag::get_recommended_flags();
            let cache = randomx_rs::RandomXCache::new(flags, share_seed.as_bytes()).unwrap();
            let vm = randomx_rs::RandomXVM::new(flags, Some(cache), None).unwrap();

            for tx in &transactions {
                if let TransactionType::MiningShare { nonce, hash, timestamp, .. } = &tx.tx_type {
                    let parts: Vec<&str> = tx.public_key.split('_').collect();
                    let l2_root = parts.get(0).cloned().unwrap_or("");
                    let tx_root = parts.get(1).cloned().unwrap_or("");
                    
                    // On valide avec les paramètres temporels du bloc précédent
                    let header_data = format!("{}{}{}{}{}{}", share_height, timestamp, share_prev_hash, nonce, l2_root, tx_root);
                    
                    if let Ok(hash_bytes) = vm.calculate_hash(header_data.as_bytes()) {
                        if hex::encode(&hash_bytes) == *hash {
                            if valid_shares.len() < 50 { valid_shares.push(tx.clone()); }
                        }
                    }
                }
            }
        }

        if !valid_shares.is_empty() {
            // 1. Calcul de la part théorique globale de la communauté (80% de la SUBVENTION SEULEMENT)
            let base_community_reward = allowed_subsidy * 80 / 100; 
            
            // 2. Calcul de la part individuelle (division entière)
            let share_reward = base_community_reward / valid_shares.len() as u64; 
            
            // 3. Recalcul EXACT de ce qui sera réellement distribué
            let exact_community_reward = share_reward * valid_shares.len() as u64; 
            
            // 4. Le validateur prend le reste de la subvention (20% + poussière) + 100% DES FRAIS L1
            let final_finder_reward = (allowed_subsidy - exact_community_reward) + l1_miner_fees;

            println!("🤝 [P2POOL] Répartition : Validateur (Base {:.9}  + Frais {:.9}  = {:.9} Watts).\nCommunauté ({:.9} Watts).", 
                ((allowed_subsidy - exact_community_reward)  as f64) / (FLAME as f64),
				(l1_miner_fees as f64) / (FLAME as f64),
				(final_finder_reward as f64) / (FLAME as f64),
                (exact_community_reward as f64) / (FLAME as f64)
            );
            println!("🤝 [P2POOL] Il y a {} parts de minages de : {:.9} Watts", 
                valid_shares.len(), 
                (share_reward as f64) / (FLAME as f64)
            );

            // 1. Output pour le trouveur (Subvention 20% + TOUS les frais)
            coinbase_outputs.push(crate::transaction::TransactionOutput {
                stealth_address: format!("COINBASE_{}", miner_address), 
                kyber_capsule: format!("COINBASE_CAPSULE_{}", current_height),
                aes_vault: final_finder_reward.to_string(), 
                lattice_commitment: crate::lattice::LWECommitment::commit(final_finder_reward, &[0u64; crate::lattice::LATTICE_DIM]),
            });

            // 2. Outputs pour la communauté
            for (i, share_tx) in valid_shares.iter().enumerate() {
                if let TransactionType::MiningShare { miner_address: share_addr, .. } = &share_tx.tx_type {
                    coinbase_outputs.push(crate::transaction::TransactionOutput {
                        stealth_address: format!("COINBASE_{}", share_addr), 
                        kyber_capsule: format!("SHARE_CAPSULE_{}_{}", current_height, i),
                        aes_vault: share_reward.to_string(), 
                        lattice_commitment: crate::lattice::LWECommitment::commit(share_reward, &[0u64; crate::lattice::LATTICE_DIM]),
                    });
                }
            }
        } else {
            // S'il est tout seul sur le réseau, il prend les 100% de la subvention + 100% des frais
            let total_solo_reward = allowed_subsidy + l1_miner_fees;
            coinbase_outputs.push(crate::transaction::TransactionOutput {
                stealth_address: format!("COINBASE_{}", miner_address), 
                kyber_capsule: format!("COINBASE_CAPSULE_{}", current_height),
                aes_vault: total_solo_reward.to_string(), 
                lattice_commitment: crate::lattice::LWECommitment::commit(total_solo_reward, &[0u64; crate::lattice::LATTICE_DIM]),
            });
        }

        // 3. On alimente la réserve (Taxe de 1% + Argent confisqué aux fermes)
        if total_lottery_tax > 0 {
            coinbase_outputs.push(crate::transaction::TransactionOutput {
                stealth_address: "LOTTERY_RESERVE".to_string(), 
                kyber_capsule: format!("TAX_CAPSULE_{}", current_height),
                aes_vault: total_lottery_tax.to_string(), 
                lattice_commitment: crate::lattice::LWECommitment::commit(total_lottery_tax, &[0u64; crate::lattice::LATTICE_DIM]),
            });
        }
		
		// ===================== LOTERIE L1 =====================
        // On s'assure juste de ne pas tirer au bloc 0
        if current_height % LOTTERY_TIME_BLOCK == 0 && current_height > 0 {
            // On récupère l'historique...
            let (mut jackpot_amount, mut tickets) = self.get_jackpot_info(current_height, l2_db_path);
            
            // On inclut les tickets du bloc courant (ceux du mempool validés)
            for tx in &valid_transactions {
                if let TransactionType::HTLCLottery { target_block, player_pubkey } = &tx.tx_type {
                    if *target_block == current_height && !tx.outputs.is_empty() {
                        let ticket_id = tx.outputs[0].kyber_capsule.clone();
                        tickets.push((ticket_id, player_pubkey.clone()));
                        
                        // On ajoute le prix de ce ticket à la cagnotte immédiate
                        for out in &tx.outputs {
                            if out.stealth_address == "LOTTERY_RESERVE" {
                                jackpot_amount += out.aes_vault.parse::<u64>().unwrap_or(0);
                            }
                        }
                    }
                }
            }

            // On trie à nouveau pour garantir un ordre déterministe avant le VRF
            tickets.sort_by(|a, b| a.0.cmp(&b.0));
            
            if !tickets.is_empty() {
                
                // TRIBUNAL VRF POUR LE LOTO (L'entropie vient du hash du bloc précédent)
                let last_block_hash = &previous_block.header.hash;
                let mut vrf_hasher = sha2::Sha256::new();
                vrf_hasher.update(last_block_hash.as_bytes());
                vrf_hasher.update(b"LOTTERY"); // Séparation de domaine
                let vrf_hash = vrf_hasher.finalize();
                
                let mut hash_bytes = [0u8; 8];
                hash_bytes.copy_from_slice(&vrf_hash[0..8]);
                let random_number = u64::from_be_bytes(hash_bytes);
                
                // Le tirage au sort cryptographique absolu !
                let winner_index = (random_number as usize) % tickets.len();
                let winner_ticket = &tickets[winner_index];
                let winner_pubkey = winner_ticket.1.clone();

                println!("🎰 [LOTO VRF] Le ticket {} remporte le Jackpot de {} Flames !", 
                         winner_ticket.0, jackpot_amount);

                let payout_output = crate::transaction::TransactionOutput {
                    stealth_address: format!("JACKPOT_{}", winner_pubkey),
                    kyber_capsule: format!("JACKPOT_PAYOUT_{}", current_height),
                    aes_vault: jackpot_amount.to_string(),
                    lattice_commitment: crate::lattice::LWECommitment::commit(jackpot_amount, &[0u64; crate::lattice::LATTICE_DIM]),
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
            public_key: "COINBASE_SIG".to_string(), wots_signature: None,
        };
        valid_transactions.insert(0, coinbase_tx);
		
		// GÉNÉRATION DU TROUSSEAU L2
        // Les clés sont maintenant reçues en argument, générées HORS du Mutex !
        let mut l2_pubkeys = Vec::with_capacity(128);
        for k in &l2_keys {
            l2_pubkeys.push(k.public_key.clone());
        }

        // Création de l'arbre de Merkle simple pour la racine
        let mut l2_root = l2_pubkeys[0].clone();
        for i in 1..128 {
            l2_root = crate::merkle_ring::MpcRingSignature::hash_nodes(&l2_root, &l2_pubkeys[i]);
        }

        let new_header = BlockHeader {
            index: current_height,
            timestamp: new_timestamp, // Utilise le temps corrigé
            previous_hash: previous_block.header.hash.clone(),
            hash: String::new(),
            nonce: 0,
            target_hex: format!("{:0>64}", self.target.to_str_radix(16)),
            l2_root, 
            tx_root: String::new(), 
        };

        let mut block = Block { header: new_header, transactions: valid_transactions };
		
		// Calcul et injection de la racine de Merkle
        block.header.tx_root = block.calculate_tx_root();
		
        (block, self.target.clone(), l2_keys) // On retourne les clés L2 !
    }
    
    pub fn resolve_fork(&mut self, new_chain: Vec<Block>) -> bool {
        if new_chain.is_empty() || new_chain[0].header.hash != self.chain[0].header.hash { return false; }

        let flags = RandomXFlag::get_recommended_flags();
        
        // On utilise le bon Dataset !
        let mut current_seed = self.get_epoch_seed(new_chain[1].header.index);
        let mut cache = RandomXCache::new(flags, current_seed.as_bytes()).unwrap();
        let mut vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap(); 

        for i in 1..new_chain.len() {
            let previous_block = &new_chain[i - 1];
            let current_block = &new_chain[i];
            if current_block.header.previous_hash != previous_block.header.hash { return false; }
            
            // Vérification Merkle pendant le fork
            if current_block.header.tx_root != current_block.calculate_tx_root() { return false; }

            let needed_seed = self.get_epoch_seed(current_block.header.index);
            if needed_seed != current_seed {
                current_seed = needed_seed;
                cache = RandomXCache::new(flags, current_seed.as_bytes()).unwrap();
                vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap();
            }

            let header_data = format!("{}{}{}{}{}{}", 
                current_block.header.index, 
                current_block.header.timestamp, 
                current_block.header.previous_hash, 
                current_block.header.nonce, 
                current_block.header.l2_root,
                current_block.header.tx_root 
            );
			let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
            let expected_hash = hex::encode(&hash_bytes);

            if current_block.header.hash != expected_hash { return false; }
        }

        // BOUCLIER ANTI-51% (MESS) POUR LES REORGANISATIONS TOTALES
        let my_work = Blockchain::calculate_total_work(&self.chain);
        let mut new_work = Blockchain::calculate_total_work(&new_chain);

        let reorg_depth = self.chain.len().saturating_sub(1);
        if reorg_depth > 10 {
            let penalty_shift = std::cmp::min((reorg_depth - 10) as u32, 256);
            println!("🛡️ [MESS] 🚨 ALERTE : Tentative de réorganisation depuis le Genesis (Profondeur: {} blocs) !", reorg_depth);
            println!("🛡️ [MESS] 📉 Pénalité appliquée : Poids de la chaîne attaquante divisé par 2^{}", penalty_shift);
            new_work = new_work >> penalty_shift;
        }

        // Si on a déjà miné des blocs, on refuse une chaîne plus faible
        if new_work <= my_work && self.chain.len() > 1 {
            println!("❌ [FORK] La nouvelle chaîne complète n'a pas assez de Preuve de Travail (MESS appliqué).");
            return false;
        }

        self.chain = new_chain;
        self.recalculate_target_from_scratch();
        
        let mut new_spent = HashSet::new();
        for block in &self.chain {
            for tx in &block.transactions {
                if tx.tx_type != TransactionType::Coinbase {
                    for input in &tx.inputs {
                        new_spent.insert(input.mpc_ring.key_image.clone());
                    }
                }
            }
        }
        self.spent_key_images = new_spent;
		
		// On met à jour le prix en RAM après une réorganisation !
        for block in self.chain.iter().rev() {
            let mut found_price = false;
            for tx in block.transactions.iter().rev() {
                if let TransactionType::DexSettlement { clearing_price_sats, .. } = &tx.tx_type {
                    crate::api::LAST_PRICE_SATS.store(*clearing_price_sats, std::sync::atomic::Ordering::Relaxed);
                    found_price = true;
                    break;
                }
            }
            if found_price { break; }
        }

        true
    }
    
    pub fn resolve_partial_fork(&mut self, new_blocks: Vec<Block>) -> bool {
		if new_blocks.is_empty() {
			println!("⚠️ [FORK] Lot de blocs vide reçu, ignoré.");
			return false;
		}

		let start_index = new_blocks[0].header.index as usize;
		if start_index == 0 {
			return self.resolve_fork(new_blocks);
		}
		if start_index > self.chain.len() {
			println!("❌ [FORK] Index trop grand ({}) > longueur de la chaîne ({})", start_index, self.chain.len());
			return false;
		}

		// Recherche de l'ancêtre commun (sécurisée)
		let mut ancestor_index = start_index.saturating_sub(1);
		let mut found_ancestor = false;

		while ancestor_index > 0 && ancestor_index < self.chain.len() {
			if self.chain[ancestor_index].header.hash == new_blocks[0].header.previous_hash {
				found_ancestor = true;
				break;
			}
			ancestor_index = ancestor_index.saturating_sub(1);
		}

		if !found_ancestor && self.chain[0].header.hash != new_blocks[0].header.previous_hash {
			println!("❌ [FORK] Impossible de trouver un ancêtre commun.");
			return false;
		}

		// Construction sécurisée de la chaîne théorique
		let end = std::cmp::min(ancestor_index + 1, self.chain.len());
		let mut theoretical_chain = self.chain[0..end].to_vec();

		// On initialise le curseur AVANT de fusionner les nouveaux blocs !
		let mut last_verified_timestamp = theoretical_chain.last()
			.map(|b| b.header.timestamp)
			.unwrap_or(0);

		// MAINTENANT on peut ajouter les nouveaux blocs
		theoretical_chain.extend(new_blocks.clone());

        // Petite fonction interne pour lire la graine sur la chaîne théorique
        let get_theoretical_seed = |height: u64, t_chain: &[Block]| -> String {
			if height <= EPOCH_BLOCKS {
				return t_chain[0].header.hash.clone();
			}
			let epoch = (height - 1) / EPOCH_BLOCKS;
			let target_block = (epoch * EPOCH_BLOCKS).saturating_sub(11);
			if (target_block as usize) < t_chain.len() {
				t_chain[target_block as usize].header.hash.clone()
			} else {
				t_chain[0].header.hash.clone()
			}
		};
		
        // ====================================================================
		// ⏱️ PARAMÈTRES DU BOUCLIER TEMPOREL (Anti-Time Warp)
		// ====================================================================
		let current_time = chrono::Utc::now().timestamp();
		let max_future_tolerance = 7200; // Tolérance de 2 heures

		// 2. Le Tribunal RandomX et de Cohérence Temporelle
		let flags = RandomXFlag::get_recommended_flags();
		let mut current_seed = get_theoretical_seed(new_blocks[0].header.index, &theoretical_chain);
		let mut cache = RandomXCache::new(flags, current_seed.as_bytes()).unwrap();
		let mut vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap(); 
		
		for block in &new_blocks {
			// ====================================================================
			// 🛡️ APPLICATION DU BOUCLIER TEMPOREL SUR LE FORK
			// ====================================================================
			if block.header.timestamp > current_time + max_future_tolerance {
				println!(
					"❌ [FORK] FRAUDE TEMPORELLE : Le bloc {} est trop loin dans le futur ! (Timestamp: {}, Actuel: {})", 
					block.header.index, block.header.timestamp, current_time
				);
				return false;
			}

			if block.header.timestamp <= last_verified_timestamp {
				println!(
					"❌ [FORK] FRAUDE TEMPORELLE : Le temps ne peut pas stagner ou reculer au bloc {} ! ({} <= {})", 
					block.header.index, block.header.timestamp, last_verified_timestamp
				);
				return false;
			}

			// Le bloc est temporellement sain, on met à jour notre curseur
			last_verified_timestamp = block.header.timestamp;
			// ====================================================================

			// Vérification Merkle pendant le fork partiel
			if block.header.tx_root != block.calculate_tx_root() { return false; }

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
				println!("❌ [FORK] La nouvelle branche contient un bloc frauduleux (Index {})", block.header.index);
				return false; 
			}
		}

        // 3. Pesée des deux chaînes (Preuve de travail) et Bouclier MESS
        let my_work = Blockchain::calculate_total_work(&self.chain);
        let mut new_work = Blockchain::calculate_total_work(&theoretical_chain);

        let reorg_depth = self.chain.len().saturating_sub(ancestor_index + 1);

        // BOUCLIER ANTI-51% (Modified Exponential Subjective Scoring)
        if reorg_depth > 10 {
            let penalty_shift = std::cmp::min((reorg_depth - 10) as u32, 256); // Limite anti-overflow
            println!("🛡️ [MESS] 🚨 ALERTE : Tentative de réorganisation profonde détectée (Profondeur: {} blocs) !", reorg_depth);
            println!("🛡️ [MESS] 📉 Pénalité appliquée : Poids de la chaîne attaquante divisé par 2^{}", penalty_shift);
            new_work = new_work >> penalty_shift;
        }

        if new_work > my_work {
            println!("✅ [FORK] Nouvelle chaîne adoptée ! On recule de {} blocs et on en applique {}.", 
                     self.chain.len() - ancestor_index - 1, new_blocks.len());
            
            self.chain = theoretical_chain;
            self.recalculate_target_from_scratch(); 

            // Remise à zéro des clés dépensées
            let mut new_spent = HashSet::new();
            for block in &self.chain {
                for tx in &block.transactions {
                    if tx.tx_type != TransactionType::Coinbase {
                        for input in &tx.inputs {
                            new_spent.insert(input.mpc_ring.key_image.clone());
                        }
                    }
                }
            }
            self.spent_key_images = new_spent;
			
			// ON LIT S IL Y A MATCH, QUAND ON A ADOPTÉ LA NOUVELLE CHAÎNE !
            for block in self.chain.iter().rev() {
                let mut found_price = false;
                for tx in block.transactions.iter().rev() {
                    if let TransactionType::DexSettlement { clearing_price_sats, .. } = &tx.tx_type {
                        crate::api::LAST_PRICE_SATS.store(*clearing_price_sats, std::sync::atomic::Ordering::Relaxed);
                        found_price = true;
                        break;
                    }
                }
                if found_price { break; }
            }
			
            return true;
        }
        
        println!("❌ [FORK] La nouvelle chaîne n'a pas assez de Preuve de Travail.");
        false
    }
    
	pub fn validate_and_add_external_block(&mut self, block: Block) -> Result<(), String> {
		let last_block = self.chain.last().unwrap();
		
		// =========================================================
		// BOUCLIER ANTI-RETOUR VERS LE FUTUR (Time Warp Attack)
		// =========================================================
		let current_time = chrono::Utc::now().timestamp();
		let max_future_tolerance = 7200; // Tolérance de 2h (7200 secondes)
		
		if block.header.timestamp > current_time + max_future_tolerance {
			return Err(format!("❌ FRAUDE TEMPORELLE : Ce bloc vient du futur ! (Timestamp: {}, Actuel: {})", block.header.timestamp, current_time));
		}
		
		// Le temps doit avancer, ou du moins ne pas reculer par rapport au dernier bloc
		if block.header.timestamp <= last_block.header.timestamp {
			return Err("❌ FRAUDE TEMPORELLE : Le temps ne peut pas reculer ou stagner par rapport au bloc précédent.".to_string());
		}
		// =========================================================

		// 1. Vérifications de base de la structure
		if block.header.index != last_block.header.index + 1 { 
			return Err("Index de bloc invalide.".to_string()); 
		}
		if block.header.previous_hash != last_block.header.hash { 
			return Err("Rupture de la chaîne.".to_string()); 
		}
		
		// BOUCLIER MERKLE : On recalcule l'arbre et on rejette si ça ne matche pas !
		if block.header.tx_root != block.calculate_tx_root() {
			return Err("❌ FRAUDE : La racine de Merkle (tx_root) est invalide ou falsifiée !".to_string());
		}

		// 2. Le Tribunal RandomX (Vérification du PoW)
		let flags = randomx_rs::RandomXFlag::get_recommended_flags();
		let seed = self.get_epoch_seed(block.header.index);
		let cache = randomx_rs::RandomXCache::new(flags, seed.as_bytes()).map_err(|_| "Erreur Cache")?;
		let vm = randomx_rs::RandomXVM::new(flags, Some(cache.clone()), None).map_err(|_| "Erreur VM")?;

		// L2 ANCHORING
		let header_data = format!("{}{}{}{}{}{}", 
			block.header.index, 
			block.header.timestamp, 
			block.header.previous_hash, 
			block.header.nonce,
			block.header.l2_root,
			block.header.tx_root // Verrouille l'intégrité des transactions !
		);
		
		let hash_bytes = vm.calculate_hash(header_data.as_bytes()).map_err(|_| "Erreur calcul")?;
		
		if block.header.hash != hex::encode(&hash_bytes) { 
			return Err("Hash frauduleux.".to_string()); 
		}

		let hash_bigint = num_bigint::BigUint::parse_bytes(block.header.hash.as_bytes(), 16).unwrap_or_default();
		if hash_bigint > self.target { 
			return Err("Preuve de travail insuffisante.".to_string()); 
		}

		// --- LOGIQUE DE CONSENSUS ÉTENDUE ---
		let mut coinbase_count = 0;
		let mut total_block_fees = 0u64;
		let mut block_key_images = HashSet::new();
		let current_height = block.header.index;
		
		// ====================================================================
		// BOUCLIER DE MATURITÉ NODE-SIDE (Infaillible)
		// On calcule les clés immatures en scannant les derniers blocs.
		// On inclut Coinbase (L1), MicroCoinbase (L2) et Jackpot (Loto)
		// ====================================================================
		let mut immature_pubkeys = std::collections::HashSet::new();
		let scan_limit = current_height.saturating_sub(MATURITY_BLOCKS);
		
		for b in self.chain.iter().rev() {
			if b.header.index <= scan_limit { break; }
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
		// ====================================================================
		
		// CALCUL MATHÉMATIQUE STRICT DE L'ÉMISSION (Indépendant des UTXOs)
        let mut expected_subsidy = INITIAL_REWARD;
        for _ in 0..current_height {
            expected_subsidy = Blockchain::get_next_base_reward(expected_subsidy);
        }

        // On prépare la VM pour les parts P2Pool UNE SEULE FOIS !
        let share_height = current_height.saturating_sub(1);
        let share_prev_hash = last_block.header.previous_hash.clone();
        let share_seed = self.get_epoch_seed(share_height);
        
        let share_vm = if seed == share_seed {
            // OPTIMISATION : On recycle le cache déjà calculé
            randomx_rs::RandomXVM::new(flags, Some(cache.clone()), None).map_err(|_| "Erreur VM Part")?
        } else {
            let new_share_cache = randomx_rs::RandomXCache::new(flags, share_seed.as_bytes()).map_err(|_| "Erreur Cache Part")?;
            randomx_rs::RandomXVM::new(flags, Some(new_share_cache), None).map_err(|_| "Erreur VM Part")?
        };

        // Premier passage : validation
        for tx in &block.transactions {
			if tx.tx_type == TransactionType::Coinbase {
				coinbase_count += 1;
				continue;
			}
			
			// BOUCLIER STRICT : Une MicroCoinbase L2 n'a STRICTEMENT RIEN à faire dans un bloc L1 !
			if tx.tx_type == TransactionType::MicroCoinbase {
				return Err("❌ FRAUDE : Présence d'une MicroCoinbase L2 dans un bloc L1 !".to_string());
			}

			// Mise à jour prix DEX
			if let TransactionType::DexSettlement { clearing_price_sats, .. } = &tx.tx_type {
				crate::api::LAST_PRICE_SATS.store(*clearing_price_sats, std::sync::atomic::Ordering::Relaxed);
				continue;
			}

			// === MATURITÉ (Vérification cryptographique in-hackable) ===
			if tx.tx_type != TransactionType::Coinbase {
				for input in &tx.inputs {
					for decoy in &input.mpc_ring.ring_decoys {
						if immature_pubkeys.contains(decoy) {
							return Err(format!(
								"❌ FRAUDE : Tentative de dépense d'une récompense immature (Coinbase/MicroCoinbase/Loto < {} blocs) !", 
								MATURITY_BLOCKS
							));
						}
					}
				}
			}
			
			// VÉRIFICATION STRICTE DES PARTS DE MINAGE (Anti-Triche P2Pool)
            if let TransactionType::MiningShare { nonce, hash, timestamp, .. } = &tx.tx_type {
                
                // Décodage des métadonnées
                let parts: Vec<&str> = tx.public_key.split('_').collect();
                let l2_root = parts.get(0).cloned().unwrap_or("");
                let tx_root = parts.get(1).cloned().unwrap_or("");
                
                // Reconstitution
                let header_data = format!("{}{}{}{}{}{}", share_height, timestamp, share_prev_hash, nonce, l2_root, tx_root);
                
                // On utilise la share_vm créée avant la boucle ! (Instantané)
                let hash_bytes = share_vm.calculate_hash(header_data.as_bytes()).map_err(|_| "Erreur VM P2Pool")?;
                
                if hex::encode(&hash_bytes) != *hash { 
                    return Err("❌ MiningShare: Hash falsifié, l2_root ou tx_root corrompus !".into()); 
                }
                
                let hash_bigint = num_bigint::BigUint::parse_bytes(hash.as_bytes(), 16).unwrap_or_default();
                if hash_bigint > (&self.target * 20u32) { 
                    return Err("❌ MiningShare: Preuve de travail insuffisante !".into()); 
                }
            }

			// Validité intrinsèque
			if !tx.is_valid() { 
				return Err("Signature WOTS+ invalide.".to_string()); 
			}

			// Anti-Double Dépense
			for input in &tx.inputs {
				if self.spent_key_images.contains(&input.mpc_ring.key_image) || 
				   !block_key_images.insert(input.mpc_ring.key_image.clone()) {
					return Err("Tentative de double-dépense détectée !".to_string());
				}
			}

			// VRAI ATOMIC SWAP – vérification SHA256 (compatible Bitcoin HTLC)
			if let TransactionType::HTLCClaim { secret } = &tx.tx_type {
				let secret_bytes = hex::decode(secret).unwrap_or_default();
				let provided_hash = hex::encode(sha2::Sha256::digest(&secret_bytes));
				if provided_hash != tx.public_key {
					return Err("❌ HTLC Claim : secret invalide".into());
				}
			}

            // BOUCLIER : Validation du délai pour un HTLCRefund entrant
            if let TransactionType::HTLCRefund { hash } = &tx.tx_type {
                let mut timeout = 0;
                let mut lock_found = false;
                for b in self.chain.iter().rev() {
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
                if !lock_found || current_height < timeout {
                    return Err(format!("❌ FRAUDE : HTLCRefund invalide ou délai non expiré ! (Actuel: {}, Timeout: {})", current_height, timeout));
                }
            }
			
			// LE TRIBUNAL DES SÉQUENCEURS EXTERNES (L2 ANCHORING)
			// BOUCLIER MINEUR L1 : Vérification stricte du Staking L2
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

				// 🛡️ VÉRIFICATION HOMOMORPHE ABSOLUE
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
			
			// On vérifie la transaction du bridge
			if let TransactionType::L2BridgeLock { l2_target_name, .. } = &tx.tx_type {
				if tx.outputs.is_empty() {
					println!("⛔ Rejet : Un L2BridgeLock doit contenir un output de verrouillage !");
					continue; // 💡 REMPLACE PAR `return Err(...)` dans validate_and_add_external_block
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

				// 🛡️ LE BOUCLIER LATTICE POUR LE BRIDGE (La pièce manquante !)
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
					continue; // 💡 REMPLACE PAR `return Err(...)` dans validate_and_add_external_block
				}

				println!("🌉 [BRIDGE L2] {} Flames verrouillés publiquement pour le réseau {}", bridge_amount, l2_target_name);
			}
			
            if let TransactionType::L2Anchor { l2_name, state_root, sequencer_signature, .. } = &tx.tx_type {
                let mut active_sequencers: std::collections::HashSet<String> = std::collections::HashSet::new();
                
                // 1. On liste tous les candidats
                for b in self.chain.iter() {
                    for past_tx in &b.transactions {
                        if let TransactionType::L2Stake { l2_name: staked_name, sequencer_pubkey } = &past_tx.tx_type {
                            if staked_name == l2_name { active_sequencers.insert(sequencer_pubkey.clone()); }
                        }
                        if let TransactionType::L2Unstake { l2_name: unstaked_name } = &past_tx.tx_type {
                            if unstaked_name == l2_name { active_sequencers.clear(); }
                        }
                    }
                }

                if active_sequencers.is_empty() {
                    return Err(format!("❌ FRAUDE : La L2 '{}' n'a aucun staker actif !", l2_name));
                }

                // 2. TRIBUNAL VRF : Qui a le droit de parler ce tour-ci ?
                let mut candidates: Vec<String> = active_sequencers.into_iter().collect();
                candidates.sort(); 

                // Le hash du bloc précédent détermine le gagnant
                let last_block_hash = &self.chain.last().unwrap().header.hash;
                
                let mut vrf_hasher = sha2::Sha256::new();
                vrf_hasher.update(last_block_hash.as_bytes());
                vrf_hasher.update(l2_name.as_bytes());
                let vrf_hash = vrf_hasher.finalize();

                let mut hash_bytes = [0u8; 8];
                hash_bytes.copy_from_slice(&vrf_hash[0..8]);
                let winner_index = (u64::from_be_bytes(hash_bytes) as usize) % candidates.len();
                let legit_sequencer = &candidates[winner_index];

                // 3. Vérification de l'Usurpation
                if let Ok(sig) = serde_json::from_str::<crate::wots::WotsSignature>(sequencer_signature) {
                    let mut hasher = sha2::Sha512::new();
                    hasher.update(state_root.as_bytes());
                    let mut hash_array = [0u8; 64];
                    hash_array.copy_from_slice(&hasher.finalize());
                    
                    // On vérifie que la signature appartient bien au GAGNANT DU VRF !
                    if !crate::wots::WotsKeyPair::verify(legit_sequencer, &sig, &hash_array) {
                        return Err(format!("❌ FRAUDE VRF : Le Séquenceur a soumis un bloc, mais il a perdu la loterie de ce tour !"));
                    }
                } else {
                    return Err(format!("❌ Signature illisible pour la L2 '{}'", l2_name));
                }
                
                println!("🔗 [INTEROPÉRABILITÉ] État '{}' ancré par le Séquenceur VRF légitime ! (Root: {})", l2_name, state_root);
            }

			total_block_fees += tx.fee;
		}

		if coinbase_count != 1 { 
			return Err("Un bloc doit contenir exactement une Coinbase.".to_string()); 
		}

		// Validation finale de la Coinbase
		let coinbase_tx = &block.transactions[0];
		let actual_reward: u64 = coinbase_tx.outputs[0].aes_vault.parse().unwrap_or(u64::MAX);
		if actual_reward > (expected_subsidy + total_block_fees) {
			return Err(format!("Inflation illégale ! Attendu: {}, Reçu: {}", 
				expected_subsidy + total_block_fees, actual_reward));
		}

        // Tout est bon → on applique
        for ki in block_key_images { 
            self.spent_key_images.insert(ki); 
        }
		
		// On s'assure que le bloc reçu a bien son target_hex (compatibilité ancienne chaîne)
		let mut final_block = block;
		if final_block.header.target_hex.is_empty() {
			final_block.header.target_hex = format!("{:0>64}", self.target.to_str_radix(16));
		}
		self.chain.push(final_block);
		self.update_target();

		println!("✅ Bloc {} validé. Masse monétaire intègre.", current_height);
		Ok(())
	}
    
    pub fn update_target(&mut self) {
        let current_len = self.chain.len(); 
        if current_len < 2 { return; }

        let window_size = 17; // Fenêtre glissante (Inspiré de Monero/Zcash) prod 144 (24h)
        let start_idx = if current_len > window_size { current_len - window_size } else { 0 };
        
        let mut total_time = 0;
        let mut num_blocks = 0;
        
        for i in (start_idx + 1)..current_len {
            let prev = &self.chain[i - 1];
            let curr = &self.chain[i];
            let mut time_taken = curr.header.timestamp - prev.header.timestamp;
            
            // Bornes de sécurité : Empêche un pirate de truquer son horloge pour faire chuter la difficulté
            if time_taken > (EXPECTED_BLOCK_TIME * 3) as i64 { time_taken = (EXPECTED_BLOCK_TIME * 3) as i64; }
            if time_taken <= 0 { time_taken = 1; } 
            
            total_time += time_taken as u64;
            num_blocks += 1;
        }
        
        if num_blocks == 0 { return; }
        let avg_time = total_time / num_blocks;

        let max_target = num_bigint::BigUint::from_bytes_be(&[0xFF; 32]);
        let dampening = 3; // Réaction agressive pour les Hashrate Spikes
        let damped_time = (avg_time + (EXPECTED_BLOCK_TIME * (dampening - 1))) / dampening;
        
        self.target = &self.target * damped_time / EXPECTED_BLOCK_TIME;
        if self.target > max_target { self.target = max_target; }
    }
    
    pub fn recalculate_target_from_scratch(&mut self) {
        let max_target = num_bigint::BigUint::from_bytes_be(&[0xFF; 32]);
        let mut current_target = &max_target >> INITIAL_DIFFICULTY_SHIFT; 
        let window_size = 17;
        
        for i in 2..=self.chain.len() {
            let start_idx = if i > window_size { i - window_size } else { 0 };
            let mut total_time = 0;
            let mut num_blocks = 0;
            
            for j in (start_idx + 1)..i {
                let prev = &self.chain[j - 1];
                let curr = &self.chain[j];
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
    
    pub fn calculate_total_work(chain_to_measure: &[Block]) -> BigUint {
        let max_target = num_bigint::BigUint::from_bytes_be(&[0xFF; 32]);
        let mut current_target = &max_target >> INITIAL_DIFFICULTY_SHIFT;
        let mut total_work = num_bigint::BigUint::from(0u32);
        let window_size = 17;

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
            total_work += &max_target / &current_target;
        }
        total_work
    }
    
    pub fn get_random_decoys(&self, count: usize) -> Vec<String> {
		let mut all_pubkeys = Vec::new();
		for block in &self.chain {
			for tx in &block.transactions {
				if tx.tx_type != TransactionType::Coinbase {
					// 💡 FIX : On utilise la clé publique WOTS+ de l'expéditeur, 
					// pas l'adresse furtive du destinataire !
					all_pubkeys.push(tx.public_key.clone());
				}
			}
		}
		
		// On dé-duplique toutes les clés disponibles sur la blockchain
		let unique_pubkeys: Vec<String> = all_pubkeys.into_iter()
			.collect::<std::collections::HashSet<_>>()
			.into_iter()
			.collect();
			
		let len = unique_pubkeys.len();
		if len == 0 { return vec![]; }
		if len <= count { return unique_pubkeys; } 

		let mut rng = rand::thread_rng();
		let mut selected = std::collections::HashSet::new();
		use rand::Rng;

		// On garantit que le nœud renverra un set 100% unique
		while selected.len() < count {
			let idx1 = rng.gen_range(0..len);
			let idx2 = rng.gen_range(0..len);
			let idx3 = rng.gen_range(0..len);
			
			let chosen_idx = idx1.max(idx2).max(idx3); // Biais vers la nouveauté
			selected.insert(unique_pubkeys[chosen_idx].clone());
		}

		selected.into_iter().collect()
	}
}