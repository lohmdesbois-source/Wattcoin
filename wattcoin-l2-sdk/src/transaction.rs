use serde::{Serialize, Deserialize};
use sha2::{Sha512, Digest};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct L2Transaction {
    pub sender_pubkey: String,   // La clé WOTS+ de l'expéditeur
    pub next_pubkey: String,     // KEY ROLLING : La nouvelle clé pour le reste du solde !
	pub receiver_address: String, // L'adresse du destinataire
    pub amount: u64,             // Montant du token L2 envoyé
    pub fee: u64,                // Frais payés au Séquenceur L2
    pub signature: String,       // Preuve cryptographique
}

impl L2Transaction {
    /// Hache les données pour vérifier la signature
    pub fn hash_data(&self) -> [u8; 64] {
        let mut hasher = Sha512::new();
        hasher.update(self.sender_pubkey.as_bytes());
        hasher.update(self.next_pubkey.as_bytes()); // On s'assure que la signature est unique !
		hasher.update(self.receiver_address.as_bytes());
        hasher.update(&self.amount.to_be_bytes());
        hasher.update(&self.fee.to_be_bytes());
        
        let mut result = [0u8; 64];
        result.copy_from_slice(&hasher.finalize());
        result
    }
}