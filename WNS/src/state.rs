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
        let mut domains = HashMap::new();
        let mut domain_owners = HashMap::new();

        // INJECTION DU SEED NODE FONDATEUR (État Genesis)
        // On associe le domaine "seed.watt" à l'IP/Port ou à la clé de routage
        domains.insert("seed.watt".to_string(), "80.78.26.243:8000".to_string());
        
        // On définit le propriétaire cryptographique (La clé publique du nœud)
        domain_owners.insert(
            "seed.watt".to_string(), 
            //"82c681c357baa2d5438bd46e9705d0e06c1507830387c96000202fce562dd49cc6eca18b7aa100a65323f5b288c12cb781d27121222d328cb09e7a58669b84d40008423c03872941d3d062f75655f28acd9b9209af520711f38c9662b1885c8038db4020038516a9b91fc0154129482823740b43c8795a304d31ba84475d07c75052db9c6e6381c0cb9a6d08caa8a45545b4bdf8c4014dc8507c8943fa353ad4c88ccc45270152b8973498b67083edcc8a12c74a7a20265d77806a3ca2c07b38a8da1d866421f62ba0e1f734af2831cd43179942244e64626a9049dc566be43b5c44d32839529dc2f98689a19d76099c310c280f74a7ab08d01c19aa2c306a81db9487fb907c616a410b0cf2756c6a79a1e6892b4fcacd2f809c8ab7b4475c8c4ea40d75506885f4365b303792a432310ccee2884a8fc92849983b879c2c84b646a27a534bc2609462618f0cb5ed328cca2a849d157b35f81b4bfcb5d543714fb75a6fda3d463a4ee82aac905a86886576e3fb2a96c6c03ec38d2569381b6669fbb66fdca98e8ba93c0c678025146ebc125637167662112aedf046df4991991c1b7271063e175d8d23b5bd3a7bc7c533f9a11dd252bcbc1010f95ba423b5b339673b1a6a6a19dbbc0c01347c9721cea6a49d0454b3c60caa104cb4b01745d90205f362d4c6499d12c1dccb494276a4175459e362878465a0ee404bcc0463491cbb15a8bd2a54c2ecb686dcbba3ab9c0551666033994ce068264bb8c7345600fc241f31520f455694f84b5d06370a4a597ed9a21ddd2cb1d94975ad6c4ad7043cda25cc65fc7057b51fd892a4f8b4619cec4e97f756ea469ecde02242c236d366b9bef1722f39b9b9735b38b73e74cb847f7511bb02291a65499e41527ae8b1c733614625a9afe3962335b078f3cf5a9278b627b46197351ef27c571154827a9e36c640249b908bf818f82b5ca0e126a3565b37543f5dc4ca086bcfcfab414e98232c1906a85c3204760a11c83f5b3b2300c24af7f877f345764e76909cc88fe77572be79b841dcc11e44632655a56624b408cab0ed27b8d9b682de28b7b78054fe8b3a05d24d136143d45224c6aa3ead308d2d67c3c89c60cb589e018b3720b38c01c631607c810fe29f8e03127627270db08b9ef2474a192275f842993a8d50875c42bcaaec9b25ec481b9fa457d8703b3141aa67941ed65c3bc75760c04a4875d784bb6007e9fb20831c66a3e03f3e6ac238d41a5792171281b02b0062c917231bf534566c7449f239b9859bc0633e411bcc80cbab9861c155974c6c21c343427823b6814806663d888d8af8124e4597d6358a16d7aa74ba14b4bb72d4422c97a17315b7ab0b43cb03d1360fea635782b3e0b58354a0ac60dc215e3ac73d74c1b2c817849a6c9ac37ea3d8243c153645b9a5f247406d403a0e763e6d25aa11a04667987bfbb61eead21a6a9749a87394fe6a902706cf28730a28047659588698d779846a90086b2636182fb7b03aac642ba9289f55885c09d9bdba988355830ab84bc229f8bbbf99074a34197aa23992675d15d2612796530a2ac1972b9e6561810b58ade2eacd21b078e3ac784cc04147fa3cf5bb576c0829f9ba732e2cab68e1bfca29b2903a9b4694e78c0a766c974c8eab3d51faed5b17a4ff4c267855".to_string()
			// local
			"5e122b79d358a6e8ab7e13146ae82f152859d2eb8e82934636150b5825371c834ec96324d5e46dbb140366ba83400ba00e74bfc22161c429c1fffa99ed98c156bbc407220ca48772dfe85dd64b2feab29a18914b9a66772fcc305070884c8281737c873101c1ca502703a0040e550a315796472ab292e72a409347673c0b2916932a0c9cc36a9dba45baf2127778b080bb5c3eb21530a58165ee0a7524d57c66eb131f7a8f040cc859a9b11a512e1008aa074221cc71507e3a1f37a27fd231b985c2c555a8849a508ba1fc88f4222ca1571bc0408d6007913555cb8ee7a8362607a87a617bf35acde0aff4c45ae575b8f03b0c42911afdd88df787bcd57403a92651d2b12fabab496bf92152fa7834788610a8cb2a33959f9875e4f1810b70b0e5578170cc7a4e8954e0820997e96f403a007045a9224034c471128d54905efb9e78501c86a645edc980f0f69dd0b504144c74ee6238f14b71c88b061a44476021374d2731c925a8a1e10c43e68efb3b8926f2a3785b79d882130beab81225b2418aabc7e94458119013d59e8a8b91ec46650c45a2aac28862d23040933ce16c5e7d2cc191f50460e8bc66595fcce2a78966b4009084a9172a885396eb196495da933f758f5bc55845b1398c6aa1ec9867c70b75f036b863a4150fa0a9c0d009a745a80de3aa5b42b7b214a329e9cc7508a770b077d9f6bdeec8a1750989e69c74cef064dc7858e1f38bba061612db7085d2395d84a0cd692c10c48133d1c645ec0ef5a01fbfd91a57b53c4c0256f57c3d9bd63a75b649b3e27168992e67d459afa8648078cc01147f1e0671e929a5b4969ce2fa5ab502af6623998b542c3d362276f6bf2cd072aabc705886774c91991d6cabeaf5b681b91be6563af23caaee8b83d05bab894653b3ecb76f9347aad0598558b8d9737bef0b9ccf4796fb06c0eaa67144b7cd5bc216ff9585e226746f173bba257500a534dd84a1b3119abca42668844c0b425826c09b60c00f0f3a212180c87b2ccccda58cdc609403fbb05939ca2db03b2b09c769a063a77207f4009e5b7565c858962d9678eab31081d72b7cf0c56070b3e749975bb88fdcf25b5da3721888a3478014e54c463f69c8088388472084322132d695aed23c97c293a2af856d56f77806b888fe091526f564f5fb8459f55865b78f56f463e267a271781040d8a9df89a68e7ab72465179e55c09ad02c759233796708b834b8854c0a9cfb124281036bd51e620b24bfacc8febb2384d661ae672addc5338e6bbd7fd3be6d5a522ff093485180a581509017162f5bade515769eb03a65b822bcca19cb29827a9c9b1bb83f24550f72b39339a234d63b4768e564097ab27c8cb1535c79fa620b596608bfb29c0b5738fda405c41b2e3fd0a698e4b183a7482afb0eb63cc62e352787d0362ff106a578b3f0a5561d9c027b1c944b830856e17527b061e1e86322e007780c880931cd1aaa57ebb444a8525f7fc19c72266a9c22cc0461bb43e01144da50c311bc7e65674ad64f8f7a2aa683c79c20bd26abc816b6162562761eab9fe18cc74d260630742a9a80a1bb410471f35e5ae35e6602205280273975aea6136743795887f274caac008e34cf6d847ed263062fcff671fed49616ec2fe3bb0870dca22dc611d03edb6644cdd2".to_string()
		);

        // Ouverture de la DB Sled
        let db = sled::open(db_path).expect("❌ Impossible d'ouvrir la DB Sled du WNS");

        Self {
			accounts: HashMap::new(),
            domains,
            domain_owners,
			storage_contracts: HashMap::new(), // Initialisation
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

    // Plus besoin du paramètre `path`, l'état connait déjà sa base de données
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
                // AJOUT DU SUPPORT POUR .chain ICI
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
                // AJOUT DU SUPPORT POUR .chain ICI AUSSI !
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
            acc_mut.nonce += 1; // 💡 On incrémente le nonce
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
        
        // 💡 Le chemin n'est plus requis
        self.save_to_disk();
        
        Ok(())
    }
}

pub type SharedL2State = Arc<Mutex<L2State>>;
