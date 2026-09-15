#![allow(dead_code)]
use serde::{Serialize, Deserialize};
use sha2::{Sha256, Sha512, Digest}; // 💡 Ajout de Sha256

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum WnsAction {
    Register, Update, Transfer, Withdraw, StorageLock, StorageProof, 
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct L2Transaction {
    pub account_address: String, 
    pub sender_pubkey: String,   
    pub next_pubkey: String,     
    pub nonce: u64,              // 💡 LE COMPTEUR ANTI-REJEU !
    
    pub action: WnsAction,       
    pub domain_name: String,     
    pub record_data: String,     
    
    pub amount: u64,             
    pub fee: u64,                
    pub signature: String,       
}

impl L2Transaction {
    pub fn hash_data(&self) -> [u8; 32] { // 💡 WOTS+ exige exactement 32 octets
        let mut hasher = Sha512::new();
        hasher.update(self.account_address.as_bytes());
        hasher.update(self.sender_pubkey.as_bytes());
        hasher.update(self.next_pubkey.as_bytes());
        hasher.update(&self.nonce.to_be_bytes()); // On sécurise le nonce
        
        let action_byte = match self.action {
            WnsAction::Register => 0u8, WnsAction::Update => 1u8, WnsAction::Transfer => 2u8,
            WnsAction::Withdraw => 3u8, WnsAction::StorageLock => 4u8, WnsAction::StorageProof => 5u8,
        };
        hasher.update(&[action_byte]);
        
        hasher.update(self.domain_name.as_bytes());
        hasher.update(self.record_data.as_bytes());
        hasher.update(&self.amount.to_be_bytes()); 
        hasher.update(&self.fee.to_be_bytes());
        
        // On réduit le SHA-512 en SHA-256 pour WOTS+
        let mut final_hasher = Sha256::new();
        final_hasher.update(hasher.finalize());
        
        let mut result = [0u8; 32];
        result.copy_from_slice(&final_hasher.finalize());
        result
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct WnsBlock {
    pub index: u64,
    pub l1_parent_hash: String, 
    pub state_root: String,
    pub sequencer_pubkey: String,
    pub transactions: Vec<L2Transaction>,
    pub signature: String, 
}