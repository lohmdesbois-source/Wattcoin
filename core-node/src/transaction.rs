#![allow(dead_code)]
use serde::{Serialize, Deserialize};
use sha2::{Sha256, Sha512, Digest};
use wots::WotsSignature;

/// Nombre maximal de sous-adresses dépensées dans UNE transaction (borne anti-DoS de la vérification)
pub const MAX_OWNERS: usize = 16;


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
    // Hache les données pour vérifier la signature
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
    // Le Séquenceur verrouille ses propres fonds pour prouver sa légitimité (Skin in the game)
    L2Stake { l2_name: String, sequencer_pubkey: String },
    // Le Séquenceur ferme sa chaîne et récupère ses fonds (après un délai de sécurité)
    L2Unstake { l2_name: String },
    // Le Séquenceur grave l'état de sa chaîne (La racine de son arbre de Merkle) sur le L1
    L2Anchor { l2_name: String, state_root: String, sequencer_signature: String, withdrawals: Vec<TransactionOutput> },
	// L'utilisateur verrouille ses WATT pour lui-même sur le L2 !
    L2BridgeLock { 
        l2_target_name: String,       // ex: "WNS"
        l2_receiver_pubkey: String,   // La clé WOTS+ de l'utilisateur sur le L2 WNS
    },
}

// L'Input
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransactionInput {
    pub utxo_id: String, // Identifiant strict (kyber_capsule d'origine)
    pub amount: u64, // On ne cache pas le montant, amount direct
    pub source_height: u64,
    #[serde(default)]
    pub owner_otp: String, // hex : révélé pour ouvrir l'engagement pq_watt_/L2_WATT_ (vide pour un output public)
    #[serde(default)]
    pub owner: u8,         // index de la preuve (dans tx.owners) qui couvre cet input — SIGNÉ
}

// Une preuve de propriété : une racine R_i + sa signature hypertree sur le hash COMPLET de la TX
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OwnerProof {
    pub root: String,                               // hex R_i (signé)
    pub signature: Option<wots::HyperSignature>,    // exclue du hash
}

// L'Output
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransactionOutput {
    pub stealth_address: String,      
    pub kyber_capsule: String,        
    pub aes_vault: String, // Contient potentiellement l'OTP/Message chiffré pour le destinataire
    pub amount: u64,       // LE MONTANT EN CLAIR, LA FIN DE L'USINE À GAZ
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transaction {
    pub tx_type: TransactionType,
    pub inputs: Vec<TransactionInput>,
    pub outputs: Vec<TransactionOutput>,
    pub fee: u64, 
    pub wots_signature: Option<WotsSignature>, // réservée à l'unstake (clé de caution unique)
    pub public_key: String, 
    #[serde(default)]
    pub owners: Vec<OwnerProof>,               // une preuve par sous-adresse dépensée (max MAX_OWNERS)
}

impl Transaction {
	pub fn hash_data(&self) -> [u8; 64] {
        let mut hasher = Sha512::new();
        
        // SPLIT CONSENSUS : Sérialisation binaire canonique pure
        let mut temp_tx = self.clone();
        temp_tx.wots_signature = None; // On exclut la signature...
        temp_tx.public_key = String::new(); // On exclut la clé publique du hash !
        for proof in temp_tx.owners.iter_mut() { proof.signature = None; } // les racines restent signées
        
        if let Ok(bytes) = bincode::serialize(&temp_tx) {
            hasher.update(&bytes);
        }
        
        let result = hasher.finalize();
        let mut hash_arr = [0u8; 64];
        hash_arr.copy_from_slice(&result);
        hash_arr
    }
	
	// Clés à usage unique consommées par cette TX (anti-réutilisation) :
    // une feuille hypertree par preuve, ou la clé de caution d'un unstake.
    pub fn one_time_keys(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.owners.iter()
            .filter_map(|o| o.signature.as_ref())
            .map(|s| s.one_time_key_hex())
            .collect();
        if let Some(s) = &self.wots_signature { keys.push(hex::encode(&s.public_key)); }
        keys
    }

    pub fn is_valid(&self) -> bool {
		let is_consensus_mint = matches!(self.tx_type, TransactionType::Coinbase | TransactionType::MicroCoinbase | TransactionType::LotteryPayout { .. });
		
		// Transactions sans frais (Certaines, comme HTLCClaim, ont besoin d'un output)
		let is_feeless = matches!(self.tx_type, 
			TransactionType::DexSettlement { .. } | 
			TransactionType::MiningShare { .. } |
			TransactionType::HTLCClaim { .. } | 
			TransactionType::HTLCRefund { .. }
		);

		// Signaux purs (Strictement 0 input et 0 output)
		let is_strictly_empty = matches!(self.tx_type, 
			TransactionType::DexSettlement { .. } | 
			TransactionType::MiningShare { .. }
		);

		// 1. Bloquer les transactions sans input (SAUF consensus/mint et feeless)
		if !is_consensus_mint && !is_feeless && self.inputs.is_empty() {
			println!("⛔ Rejet : Transaction standard sans input.");
			return false;
		}

		// 2. Empêcher la création d'outputs fantômes sur les signaux P2P
		if is_strictly_empty && (!self.outputs.is_empty() || !self.inputs.is_empty()) {
			println!("⛔ Rejet : Les DexSettlement et MiningShare ne peuvent avoir ni input ni output.");
			return false;
		}

		// 3. Règle structurelle stricte pour HTLCClaim
		if let TransactionType::HTLCClaim { .. } = self.tx_type {
			if !self.inputs.is_empty() || self.outputs.len() != 1 {
				println!("⛔ Rejet : HTLCClaim doit avoir 0 input et exactement 1 output.");
				return false;
			}
		}
		
		// 3.5 POUR LE REFUND
		if let TransactionType::HTLCRefund { .. } = self.tx_type {
			if !self.inputs.is_empty() || self.outputs.len() != 1 {
				println!("⛔ Rejet : HTLCRefund doit avoir 0 input et exactement 1 output.");
				return false;
			}
		}

		// 4. Bloquer l'underflow sur les frais
		let max_supply_flames = 10_000_000_000_u64 * 1_000_000_000_u64;
		if self.fee > max_supply_flames {
			println!("⛔ Rejet : Frais aberrants.");
			return false;
		}

        // 5. Signatures
        //    - Unstake : UNE clé de caution WOTS, aucune preuve hypertree
        //    - Sans input (mint, feeless) : ni signature ni preuve
        //    - Toute autre dépense : une preuve par sous-adresse, chacune signant TOUTE la TX
        let hash = self.hash_data();
        let reject = |msg: &str| { println!("⛔ Rejet : {}", msg); false };
        if matches!(self.tx_type, TransactionType::L2Unstake { .. }) {
            let Some(sig) = &self.wots_signature else { return reject("unstake sans signature de caution."); };
            if !self.owners.is_empty() || !wots::Wots::verify(sig, &hash) { return reject("signature WOTS+ (unstake) invalide."); }
        } else if self.inputs.is_empty() {
            if self.wots_signature.is_some() || !self.owners.is_empty() { return reject("signature sur une transaction sans input."); }
        } else {
            if self.wots_signature.is_some() { return reject("signature WOTS simple interdite pour une dépense."); }
            if self.owners.is_empty() || self.owners.len() > MAX_OWNERS { return reject("nombre de preuves de propriété invalide."); }
            // a) Chaque input désigne une preuve existante, et chaque preuve couvre au moins un input
            let mut used = vec![false; self.owners.len()];
            for input in &self.inputs {
                match used.get_mut(input.owner as usize) {
                    Some(u) => *u = true,
                    None => return reject("input rattaché à une preuve inexistante."),
                }
            }
            if used.contains(&false) { return reject("preuve de propriété inutilisée."); }
            // b) Racines distinctes, feuilles distinctes, chaque preuve signe le hash complet
            let mut roots = std::collections::HashSet::new();
            let mut leaves = std::collections::HashSet::new();
            for proof in &self.owners {
                let Some(root) = decode32(&proof.root) else { return reject("racine de propriété invalide."); };
                let Some(sig) = &proof.signature else { return reject("preuve de propriété non signée."); };
                if !roots.insert(root) { return reject("racine de propriété en double."); }
                if !leaves.insert(sig.one_time_key_hex()) { return reject("feuille hypertree réutilisée dans la transaction."); }
                if !sig.verify(&hash, &root) { return reject("signature hypertree invalide."); }
            }
        }

        // 6. Validation de la Preuve de Secret (HTLC)
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

        // VÉRIFICATION MATHÉMATIQUE CLASSIQUE ET INFAILLIBLE
        if !is_consensus_mint && !is_feeless {
            let mut sum_in = 0u64;
            let mut sum_out = 0u64;
            
            for input in &self.inputs {
                sum_in = match sum_in.checked_add(input.amount) {
                    Some(s) => s,
                    None => return false, // Anti-Overflow
                };
            }
            
            for out in &self.outputs {
                sum_out = match sum_out.checked_add(out.amount) {
                    Some(s) => s,
                    None => return false, // Anti-Overflow
                };
            }
            
            let expected_in = match sum_out.checked_add(self.fee) {
                Some(s) => s,
                None => return false,
            };

            if sum_in != expected_in {
                println!("⛔ Rejet : Les montants ne s'équilibrent pas (Entrée: {}, Sortie+Frais: {}).", sum_in, expected_in);
                return false;
            }
        }
        
        true
    }
}

// =====================================================================
// PROPRIÉTÉ DES OUTPUTS : un output appartient à une racine hypertree R
// =====================================================================

/// Engagement caché d'un output : H("wattcoin/owner" || R || otp)
pub fn owner_commit(root: &[u8; 32], otp: &[u8; 32]) -> String {
    let mut h = Sha256::new();
    h.update(b"wattcoin/owner");
    h.update(root);
    h.update(otp);
    hex::encode(h.finalize())
}

pub fn decode32(hex_str: &str) -> Option<[u8; 32]> {
    hex::decode(hex_str).ok()?.try_into().ok()
}

/// La racine `root_hex` peut-elle dépenser cet output ?
/// - pq_watt_<C> / L2_WATT_<C>               : C == owner_commit(R, otp)  (caché)
/// - COINBASE_… / JACKPOT_… / <kyber>.<R>    : R public en fin d'adresse
/// - tout le reste (htlc_watt_, LOTTERY_RESERVE, BRIDGE_L2_, L2_STAKE_…) : jamais par cette voie
pub fn can_spend(out: &TransactionOutput, root_hex: &str, otp_hex: &str) -> bool {
    let Some(root) = decode32(root_hex) else { return false };
    let addr = &out.stealth_address;

    // Récompense de séquenceur : L2 mais PUBLIQUE (testé AVANT "L2_WATT_", qui est son préfixe)
    if let Some(rest) = addr.strip_prefix("L2_WATT_PUB_") {
        return rest.rsplit('.').next() == Some(hex::encode(root).as_str());
    }

    for prefix in ["pq_watt_", "L2_WATT_"] {
        if let Some(commit) = addr.strip_prefix(prefix) {
            let Some(otp) = decode32(otp_hex) else { return false };
            return commit == owner_commit(&root, &otp);
        }
    }

    let public_owner = if let Some(rest) = addr.strip_prefix("COINBASE_").or_else(|| addr.strip_prefix("JACKPOT_")) {
        rest.rsplit('.').next().unwrap_or(rest)
    } else if let Some((_, r)) = addr.rsplit_once('.') {
        r
    } else {
        return false;
    };
    public_owner == hex::encode(root)
}

/// Adresse de propriétaire valide : "<kyber>.<R_hex>" ou "<R_hex>" (R = 32 octets)
pub fn is_owner_address(addr: &str) -> bool {
    decode32(addr.rsplit('.').next().unwrap_or(addr)).is_some()
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    fn out(addr: &str) -> TransactionOutput {
        TransactionOutput { stealth_address: addr.to_string(), kyber_capsule: String::new(), aes_vault: String::new(), amount: 1 }
    }

    #[test]
    fn engagement_cache() {
        let (r, otp) = ([1u8; 32], [2u8; 32]);
        let o = out(&format!("pq_watt_{}", owner_commit(&r, &otp)));
        assert!(can_spend(&o, &hex::encode(r), &hex::encode(otp)));
        assert!(!can_spend(&o, &hex::encode([9u8; 32]), &hex::encode(otp))); // autre racine
        assert!(!can_spend(&o, &hex::encode(r), &hex::encode([3u8; 32])));   // autre otp
    }

    #[test]
    fn proprietaire_public() {
        let r = hex::encode([1u8; 32]);
        assert!(can_spend(&out(&format!("COINBASE_{}", r)), &r, ""));
        assert!(can_spend(&out(&format!("JACKPOT_kyberB64.{}", r)), &r, ""));
        assert!(can_spend(&out(&format!("kyberB64.{}", r)), &r, ""));
        assert!(!can_spend(&out(&format!("COINBASE_{}", r)), &hex::encode([2u8; 32]), ""));
    }

    #[test]
    fn jamais_depensables() {
        let r = hex::encode([1u8; 32]);
        for a in ["htlc_watt_abc", "LOTTERY_RESERVE", "BRIDGE_L2_WNS", "L2_STAKE_abc"] {
            assert!(!can_spend(&out(a), &r, &r));
        }
    }
	
    #[test]
    fn l2_public() {
        let r = hex::encode([1u8; 32]);
        assert!(can_spend(&out(&format!("L2_WATT_PUB_kyberB64.{}", r)), &r, ""));
        assert!(can_spend(&out(&format!("L2_WATT_PUB_{}", r)), &r, ""));
        assert!(!can_spend(&out(&format!("L2_WATT_PUB_{}", r)), &hex::encode([2u8; 32]), ""));
        assert!(is_owner_address(&format!("kyberB64.{}", r)));
        assert!(!is_owner_address("adresse_bidon"));
    }
	
    // subs[j] = sous-adresse de la preuve j ; owner_of[i] = preuve qui couvre l'input i
    fn signed_tx(subs: &[u64], owner_of: &[u8]) -> Transaction {
        let mut tx = Transaction { tx_type: TransactionType::Standard, inputs: vec![], outputs: vec![], fee: 0,
            wots_signature: None, public_key: String::new(), owners: vec![] };
        for (i, o) in owner_of.iter().enumerate() {
            tx.inputs.push(TransactionInput { utxo_id: format!("u{}", i), amount: 1, source_height: 0, owner_otp: String::new(), owner: *o });
        }
        tx.outputs.push(TransactionOutput { stealth_address: "pq_watt_x".into(), kyber_capsule: String::new(), aes_vault: String::new(), amount: owner_of.len() as u64 });
        let mut keys: Vec<wots::HyperKeys> = subs.iter().map(|s| wots::HyperKeys::new(&[7u8; 32], *s)).collect();
        for k in &keys { tx.owners.push(OwnerProof { root: hex::encode(k.root()), signature: None }); }
        let msg = tx.hash_data();
        for (j, k) in keys.iter_mut().enumerate() {
            tx.owners[j].signature = Some(k.sign(j as u64, &msg, |_| false, |_| true).unwrap());
        }
        tx
    }

    #[test]
    fn multi_proprietaires() {
        let tx = signed_tx(&[0, 1], &[0, 1, 1]);
        assert!(tx.is_valid());
        assert_eq!(tx.one_time_keys().len(), 2);

        let mut t = tx.clone(); t.inputs[0].owner = 1;          // input réattribué après signature
        assert!(!t.is_valid());
        let mut t = tx.clone(); t.owners.swap(0, 1);            // preuves permutées
        assert!(!t.is_valid());
        let mut t = tx.clone(); t.owners[1].signature = None;   // preuve non signée
        assert!(!t.is_valid());
        let mut t = tx.clone(); t.owners.pop();                 // input sans preuve
        assert!(!t.is_valid());

        assert!(!signed_tx(&[0, 1], &[0, 0]).is_valid());       // preuve inutile, pourtant bien signée
        assert!(!signed_tx(&[0, 0], &[0, 1]).is_valid());       // même racine deux fois
    }
}