#![allow(dead_code)]
use serde::{Serialize, Deserialize};
use sha2::{Sha512, Digest};
use crate::lattice::LWECommitment;
use wots::WotsSignature;


// ==================== WNS (LAYER 2) ====================
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum WnsAction {
    Register,
    Update,
    Transfer,
    Withdraw, // Pour l'Unpeg (Brûler sur L2, Récupérer sur L1)
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct L2Transaction {
    pub sender_pubkey: String,   // La clé WOTS+ de l'expéditeur
    pub next_pubkey: String,     // KEY ROLLING : La nouvelle clé pour le reste du solde
    pub action: WnsAction,       // L'action à effectuer
    pub domain_name: String,     // Le domaine, ex: "felps.watt"
    pub record_data: String,     // L'adresse Mixnet, IP, ou pubkey du nouveau proprio
    pub amount: u64,             // Montant à retirer/brûler (0 pour les autres actions)
    pub fee: u64,                // Frais = Prix d'enchère pour le domaine
    pub signature: String,       // Preuve cryptographique WOTS+
}

impl L2Transaction {
    /// Hache les données pour vérifier la signature
    pub fn hash_data(&self) -> [u8; 64] {
        let mut hasher = sha2::Sha512::new();
        hasher.update(self.sender_pubkey.as_bytes());
        hasher.update(self.next_pubkey.as_bytes());
        
        let action_byte = match self.action {
            WnsAction::Register => 0u8,
            WnsAction::Update => 1u8,
            WnsAction::Transfer => 2u8,
            WnsAction::Withdraw => 3u8,
        };
        hasher.update(&[action_byte]);
        
        hasher.update(self.domain_name.as_bytes());
        hasher.update(self.record_data.as_bytes());
        hasher.update(&self.amount.to_be_bytes()); 
        hasher.update(&self.fee.to_be_bytes());
        
        let mut result = [0u8; 64];
        result.copy_from_slice(&hasher.finalize());
        result
    }
}

// ==================== SWAP CONTRACT ====================
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapContract {
    pub buyer_watt_address: String,   
    pub buyer_btc_address: String,
    pub buyer_btc_pubkey: String,
    pub seller_watt_address: String,
    pub seller_btc_address: String,   
    pub seller_btc_pubkey: String,
    pub watt_amount_flames: u64,
    pub btc_amount_sats: u64,
    pub htlc_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionType {
    Coinbase,
	MicroCoinbase,
    Standard,
    HTLCLock { hash: String, timeout_block: u64 },
    HTLCClaim { secret: String },
    HTLCRefund { hash: String },
    DexSettlement { clearing_price_sats: u64, total_volume_flames: u64, swaps: Vec<SwapContract> },
    HTLCLottery { target_block: u64, player_pubkey: String },
    LotteryPayout { target_block: u64, winner_pubkey: String },
	MiningShare { miner_address: String, nonce: u64, hash: String, timestamp: i64 },
	// POUR L'OUVERTURE AUX L2 EXTERNES :
    /// Le Séquenceur verrouille ses propres fonds pour prouver sa légitimité (Skin in the game)
    L2Stake { l2_name: String, sequencer_pubkey: String },
    /// Le Séquenceur ferme sa chaîne et récupère ses fonds (après un délai de sécurité)
    L2Unstake { l2_name: String },
    /// Le Séquenceur grave l'état de sa chaîne (La racine de son arbre de Merkle) sur le L1
    L2Anchor { l2_name: String, state_root: String, sequencer_signature: String, withdrawals: Vec<TransactionOutput> },
	// L'utilisateur verrouille ses WATT pour lui-même sur le L2 !
    L2BridgeLock { 
        l2_target_name: String,       // ex: "AVA"
        l2_receiver_pubkey: String,   // La clé WOTS+ de l'utilisateur sur le L2 AVA
    },
}

// L'Input Anonyme
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInput {
    pub utxo_id: String, // Identifiant strict (kyber_capsule d'origine)
    pub commitment: LWECommitment,  
    pub source_height: u64,
}

// L'Output Masqué (Capsule Kyber + Montant Masqué)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransactionOutput {
    pub stealth_address: String,      
    pub kyber_capsule: String,        
    pub aes_vault: String,            
    pub lattice_commitment: LWECommitment, 
	// Emplacement cryptographique pour le ZK Range Proof
    // Empêche les montants négatifs (underflow) et les montants colossaux
    pub range_proof: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub tx_type: TransactionType,
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
    pub fee: u64, 
    pub wots_signature: Option<WotsSignature>,
    pub public_key: String, 
}

impl Transaction {
	pub fn hash_data(&self) -> [u8; 64] {
		let mut hasher = Sha512::new();
        
		// SPLIT CONSENSUS : Sérialisation binaire canonique pure
		let mut temp_tx = self.clone();
		temp_tx.wots_signature = None; // On exclut la signature de son propre hash
		
		if let Ok(bytes) = bincode::serialize(&temp_tx) {
			hasher.update(&bytes);
		}
        
		let result = hasher.finalize();
		let mut hash_arr = [0u8; 64];
		hash_arr.copy_from_slice(&result);
		hash_arr
	}

    pub fn is_valid(&self) -> bool {
        let is_consensus_mint = matches!(self.tx_type, TransactionType::Coinbase | TransactionType::MicroCoinbase | TransactionType::LotteryPayout { .. });
        let is_feeless_empty = matches!(self.tx_type, TransactionType::DexSettlement { .. } | TransactionType::MiningShare { .. });

        // 1. Bloquer les transactions sans input (SAUF consensus/mint)
        if !is_consensus_mint && !is_feeless_empty && self.inputs.is_empty() {
            println!("⛔ Rejet : Transaction standard sans input.");
            return false;
        }

        // 2. Empêcher la création d'outputs fantômes sur les signaux P2P (Dex/Minage)
        if is_feeless_empty && (!self.outputs.is_empty() || !self.inputs.is_empty()) {
            println!("⛔ Rejet : Les DexSettlement et MiningShare ne peuvent avoir ni input ni output.");
            return false;
        }

        // 3. Bloquer l'underflow sur les frais
        // Limite fixée à 10 milliards de WATT (10^19 Flames), ce qui rentre parfaitement dans un u64
        let max_supply_flames = 10_000_000_000_u64 * 1_000_000_000_u64;
        if self.fee > max_supply_flames {
            println!("⛔ Rejet : Frais aberrants.");
            return false;
        }

        // 4. Validation stricte des signatures WOTS+
        if let Some(sig) = &self.wots_signature {
            let hash = self.hash_data();
            let mut hash_32 = [0u8; 32];
            hash_32.copy_from_slice(&hash[0..32]);
            
            if !wots::Wots::verify(sig, &hash_32) {
                println!("⛔ Rejet : Signature WOTS+ invalide.");
                return false;
            }
        } else if !is_consensus_mint && !is_feeless_empty {
            println!("⛔ Rejet : Transaction non signée.");
            return false;
        }

        // 5. Validation de la Preuve de Secret (HTLC)
        if let TransactionType::HTLCClaim { secret } = &self.tx_type {
            if secret.is_empty() { return false; }
            let secret_bytes = hex::decode(secret).unwrap_or_default();
            let real_hash = hex::encode(sha2::Sha256::digest(&secret_bytes));
            if real_hash != self.public_key { return false; }
        }

        if self.inputs.len() > 256 || self.outputs.len() > 256 { return false; }

        let mut total_vault_size = 0;
        for out in &self.outputs { total_vault_size += out.aes_vault.len(); }
        if total_vault_size > 8_388_608 { return false; }

        // 6. Validation Homomorphe (Appliquée SEULEMENT si la transaction dépense des UTXOs)
        if !self.inputs.is_empty() {
            let in_commitments: Vec<_> = self.inputs.iter().map(|i| i.commitment.clone()).collect();
            let out_commitments: Vec<_> = self.outputs.iter().map(|o| o.lattice_commitment.clone()).collect();
            if !LWECommitment::verify_balance(&in_commitments, &out_commitments, self.fee) { 
                return false; 
            }
        }

        // 7. RANGE PROOF UNIVERSEL : Personne n'y échappe, pas même la Loterie ou la Coinbase !
        for out in &self.outputs {
            if !LWECommitment::verify_range_proof(&out.lattice_commitment, &out.range_proof) {
                println!("⛔ Rejet : Range Proof invalide (Montant illégal ou underflow).");
                return false;
            }
        }
        
        true
    }
}