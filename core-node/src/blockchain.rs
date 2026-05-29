use crate::block::{Block, BlockHeader};
use crate::transaction::{Transaction, TransactionType};
use std::fs;
use num_bigint::BigUint;
use std::collections::HashSet;
use randomx_rs::{RandomXFlag, RandomXCache, RandomXVM};
use rand::seq::SliceRandom;
use crate::WattError;
use crate::lattice::LATTICE_DIM; 

const FLAME: u64 = 1_000_000_000;
const MATURITY_BLOCKS: u64 = 3; 
const EXPECTED_BLOCK_TIME: u64 = 60;    
// 18.000.000 / (144 blocs/jour * 365 jours * 20 ans) = ~1.7 Watts/bloc
const INITIAL_REWARD: u64 = 15 * FLAME; // 15 Watts
const TAIL_EMISSION: u64 = 600_000_000; // 0.6 Watts
const EMISSION_DECAY_SHIFT: u32 = 18;   // Ajusté pour ~21 ans
const INITIAL_DIFFICULTY_SHIFT: u32 = 12;
pub const LOTTERY_TIME_BLOCK: u64 = 10;
// 💡 Changement de Dataset tous les 51 blocs pour tuer les ASICs !
pub const EPOCH_BLOCKS: u64 = 51; 

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
        let target_block = ((height - 1) / EPOCH_BLOCKS) * EPOCH_BLOCKS;
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
                        spent_key_images.insert(input.pq_ring_signature.key_image.clone());
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
	
	/// 💡 OPTIMISATION PRO (O(1)) : Calcule la récompense directement 
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
	
    // 💡 Calcul de la Supply Totale (Précision Absolue)
    pub fn get_total_supply(&self) -> u64 {
        let mut supply = 0;
        // On commence à 1 car le bloc 0 (Genesis) est souvent un cas spécial
        for block in &self.chain {
            for tx in &block.transactions {
                if tx.tx_type == TransactionType::Coinbase {
                    // On extrait la valeur réelle inscrite dans le vault du mineur
                    if let Ok(reward) = tx.outputs[0].aes_vault.parse::<u64>() {
                        supply += reward;
                    }
                }
            }
        }
        supply
    }

    // 💡 Calcul du Jackpot en cours
    pub fn get_jackpot_info(&self, target_height: u64) -> (u64, Vec<(String, String)>) {
		let mut tickets = Vec::new();
		let mut pot = 0u64;

		if target_height < LOTTERY_TIME_BLOCK { return (0, tickets); }
		let start = target_height - LOTTERY_TIME_BLOCK;

		for i in start..target_height {
			if (i as usize) >= self.chain.len() { continue; }
			let block = &self.chain[i as usize];

			for tx in &block.transactions {
				// 1. On lit les taxes minées par la Coinbase (L'argent est dans LOTTERY_RESERVE)
				if tx.tx_type == TransactionType::Coinbase {
					for out in &tx.outputs {
						if out.stealth_address == "LOTTERY_RESERVE" {
							pot += out.aes_vault.parse::<u64>().unwrap_or(0);
						}
					}
				}

				// 2. On lit les tickets achetés (L'argent est AUSSI dans LOTTERY_RESERVE)
				if let TransactionType::HTLCLottery { target_block, player_pubkey } = &tx.tx_type {
					if *target_block == target_height && !tx.outputs.is_empty() {
						let ticket_id = tx.outputs[0].kyber_capsule.clone();
						tickets.push((ticket_id, player_pubkey.clone()));
						
						// On lit le montant EXACT payé pour le ticket
						for out in &tx.outputs {
							if out.stealth_address == "LOTTERY_RESERVE" {
								pot += out.aes_vault.parse::<u64>().unwrap_or(0);
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
		let current_height = self.chain.len() as u64;
		// Calcul du prochain palier 
		let next_draw = current_height + (LOTTERY_TIME_BLOCK - (current_height % LOTTERY_TIME_BLOCK));
		
		// On réutilise la vraie logique de calcul !
		self.get_jackpot_info(next_draw)
	}

    pub fn prepare_block_template(&mut self, transactions: Vec<Transaction>, miner_address: &str) -> (Block, BigUint) {
        let current_height = self.chain.len() as u64;
        println!("\n⏳ Préparation du Bloc {}...", current_height);

        let mut valid_transactions = Vec::new();
        let mut total_fees = 0; 

        for tx in transactions {
            if tx.is_valid() { 
                // 🛡️ VÉRIFICATION DE MATURITÉ (uniquement sur les Coinbase, comme Bitcoin)
				let mut immature = false;

				if tx.tx_type != TransactionType::Coinbase {
					for input in &tx.inputs {
						// Si cet input dépense une récompense de minage
						if input.source_height > 0 {  
							let confirmations = current_height.saturating_sub(input.source_height);
							
							if confirmations < MATURITY_BLOCKS {
								immature = true;
								println!("⛔ Input immature (Coinbase) : {} confirmations < {} (hauteur source: {})", 
										 confirmations, MATURITY_BLOCKS, input.source_height);
								break;
							}
						}
						// Si source_height == 0 → c'est une TX normale → pas de maturité
					}
				}

				if immature {
					continue; // On ignore cette TX pour le bloc en cours
				}
				
				// Exception pour les payouts de loterie (ils n'ont pas de fee et ne doivent pas être ignorés)
                if matches!(tx.tx_type, TransactionType::LotteryPayout { .. }) {
                    valid_transactions.push(tx);
                    continue;
                }

                // 🛡️ 2. VÉRIFICATION DOUBLE DÉPENSE
                let mut double_spend = false;
                if tx.tx_type != TransactionType::Coinbase {
                    for input in &tx.inputs {
                        if self.spent_key_images.contains(&input.pq_ring_signature.key_image) {
                            double_spend = true;
                            break;
                        }
                    }
                }
                
                if !double_spend {
                    println!("💸 Transaction détectée ! (Pourboire: {} Flames)", tx.fee);
                    total_fees += tx.fee; 
                    // 💡 C'est la toute DERNIÈRE opération qu'on fait avec `tx` pour éviter l'erreur de "move"
                    valid_transactions.push(tx); 
                } else {
                    println!("🗑️ Transaction ignorée par le mineur (Déjà dépensée).");
                }
            }
        }
        
        let previous_block = self.chain.last().unwrap();
        let mut time_taken = chrono::Utc::now().timestamp() - previous_block.header.timestamp;
        if time_taken <= 0 { time_taken = 1; } 
        
        let max_target = BigUint::from_bytes_be(&[0xFF; 32]);
        let initial_target = &max_target >> INITIAL_DIFFICULTY_SHIFT; 

        let difficulty_x100 = (&initial_target * 100u64) / &self.target;
        let diff_int = &difficulty_x100 / 100u64;
        let diff_dec = &difficulty_x100 % 100u64;

        if current_height > 1 { println!("⚙️  Dernier bloc miné en {}s", time_taken); }
        println!("🎯 Difficulté cible : {}.{:02}x", diff_int, diff_dec);

        // 💡 OPTIMISATION O(1) : Calcul de la récompense "Smooth"
        let expected_subsidy = if current_height == 0 {
            INITIAL_REWARD
        } else {
            // On retrouve la récompense de base du bloc précédent
            let prev_block = self.chain.last().unwrap();
            let mut prev_fees = 0;
            for tx in &prev_block.transactions {
                if tx.tx_type != TransactionType::Coinbase {
                    prev_fees += tx.fee;
                }
            }
            
            let prev_total_reward: u64 = prev_block.transactions[0].outputs[0].aes_vault.parse().unwrap_or(INITIAL_REWARD);
            let prev_base_reward = prev_total_reward.saturating_sub(prev_fees);
            
            // Appel de la fonction O(1) !
            Self::get_next_base_reward(prev_base_reward)
        };

        // Note : expected_subsidy intègre DÉJÀ la vérification du TAIL_EMISSION
        // grâce à notre fonction get_next_base_reward() !
        
        let lottery_tax = total_fees / 100; // 1% de taxe
        let miner_fees = total_fees - lottery_tax;
        let mut calculated_reward = expected_subsidy + miner_fees;
        if calculated_reward < TAIL_EMISSION { calculated_reward = TAIL_EMISSION; }

        println!("📉 Émission monétaire : {:.9} Watts", (expected_subsidy as f64) / (FLAME as f64));
        println!("📉 Le mineur gagne : {:.9} Watts (Taxe Loterie: {} Flames)", (calculated_reward as f64) / (FLAME as f64), lottery_tax);

        let mut coinbase_outputs = vec![
            crate::transaction::TransactionOutput {
                stealth_address: format!("COINBASE_{}", miner_address), 
                kyber_capsule: format!("COINBASE_CAPSULE_{}", current_height),
                aes_vault: calculated_reward.to_string(), 
                lattice_commitment: crate::lattice::LWECommitment::commit(calculated_reward, [0, 0, 0, 0]),
            }
        ];

        // 💡 On envoie la taxe dans la réserve transparente !
        if lottery_tax > 0 {
            coinbase_outputs.push(crate::transaction::TransactionOutput {
                stealth_address: "LOTTERY_RESERVE".to_string(), 
                kyber_capsule: format!("TAX_CAPSULE_{}", current_height),
                aes_vault: lottery_tax.to_string(), 
                lattice_commitment: crate::lattice::LWECommitment::commit(lottery_tax, [0, 0, 0, 0]),
            });
        }
		
		// ===================== LOTERIE L1 =====================
        // ✅ On s'assure juste de ne pas tirer au bloc 0
		if current_height % LOTTERY_TIME_BLOCK == 0 && current_height > 0 {
            let (jackpot_amount, tickets) = self.get_jackpot_info(current_height);
            
            if !tickets.is_empty() {
                let winner_ticket = &tickets[0];
                let winner_pubkey = winner_ticket.1.clone();

                println!("🎰 [LOTO L1] Le ticket {} remporte le Jackpot de {} Flames !", 
                         winner_ticket.0, jackpot_amount);

                let payout_output = crate::transaction::TransactionOutput {
                    stealth_address: format!("JACKPOT_{}", winner_pubkey),
                    kyber_capsule: format!("JACKPOT_PAYOUT_{}", current_height),
                    aes_vault: jackpot_amount.to_string(),
                    lattice_commitment: crate::lattice::LWECommitment::commit(jackpot_amount, [0; LATTICE_DIM]),
                };

                let lottery_payout_tx = Transaction {
                    tx_type: TransactionType::LotteryPayout { 
                        target_block: current_height, 
                        winner_pubkey 
                    },
                    inputs: vec![],
                    outputs: vec![payout_output],
                    fee: 0,
                    dilithium_signature: "LOTTERY_PAYOUT".to_string(),
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
            dilithium_signature: "COINBASE_SIG".to_string(),
        };
        valid_transactions.insert(0, coinbase_tx);

        let new_header = BlockHeader {
			index: current_height,
			timestamp: chrono::Utc::now().timestamp(),
			previous_hash: previous_block.header.hash.clone(),
			hash: String::new(),
			nonce: 0,
			target_hex: format!("{:0>64}", self.target.to_str_radix(16)),   // ← on stocke le target du moment
		};

        let block = Block { header: new_header, transactions: valid_transactions };
        (block, self.target.clone())
    }

    pub fn prune_old_signatures(&mut self) {
        let current_height = self.chain.len() as u64;
        if current_height <= MATURITY_BLOCKS { return; }

        let prune_target = current_height - MATURITY_BLOCKS;
        let mut pruned_count = 0;

        for block in &mut self.chain {
            if block.header.index < prune_target {
                for tx in &mut block.transactions {
                    if tx.dilithium_signature != "PRUNED" && tx.tx_type != TransactionType::Coinbase {
                        tx.dilithium_signature.clear(); 
                        tx.dilithium_signature.push_str("PRUNED"); 
                        for input in &mut tx.inputs {
                            input.pq_ring_inputs.clear(); 
                        }
                        pruned_count += 1;
                    }
                }
            } else {
                break; 
            }
        }

        if pruned_count > 0 {
            println!("🪓 Pruning automatique : {} signatures quantiques purgées pour sauver l'espace !", pruned_count);
        }
    }
    
    pub fn resolve_fork(&mut self, new_chain: Vec<Block>) -> bool {
        if new_chain.is_empty() || new_chain[0].header.hash != self.chain[0].header.hash { return false; }

        let flags = RandomXFlag::get_recommended_flags();
        
        // 💡 FIX : On utilise le bon Dataset !
        let mut current_seed = self.get_epoch_seed(new_chain[1].header.index);
        let mut cache = RandomXCache::new(flags, current_seed.as_bytes()).unwrap();
        let mut vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap(); 

        for i in 1..new_chain.len() {
            let previous_block = &new_chain[i - 1];
            let current_block = &new_chain[i];
            if current_block.header.previous_hash != previous_block.header.hash { return false; }
            
            // Si on change d'époque pendant le fork, on change la VM légère !
            let needed_seed = self.get_epoch_seed(current_block.header.index);
            if needed_seed != current_seed {
                current_seed = needed_seed;
                cache = RandomXCache::new(flags, current_seed.as_bytes()).unwrap();
                vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap();
            }

            let header_data = format!("{}{}{}{}", current_block.header.index, current_block.header.timestamp, current_block.header.previous_hash, current_block.header.nonce);
            let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
            let expected_hash = hex::encode(&hash_bytes);

            if current_block.header.hash != expected_hash { return false; }
        }

        self.chain = new_chain;
        self.recalculate_target_from_scratch(); 
        
        let mut new_spent = HashSet::new();
        for block in &self.chain {
            for tx in &block.transactions {
                if tx.tx_type != TransactionType::Coinbase {
                    for input in &tx.inputs {
                        new_spent.insert(input.pq_ring_signature.key_image.clone());
                    }
                }
            }
        }
        self.spent_key_images = new_spent;

        true
    }
    
    pub fn resolve_partial_fork(&mut self, new_blocks: Vec<Block>) -> bool {
        if new_blocks.is_empty() { return false; }

        let start_index = new_blocks[0].header.index as usize;
        if start_index == 0 { return self.resolve_fork(new_blocks); }
        if start_index > self.chain.len() { return false; }

        // 1. Recherche de l'ancêtre commun
        let mut ancestor_index = start_index.saturating_sub(1);
        let mut found_ancestor = false;

        while ancestor_index > 0 {
            if self.chain[ancestor_index].header.hash == new_blocks[0].header.previous_hash {
                found_ancestor = true;
                break;
            }
            ancestor_index -= 1;
        }

        if !found_ancestor && self.chain[0].header.hash != new_blocks[0].header.previous_hash {
            println!("❌ [FORK] Impossible de trouver un ancêtre commun avec ce lot. Le nœud a besoin d'un historique plus ancien !");
            return false;
        }

        // 💡 LA MAGIE EST ICI : On fusionne virtuellement la chaîne AVANT de valider !
        // Cela permet au validateur d'utiliser les bons blocs pour les changements d'Époque.
        let mut theoretical_chain = self.chain[0..=ancestor_index].to_vec();
        theoretical_chain.extend(new_blocks.clone());

        // Petite fonction interne pour lire la graine sur la chaîne théorique
        let get_theoretical_seed = |height: u64, t_chain: &[Block]| -> String {
            if height <= EPOCH_BLOCKS {
                return t_chain[0].header.hash.clone();
            }
            let target_block = ((height - 1) / EPOCH_BLOCKS) * EPOCH_BLOCKS;
            if (target_block as usize) < t_chain.len() {
                t_chain[target_block as usize].header.hash.clone()
            } else {
                t_chain[0].header.hash.clone()
            }
        };

        // 2. Le Tribunal RandomX
        let flags = RandomXFlag::get_recommended_flags();
        let mut current_seed = get_theoretical_seed(new_blocks[0].header.index, &theoretical_chain);
        let mut cache = RandomXCache::new(flags, current_seed.as_bytes()).unwrap();
        let mut vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap(); 

        for block in &new_blocks {
            let needed_seed = get_theoretical_seed(block.header.index, &theoretical_chain);
            if needed_seed != current_seed {
                current_seed = needed_seed;
                cache = RandomXCache::new(flags, current_seed.as_bytes()).unwrap();
                vm = RandomXVM::new(flags, Some(cache.clone()), None).unwrap();
            }

            let header_data = format!("{}{}{}{}", block.header.index, block.header.timestamp, block.header.previous_hash, block.header.nonce);
            let hash_bytes = vm.calculate_hash(header_data.as_bytes()).unwrap();
            
            if hex::encode(&hash_bytes) != block.header.hash { 
                println!("❌ [FORK] La nouvelle branche contient un bloc frauduleux (Index {})", block.header.index);
                return false; 
            }
        }

        // 3. Pesée des deux chaînes (Preuve de travail)
        let my_work = Blockchain::calculate_total_work(&self.chain);
        let new_work = Blockchain::calculate_total_work(&theoretical_chain);

        if new_work > my_work || (new_work == my_work && new_blocks.last().unwrap().header.timestamp < self.chain.last().unwrap().header.timestamp) {
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
                            new_spent.insert(input.pq_ring_signature.key_image.clone());
                        }
                    }
                }
            }
            self.spent_key_images = new_spent;
            return true;
        }
        
        println!("❌ [FORK] La nouvelle chaîne n'a pas assez de Preuve de Travail.");
        false
    }
    
	pub fn validate_and_add_external_block(&mut self, block: Block) -> Result<(), String> {
		let last_block = self.chain.last().unwrap();
		
		// 1. Vérifications de base de la structure
		if block.header.index != last_block.header.index + 1 { 
			return Err("Index de bloc invalide.".to_string()); 
		}
		if block.header.previous_hash != last_block.header.hash { 
			return Err("Rupture de la chaîne.".to_string()); 
		}

		// 2. Le Tribunal RandomX (Vérification du PoW)
		let flags = randomx_rs::RandomXFlag::get_recommended_flags();
		let seed = self.get_epoch_seed(block.header.index);
		let cache = randomx_rs::RandomXCache::new(flags, seed.as_bytes()).map_err(|_| "Erreur Cache")?;
		let vm = randomx_rs::RandomXVM::new(flags, Some(cache), None).map_err(|_| "Erreur VM")?;

		let header_data = format!("{}{}{}{}", 
			block.header.index, 
			block.header.timestamp, 
			block.header.previous_hash, 
			block.header.nonce
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

		// Calcul de la récompense théorique
		let expected_subsidy = if current_height == 0 {
			INITIAL_REWARD
		} else {
			let prev_block = self.chain.last().unwrap();
			let mut prev_fees = 0;
			for tx in &prev_block.transactions {
				if tx.tx_type != TransactionType::Coinbase {
					prev_fees += tx.fee;
				}
			}
			let prev_total_reward: u64 = prev_block.transactions[0].outputs[0].aes_vault.parse().unwrap_or(INITIAL_REWARD);
			let prev_base_reward = prev_total_reward.saturating_sub(prev_fees);
			Self::get_next_base_reward(prev_base_reward)
		};

		// Premier passage : validation
		for tx in &block.transactions {
			if tx.tx_type == TransactionType::Coinbase {
				coinbase_count += 1;
				continue;
			}

			// Mise à jour prix DEX
			if let TransactionType::DexSettlement { clearing_price_sats, .. } = &tx.tx_type {
				crate::api::LAST_PRICE_SATS.store(*clearing_price_sats, std::sync::atomic::Ordering::Relaxed);
				continue;
			}

			// === MATURITÉ (seulement pour les inputs venant d'une coinbase) ===
			for input in &tx.inputs {
				if input.source_height > 0 {  // Cet input dépense une récompense de minage
					let confirmations = current_height.saturating_sub(input.source_height);
					if confirmations < MATURITY_BLOCKS {
						return Err(format!(
							"Input immature ! Seulement {} confirmations (minimum requis : {})", 
							confirmations, MATURITY_BLOCKS
						));
					}
				}
			}

			// Validité intrinsèque
			if !tx.is_valid() { 
				return Err("Signature ou preuve ZKP invalide.".to_string()); 
			}

			// Vérification balance LWE
			let in_commitments: Vec<_> = tx.inputs.iter().map(|i| i.commitment.clone()).collect();
			let out_commitments: Vec<_> = tx.outputs.iter().map(|o| o.lattice_commitment.clone()).collect();
			if !crate::lattice::LWECommitment::verify_balance(&in_commitments, &out_commitments, tx.fee) {
				return Err("Déséquilibre monétaire détecté dans une transaction !".to_string());
			}

			// Anti-Double Dépense
			for input in &tx.inputs {
				if self.spent_key_images.contains(&input.pq_ring_signature.key_image) || 
				   !block_key_images.insert(input.pq_ring_signature.key_image.clone()) {
					return Err("Tentative de double-dépense détectée !".to_string());
				}
			}

			// ✅ FIX ATOMIC SWAP – vérification réelle du HTLCClaim (dans la boucle)
			if let TransactionType::HTLCClaim { secret } = &tx.tx_type {
				let secret_bytes = hex::decode(secret).unwrap_or_default();
				let provided_hash = hex::encode(blake3::hash(&secret_bytes).as_bytes());
				if provided_hash != tx.dilithium_signature {
					return Err("❌ HTLC Claim : secret invalide".into());
				}
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
		self.prune_old_signatures();
		self.update_target();

		println!("✅ Bloc {} validé. Masse monétaire intègre.", current_height);
		Ok(())
	}
    
    pub fn update_target(&mut self) {
        let current_len = self.chain.len(); 
        if current_len < 2 { return; }

        let window_size = 17; // 💡 Fenêtre glissante (Inspiré de Monero/Zcash) prod 144 (24h)
        let start_idx = if current_len > window_size { current_len - window_size } else { 0 };
        
        let mut total_time = 0;
        let mut num_blocks = 0;
        
        for i in (start_idx + 1)..current_len {
            let prev = &self.chain[i - 1];
            let curr = &self.chain[i];
            let mut time_taken = curr.header.timestamp - prev.header.timestamp;
            
            // 🛡️ Bornes de sécurité : Empêche un pirate de truquer son horloge pour faire chuter la difficulté
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
        let mut all_stealth = Vec::new();
        for block in &self.chain {
            for tx in &block.transactions {
                if tx.tx_type != TransactionType::Coinbase {
                    for out in &tx.outputs {
                        all_stealth.push(out.stealth_address.clone());
                    }
                }
            }
        }
        if all_stealth.is_empty() { return vec![]; }
        let mut rng = rand::thread_rng();
        all_stealth.shuffle(&mut rng);
        all_stealth.into_iter().take(count).collect()
    }
}