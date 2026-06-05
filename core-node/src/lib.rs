// src/lib.rs
// Wattcoin Core - Librairie (pour les tests et l'audit)



use std::sync::{Arc, Mutex};
use std::collections::HashSet;

pub mod block;
pub mod blockchain;
pub mod transaction;
pub mod network;
pub mod api;
pub mod lattice;

pub type SharedPeers = Arc<Mutex<HashSet<String>>>; 

// ===================================================================
// 🔥 SWITCH LOCAL / PROD (tu changes juste cette ligne)
pub const LOCAL_DEV_MODE: bool = true;   // ← true = local (127.0.0.1, sans Tor, ultra rapide)
// const LOCAL_DEV_MODE: bool = false; // ← pour PROD : décommente celle-ci + commente la ligne du dessus
// ===================================================================

#[derive(Debug)]
pub enum WattError {
    Crypto(String),
    Network(String),
    Io(std::io::Error),
    Vault(String),
    Json(serde_json::Error),
}

impl From<std::io::Error> for WattError {
    fn from(err: std::io::Error) -> Self { WattError::Io(err) }
}

impl From<serde_json::Error> for WattError {
    fn from(err: serde_json::Error) -> Self { WattError::Json(err) }
}