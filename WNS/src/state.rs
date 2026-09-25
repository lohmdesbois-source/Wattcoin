// wattcoin_name_service/src/state.rs

use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sha2::{Sha512, Digest};
use crate::transaction::{L2Transaction, WnsAction, WnsBlock};

// La définition d'un Défi (Le Corrigé d'Examen)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StorageChallenge {
    pub chunk_index: u64,             // Le numéro du morceau du fichier (ex: 42)
    pub nonce: String,                // Le nombre aléatoire chaotique (ex: "8f4a2b...")
    pub expected_answer_hash: String, // Le Hash du corrigé généré par le Wallet
}

// Le Contrat de Stockage
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StorageContract {
    pub host_pubkey: String,       // À qui l'argent est destiné
    pub total_chunks: u64,         // Le nombre de morceaux
    pub locked_flames: u64,        // L'argent restant dans le contrat
    pub payment_per_proof: u64,    // Combien l'hébergeur gagne par preuve validée
    pub last_proof_block: u64,     // Le dernier bloc où l'hébergeur a donné signe de vie
    pub pending_challenges: Vec<StorageChallenge>, // La liste secrète des défis restants !
}

// Le modèle de Compte
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct L2Account {
    pub balance: u64,
    pub nonce: u64, // Le compteur d'état !
    pub authorized_wots_key: String, 
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct L2State {
    pub accounts: HashMap<String, L2Account>,         
    pub domains: HashMap<String, String>,         
    pub domain_owners: HashMap<String, String>,  
    pub storage_contracts: HashMap<String, StorageContract>,    
    pub mempool: Vec<L2Transaction>,              
    pub block_index: u64,  
    pub last_l1_block: u64,
    
    // 💡 SLED : On garde la connexion DB ouverte directement dans l'état !
    // On demande à Serde de l'ignorer quand il envoie l'état sur le réseau P2P.
    #[serde(skip)]
    pub db: Option<sled::Db>,
}

impl L2State {
    pub fn new(db_path: &str) -> Self {
        // 🧹 GRAND NETTOYAGE : Plus aucun serveur racine (Seed) n'est codé en dur !
        // L'infrastructure réseau est déléguée à 100% à la DHT (RandomX/Kyber).
        // Le WNS ne gère plus que les vrais paiements d'alias et le stockage.
        
        let db = sled::open(db_path).expect("❌ Impossible d'ouvrir la DB Sled du WNS");

        Self {
            accounts: HashMap::new(),
            domains: HashMap::new(),
            domain_owners: HashMap::new(),
            storage_contracts: HashMap::new(),
            mempool: Vec::new(),
            block_index: 0,
            last_l1_block: 0,
            db: Some(db),
        }
    }

    pub fn load_from_disk(path: &str) -> Option<Self> {
        if let Ok(db) = sled::open(path) {
            // Lecture atomique du blob principal WNS
            if let Ok(Some(data)) = db.get("wns_state") {
                // On utilise Bincode pour une lecture binaire fulgurante
                if let Ok(mut state) = bincode::deserialize::<Self>(&data) {
                    println!("💾 [L2 STATE] Base de données Sled chargée avec succès ! (Reprise au Bloc #{})", state.block_index);
                    state.db = Some(db); // Raccrochage de la connexion
                    return Some(state);
                }
            }
        }
        None
    }

    pub fn save_to_disk(&self) {
        if let Some(db) = &self.db {
            if let Ok(encoded) = bincode::serialize(self) {
                let _ = db.insert("wns_state", encoded);
                let _ = db.flush();
            }
        }
    }

    pub fn process_mempool(&mut self, sequencer_address: &str) -> (u64, Vec<L2Transaction>, u64, Vec<(String, u64)>) {
        let mut txs = std::mem::take(&mut self.mempool);
        txs.sort_by(|a, b| b.fee.cmp(&a.fee));

        let mut valid_txs = Vec::new();
        let mut total_fees = 0u64;
        let mut withdrawals = Vec::new(); 
        let mut kept_in_mempool = Vec::new(); 
        
        let current_block = self.block_index;
        let max_blocks_without_proof = 100; 
        
        let mut contracts_to_remove = Vec::new();
        for (domain, contract) in &self.storage_contracts {
            if current_block > contract.last_proof_block + max_blocks_without_proof {
                println!("🚨 [SLA] L'hébergeur de '{}' a censuré/perdu le fichier. Contrat rompu !", domain);
                contracts_to_remove.push((domain.clone(), contract.locked_flames));
            }
        }

        for (domain, refund_amount) in contracts_to_remove {
            self.storage_contracts.remove(&domain);
            if let Some(owner_account) = self.domain_owners.get(&domain) {
                if let Some(acc) = self.accounts.get_mut(owner_account) {
                    acc.balance += refund_amount;
                    println!("💸 Remboursement KISS : {} Flames renvoyés au compte {}.", refund_amount, owner_account);
                }
            }
        }
        
        for tx in txs {
            if tx.fee < 1500 { continue; } 

            if matches!(tx.action, WnsAction::Register | WnsAction::Update) {
                // Seuls les alias payants .watt et .chain sont acceptés sur le registre.
                let is_valid_watt = tx.domain_name.ends_with(".watt") && tx.domain_name.len() > 5;
                let is_valid_chain = tx.domain_name.ends_with(".chain") && tx.domain_name.len() > 6;

                if !is_valid_watt && !is_valid_chain {
                    println!("❌ [CONSENSUS L2] Rejet : Le domaine '{}' viole les règles strictes du registre.", tx.domain_name);
                    continue; 
                }
            }
            
            let account_opt = self.accounts.get(&tx.account_address);
            if account_opt.is_none() {
                kept_in_mempool.push(tx);
                continue;
            }

            let account = account_opt.unwrap();
            if account.authorized_wots_key != tx.sender_pubkey {
                println!("❌ Rejet : Signature WOTS+ non autorisée pour ce compte.");
                continue; 
            }
            
            // 💡 VÉRIFICATION DU NONCE
            if tx.nonce != account.nonce + 1 {
                println!("❌ Rejet : Nonce invalide. Attendu {}, Reçu {}", account.nonce + 1, tx.nonce);
                continue; 
            }

            let total_cost = tx.fee + tx.amount; 
            let sender_balance = account.balance;
            
            if sender_balance >= total_cost {
                
                let account = self.accounts.get_mut(&tx.account_address).unwrap();
                account.balance -= total_cost;
                account.nonce += 1; // 💡 ON INCRÉMENTE LE NONCE ICI !
                account.authorized_wots_key = tx.next_pubkey.clone(); 
                
                match tx.action {
                    WnsAction::Register => {
                        if !self.domains.contains_key(&tx.domain_name) {
                            // record_data contiendra désormais la clé Kyber pour les paiements
                            self.domains.insert(tx.domain_name.clone(), tx.record_data.clone());
                            self.domain_owners.insert(tx.domain_name.clone(), tx.account_address.clone());
                            println!("✅ [WNS] '{}' adjugé pour {} Flames !", tx.domain_name, tx.fee);
                        }
                    },
                    WnsAction::Update => {
                        if self.domain_owners.get(&tx.domain_name) == Some(&tx.account_address) {
                            self.domains.insert(tx.domain_name.clone(), tx.record_data.clone());
                            self.domain_owners.insert(tx.domain_name.clone(), tx.next_pubkey.clone());
                        }
                    },
                    WnsAction::Transfer => {
                        if self.domain_owners.get(&tx.domain_name) == Some(&tx.account_address) {
                            self.domain_owners.insert(tx.domain_name.clone(), tx.record_data.clone());
                        }
                    },
                    WnsAction::Withdraw => {
                        if tx.amount > 0 && !tx.record_data.is_empty() {
                            withdrawals.push((tx.record_data.clone(), tx.amount));
                            println!("🔥 [BURN] {} Flames détruits sur L2 pour retrait vers L1 ({})", tx.amount, tx.record_data);
                        }
                    },
                    WnsAction::StorageLock => {
                        if self.domain_owners.get(&tx.domain_name) == Some(&tx.account_address) && tx.amount > 0 {
                            let parts: Vec<&str> = tx.record_data.splitn(3, '|').collect();
                            if parts.len() == 3 {
                                if let Ok(challenges) = serde_json::from_str::<Vec<StorageChallenge>>(parts[2]) {
                                    let num_proofs = challenges.len() as u64;
                                    if num_proofs > 0 {
                                        let payment_per_proof = tx.amount / num_proofs; 
                                        let contract = StorageContract {
                                            host_pubkey: parts[0].to_string(),
                                            total_chunks: parts[1].parse().unwrap_or(1),
                                            locked_flames: tx.amount,
                                            payment_per_proof,
                                            last_proof_block: current_block,
                                            pending_challenges: challenges, 
                                        };
                                        self.storage_contracts.insert(tx.domain_name.clone(), contract);
                                        println!("🔒 Contrat de Stockage verrouillé pour '{}' ({} Flames, {} défis)", tx.domain_name, tx.amount, num_proofs);
                                    }
                                }
                            }
                        }
                    },
                    WnsAction::StorageProof => {
                        if let Some(contract) = self.storage_contracts.get_mut(&tx.domain_name) {
                            if contract.host_pubkey == tx.account_address {
                                if !contract.pending_challenges.is_empty() {
                                    let challenge_idx = (self.last_l1_block as usize) % contract.pending_challenges.len();
                                    let challenge = &contract.pending_challenges[challenge_idx];
                                    
                                    let mut hasher = Sha512::new();
                                    hasher.update(tx.record_data.as_bytes());
                                    let wns_check_hash = hex::encode(hasher.finalize());
                                    
                                    if wns_check_hash == challenge.expected_answer_hash {
                                        let payout = std::cmp::min(contract.payment_per_proof, contract.locked_flames);
                                        contract.locked_flames -= payout;
                                        contract.last_proof_block = current_block;

                                        let host_account = self.accounts.entry(contract.host_pubkey.clone()).or_insert(L2Account {
                                            balance: 0,
                                            nonce: 0,
                                            authorized_wots_key: contract.host_pubkey.clone(),
                                        });
                                        host_account.balance += payout;
                                        
                                        contract.pending_challenges.remove(challenge_idx);
                                    }
                                }
                            }
                        }
                    }
                }
                valid_txs.push(tx.clone());
                total_fees += tx.fee;
            } else {
                kept_in_mempool.push(tx);
            }
        }

        self.mempool = kept_in_mempool;

        if !valid_txs.is_empty() {
            self.block_index += 1;
            let seq_account = self.accounts.entry(sequencer_address.to_string()).or_insert(L2Account {
                balance: 0,
                nonce: 0,
                authorized_wots_key: sequencer_address.to_string(),
            });
            seq_account.balance += total_fees;
        }

        (self.block_index, valid_txs, total_fees, withdrawals)
    }

    pub fn compute_state_root(&self) -> String {
        let mut hasher = Sha512::new();
        
        if !self.accounts.is_empty() {
            let mut keys: Vec<&String> = self.accounts.keys().collect();
            keys.sort(); 
            for key in keys {
                hasher.update(key.as_bytes());
                let acc = self.accounts.get(key).unwrap();
                hasher.update(&acc.balance.to_be_bytes());
                hasher.update(acc.authorized_wots_key.as_bytes());
            }
        }

        if !self.domains.is_empty() {
            let mut dom_keys: Vec<&String> = self.domains.keys().collect();
            dom_keys.sort(); 
            for k in dom_keys {
                hasher.update(k.as_bytes());
                hasher.update(self.domains.get(k).unwrap().as_bytes());
                hasher.update(self.domain_owners.get(k).unwrap().as_bytes());
            }
        }
        
        hex::encode(hasher.finalize())
    }
    
    pub fn apply_incoming_wns_block(&mut self, block: &WnsBlock) -> Result<(), String> {
        if block.index != self.block_index + 1 { return Err("Index invalide".to_string()); }
        
        let mut temp_state = self.clone(); 
        let mut total_fees = 0;

        for tx in &block.transactions {
            if matches!(tx.action, WnsAction::Register | WnsAction::Update) {
                let is_valid_watt = tx.domain_name.ends_with(".watt") && tx.domain_name.len() > 5;
                let is_valid_chain = tx.domain_name.ends_with(".chain") && tx.domain_name.len() > 6;

                if !is_valid_watt && !is_valid_chain {
                    return Err(format!("Le Séquenceur a inclus un domaine illégal : {}", tx.domain_name));
                }
            }

            let account = temp_state.accounts.get(&tx.account_address).ok_or("Compte inexistant")?;
            if account.authorized_wots_key != tx.sender_pubkey { return Err("Signature invalide".to_string()); }
            if tx.nonce != account.nonce + 1 { return Err("Nonce invalide".to_string()); } // 💡 Protection
            
            let total_cost = tx.fee + tx.amount;
            if account.balance < total_cost { return Err("Fonds L2 insuffisants".to_string()); }

            let acc_mut = temp_state.accounts.get_mut(&tx.account_address).unwrap();
            acc_mut.balance -= total_cost;
            acc_mut.nonce += 1; 
            acc_mut.authorized_wots_key = tx.next_pubkey.clone();
            
            match tx.action {
                WnsAction::Register => {
                    temp_state.domains.insert(tx.domain_name.clone(), tx.record_data.clone());
                    temp_state.domain_owners.insert(tx.domain_name.clone(), tx.account_address.clone());
                },
                WnsAction::Update => {
                    if temp_state.domain_owners.get(&tx.domain_name) == Some(&tx.account_address) {
                        temp_state.domains.insert(tx.domain_name.clone(), tx.record_data.clone());
                        temp_state.domain_owners.insert(tx.domain_name.clone(), tx.next_pubkey.clone());
                    }
                },
                WnsAction::Transfer => {
                    if temp_state.domain_owners.get(&tx.domain_name) == Some(&tx.account_address) {
                        temp_state.domain_owners.insert(tx.domain_name.clone(), tx.record_data.clone());
                    }
                },
                WnsAction::Withdraw => {},
                WnsAction::StorageLock => {
                    if temp_state.domain_owners.get(&tx.domain_name) == Some(&tx.account_address) && tx.amount > 0 {
                        let parts: Vec<&str> = tx.record_data.splitn(3, '|').collect();
                        if parts.len() == 3 {
                            if let Ok(challenges) = serde_json::from_str::<Vec<StorageChallenge>>(parts[2]) {
                                let num_proofs = challenges.len() as u64;
                                if num_proofs > 0 {
                                    let payment_per_proof = tx.amount / num_proofs; 
                                    let contract = StorageContract {
                                        host_pubkey: parts[0].to_string(),
                                        total_chunks: parts[1].parse().unwrap_or(1),
                                        locked_flames: tx.amount,
                                        payment_per_proof,
                                        last_proof_block: temp_state.block_index, 
                                        pending_challenges: challenges,
                                    };
                                    temp_state.storage_contracts.insert(tx.domain_name.clone(), contract);
                                }
                            }
                        }
                    }
                },
                WnsAction::StorageProof => {
                    if let Some(contract) = temp_state.storage_contracts.get_mut(&tx.domain_name) {
                        if contract.host_pubkey == tx.account_address && !contract.pending_challenges.is_empty() {
                            let challenge_idx = (self.last_l1_block as usize) % contract.pending_challenges.len();
                            let challenge = &contract.pending_challenges[challenge_idx];
                            
                            let mut hasher = Sha512::new();
                            hasher.update(tx.record_data.as_bytes());
                            let wns_check_hash = hex::encode(hasher.finalize());
                            
                            if wns_check_hash == challenge.expected_answer_hash {
                                let payout = std::cmp::min(contract.payment_per_proof, contract.locked_flames);
                                contract.locked_flames -= payout;
                                contract.last_proof_block = temp_state.block_index;

                                let host_account = temp_state.accounts.entry(contract.host_pubkey.clone()).or_insert(L2Account {
                                    balance: 0,
                                    nonce: 0,
                                    authorized_wots_key: contract.host_pubkey.clone(),
                                });
                                host_account.balance += payout;
                                contract.pending_challenges.remove(challenge_idx);
                            }
                        }
                    }
                }
            }
            total_fees += tx.fee;
        }

        let seq_account = temp_state.accounts.entry(block.sequencer_pubkey.clone()).or_insert(L2Account {
            balance: 0,
            nonce: 0,
            authorized_wots_key: block.sequencer_pubkey.clone(),
        });
        seq_account.balance += total_fees;

        let calculated_root = temp_state.compute_state_root();
        if calculated_root != block.state_root {
            return Err("State Root frauduleux ! Le séquenceur a triché sur les soldes.".to_string());
        }

        temp_state.block_index += 1;
        temp_state.mempool.retain(|m_tx| !block.transactions.iter().any(|b_tx| b_tx.signature == m_tx.signature));
        
        *self = temp_state;
        
        self.save_to_disk();
        
        Ok(())
    }
}

pub type SharedL2State = Arc<Mutex<L2State>>;