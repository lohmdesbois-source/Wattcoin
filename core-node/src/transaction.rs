use serde::{Serialize, Deserialize};
use sha2::{Sha512, Digest};
use crate::wots::WotsSignature;
use crate::lattice::LWECommitment;
use crate::merkle_ring::MpcRingSignature;

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
}

// 🛡️ L'Input Anonyme (MPC Ring Signature + Montant Masqué)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInput {
    pub mpc_ring: MpcRingSignature, 
    pub commitment: LWECommitment,  
    pub source_height: u64,
}

// 🛡️ L'Output Masqué (Capsule Kyber + Montant Masqué)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionOutput {
    pub stealth_address: String,      
    pub kyber_capsule: String,        
    pub aes_vault: String,            
    pub lattice_commitment: LWECommitment, 
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
        let tx_data = format!("{:?}{:?}{}", self.tx_type, self.outputs, self.fee);
        hasher.update(tx_data.as_bytes());
        let result = hasher.finalize();
        let mut hash_arr = [0u8; 64];
        hash_arr.copy_from_slice(&result);
        hash_arr
    }

    pub fn is_valid(&self) -> bool {
        if matches!(self.tx_type,
            TransactionType::Coinbase 
			| TransactionType::MicroCoinbase
            | TransactionType::DexSettlement { .. } 
            | TransactionType::LotteryPayout { .. }
			| TransactionType::MiningShare { .. }
			) {
            return true;
        }

        if let TransactionType::HTLCClaim { secret } = &self.tx_type {
            if secret.is_empty() { return false; }
            let secret_bytes = hex::decode(secret).unwrap_or_default();
            let real_hash = hex::encode(sha2::Sha256::digest(&secret_bytes));
            return real_hash == self.public_key; 
        }

        // 1. Vérification Homomorphe des Montants (Lattice LWE)
        let in_commitments: Vec<_> = self.inputs.iter().map(|i| i.commitment.clone()).collect();
        let out_commitments: Vec<_> = self.outputs.iter().map(|o| o.lattice_commitment.clone()).collect();
        if !LWECommitment::verify_balance(&in_commitments, &out_commitments, self.fee) { 
            return false; 
        }

        let tx_hash = self.hash_data();

        // 2. Vérification de l'Anonymat de l'Expéditeur (MPC)
        for input in &self.inputs {
            if !input.mpc_ring.verify(&tx_hash) { return false; }
        }

        // 3. Vérification de la signature WOTS+ finale
        if let Some(sig) = &self.wots_signature {
            crate::wots::WotsKeyPair::verify(&self.public_key, sig, &tx_hash)
        } else {
            false
        }
    }
}