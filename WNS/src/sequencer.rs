// wattcoin_name_service/src/sequencer.rs
// =====================================================================
// IDENTITÉ HYPERTREE DU SÉQUENCEUR WNS
// - Racine R (stakée sur le L1 comme "sequencer_pubkey")
// - UNE feuille NEUVE par signature (ancrage, preuve de carburant)
// Le L1 vérifie la signature contre R et BRÛLE la feuille : il reste le seul juge.
// =====================================================================
use wots::{HyperError, HyperKeys, HyperSignature, LEAVES_PER_SUBADDRESS};

pub struct SequencerSigner {
    keys: HyperKeys,
    journal: sled::Tree, // next_leaf + sig/<pk_hex> (feuilles déjà utilisées)
}

impl SequencerSigner {
    /// Construit l'arbre haut (64 clés) de la sous-adresse 0 de la graine du séquenceur
    pub fn new(seed: &[u8; 32], journal: sled::Tree) -> Self {
        Self { keys: HyperKeys::new(seed, 0), journal }
    }

    /// R en hex : la clé à staker sur le L1 (et le compte des frais sur le L2)
    pub fn root_hex(&self) -> String {
        hex::encode(self.keys.root())
    }

    /// Signe `msg` avec une feuille JAMAIS utilisée. La feuille est réservée et écrite
    /// sur disque AVANT que la signature existe : un crash ou un redémarrage ne peut
    /// jamais faire signer deux messages par la même feuille.
    pub fn sign(&mut self, msg: &[u8; 64]) -> Result<HyperSignature, String> {
        let mut leaf = self.journal.get("next_leaf").map_err(|e| e.to_string())?
            .and_then(|v| <[u8; 8]>::try_from(v.as_ref()).ok())
            .map(u64::from_be_bytes)
            .unwrap_or(0);
        let journal = &self.journal;

        loop {
            if leaf >= LEAVES_PER_SUBADDRESS {
                return Err("❌ Séquenceur : 65 536 signatures épuisées, nouvelle identité (et nouveau stake) requise.".into());
            }
            let mut db_err: Option<String> = None;
            let res = self.keys.sign(leaf, msg, |_| false, |pk: &str| {
                match journal.compare_and_swap(format!("sig/{}", pk).as_bytes(), None as Option<&[u8]>, Some(&[1u8][..])) {
                    Ok(Ok(())) => match journal.flush() {
                        Ok(_) => true,
                        Err(e) => { db_err = Some(e.to_string()); false }
                    },
                    Ok(Err(_)) => false, // feuille déjà utilisée
                    Err(e) => { db_err = Some(e.to_string()); false }
                }
            });
            if let Some(e) = db_err { return Err(format!("❌ Journal du séquenceur inaccessible : {}", e)); }

            leaf += 1;
            journal.insert("next_leaf", &leaf.to_be_bytes()[..]).map_err(|e| e.to_string())?;
            match res {
                Ok(sig) => {
                    journal.flush().map_err(|e| e.to_string())?;
                    return Ok(sig);
                }
                Err(HyperError::KeyAlreadySignedLocally(_)) | Err(HyperError::KeyUsedOnChainOrMempool(_)) => continue,
                Err(e) => return Err(format!("❌ Signature du séquenceur impossible : {:?}", e)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jamais_deux_fois_la_meme_feuille() {
        let db = sled::Config::new().temporary(true).open().unwrap();
        let tree = db.open_tree("sequencer_leaves").unwrap();
        let mut s = SequencerSigner::new(&[5u8; 32], tree.clone());
        let root = s.keys.root();

        let m1 = wattcoin_core::transaction::anchor_message("WNS", "etat_1");
        let m2 = wattcoin_core::transaction::anchor_message("WNS", "etat_2");
        let a = s.sign(&m1).unwrap();
        let b = s.sign(&m2).unwrap();
        assert!(a.verify(&m1, &root) && b.verify(&m2, &root));
        assert_ne!(a.one_time_key_hex(), b.one_time_key_hex());

        // Redémarrage avec compteur perdu : le journal saute quand même les feuilles déjà signées
        tree.remove("next_leaf").unwrap();
        let mut s2 = SequencerSigner::new(&[5u8; 32], tree);
        let c = s2.sign(&m1).unwrap();
        assert!(c.one_time_key_hex() != a.one_time_key_hex() && c.one_time_key_hex() != b.one_time_key_hex());
    }
}
