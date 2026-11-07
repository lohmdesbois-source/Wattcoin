// wattcoin_name_service/src/state.rs

use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sha2::{Sha512, Digest};
use crate::transaction::{L2Transaction, WnsAction, WnsBlock};

/// Frais L2 minimum par transaction
pub const MIN_L2_FEE: u64 = 1500;
/// Retraits max par bloc (le L1 accepte 256 sorties : il faut de la place pour les frais et la monnaie du gaz)
pub const MAX_UNPEG_PER_BLOCK: usize = 200;
/// Un contrat de stockage est rompu après ce nombre de blocs sans preuve
const MAX_BLOCKS_WITHOUT_PROOF: u64 = 100;

// La définition d'un Défi (Le Corrigé d'Examen)
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StorageChallenge {
    pub chunk_index: u64,             // Le numéro du morceau du fichier (ex: 42)
    pub nonce: String,                // Le nombre aléatoire chaotique (ex: "8f4a2b...")
    pub expected_answer_hash: String, // Le Hash du corrigé généré par le Wallet
}

// Le Contrat de Stockage
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct StorageContract {
    pub host_pubkey: String,       // À qui l'argent est destiné
    pub total_chunks: u64,         // Le nombre de morceaux
    pub locked_flames: u64,        // L'argent restant dans le contrat
    pub payment_per_proof: u64,    // Combien l'hébergeur gagne par preuve validée
    pub last_proof_block: u64,     // Le dernier bloc où l'hébergeur a donné signe de vie
    pub pending_challenges: Vec<StorageChallenge>, // La liste secrète des défis restants !
}

// Le modèle de Compte
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct L2Account {
    pub balance: u64,
    pub nonce: u64, // Le compteur d'état !
    pub authorized_wots_key: String, 
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct L2State {
    pub accounts: HashMap<String, L2Account>,         
    pub domains: HashMap<String, String>,         
    pub domain_owners: HashMap<String, String>,  
    pub storage_contracts: HashMap<String, StorageContract>,    
    pub mempool: Vec<L2Transaction>,              
    pub block_index: u64,      // = l2_height du dernier bloc (ancré)
    pub last_l1_block: u64,    // dépôts L1 crédités jusqu'à ce bloc
    pub last_root: String,     // racine du dernier bloc = prev_root du prochain ancrage
    
    // 💡 SLED : On garde la connexion DB ouverte directement dans l'état !
    // On demande à Serde de l'ignorer quand il envoie l'état sur le réseau P2P.
    #[serde(skip)]
    pub db: Option<sled::Db>,
}

/// Pourquoi une TX n'entre pas dans le bloc
pub enum TxVerdict {
    Keep,           // pas encore applicable (compte pas encore crédité, solde insuffisant, nonce futur) : reste en mempool
    Reject(String), // invalide : jetée
}

/// Seuls les alias .watt et .chain existent dans le registre
pub fn valid_domain(d: &str) -> bool {
    (d.ends_with(".watt") && d.len() > 5) || (d.ends_with(".chain") && d.len() > 6)
}

/// Destination d'un retrait : "<kyber>.<R>" (EXACTEMENT la règle du L1 pour une sortie UNPEG_)
pub fn is_l1_owner_address(addr: &str) -> bool {
    addr.rsplit_once('.').map_or(false, |(_, r)| wattcoin_core::transaction::decode32(r).is_some())
}

impl L2State {
    pub fn new(db_path: &str) -> Self {
        let db = sled::open(db_path).expect("❌ Impossible d'ouvrir la DB Sled du WNS");
        Self {
            accounts: HashMap::new(),
            domains: HashMap::new(),
            domain_owners: HashMap::new(),
            storage_contracts: HashMap::new(),
            mempool: Vec::new(),
            block_index: 0,
            last_l1_block: 0,
            last_root: wattcoin_core::transaction::L2_GENESIS_ROOT.to_string(),
            db: Some(db),
        }
    }

    pub fn load_from_disk(path: &str) -> Option<Self> {
        if let Ok(db) = sled::open(path) {
            if let Ok(Some(data)) = db.get("wns_state") {
                if let Ok(mut state) = bincode::deserialize::<Self>(&data) {
                    println!("💾 [L2 STATE] Base de données Sled chargée avec succès ! (Reprise au Bloc #{})", state.block_index);
                    state.db = Some(db);
                    return Some(state);
                }
            }
        }
        None
    }

    pub fn save_to_disk(&self) {
        if let Some(db) = &self.db {
            if let Ok(encoded) = bincode::serialize(self) {
                let _ = db.insert("wns_state", encoded);
                let _ = db.flush();
            }
        }
    }

    /// TOUT l'argent du L2 : soldes + fonds bloqués dans les contrats de stockage.
    /// Doit égaler la réserve du pont sur le L1 (compteur 1:1).
    pub fn l2_total(&self) -> u64 {
        let balances = self.accounts.values().fold(0u64, |t, a| t.saturating_add(a.balance));
        self.storage_contracts.values().fold(balances, |t, c| t.saturating_add(c.locked_flames))
    }

    /// Retour à l'état `snapshot` (ancrage refusé ou jamais gravé). Les TX L2 du bloc annulé repartent
    /// en mempool, celles arrivées entre-temps sont gardées. Les dépôts seront relus par le scanner
    /// depuis snapshot.last_l1_block : rien n'est perdu, rien n'est compté deux fois.
    pub fn rollback_to(&mut self, snapshot: L2State) {
        let arrived: Vec<L2Transaction> = self.mempool.iter()
            .filter(|t| !snapshot.mempool.iter().any(|s| s.signature == t.signature))
            .cloned().collect();
        let db = self.db.take();
        *self = snapshot;
        self.db = db;
        self.mempool.extend(arrived);
        self.save_to_disk();
    }

    // Contrats rompus (hébergeur muet) : les fonds RESTENT sur le L2, rendus au propriétaire du domaine
    fn expire_contracts(&mut self) {
        let now = self.block_index;
        let mut expired: Vec<String> = self.storage_contracts.iter()
            .filter(|(_, c)| now > c.last_proof_block + MAX_BLOCKS_WITHOUT_PROOF)
            .map(|(d, _)| d.clone())
            .collect();
        expired.sort(); // ordre déterministe : séquenceur et pairs font exactement pareil
        for domain in expired {
            let Some(contract) = self.storage_contracts.remove(&domain) else { continue };
            let owner = self.domain_owners.get(&domain).cloned().unwrap_or_default();
            let acc = self.accounts.entry(owner.clone())
                .or_insert(L2Account { balance: 0, nonce: 0, authorized_wots_key: String::new() });
            acc.balance += contract.locked_flames;
            println!("🚨 [SLA] Contrat de '{}' rompu : {} Flames rendus au compte {}.", domain, contract.locked_flames, owner);
        }
    }

    // Règle UNIQUE d'une TX L2 (séquenceur ET pairs) : tout est vérifié AVANT de toucher à l'état,
    // et aucun Flame ne disparaît (seuls les frais et les retraits sortent, vers le L1, dans l'ancrage).
    // Retourne le retrait (adresse L1, montant) si c'en est un.
    fn apply_tx(&mut self, tx: &L2Transaction) -> Result<Option<(String, u64)>, TxVerdict> {
        let reject = |m: &str| TxVerdict::Reject(m.to_string());

        // 1. Forme
        if tx.fee < MIN_L2_FEE { return Err(reject("frais L2 trop bas")); }
        match tx.action {
            WnsAction::Withdraw => {
                if tx.amount == 0 || !is_l1_owner_address(&tx.record_data) {
                    return Err(reject("retrait : montant nul ou adresse L1 invalide (attendu <kyber>.<R>)"));
                }
            }
            WnsAction::StorageLock => {
                if tx.amount == 0 { return Err(reject("contrat de stockage sans fonds")); }
            }
            _ => {
                if tx.amount != 0 { return Err(reject("montant interdit pour cette action (il serait perdu)")); }
            }
        }
        if matches!(tx.action, WnsAction::Register | WnsAction::Update) && !valid_domain(&tx.domain_name) {
            return Err(reject("domaine hors registre (.watt / .chain)"));
        }

        // 2. Signature WOTS+ de la clé autorisée du compte, nonce suivant, solde suffisant
        let sig_ok = serde_json::from_str::<wots::WotsSignature>(&tx.signature)
            .map_or(false, |s| hex::encode(&s.public_key) == tx.sender_pubkey && wots::Wots::verify(&s, &tx.hash_data()));
        if !sig_ok { return Err(reject("signature WOTS+ invalide")); }
        let Some(acc) = self.accounts.get(&tx.account_address) else { return Err(TxVerdict::Keep) };
        if acc.authorized_wots_key != tx.sender_pubkey { return Err(reject("clé WOTS+ non autorisée pour ce compte")); }
        if tx.nonce > acc.nonce + 1 { return Err(TxVerdict::Keep); }
        if tx.nonce != acc.nonce + 1 { return Err(reject("nonce déjà utilisé")); }
        let total_cost = tx.fee.checked_add(tx.amount).ok_or_else(|| reject("débordement"))?;
        if acc.balance < total_cost { return Err(TxVerdict::Keep); }

        // 3. Contrat de stockage : préparé AVANT le débit (sinon les fonds disparaîtraient)
        let contract = if tx.action == WnsAction::StorageLock {
            if self.domain_owners.get(&tx.domain_name) != Some(&tx.account_address) {
                return Err(reject("stockage : domaine d'un autre compte"));
            }
            let parts: Vec<&str> = tx.record_data.splitn(3, '|').collect();
            let challenges = (parts.len() == 3)
                .then(|| serde_json::from_str::<Vec<StorageChallenge>>(parts[2]).ok())
                .flatten()
                .filter(|c| !c.is_empty())
                .ok_or_else(|| reject("stockage : défis illisibles"))?;
            Some(StorageContract {
                host_pubkey: parts[0].to_string(),
                total_chunks: parts[1].parse().unwrap_or(1),
                locked_flames: tx.amount,
                payment_per_proof: tx.amount / challenges.len() as u64,
                last_proof_block: self.block_index,
                pending_challenges: challenges,
            })
        } else { None };

        // 4. Effets
        let acc = self.accounts.get_mut(&tx.account_address).expect("compte vérifié ci-dessus");
        acc.balance -= total_cost;
        acc.nonce += 1;
        acc.authorized_wots_key = tx.next_pubkey.clone(); // KEY ROLLING

        match tx.action {
            WnsAction::Register => {
                if !self.domains.contains_key(&tx.domain_name) {
                    self.domains.insert(tx.domain_name.clone(), tx.record_data.clone());
                    self.domain_owners.insert(tx.domain_name.clone(), tx.account_address.clone());
                    println!("✅ [WNS] '{}' adjugé pour {} Flames !", tx.domain_name, tx.fee);
                }
            }
            WnsAction::Update => {
                // Le propriétaire reste le COMPTE (pas la clé WOTS suivante, sinon il perdrait son domaine)
                if self.domain_owners.get(&tx.domain_name) == Some(&tx.account_address) {
                    self.domains.insert(tx.domain_name.clone(), tx.record_data.clone());
                }
            }
            WnsAction::Transfer => {
                if self.domain_owners.get(&tx.domain_name) == Some(&tx.account_address) {
                    self.domain_owners.insert(tx.domain_name.clone(), tx.record_data.clone());
                }
            }
            WnsAction::Withdraw => {
                println!("🌉 [WNS] Retrait de {} Flames vers le L1 ({})", tx.amount, tx.record_data);
                return Ok(Some((tx.record_data.clone(), tx.amount)));
            }
            WnsAction::StorageLock => {
                if let Some(c) = contract {
                    println!("🔒 Contrat de Stockage verrouillé pour '{}' ({} Flames, {} défis)", tx.domain_name, c.locked_flames, c.pending_challenges.len());
                    self.storage_contracts.insert(tx.domain_name.clone(), c);
                }
            }
            WnsAction::StorageProof => {
                let block_index = self.block_index; // déterministe (séquenceur et pairs)
                if let Some(contract) = self.storage_contracts.get_mut(&tx.domain_name) {
                    if contract.host_pubkey == tx.account_address && !contract.pending_challenges.is_empty() {
                        let idx = (block_index as usize) % contract.pending_challenges.len();
                        let answer = hex::encode(Sha512::digest(tx.record_data.as_bytes()));
                        if answer == contract.pending_challenges[idx].expected_answer_hash {
                            let payout = contract.payment_per_proof.min(contract.locked_flames);
                            contract.locked_flames -= payout;
                            contract.last_proof_block = block_index;
                            contract.pending_challenges.remove(idx);
                            let host = contract.host_pubkey.clone();
                            self.accounts.entry(host.clone())
                                .or_insert(L2Account { balance: 0, nonce: 0, authorized_wots_key: host })
                                .balance += payout;
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    /// Séquence la mempool en UN bloc : (TX incluses, frais, retraits [(adresse L1, montant)]).
    /// Les frais QUITTENT le L2 (versés au séquenceur sur le L1 par l'ancrage) : le total 1:1 reste exact.
    pub fn process_mempool(&mut self) -> (Vec<L2Transaction>, u64, Vec<(String, u64)>) {
        self.expire_contracts();
        let mut txs = std::mem::take(&mut self.mempool);
        txs.sort_by(|a, b| b.fee.cmp(&a.fee));

        let (mut valid, mut fees, mut withdrawals, mut kept) = (Vec::new(), 0u64, Vec::new(), Vec::new());
        for tx in txs {
            if tx.action == WnsAction::Withdraw && withdrawals.len() >= MAX_UNPEG_PER_BLOCK {
                kept.push(tx); // bloc plein de retraits : au prochain
                continue;
            }
            match self.apply_tx(&tx) {
                Ok(w) => {
                    fees += tx.fee;
                    if let Some(w) = w { withdrawals.push(w); }
                    valid.push(tx);
                }
                Err(TxVerdict::Keep) => kept.push(tx),
                Err(TxVerdict::Reject(e)) => println!("❌ [WNS] TX rejetée : {}", e),
            }
        }
        self.mempool = kept;
        (valid, fees, withdrawals)
    }

    pub fn compute_state_root(&self) -> String {
        let mut hasher = Sha512::new();

        let mut keys: Vec<&String> = self.accounts.keys().collect();
        keys.sort();
        for key in keys {
            let acc = &self.accounts[key];
            hasher.update(key.as_bytes());
            hasher.update(acc.balance.to_be_bytes());
            hasher.update(acc.nonce.to_be_bytes());
            hasher.update(acc.authorized_wots_key.as_bytes());
        }

        let mut dom_keys: Vec<&String> = self.domains.keys().collect();
        dom_keys.sort();
        for k in dom_keys {
            hasher.update(k.as_bytes());
            hasher.update(self.domains[k].as_bytes());
            hasher.update(self.domain_owners.get(k).map(|s| s.as_str()).unwrap_or("").as_bytes());
        }

        let mut contracts: Vec<&String> = self.storage_contracts.keys().collect();
        contracts.sort();
        for k in contracts {
            let c = &self.storage_contracts[k];
            hasher.update(k.as_bytes());
            hasher.update(c.host_pubkey.as_bytes());
            hasher.update(c.locked_flames.to_be_bytes());
        }

        hasher.update(self.l2_total().to_be_bytes()); // le total 1:1 est engagé dans la racine
        hex::encode(hasher.finalize())
    }

    /// Bloc reçu d'un séquenceur : rejoué avec la MÊME règle (apply_tx), puis comparé au maillon signé
    pub fn apply_incoming_wns_block(&mut self, block: &WnsBlock) -> Result<(), String> {
        if block.index != self.block_index + 1 || block.prev_root != self.last_root {
            return Err("Bloc hors séquence (index ou racine précédente)".to_string());
        }

        let mut next = self.clone();
        next.expire_contracts();
        let (mut fees, mut withdrawals) = (0u64, Vec::new());
        for tx in &block.transactions {
            match next.apply_tx(tx) {
                Ok(w) => {
                    fees += tx.fee;
                    if let Some(w) = w { withdrawals.push(w); }
                }
                Err(TxVerdict::Keep) => return Err("TX pas encore applicable incluse dans le bloc".to_string()),
                Err(TxVerdict::Reject(e)) => return Err(format!("TX invalide incluse : {}", e)),
            }
        }

        // Retraits signés = ceux des TX, puis les frais vers l'adresse <kyber>.<R> du séquenceur (adresse vide = marqueur)
        if fees > 0 { withdrawals.push((String::new(), fees)); }
        let fee_suffix = format!(".{}", block.sequencer_pubkey);
        let withdrawals_ok = block.withdrawals.len() == withdrawals.len()
            && block.withdrawals.iter().zip(&withdrawals).enumerate().all(|(i, (o, (addr, amount)))| {
                let addr_ok = if addr.is_empty() { o.stealth_address.ends_with(&fee_suffix) } else { &o.stealth_address == addr };
                addr_ok && o.amount == *amount
                    && o.kyber_capsule == wattcoin_core::transaction::unpeg_capsule(&block.l2_name, block.index, i)
            });
        if !withdrawals_ok {
            return Err("Les retraits signés ne correspondent pas aux TX du bloc".to_string());
        }
        if next.l2_total() != block.l2_total || next.compute_state_root() != block.state_root {
            return Err("State Root ou total 1:1 frauduleux ! Le séquenceur a triché sur les soldes.".to_string());
        }

        next.block_index = block.index;
        next.last_root = block.state_root.clone();
        next.mempool.retain(|m| !block.transactions.iter().any(|b| b.signature == m.signature));
        *self = next;
        self.save_to_disk();
        Ok(())
    }
}

pub type SharedL2State = Arc<Mutex<L2State>>;

#[cfg(test)]
mod tests {
    use super::*;
    use wattcoin_core::transaction::{unpeg_capsule, TransactionOutput, L2_GENESIS_ROOT};

    const SEED: [u8; 32] = [3u8; 32];

    // TX L2 du compte "alice", signée par sa clé WOTS n°i, qui roule vers la n°i+1
    fn signed(i: u64, nonce: u64, action: WnsAction, domain: &str, record: &str, amount: u64, fee: u64) -> L2Transaction {
        let mut tx = L2Transaction {
            account_address: "alice".into(),
            sender_pubkey: hex::encode(wots::Wots::public_key(&SEED, i)),
            next_pubkey: hex::encode(wots::Wots::public_key(&SEED, i + 1)),
            nonce, action,
            domain_name: domain.into(), record_data: record.into(),
            amount, fee, signature: String::new(),
        };
        let sig = wots::OneTimeKey::generate(&SEED, i).sign(&tx.hash_data());
        tx.signature = serde_json::to_string(&sig).unwrap();
        tx
    }

    // Un WNS neuf où alice a déposé `amount` Flames par le pont
    fn alice_with(amount: u64) -> L2State {
        let mut s = L2State {
            accounts: HashMap::new(), domains: HashMap::new(), domain_owners: HashMap::new(),
            storage_contracts: HashMap::new(), mempool: Vec::new(),
            block_index: 0, last_l1_block: 0, last_root: L2_GENESIS_ROOT.to_string(),
            db: Some(sled::Config::new().temporary(true).open().unwrap()),
        };
        s.accounts.insert("alice".into(), L2Account { balance: amount, nonce: 0,
            authorized_wots_key: hex::encode(wots::Wots::public_key(&SEED, 0)) });
        s
    }

    fn dest() -> String { format!("kyberB64.{}", hex::encode([4u8; 32])) }

    #[test]
    fn depot_achat_retrait_compteur_1_1() {
        let mut s = alice_with(10_000);
        s.mempool.push(signed(0, 1, WnsAction::Register, "watty.watt", "rec", 0, 2000));
        s.mempool.push(signed(1, 2, WnsAction::Withdraw, "", &dest(), 5000, 1500));

        let (txs, fees, w) = s.process_mempool();
        assert_eq!(txs.len(), 2);
        assert_eq!(fees, 3500);
        assert_eq!(w, vec![(dest(), 5000)]);
        assert_eq!(s.accounts["alice"].balance, 1500);
        assert_eq!(s.domain_owners["watty.watt"], "alice");
        // 1:1 : dépôts − retraits − frais (les frais sortent aussi, vers le séquenceur sur le L1)
        assert_eq!(s.l2_total(), 10_000 - 5000 - 3500);
    }

    #[test]
    fn aucun_flame_ne_disparait() {
        let cases: Vec<(L2Transaction, bool)> = vec![ // (tx, reste en mempool ?)
            (signed(0, 1, WnsAction::Withdraw, "", &hex::encode([4u8; 32]), 500, 1500), false), // R nu : indépensable sur le L1
            (signed(0, 1, WnsAction::Register, "a.watt", "x", 7, 2000), false),                  // montant qui serait perdu
            (signed(0, 1, WnsAction::Withdraw, "", &dest(), 50_000, 1500), true),                // solde insuffisant : attend
            (signed(0, 2, WnsAction::Register, "a.watt", "x", 0, 2000), true),                   // nonce futur : attend
            ({ let mut t = signed(0, 1, WnsAction::Register, "a.watt", "x", 0, 2000); t.fee = 9000; t }, false), // modifiée après signature
        ];
        for (tx, kept) in cases {
            let mut s = alice_with(10_000);
            s.mempool.push(tx);
            let (txs, fees, w) = s.process_mempool();
            assert!(txs.is_empty() && fees == 0 && w.is_empty());
            assert_eq!(s.l2_total(), 10_000);
            assert_eq!(s.mempool.len(), if kept { 1 } else { 0 });
        }
    }

    #[test]
    fn un_pair_rejoue_le_bloc_et_refuse_la_triche() {
        let mut seq = alice_with(10_000);
        let peer = alice_with(10_000);
        seq.mempool.push(signed(0, 1, WnsAction::Register, "watty.watt", "rec", 0, 2000));
        seq.mempool.push(signed(1, 2, WnsAction::Withdraw, "", &dest(), 5000, 1500));
        let (txs, fees, mut unpeg) = seq.process_mempool();

        let r = "ab".repeat(32); // racine R du séquenceur
        unpeg.push((format!("kyberWNS.{}", r), fees));
        let withdrawals = unpeg.iter().enumerate().map(|(i, (a, m))| TransactionOutput {
            stealth_address: a.clone(), kyber_capsule: unpeg_capsule("WNS", 1, i), aes_vault: m.to_string(), amount: *m,
        }).collect();
        let block = WnsBlock {
            index: 1, l2_name: "WNS".into(), l1_parent_hash: String::new(), prev_root: L2_GENESIS_ROOT.into(),
            state_root: seq.compute_state_root(), l1_height: 0, l2_total: seq.l2_total(),
            withdrawals, sequencer_pubkey: r, transactions: txs, signature: String::new(),
        };

        let mut ok = peer.clone();
        assert!(ok.apply_incoming_wns_block(&block).is_ok());
        assert_eq!((ok.block_index, ok.last_root.clone()), (1, block.state_root.clone()));

        let mut b = block.clone(); b.withdrawals[0].stealth_address = format!("kyberB64.{}", hex::encode([5u8; 32])); // retrait détourné
        assert!(peer.clone().apply_incoming_wns_block(&b).is_err());
        let mut b = block.clone(); b.withdrawals[1].amount += 1; // le séquenceur se sert plus que ses frais
        assert!(peer.clone().apply_incoming_wns_block(&b).is_err());
        let mut b = block.clone(); b.l2_total += 1;               // total 1:1 gonflé
        assert!(peer.clone().apply_incoming_wns_block(&b).is_err());
        let mut b = block.clone(); b.prev_root = "autre".into();  // mauvais maillon
        assert!(peer.clone().apply_incoming_wns_block(&b).is_err());
    }
}