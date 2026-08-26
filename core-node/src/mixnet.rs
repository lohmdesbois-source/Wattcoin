use serde::{Serialize, Deserialize};
use std::convert::TryFrom;
use pqc_kyber::decapsulate;
use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};

/// Le paquet blindé qui circule sur le réseau TCP
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OnionPacket {
    pub kyber_capsule: Vec<u8>,     // 👈 Binaire !
    pub encrypted_payload: Vec<u8>, // 👈 Binaire !
}

/// Le contenu de la couche une fois déchiffrée
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HopPayload {
    pub next_hop_address: String,
    pub inner_data: Vec<u8>,        // 👈 Binaire !
}

impl OnionPacket {
    /// La fonction "Épluchage" exécutée par le Nœud
    pub fn peel(&self, my_kyber_secret_hex: &str) -> Result<HopPayload, String> {
        let sk_bytes = hex::decode(my_kyber_secret_hex).map_err(|_| "Clé secrète Kyber invalide")?;
        
        if self.encrypted_payload.len() <= 12 { return Err("Payload trop court".into()); }

        let shared_secret = decapsulate(&self.kyber_capsule, &sk_bytes)
            .map_err(|_| "Erreur Kyber : Cette capsule n'est pas pour moi")?;

        let nonce_bytes = Nonce::try_from(&self.encrypted_payload[0..12]).map_err(|_| "Nonce AES invalide")?;
        let key_bytes = Key::<Aes256Gcm>::try_from(shared_secret.as_slice()).map_err(|_| "Clé AES invalide")?;
        let cipher = Aes256Gcm::new(&key_bytes); 
        
        let plaintext = cipher.decrypt(&nonce_bytes, &self.encrypted_payload[12..]) 
            .map_err(|_| "Erreur AES : Échec GCM")?;

        // 💡 DÉCODAGE BINAIRE (bincode) DU COEUR DE L'OIGNON
        let hop_payload: HopPayload = bincode::deserialize(&plaintext)
            .map_err(|_| "Impossible de désérialiser HopPayload en binaire")?;

        Ok(hop_payload)
    }
}