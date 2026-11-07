// src/owner.rs
// =====================================================================
// PROPRIÉTÉ POST-QUANTIQUE — CÔTÉ WALLET
// Le wallet PRÉPARE (sous-adresses R_i, engagements, preuves signées).
// Il ne décide de RIEN : le nœud (consensus) revérifie tout.
// =====================================================================
use sha2::{Digest, Sha256};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use wattcoin_core::transaction::{can_spend, decode32, owner_commit, OwnerProof, Transaction, TransactionOutput, MAX_OWNERS};


/// Restauration : nombre d'index consécutifs sans paiement avant d'arrêter la recherche.
/// Large : chaque « Copier » / « QR » / retrait WNS consomme un index, payé ou non.
/// (Coût unique : chaque R_i calculée est mémorisée dans le carnet.)
pub const ADDRESS_GAP: u64 = 200;


fn sha256(parts: &[&[u8]]) -> [u8; 32] {
    let mut h = Sha256::new();
    for p in parts { h.update(p); }
    let mut out = [0u8; 32];
    out.copy_from_slice(&h.finalize());
    out
}

/// Graine de l'hypertree, séparée de Kyber (master_seed[0..32]) et de Bitcoin (BIP32)
pub fn owner_master(master_seed_hex: &str) -> Result<[u8; 32], String> {
    let seed = hex::decode(master_seed_hex).map_err(|_| "Graine maître illisible.".to_string())?;
    Ok(sha256(&[b"wattcoin/owner/master", &seed]))
}

/// Adresse de destination "<kyber_b64>.<R_hex>" → (clé publique Kyber, R)
pub fn parse_recipient(addr: &str) -> Result<(Vec<u8>, [u8; 32]), String> {
    let clean = addr.trim().trim_start_matches("wattcoin:");
    let (kyber_b64, root_hex) = clean.rsplit_once('.')
        .ok_or("❌ Adresse incomplète : copie-la avec le bouton « Recevoir » du destinataire (format <clé>.<racine>).")?;
    let kyber = URL_SAFE_NO_PAD.decode(kyber_b64).map_err(|_| "❌ Clé Kyber du destinataire illisible.".to_string())?;
    let root = decode32(root_hex).ok_or("❌ Racine de propriété du destinataire invalide.")?;
    Ok((kyber, root))
}

/// stealth_address d'un output caché : préfixe + H("wattcoin/owner" || R || otp)
pub fn stealth_address(prefix: &str, root: &[u8; 32], otp: &[u8; 32]) -> String {
    format!("{}{}", prefix, owner_commit(root, otp))
}

/// Texte chiffré pour le destinataire : "montant|otp|R"
pub fn payload(amount: u64, otp: &[u8; 32], root: &[u8; 32]) -> String {
    format!("{}|{}|{}", amount, hex::encode(otp), hex::encode(root))
}

/// R annoncée par un payload "montant|otp|R", SEULEMENT si elle ouvre l'engagement on-chain.
/// (Ne dit pas si R est à moi : c'est le rôle de OwnerBook::sub_of.)
pub fn hidden_root(out: &TransactionOutput, payload: &str) -> Option<([u8; 32], String)> {
    let addr = &out.stealth_address;
    if !(addr.starts_with("pq_watt_") || (addr.starts_with("L2_WATT_") && !addr.starts_with("L2_WATT_PUB_"))) { return None; }
    let mut parts = payload.split('|');
    let amount: u64 = parts.next()?.parse().ok()?;
    let otp_hex = parts.next()?;
    let root = decode32(parts.next()?)?;
    if amount != out.amount { return None; } // seul le montant on-chain compte
    can_spend(out, &hex::encode(root), otp_hex).then(|| (root, otp_hex.to_string()))
}

/// Un output qui m'appartient : ce qu'il faut pour le dépenser
#[derive(Clone, Debug)]
pub struct Owned {
    pub sub: u64,         // sous-adresse propriétaire
    pub otp_hex: String,  // vide pour un output public
}

/// Carnet des sous-adresses (arbre Sled "owner" de la base du wallet)
///   sub/<i>      → R_i || tag   (tag = authentifie l'entrée avec la graine)
///   root/<R_hex> → i
///   next_sub     → prochain index du bouton « Recevoir » (≥ 1 ; 0 = minage)
///   leaf/<i>     → prochaine feuille à essayer pour la sous-adresse i
///   sig/<pk_hex> → journal : feuille déjà utilisée pour signer
pub struct OwnerBook {
    master: [u8; 32],
    tree: sled::Tree,
}

impl OwnerBook {
    pub fn open(master_seed_hex: &str) -> Result<Self, String> {
        let tree = crate::wallet_db()?.open_tree("owner").map_err(|e| e.to_string())?;
        Ok(Self { master: owner_master(master_seed_hex)?, tree })
    }

    fn get_u64(&self, key: &str) -> Option<u64> {
        let v = self.tree.get(key).ok()??;
        Some(u64::from_be_bytes(v.as_ref().try_into().ok()?))
    }

    fn set_u64(&self, key: &str, v: u64) -> Result<(), String> {
        self.tree.insert(key, &v.to_be_bytes()[..]).map(|_| ()).map_err(|e| e.to_string())
    }

    fn tag(&self, sub: u64, root: &[u8; 32]) -> [u8; 32] {
        sha256(&[b"wattcoin/owner/tag", &self.master, &sub.to_be_bytes(), root])
    }

    /// R_i : calculée une fois (64 clés WOTS) puis mémorisée.
    /// Une entrée modifiée sur le disque est ignorée et recalculée.
    pub fn root(&self, sub: u64) -> Result<[u8; 32], String> {
        let key = format!("sub/{}", sub);
        if let Some(v) = self.tree.get(&key).map_err(|e| e.to_string())? {
            if v.len() == 64 {
                let root: [u8; 32] = v[..32].try_into().unwrap();
                if v[32..] == self.tag(sub, &root) { return Ok(root); }
            }
        }
        let root = wots::HyperKeys::new(&self.master, sub).root();
        let mut entry = root.to_vec();
        entry.extend_from_slice(&self.tag(sub, &root));
        self.tree.insert(key.as_bytes(), entry).map_err(|e| e.to_string())?;
        self.set_u64(&format!("root/{}", hex::encode(root)), sub)?;
        self.tree.flush().map_err(|e| e.to_string())?;
        Ok(root)
    }

    /// Adresse de minage (stable) : "<kyber>.<R_0>"
    pub fn mining_address(&self, kyber_b64: &str) -> Result<String, String> {
        Ok(format!("{}.{}", kyber_b64, hex::encode(self.root(0)?)))
    }

    /// Bouton « Recevoir » : une sous-adresse NEUVE à chaque appel, "<kyber>.<R_i>" (i ≥ 1)
    pub fn new_receive_address(&self, kyber_b64: &str) -> Result<String, String> {
        // Réservation atomique : deux clics simultanés n'obtiennent jamais le même index
        let old = self.tree.fetch_and_update("next_sub", |old| {
            let n = old.and_then(|v| <[u8; 8]>::try_from(v).ok()).map(u64::from_be_bytes).unwrap_or(1);
            Some((n + 1).to_be_bytes().to_vec())
        }).map_err(|e| e.to_string())?;
        let sub = old.and_then(|v| <[u8; 8]>::try_from(v.as_ref()).ok()).map(u64::from_be_bytes).unwrap_or(1);
        Ok(format!("{}.{}", kyber_b64, hex::encode(self.root(sub)?)))
    }

    /// Sous-adresse qui possède R (None = pas à moi)
    pub fn sub_of(&self, root: &[u8; 32]) -> Option<u64> {
        let sub = self.get_u64(&format!("root/{}", hex::encode(root)))?;
        (self.root(sub).ok()? == *root).then_some(sub)
    }

    /// Output public (COINBASE_, JACKPOT_, L2_WATT_PUB_, <kyber>.<R>) : à moi ?
    pub fn owns_public(&self, out: &TransactionOutput) -> Option<Owned> {
        let addr = &out.stealth_address;
        if addr.starts_with("pq_watt_") || (addr.starts_with("L2_WATT_") && !addr.starts_with("L2_WATT_PUB_")) { return None; }
        let root = decode32(addr.rsplit(|c: char| c == '.' || c == '_').next()?)?;
        let sub = self.sub_of(&root)?;
        // Dernier mot à la règle du consensus, à l'identique
        can_spend(out, &hex::encode(root), "").then(|| Owned { sub, otp_hex: String::new() })
    }

    /// Output caché (pq_watt_ / L2_WATT_) : `payload` = texte déchiffré "montant|otp|R".
    /// Le payload n'est PAS cru : l'engagement on-chain est recalculé.
    pub fn owns_hidden(&self, out: &TransactionOutput, payload: &str) -> Option<Owned> {
        let (root, otp_hex) = hidden_root(out, payload)?;
        Some(Owned { sub: self.sub_of(&root)?, otp_hex })
    }

    /// Restauration (48 mots) : le carnet est vide. On redérive les R_i à partir du prochain
    /// index libre jusqu'à retrouver celles de `wanted`, ou `gap` index consécutifs sans paiement.
    pub fn discover(&self, wanted: &std::collections::HashSet<[u8; 32]>, gap: u64) -> Result<(), String> {
        // R_0 (minage, monnaie rendue, alias WNS) n'est jamais dans la plage next_sub (≥ 1) : on la mémorise d'abord
        self.root(0)?;
        let mut left = wanted.iter().filter(|r| self.sub_of(r).is_none()).count();
        let mut sub = self.get_u64("next_sub").unwrap_or(1);
        let mut misses = 0;
        while left > 0 && misses < gap {
            let root = self.root(sub)?; // mémorisée : la prochaine recherche est instantanée
            sub += 1;
            if wanted.contains(&root) {
                left -= 1;
                misses = 0;
                self.set_u64("next_sub", sub)?; // « Recevoir » ne redonnera pas un index déjà payé
            } else {
                misses += 1;
            }
        }
        self.tree.flush().map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Journal local : la feuille est réservée et ÉCRITE SUR DISQUE avant que la signature existe
    /// (anti double-clic / crash). Ce n'est pas une sécurité : le nœud brûle la feuille on-chain.
    fn reserve(&self, pk_hex: &str) -> Result<bool, String> {
        let key = format!("sig/{}", pk_hex);
        let won = self.tree
            .compare_and_swap(key.as_bytes(), None as Option<&[u8]>, Some(&[1u8][..]))
            .map_err(|e| e.to_string())?
            .is_ok();
        if won { self.tree.flush().map_err(|e| e.to_string())?; }
        Ok(won)
    }

    /// Rattache chaque input à sa sous-adresse, puis signe UNE preuve par sous-adresse.
    /// `subs[i]` = sous-adresse de tx.inputs[i]. À appeler quand la TX est COMPLÈTE (outputs, frais).
    /// `is_key_used(pk_hex)` : feuille déjà vue on-chain ou en attente (mempool locale).
    pub fn sign(&self, tx: &mut Transaction, subs: &[u64], mut is_key_used: impl FnMut(&str) -> bool) -> Result<(), String> {
        if tx.inputs.is_empty() || subs.len() != tx.inputs.len() {
            return Err("Erreur interne : sous-adresses ≠ inputs.".into());
        }

        // 1. Une preuve par sous-adresse distincte, dans l'ordre d'apparition
        let mut order: Vec<u64> = Vec::new();
        for (input, sub) in tx.inputs.iter_mut().zip(subs) {
            let j = match order.iter().position(|s| s == sub) {
                Some(j) => j,
                None => { order.push(*sub); order.len() - 1 }
            };
            input.owner = j as u8;
        }
        if order.len() > MAX_OWNERS {
            return Err(format!("❌ Trop de sous-adresses dans un seul envoi ({} max) : fais plusieurs envois.", MAX_OWNERS));
        }
        tx.wots_signature = None;
        tx.owners = order.iter()
            .map(|s| Ok(OwnerProof { root: hex::encode(self.root(*s)?), signature: None }))
            .collect::<Result<Vec<_>, String>>()?;

        // 2. Le message couvre TOUT : inputs (et leur index de preuve), outputs, frais, racines
        let msg = tx.hash_data();
        for (j, sub) in order.iter().enumerate() {
            let mut keys = wots::HyperKeys::new(&self.master, *sub);
            let leaf_key = format!("leaf/{}", sub);
            let mut leaf = self.get_u64(&leaf_key).unwrap_or(0);
            let sig = loop {
                if leaf >= wots::LEAVES_PER_SUBADDRESS {
                    return Err(format!("❌ Sous-adresse {} épuisée (65 536 signatures).", sub));
                }
                let mut db_err: Option<String> = None;
                let res = keys.sign(leaf, &msg, &mut is_key_used, |pk: &str| match self.reserve(pk) {
                    Ok(won) => won,
                    Err(e) => { db_err = Some(e); false }
                });
                if let Some(e) = db_err { return Err(format!("❌ Journal de signature inaccessible : {}", e)); }
                match res {
                    Ok(sig) => break sig,
                    // Feuille déjà brûlée (on-chain, mempool ou journal) : on passe à la suivante
                    Err(wots::HyperError::KeyUsedOnChainOrMempool(_)) | Err(wots::HyperError::KeyAlreadySignedLocally(_)) => leaf += 1,
                    Err(e) => return Err(format!("❌ Signature impossible : {:?}", e)),
                }
            };
            self.set_u64(&leaf_key, leaf + 1)?;
            tx.owners[j].signature = Some(sig);
        }
        self.tree.flush().map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wattcoin_core::transaction::{TransactionInput, TransactionType};

    const KYBER: &str = "S1lCRVI"; // fausse clé Kyber (base64 de "KYBER")

    fn book() -> OwnerBook {
        let db = sled::Config::new().temporary(true).open().unwrap();
        OwnerBook { master: owner_master(&hex::encode([7u8; 64])).unwrap(), tree: db.open_tree("owner").unwrap() }
    }

    fn out(addr: String, amount: u64) -> TransactionOutput {
        TransactionOutput { stealth_address: addr, kyber_capsule: String::new(), aes_vault: String::new(), amount }
    }

    #[test]
    fn reception_cachee_et_publique() {
        let b = book();
        let addr = b.new_receive_address(KYBER).unwrap();
        assert_ne!(addr, b.new_receive_address(KYBER).unwrap());          // jetable : jamais deux fois la même
        let (_, r) = parse_recipient(&addr).unwrap();
        assert_eq!(b.sub_of(&r), Some(1));

        let otp = [3u8; 32];
        let o = out(stealth_address("pq_watt_", &r, &otp), 50);
        assert_eq!(b.owns_hidden(&o, &payload(50, &otp, &r)).map(|w| w.sub), Some(1));
        assert!(b.owns_hidden(&o, &payload(50, &[4u8; 32], &r)).is_none());   // mauvais otp
        assert!(b.owns_hidden(&o, &payload(51, &otp, &r)).is_none());         // montant menteur
        assert!(b.owns_hidden(&o, &payload(50, &otp, &[9u8; 32])).is_none()); // racine étrangère

        let r0 = hex::encode(b.root(0).unwrap());
        assert_eq!(b.mining_address(KYBER).unwrap(), format!("{}.{}", KYBER, r0));
        assert_eq!(b.owns_public(&out(format!("COINBASE_{}.{}", KYBER, r0), 1)).map(|w| w.sub), Some(0));
        assert_eq!(b.owns_public(&out(format!("L2_WATT_PUB_{}", r0), 1)).map(|w| w.sub), Some(0));
        assert!(b.owns_public(&out(format!("COINBASE_{}", hex::encode([9u8; 32])), 1)).is_none());
    }

    #[test]
    fn restauration_retrouve_les_sous_adresses() {
        let old = book();
        let r5 = old.root(5).unwrap();                                       // payée avant la restauration
        let fresh = book();                                                  // mêmes 48 mots, carnet vide
        assert_eq!(fresh.sub_of(&r5), None);
        fresh.discover(&[r5].into_iter().collect(), ADDRESS_GAP).unwrap();
        assert_eq!(fresh.sub_of(&r5), Some(5));
        assert_eq!(fresh.get_u64("next_sub"), Some(6));                      // « Recevoir » repart après
        let r0 = old.root(0).unwrap();                                       // monnaie / alias : la R_0
        let fresh0 = book();
        fresh0.discover(&[r0].into_iter().collect(), ADDRESS_GAP).unwrap();
        assert_eq!(fresh0.sub_of(&r0), Some(0));                             // retrouvée malgré next_sub ≥ 1
        let junk = [9u8; 32];                                                // R inconnue : s'arrête au gap
        fresh.discover(&[junk].into_iter().collect(), 3).unwrap();
        assert_eq!(fresh.sub_of(&junk), None);
    }

    #[test]
    fn signature_multi_sous_adresses() {
        let b = book();
        let mut tx = Transaction { tx_type: TransactionType::Standard, inputs: vec![], outputs: vec![out("pq_watt_x".into(), 3)],
            fee: 0, wots_signature: None, public_key: String::new(), owners: vec![] };
        for i in 0..3 {
            tx.inputs.push(TransactionInput { utxo_id: format!("u{}", i), amount: 1, source_height: 0, owner_otp: String::new(), owner: 0 });
        }

        b.sign(&mut tx, &[0, 2, 0], |_| false).unwrap();
        assert!(tx.is_valid());                                              // le juge : la règle du nœud
        assert_eq!(tx.owners.len(), 2);
        assert_eq!(tx.inputs.iter().map(|i| i.owner).collect::<Vec<_>>(), vec![0, 1, 0]);
        let first = tx.one_time_keys();

        b.sign(&mut tx, &[0, 2, 0], |_| false).unwrap();                     // 2e envoi : feuilles neuves
        assert!(tx.is_valid());
        let second = tx.one_time_keys();
        assert!(second.iter().all(|k| !first.contains(k)));

        b.set_u64("leaf/0", 0).unwrap();                                     // compteur perdu (crash…)
        b.sign(&mut tx, &[0, 0, 0], |_| false).unwrap();                     // le journal saute quand même 0 et 1
        assert!(tx.is_valid());
        let third = &tx.one_time_keys()[0];
        assert!(!first.contains(third) && !second.contains(third));
    }
}
