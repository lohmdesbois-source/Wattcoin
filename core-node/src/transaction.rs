use serde::{Serialize, Deserialize};
use crate::lattice::LWECommitment;

const LATTICE_Q: u32 = 8380417; 
const LATTICE_DIM: usize = 4;   

// Le contrat est maintenant au cœur de la blockchain !
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SwapContract {
    pub buyer_btc_address: String,
    pub buyer_btc_pubkey: String,
    pub seller_watt_address: String,
    pub seller_btc_pubkey: String,
    pub watt_amount_flames: u64,
    pub btc_amount_sats: u64,
    pub htlc_secret: String,
    pub htlc_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TransactionType {
    Coinbase,
    Standard,
    HTLCLock { hash: String, timeout_block: u64 }, 
    HTLCClaim { secret: String },                  
    HTLCRefund { hash: String },                   
    DexSettlement { clearing_price_sats: u64, total_volume_flames: u64, swaps: Vec<SwapContract> }, // 💡 LE DEX ON-CHAIN
	HTLCLottery { target_block: u64, player_pubkey: String }, // 🎟️ Le ticket
    LotteryPayout { target_block: u64, winner_pubkey: String }, // 🎰 Le gain
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PQLatticeRingSignature {
    pub key_image: String,          
    pub c0: String,                 
    pub z_responses: Vec<Vec<u32>>, 
    pub p_keys: Vec<Vec<u32>>, 
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInput {
    pub pq_ring_inputs: Vec<String>,
    pub pq_ring_signature: PQLatticeRingSignature,
    pub commitment: LWECommitment,
	pub source_height: u64,
}

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
    pub dilithium_signature: String,  
}

impl Transaction {
    pub fn is_valid(&self) -> bool {
        // 💡 Les transactions DexSettlement sont générées par les mineurs, pas besoin de ZKP
        if self.tx_type == TransactionType::Coinbase 
            || self.dilithium_signature == "PRUNED" 
            || matches!(self.tx_type, TransactionType::DexSettlement { .. })
            || matches!(self.tx_type, TransactionType::LotteryPayout { .. }) // Le mineur le génère sans signature
        { 
            return true; 
        }

        // 💡 Sécurité du Ticket de Loterie
        if let TransactionType::HTLCLottery { .. } = &self.tx_type {
            if self.outputs.is_empty() || self.outputs[0].stealth_address != "LOTTERY_RESERVE" { return false; }
            if self.outputs[0].aes_vault != "10000000000" { return false; } // Le ticket DOIT coûter 10 WATT
        }

        // =================================================================
        // 🔐 1. LE TRIBUNAL DES CONTRATS INTELLIGENTS (HTLC)
        // =================================================================
        if let TransactionType::HTLCClaim { secret } = &self.tx_type {
            if secret.is_empty() { return false; }
            if self.inputs.is_empty() { return false; }

            let secret_bytes = hex::decode(secret).unwrap_or_default();
            let calculated_hash = hex::encode(blake3::hash(&secret_bytes).as_bytes());

            if calculated_hash != self.dilithium_signature { return false; }
            return true; 
        }

        if let TransactionType::HTLCLock { hash, timeout_block: _ } = &self.tx_type {
            if hash.len() != 64 { return false; }
        }
        
        if let TransactionType::HTLCRefund { hash } = &self.tx_type {
            if hash.len() != 64 || self.inputs.is_empty() { return false; }
            return true; 
        }

        // =================================================================
        // ⚖️ 2. LE MASQUAGE POST-QUANTIQUE (LWE)
        // =================================================================
        let in_commitments: Vec<_> = self.inputs.iter().map(|i| i.commitment.clone()).collect();
        let out_commitments: Vec<_> = self.outputs.iter().map(|o| o.lattice_commitment.clone()).collect();

        if !LWECommitment::verify_balance(&in_commitments, &out_commitments, self.fee) { return false; }

        // =================================================================
        // 🌀 3. L'ÉPREUVE DU CERCLE LATTICE
        // =================================================================
        let tx_data = format!("{:?}{}", self.outputs, self.fee);

        for input in &self.inputs {
            let n = input.pq_ring_inputs.len();
            let pq_ring = &input.pq_ring_signature;
            
            if n == 0 || pq_ring.z_responses.len() != n || pq_ring.p_keys.len() != n { return false; }

            let mut current_c = hex::decode(&pq_ring.c0).unwrap_or_default();

            for i in 0..n {
                let pk_hex = &input.pq_ring_inputs[i];
                let z_vec = &pq_ring.z_responses[i];
                let p_vector = &pq_ring.p_keys[i]; 

                let c_i = u32::from_le_bytes(current_c[0..4].try_into().unwrap_or([0;4])) % LATTICE_Q;

                let mut r_i = vec![0u32; LATTICE_DIM];
                for j in 0..LATTICE_DIM {
                    let base_g = (j as u32 + 1) * 1337; 
                    let part1 = (z_vec[j] as u64 * base_g as u64) % LATTICE_Q as u64;
                    let part2 = (c_i as u64 * p_vector[j] as u64) % LATTICE_Q as u64;
                    r_i[j] = ((part1 + part2) % LATTICE_Q as u64) as u32;
                }

                let mut hasher = blake3::Hasher::new();
                hasher.update(tx_data.as_bytes());
                hasher.update(pk_hex.as_bytes()); 
                for val in r_i { hasher.update(&val.to_le_bytes()); }
                current_c = hasher.finalize().as_bytes().to_vec();
            }

            if hex::encode(&current_c) != pq_ring.c0 { return false; }
        }
        true 
    }
}
