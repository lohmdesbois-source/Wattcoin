use serde::{Serialize, Deserialize};
use sha2::{Sha512, Digest};
use std::time::{SystemTime, UNIX_EPOCH};
use reqwest::Client; // Pour interroger l'API WNS

// On importe tes outils cryptographiques existants
use crate::lattice::{LatticeKeyPair, LatticeSignature};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct DhtRecord {
    pub domain_name: String,      // ex: "watty.watt"
    pub mixnet_address: String,   // L'adresse de routage (le nœud d'entrée du Mixnet)
    pub timestamp: u64,           // Horodatage pour éviter les attaques par rejeu
    pub signature: String,        // Signature Lattice prouvant l'authenticité
}

impl DhtRecord {
    /// Le serveur/navigateur crée son enregistrement et le signe
    pub fn new(domain_name: String, mixnet_address: String, secret_key: &[u64]) -> Self {
        let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        
        let mut record = Self {
            domain_name,
            mixnet_address,
            timestamp,
            signature: String::new(),
        };

        let hash = record.hash_data();
        
        // Signature Lattice Unifiée ! (Plus de public_seed nécessaire)
        let sig = LatticeKeyPair::sign(secret_key, &hash);
        record.signature = serde_json::to_string(&sig).unwrap();
        
        record
    }

    /// Hachage strict des données pour la vérification
    pub fn hash_data(&self) -> [u8; 64] {
        let mut hasher = Sha512::new();
        hasher.update(self.domain_name.as_bytes());
        hasher.update(self.mixnet_address.as_bytes());
        hasher.update(&self.timestamp.to_be_bytes());
        
        let mut result = [0u8; 64];
        result.copy_from_slice(&hasher.finalize());
        result
    }

    /// Le Tribunal du Navigateur : Est-ce que cette route est légitime ?
    pub fn is_valid(&self, expected_pubkey: &str) -> bool {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        
        if self.timestamp > now + 60 || self.timestamp < now - (48 * 3600) {
            println!("⏳ [DHT] Route rejetée : Horodatage expiré ou falsifié.");
            return false;
        }

        // Bouclier Lattice
        if let Ok(sig) = serde_json::from_str::<LatticeSignature>(&self.signature) {
            let hash = self.hash_data();
            LatticeKeyPair::verify(expected_pubkey, &sig, &hash)
        } else {
            false
        }
    }
}

// --- CLIENT WNS ---
/// Structure pour désérialiser la réponse de l'API WNS
#[derive(Deserialize, Debug)]
pub struct WnsResolveResponse {
    pub success: bool,
    pub domain: Option<String>,
    pub record_data: Option<String>, // Contient l'adresse IP/Mixnet
    pub owner_pubkey: Option<String>,
    pub error: Option<String>,
}

/// Résout un nom de domaine en interrogeant un Séquenceur WNS (L2)
/// `wns_api_url` est l'adresse de l'API du séquenceur WNS (ex: "http://80.78.26.243/wns")
pub async fn resolve_domain_wns(domain: &str, wns_api_url: &str) -> Result<WnsResolveResponse, String> {
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| format!("Erreur création client HTTP : {}", e))?;

    let url = format!("{}/resolve/{}", wns_api_url.trim_end_matches('/'), domain);

    let res = client.get(&url).send().await.map_err(|e| format!("Erreur réseau lors de la résolution WNS : {}", e))?;

    if res.status().is_success() {
        let json: WnsResolveResponse = res.json().await.map_err(|e| format!("Erreur parsing JSON WNS : {}", e))?;
        if json.success {
            Ok(json)
        } else {
            Err(json.error.unwrap_or_else(|| "Erreur inconnue renvoyée par le WNS".to_string()))
        }
    } else {
        Err(format!("Erreur HTTP {} renvoyée par le WNS", res.status()))
    }
}