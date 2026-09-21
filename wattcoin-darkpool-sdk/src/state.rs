use serde::{Serialize, Deserialize};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::fs;
use sha2::{Sha512, Digest};
use wattcoin_core::transaction::Transaction; // ON UTILISE LE FORMAT L1 !

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DarkpoolState {
    pub spent_key_images: HashSet<String>, // Le bouclier anti-double dépense
    pub mempool: Vec<Transaction>,
    pub block_index: u64,  
}

impl DarkpoolState {
    pub fn new() -> Self {
        Self {
            spent_key_images: HashSet::new(),
            mempool: Vec::new(),
            block_index: 0,
        }
    }

    pub fn load_from_disk(path: &str) -> Option<Self> {
        if let Ok(data) = fs::read_to_string(path) {
            if let Ok(state) = serde_json::from_str::<Self>(&data) {
                println!("💾 [DARKPOOL] Base de données chargée (Reprise au Bloc #{})", state.block_index);
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

    /// Filtre le mempool : on jette les doubles dépenses, on valide les frais
    pub fn process_mempool(&mut self) -> (u64, usize, u64) {
		self.block_index += 1; 
		let txs = std::mem::take(&mut self.mempool);
		let mut valid_tx_count = 0;
		let mut total_fees = 0u64;

		for tx in txs {
			let mut double_spend = false;
			let mut key_image = String::new();
			
			if let Some(sig) = &tx.wots_signature {
				key_image = hex::encode(&sig.public_key);
				if self.spent_key_images.contains(&key_image) {
					double_spend = true;
				}
			}

			if !double_spend && !key_image.is_empty() {
				self.spent_key_images.insert(key_image);
				total_fees += tx.fee;
				valid_tx_count += 1;
			} else {
				println!("⚠️ [DARKPOOL] Tentative de double dépense bloquée ou signature manquante !");
			}
		}

		(self.block_index, valid_tx_count, total_fees)
	}

    /// La racine d'état n'est plus le solde des gens, mais le HASH des Key Images brûlés !
    pub fn compute_state_root(&self) -> String {
        if self.spent_key_images.is_empty() {
            return "DARKPOOL_EMPTY_ROOT_HASH".to_string();
        }
        
        let mut images: Vec<&String> = self.spent_key_images.iter().collect();
        images.sort(); // Déterminisme

        let mut hasher = Sha512::new();
        for img in images {
            hasher.update(img.as_bytes());
        }
        hex::encode(hasher.finalize())
    }
}

pub type SharedDarkpoolState = Arc<Mutex<DarkpoolState>>;