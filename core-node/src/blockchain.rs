use crate::block::{Block, BlockHeader};
use crate::transaction::{Transaction, TransactionType};
use std::fs;
use num_bigint::BigUint;
use std::collections::HashSet;
use randomx_rs::{RandomXFlag, RandomXCache, RandomXVM};
use rand::seq::SliceRandom;
use crate::WattError;

const FLAME: u64 = 1_000_000_000;
const MATURITY_BLOCKS: u64 = 3; 
const EXPECTED_BLOCK_TIME: u64 = 600;    
// 18.000.000 / (144 blocs/jour * 365 jours * 20 ans) = ~1.7 Watts/bloc
const INITIAL_REWARD: u64 = 15 * FLAME; // 15 Watts
const TAIL_EMISSION: u64 = 600_000_000; // 0.6 Watts
const EMISSION_DECAY_SHIFT: u32 = 18;   // Ajusté pour ~21 ans
const INITIAL_DIFFICULTY_SHIFT: u32 = 12;
pub const LOTTERY_TIME_BLOCK: u64 = 10;

// 💡 NOUVEAU : Changement de Dataset tous les 20 blocs pour tuer les ASICs !
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
    
    // 💡 NOUVEAU : Trouve la graine RandomX appropriée pour une hauteur de bloc donnée
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
        let chain: Vec<Block> = serde_json::from_str(&data)?;
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
        for i in 1..self.chain.len() {
            let prev_block = &self.chain[i - 1];
            let mut prev_fees = 0;
            for tx in &prev_block.transactions {
                if tx.tx_type != TransactionType::Coinbase { prev_fees += tx.fee; }
            }
            let prev_total_reward: u64 = if i == 1 { INITIAL_REWARD } else {
                prev_block.transactions[0].outputs[0].aes_vault.parse().unwrap_or(INITIAL_REWARD)
            };
            let prev_base_reward = prev_total_reward.saturating_sub(prev_fees);
            supply += Self::get_next_base_reward(prev_base_reward);
        }
        supply
    }

    // 💡 Calcul du Jackpot en cours
    pub fn get_jackpot_info(&self, target_height: u64) -> (u64, Vec<(String, String)>) {
        let mut tickets = Vec::new();
        let mut pot = 0u64;
        if target_height < 10 { return (0, tickets); }
        let start = target_height - 10;
        
        for i in start..target_height {
            if (i as usize) < self.chain.len() {
                let block = &self.chain[i as usize];
                for tx in &block.transactions {
                    if tx.tx_type == TransactionType::Coinbase && tx.outputs.len() > 1 {
                        if tx.outputs[1].stealth_address == "LOTTERY_RESERVE" {
                            pot += tx.outputs[1].aes_vault.parse::<u64>().unwrap_or(0);
                        }
                    }
                    if let TransactionType::HTLCLottery { target_block, player_pubkey } = &tx.tx_type {
                        if *target_block == target_height {
                            // 💡 FIX CRITIQUE : On utilise la capsule (persistante) au lieu de la signature (qui est PRUNED)
                            let ticket_id = if !tx.outputs.is_empty() { tx.outputs[0].kyber_capsule.clone() } else { tx.dilithium_signature.clone() };
                            tickets.push((ticket_id, player_pubkey.clone()));
                            pot += 10_000_000_000; 
                        }
                    }
                }
            }
        }
        tickets.sort_by(|a, b| a.0.cmp(&b.0)); 
        (pot, tickets)
    }

    pub fn get_current_jackpot(&self) -> u64 {
        let current_height = self.chain.len() as u64;
        let target_height = current_height + (10 - (current_height % 10)); // Prochain tirage
        let (pot, _) = self.get_jackpot_info(target_height);
        pot
    }

    pub fn prepare_block_template(&mut self, transactions: Vec<Transaction>, miner_address: &str) -> (Block, BigUint) {
        let current_height = self.chain.len() as u64;
        println!("\n⏳ Préparation du Bloc {}...", current_height);

        let mut valid_transactions = Vec::new();
        let mut total_fees = 0; 

        for tx in transactions {
            if tx.is_valid() { 
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
		if block.header.index != last_block.header.index + 1 { return Err("Index de bloc invalide.".to_string()); }
		if block.header.previous_hash != last_block.header.hash { return Err("Rupture de la chaîne.".to_string()); }

		// 2. Le Tribunal RandomX (Vérification du PoW)
		let flags = randomx_rs::RandomXFlag::get_recommended_flags();
		let seed = self.get_epoch_seed(block.header.index);
		let cache = randomx_rs::RandomXCache::new(flags, seed.as_bytes()).map_err(|_| "Erreur Cache")?;
		let vm = randomx_rs::RandomXVM::new(flags, Some(cache), None).map_err(|_| "Erreur VM")?;

		let header_data = format!("{}{}{}{}", block.header.index, block.header.timestamp, block.header.previous_hash, block.header.nonce);
		let hash_bytes = vm.calculate_hash(header_data.as_bytes()).map_err(|_| "Erreur calcul")?;
		
		if block.header.hash != hex::encode(&hash_bytes) { return Err("Hash frauduleux.".to_string()); }

		let hash_bigint = num_bigint::BigUint::parse_bytes(block.header.hash.as_bytes(), 16).unwrap_or_default();
		if hash_bigint > self.target { return Err("Preuve de travail insuffisante.".to_string()); }

		// --- LOGIQUE DE CONSENSUS ÉTENDUE ---
		let mut coinbase_count = 0;
		let mut total_block_fees = 0u64;
		let mut block_key_images = HashSet::new();

		// Calcul de la récompense théorique attendue (basée sur ta formule decay)
		let current_height = block.header.index;
    
		// 💡 Validation stricte de la récompense attendue en O(1)
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

		// Premier passage : On calcule les frais totaux et on valide les TX standards
		for tx in &block.transactions {
			if tx.tx_type == TransactionType::Coinbase {
				coinbase_count += 1;
				continue;
			}

			// 💡 Le Nœud met à jour son oracle de prix global en lisant le bloc !
			if let TransactionType::DexSettlement { clearing_price_sats, .. } = &tx.tx_type {
				crate::api::LAST_PRICE_SATS.store(*clearing_price_sats, std::sync::atomic::Ordering::Relaxed);
				continue; // 💡 Le prix est mis à jour en mémoire vive. Pas de fichier vulnérable.
			}

			// A. Vérification de la validité intrinsèque (ZKP / Ring / LWE)
			if !tx.is_valid() { return Err("Signature ou preuve ZKP invalide.".to_string()); }

			// B. Vérification mathématique de la non-création de monnaie (LWE Balance)
			let in_commitments: Vec<_> = tx.inputs.iter().map(|i| i.commitment.clone()).collect();
			let out_commitments: Vec<_> = tx.outputs.iter().map(|o| o.lattice_commitment.clone()).collect();
			if !crate::lattice::LWECommitment::verify_balance(&in_commitments, &out_commitments, tx.fee) {
				return Err("Déséquilibre monétaire détecté dans une transaction !".to_string());
			}

			// C. Anti-Double Dépense
			for input in &tx.inputs {
				if self.spent_key_images.contains(&input.pq_ring_signature.key_image) || 
				   !block_key_images.insert(input.pq_ring_signature.key_image.clone()) {
					return Err("Tentative de double-dépense détectée !".to_string());
				}
			}
			total_block_fees += tx.fee;
		}

		if coinbase_count != 1 { return Err("Un bloc doit contenir exactement une Coinbase.".to_string()); }

		// Deuxième passage : On valide la Coinbase (Reward + Fees)
		let coinbase_tx = &block.transactions[0];
		if coinbase_tx.tx_type != TransactionType::Coinbase { return Err("La première TX doit être la Coinbase.".to_string()); }
		
		// Extraction du montant réel miné (stocké dans l'AES Vault de la Coinbase)
		let actual_reward: u64 = coinbase_tx.outputs[0].aes_vault.parse().unwrap_or(u64::MAX);
		if actual_reward > (expected_subsidy + total_block_fees) {
			return Err(format!("Inflation illégale ! Attendu: {}, Reçu: {}", 
				expected_subsidy + total_block_fees, actual_reward));
		}
		
		// 🎰 VÉRIFICATION DU JACKPOT L1
        if current_height % LOTTERY_TIME_BLOCK == 0 && current_height > 0 {
            let (pot, tickets) = self.get_jackpot_info(current_height);
            if !tickets.is_empty() {
                let prev_hash_bytes = hex::decode(&block.header.previous_hash).unwrap_or(vec![0; 32]);
                let mut seed_arr = [0u8; 8];
                seed_arr.copy_from_slice(&prev_hash_bytes[0..8]);
                let seed_num = u64::from_le_bytes(seed_arr);
                let winner_idx = (seed_num % (tickets.len() as u64)) as usize;
                let winner = &tickets[winner_idx];

                let mut found_payout = false;
                for tx in &block.transactions {
                    if let TransactionType::LotteryPayout { target_block, winner_pubkey } = &tx.tx_type {
                        if *target_block != current_height { return Err("Mauvais bloc cible pour le payout".to_string()); }
                        if winner_pubkey != &winner.1 { return Err("Mauvais gagnant désigné par le mineur !".to_string()); }
                        let out = &tx.outputs[0];
                        if out.stealth_address != format!("JACKPOT_{}", winner.1) { return Err("Mauvais destinataire Jackpot !".to_string()); }
                        if out.aes_vault != pot.to_string() { return Err("Mauvais montant Jackpot !".to_string()); }
                        found_payout = true;
                    }
                }
                if !found_payout { return Err("Le bloc DOIT contenir le paiement du Jackpot !".to_string()); }
            }
        }

		// Si tout est OK, on met à jour l'état
		for ki in block_key_images { self.spent_key_images.insert(ki); }
		self.chain.push(block);
		self.prune_old_signatures();
		self.update_target();

		println!("✅ Bloc {} validé. Masse monétaire intègre.", current_height);
		Ok(())
	}
    
    pub fn update_target(&mut self) {
        let current_len = self.chain.len(); 
        if current_len < 2 { return; }

        let window_size = 17; // 💡 Fenêtre glissante (Inspiré de Monero/Zcash)
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