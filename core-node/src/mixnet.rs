use serde::{Serialize, Deserialize};
use std::convert::TryFrom;
use pqc_kyber::decapsulate;
use aes_gcm::{Aes256Gcm, Key, Nonce, aead::{Aead, KeyInit}};

/// Le paquet blindé qui circule sur le réseau TCP
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct OnionPacket {
    /// La capsule Kyber (ML-KEM) destinée au nœud actuel
    pub kyber_capsule: String, 
    
    /// Les 12 premiers octets = Nonce AES. Le reste = Le payload chiffré.
    pub encrypted_payload: String, 
}

/// Le contenu de la couche une fois déchiffrée
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct HopPayload {
    /// L'adresse IP:PORT du prochain nœud. Si vide, on est la destination finale.
    pub next_hop_address: String,
    
    /// Le prochain OnionPacket (en JSON), ou bien la requête finale brute
    pub inner_data: String, 
}

impl OnionPacket {
    /// La fonction "Épluchage" exécutée par le Nœud
    pub fn peel(&self, my_kyber_secret_hex: &str) -> Result<HopPayload, String> {
        // 1. Décodage hexadécimal
        let sk_bytes = hex::decode(my_kyber_secret_hex)
            .map_err(|_| "Clé secrète Kyber invalide")?;
        let capsule_bytes = hex::decode(&self.kyber_capsule)
            .map_err(|_| "Capsule Kyber invalide")?;
        let cipher_bytes = hex::decode(&self.encrypted_payload)
            .map_err(|_| "Payload chiffré invalide")?;

        if cipher_bytes.len() <= 12 {
            return Err("Payload trop court pour contenir un Nonce AES".to_string());
        }

        // 2. Déchiffrement Post-Quantique (Kyber) pour obtenir le secret partagé
        let shared_secret = decapsulate(&capsule_bytes, &sk_bytes)
            .map_err(|_| "Erreur Kyber : Cette capsule n'est pas pour moi")?;

        // 3. Extraction du Nonce et déchiffrement AES-256-GCM
        // 💡 CORRECTION : On extrait les variables d'abord, puis on passe leurs références (&) !
        let nonce_bytes = Nonce::try_from(&cipher_bytes[0..12])
            .map_err(|_| "Nonce AES de taille invalide")?;
            
        let key_bytes = Key::<Aes256Gcm>::try_from(shared_secret.as_slice())
            .map_err(|_| "Clé AES de taille invalide")?;
            
        let cipher = Aes256Gcm::new(&key_bytes); // 👈 On prête la clé
        
        let plaintext = cipher.decrypt(&nonce_bytes, &cipher_bytes[12..]) // 👈 On prête le nonce
            .map_err(|_| "Erreur AES : Échec de l'intégrité (GCM) ou mauvais secret")?;

        // 4. Désérialisation du JSON intérieur
        let payload_str = String::from_utf8(plaintext)
            .map_err(|_| "Le payload déchiffré n'est pas du texte valide")?;
            
        let hop_payload: HopPayload = serde_json::from_str(&payload_str)
            .map_err(|_| "Impossible de parser le HopPayload")?;

        Ok(hop_payload)
    }
}