use serde::{Serialize, Deserialize};
use sha2::{Sha256, Sha512, Digest};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct L2Transaction {
    pub sender_pubkey: String,   // La clé Lattice de l'expéditeur
	pub receiver_address: String, // L'adresse du destinataire
    pub amount: u64,             // Montant du token L2 envoyé
    pub fee: u64,                // Frais payés au Séquenceur L2
    pub signature: String,       // Preuve cryptographique
}

impl L2Transaction {
    /// Hache les données pour vérifier la signature
    pub fn hash_data(&self) -> [u8; 32] { 
        let mut hasher = Sha512::new();
        hasher.update(self.sender_pubkey.as_bytes());
        hasher.update(self.receiver_address.as_bytes());
        hasher.update(&self.amount.to_be_bytes());
        hasher.update(&self.fee.to_be_bytes());
        
        let mut final_hasher = Sha256::new();
        final_hasher.update(hasher.finalize());
        
        let mut result = [0u8; 32];
        result.copy_from_slice(&final_hasher.finalize());
        result
    }
}