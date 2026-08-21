use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::fs; // AJOUT POUR LE FICHIER
use sha2::{Sha512, Digest};
use crate::transaction::L2Transaction;

#[derive(Serialize, Deserialize, Clone, Debug)] // PERMET LA TRADUCTION EN JSON
pub struct L2State {
    pub balances: HashMap<String, u64>,
    pub mempool: Vec<L2Transaction>,
    pub block_index: u64,  
    pub block_reward: u64, 
}

impl L2State {
    pub fn new(premine_pubkey: Option<String>, premine_amount: u64, block_reward: u64) -> Self {
        let mut balances = HashMap::new();
        
        // ALLOCATION GENESIS
        if let Some(pubkey) = premine_pubkey {
            if premine_amount > 0 {
                balances.insert(pubkey.clone(), premine_amount);
                println!("💎 [GENESIS] Prémine de {} jetons allouée à {}...", premine_amount, &pubkey[..15]);
            }
        }

        Self {
            balances,
            mempool: Vec::new(),
            block_index: 0,
            block_reward,
        }
    }

    // ====================================================================
    // PERSISTANCE DE L'ÉTAT (JSON)
    // ====================================================================
    pub fn load_from_disk(path: &str) -> Option<Self> {
        if let Ok(data) = fs::read_to_string(path) {
            if let Ok(state) = serde_json::from_str::<Self>(&data) {
                println!("💾 [L2 STATE] Base de données chargée avec succès ! (Reprise au Bloc #{})", state.block_index);
                return Some(state);
            }
        }
        None
    }

    pub fn save_to_disk(&self, path: &str) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, json);
        }
    }
    // ====================================================================

    /// Traite le Mempool, incrémente l'index, et paie le Séquenceur
    pub fn process_mempool(&mut self, sequencer_address: &str) -> (u64, usize, u64) {
        self.block_index += 1; // 💡 Le bloc avance !
        
        let txs = std::mem::take(&mut self.mempool);
        let tx_count = txs.len();
        let mut total_fees = 0u64;

        for tx in txs {
            let sender_balance = *self.balances.get(&tx.sender_pubkey).unwrap_or(&0);
            let total_cost = tx.amount + tx.fee;

            if sender_balance >= total_cost {
                self.balances.remove(&tx.sender_pubkey);
                let remaining_balance = sender_balance - total_cost;
                if remaining_balance > 0 {
                    self.balances.insert(tx.next_pubkey.clone(), remaining_balance);
                }
                
                let receiver_balance = *self.balances.get(&tx.receiver_address).unwrap_or(&0);
                self.balances.insert(tx.receiver_address.clone(), receiver_balance + tx.amount);
                total_fees += tx.fee;
            }
        }

        // 👑 Le Séquenceur encaisse son salaire (Frais + Coinbase) !
        let seq_balance = *self.balances.get(sequencer_address).unwrap_or(&0);
        self.balances.insert(
            sequencer_address.to_string(), 
            seq_balance + total_fees + self.block_reward
        );

        (self.block_index, tx_count, total_fees)
    }

    /// Génère la Preuve d'État des Soldes (Le State Root pour le L1)
    pub fn compute_state_root(&self) -> String {
        if self.balances.is_empty() {
            return "L2_EMPTY_ROOT_HASH".to_string();
        }
        
        let mut keys: Vec<&String> = self.balances.keys().collect();
        keys.sort(); // Tri alphabétique obligatoire

        let mut hasher = Sha512::new();
        for key in keys {
            hasher.update(key.as_bytes());
            hasher.update(&self.balances.get(key).unwrap().to_be_bytes());
        }
        
        hex::encode(hasher.finalize())
    }
}

pub type SharedL2State = Arc<Mutex<L2State>>;