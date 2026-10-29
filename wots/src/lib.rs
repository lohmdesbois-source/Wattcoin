// wots.rs
// cargo test --release -- --nocapture

use hkdf::Hkdf;
use sha2::{Sha256, Sha512, Digest}; // Sha256 : chaînes WOTS + Merkle. Sha512 : messages signés
use serde::{Serialize, Deserialize};

pub const WOTS_CHAINS: usize = 64 + 2; // 64 octets pour le hash (SHA-512) + 2 pour le checksum
pub const WOTS_W: usize = 256;         // Paramètre de Winternitz

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WotsSignature {
    pub index: u64,
    pub public_key: Vec<u8>,         // Fera 32 octets (hash compressé)
    pub signature_bytes: Vec<u8>,    // Fera 66 * 32 = 2112 octets
}

#[derive(Debug, PartialEq)]
pub enum WotsError {
    KeyUsedOnChainOrMempool(u64),
    KeyAlreadySignedLocally(u64),
}

pub struct Wots;

impl Wots {
    // Génération déterministe via HKDF (Strict & Immuable)
    pub(crate) fn generate_keypair(master_seed: &[u8], index: u64) -> (Vec<[u8; 32]>, Vec<u8>) {
        // Dérivation d'une sous-graine unique pour cet index
        let hk = Hkdf::<Sha256>::new(None, master_seed);
        let mut sub_seed = [0u8; 32];
        hk.expand(&index.to_be_bytes(), &mut sub_seed).expect("HKDF expand failed");

        // Génération de la matrice de clés secrètes (WOTS_CHAINS * 32 octets)
        let mut secret_key = Vec::with_capacity(WOTS_CHAINS);
        let prf_hk = Hkdf::<Sha256>::new(None, &sub_seed);
        
        let mut pk_hasher = Sha256::new();

        for i in 0..WOTS_CHAINS {
            let mut sk_chunk = [0u8; 32];
            // On utilise l'index de la chaîne comme contexte pour générer le chunk
            prf_hk.expand(&(i as u32).to_be_bytes(), &mut sk_chunk).expect("HKDF expand failed");
            secret_key.push(sk_chunk);

            let mut pk_chunk = sk_chunk;
            for _ in 0..(WOTS_W - 1) {
                let mut h = Sha256::new();
                h.update(&pk_chunk);
                pk_chunk = h.finalize().into();
            }
            pk_hasher.update(&pk_chunk);
        }

        let compressed_pk: [u8; 32] = pk_hasher.finalize().into();

        (secret_key, compressed_pk.to_vec()) 
    }
	
    // Clé publique seule (pour générer les adresses du wallet). La clé secrète n'est jamais exposée.
    pub fn public_key(master_seed: &[u8; 32], index: u64) -> [u8; 32] {
        let (_secret_key, pk) = Self::generate_keypair(master_seed, index);
        let mut out = [0u8; 32];
        out.copy_from_slice(&pk);
        out
    }

    // Signe avec la clé qui POSSÈDE le UTXO dépensé (owner_index), jamais "la première libre".
    // Double verrou :
    //  - is_key_used : la clé apparaît-elle on-chain OU dans le mempool ?
    //  - reserve_index : réservation ATOMIQUE + flush disque dans le journal local.
    //    Doit renvoyer false si l'index est déjà réservé OU si l'écriture échoue.
    pub fn sign_for_utxo<F, R>(
        master_seed: &[u8; 32],
        owner_index: u64,
        message_hash_512: &[u8; 64],
        mut is_key_used: F,
        mut reserve_index: R,
    ) -> Result<WotsSignature, WotsError>
    where
        F: FnMut(&str) -> bool,
        R: FnMut(u64) -> bool,
    {
        let (secret_key, public_key) = Self::generate_keypair(master_seed, owner_index);
        let pk_hex = hex::encode(&public_key);

        // 1. Chain + mempool
        if is_key_used(&pk_hex) {
            return Err(WotsError::KeyUsedOnChainOrMempool(owner_index));
        }

        // 2. Journal local : bloque le double clic, les retries et les crashs.
        //    La réservation est écrite AVANT que la signature n'existe.
        if !reserve_index(owner_index) {
            return Err(WotsError::KeyAlreadySignedLocally(owner_index));
        }

        Ok(Self::sign(&secret_key, owner_index, message_hash_512, &public_key))
    }

    // Fonction de signature interne (Privée ! Le dev ne peut appeler que sign_safe_onchain)
    fn sign(
        secret_key: &[[u8; 32]], 
        index: u64, 
        message_hash: &[u8; 64], // SHA-512
        public_key: &[u8]
    ) -> WotsSignature {
        assert_eq!(secret_key.len(), WOTS_CHAINS, "Clé secrète invalide !");
        let mut signature = Vec::with_capacity(WOTS_CHAINS * 32);
        let mut checksum = 0u32;

        for i in 0..64 { // Boucle sur 64 octets (SHA-512)
            let msg_byte = message_hash[i] as usize;
            checksum += (WOTS_W - 1 - msg_byte) as u32;
            
            let mut sig_chunk = secret_key[i];
            for _ in 0..msg_byte {
                let mut h = Sha256::new();
                h.update(&sig_chunk);
                sig_chunk = h.finalize().into();
            }
            signature.extend_from_slice(&sig_chunk);
        }

        let checksum_bytes = [(checksum >> 8) as u8, (checksum & 0xFF) as u8];
        for i in 0..2 {
            let msg_byte = checksum_bytes[i] as usize;
            let mut sig_chunk = secret_key[64 + i]; // Index décalé à 64
            for _ in 0..msg_byte {
                let mut h = Sha256::new();
                h.update(&sig_chunk);
                sig_chunk = h.finalize().into();
            }
            signature.extend_from_slice(&sig_chunk);
        }

        WotsSignature {
            index,
            public_key: public_key.to_vec(),
            signature_bytes: signature,
        }
    }

    /// Vérifie la signature WOTS+ à partir de la clé publique compressée
    pub fn verify(wots_sig: &WotsSignature, message_hash: &[u8; 64]) -> bool { // SHA-512 (64 octets)
        let pk_bytes = &wots_sig.public_key;

        // La clé publique DOIT faire 32 octets (compression), et la signature 2112 octets (66 * 32)
        if pk_bytes.len() != 32 || wots_sig.signature_bytes.len() != WOTS_CHAINS * 32 {
            return false;
        }

        let sig_bytes = &wots_sig.signature_bytes;
        let mut checksum = 0u32;
        
        // Hasher pour reconstruire l'empreinte compressée
        let mut pk_hasher = Sha256::new();

        for i in 0..64 { // Boucle sur 64 octets
            let msg_byte = message_hash[i] as usize;
            checksum += (WOTS_W - 1 - msg_byte) as u32;

            let mut current_chunk = [0u8; 32];
            current_chunk.copy_from_slice(&sig_bytes[i*32..(i+1)*32]);

            for _ in 0..(WOTS_W - 1 - msg_byte) {
                let mut h = Sha256::new();
                h.update(&current_chunk);
                current_chunk = h.finalize().into();
            }
            pk_hasher.update(&current_chunk);
        }

        let checksum_bytes = [(checksum >> 8) as u8, (checksum & 0xFF) as u8];
        for i in 0..2 {
            let msg_byte = checksum_bytes[i] as usize;
            let mut current_chunk = [0u8; 32];
            current_chunk.copy_from_slice(&sig_bytes[(64+i)*32..(65+i)*32]); // Index décalé à 64

            for _ in 0..(WOTS_W - 1 - msg_byte) {
                let mut h = Sha256::new();
                h.update(&current_chunk);
                current_chunk = h.finalize().into();
            }
            pk_hasher.update(&current_chunk);
        }

        // On finalise le hachage et on compare avec la clé de 32 octets fournie
        let derived_pk: [u8; 32] = pk_hasher.finalize().into();
        derived_pk.as_slice() == pk_bytes.as_slice()
    }
}

// =====================================================================
// CLÉ À USAGE UNIQUE : sign() CONSOMME la clé → une 2e signature ne compile pas.
// Pour les clés jetables (séquenceur L2, unstake). Volontairement sans Clone.
// =====================================================================
pub struct OneTimeKey {
    index: u64,
    secret_key: Vec<[u8; 32]>,
    public_key: Vec<u8>,
}

impl OneTimeKey {
    pub fn generate(master_seed: &[u8; 32], index: u64) -> Self {
        let (secret_key, public_key) = Wots::generate_keypair(master_seed, index);
        Self { index, secret_key, public_key }
    }

    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    pub fn sign(self, message_hash: &[u8; 64]) -> WotsSignature {
        Wots::sign(&self.secret_key, self.index, message_hash, &self.public_key)
    }
}

// =====================================================================
// MERKLE : feuilles et nœuds séparés par un préfixe (0x00 / 0x01)
// =====================================================================
pub mod merkle {
    use sha2::{Digest, Sha256};

    pub fn hash_leaf(pk: &[u8; 32]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update([0x00]);
        h.update(pk);
        h.finalize().into()
    }

    pub fn hash_node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update([0x01]);
        h.update(left);
        h.update(right);
        h.finalize().into()
    }

    /// levels[0] = feuilles hachées, levels.last() = [racine]. Nombre de clés : puissance de 2.
    pub fn build(leaf_pks: &[[u8; 32]]) -> Vec<Vec<[u8; 32]>> {
        assert!(leaf_pks.len().is_power_of_two(), "Merkle : nombre de feuilles non puissance de 2");
        let mut levels = vec![leaf_pks.iter().map(hash_leaf).collect::<Vec<_>>()];
        while levels.last().unwrap().len() > 1 {
            let next = levels.last().unwrap()
                .chunks(2)
                .map(|p| hash_node(&p[0], &p[1]))
                .collect();
            levels.push(next);
        }
        levels
    }

    pub fn root(levels: &[Vec<[u8; 32]>]) -> [u8; 32] {
        levels.last().unwrap()[0]
    }

    pub fn auth_path(levels: &[Vec<[u8; 32]>], mut index: usize) -> Vec<[u8; 32]> {
        let mut path = Vec::with_capacity(levels.len() - 1);
        for level in &levels[..levels.len() - 1] {
            path.push(level[index ^ 1]);
            index >>= 1;
        }
        path
    }

    pub fn root_from_path(leaf_pk: &[u8; 32], mut index: u64, path: &[[u8; 32]]) -> [u8; 32] {
        let mut node = hash_leaf(leaf_pk);
        for sibling in path {
            node = if index & 1 == 0 { hash_node(&node, sibling) } else { hash_node(sibling, &node) };
            index >>= 1;
        }
        node
    }
}

// =====================================================================
// HYPERTREE (XMSS^MT à 2 étages) : 64 arbres bas × 1024 feuilles
// Une sous-adresse = une racine R_i. Une dépense = une feuille neuve.
// =====================================================================
pub const TOP_HEIGHT: usize = 6;   // 64 clés hautes
pub const LOW_HEIGHT: usize = 10;  // 1024 clés par arbre bas
pub const LEAVES_PER_SUBADDRESS: u64 = 1u64 << (TOP_HEIGHT + LOW_HEIGHT); // 65 536

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HyperSignature {
    pub top_leaf: u32,
    pub top_sig: WotsSignature,      // signe la racine de l'arbre bas (toujours le même message)
    pub top_path: Vec<[u8; 32]>,     // TOP_HEIGHT hachages
    pub low_leaf: u32,
    pub low_sig: WotsSignature,      // signe la transaction
    pub low_path: Vec<[u8; 32]>,     // LOW_HEIGHT hachages
}

#[derive(Debug, PartialEq)]
pub enum HyperError {
    LeafOutOfRange(u64),
    KeyUsedOnChainOrMempool(u64),
    KeyAlreadySignedLocally(u64),
}

fn derive_seed(master: &[u8; 32], label: &[u8], a: u64, b: u64) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, master);
    let mut info = Vec::with_capacity(label.len() + 16);
    info.extend_from_slice(label);
    info.extend_from_slice(&a.to_be_bytes());
    info.extend_from_slice(&b.to_be_bytes());
    let mut out = [0u8; 32];
    hk.expand(&info, &mut out).expect("HKDF expand failed");
    out
}

fn top_seed(master: &[u8; 32], sub_index: u64) -> [u8; 32] {
    derive_seed(master, b"wattcoin/hypertree/top", sub_index, 0)
}

fn low_seed(master: &[u8; 32], sub_index: u64, top_leaf: u32) -> [u8; 32] {
    derive_seed(master, b"wattcoin/hypertree/low", sub_index, top_leaf as u64)
}

fn leaf_pks(tree_seed: &[u8; 32], count: usize) -> Vec<[u8; 32]> {
    (0..count as u64).map(|k| Wots::public_key(tree_seed, k)).collect()
}

fn top_message(low_root: &[u8; 32]) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(b"wattcoin/hypertree/low_root");
    h.update(low_root);
    let mut out = [0u8; 64];
    out.copy_from_slice(&h.finalize());
    out
}

impl HyperSignature {
    /// Clé à usage unique de cette dépense : c'est elle qui part dans spent_key_images
    pub fn one_time_key_hex(&self) -> String {
        hex::encode(&self.low_sig.public_key)
    }

    /// Vérification complète jusqu'à la racine attendue R_i
    pub fn verify(&self, msg: &[u8; 64], expected_root: &[u8; 32]) -> bool {
        if self.top_leaf as usize >= (1 << TOP_HEIGHT) || self.low_leaf as usize >= (1 << LOW_HEIGHT) { return false; }
        if self.top_path.len() != TOP_HEIGHT || self.low_path.len() != LOW_HEIGHT { return false; }

        // 1. La feuille basse a signé la transaction
        if !Wots::verify(&self.low_sig, msg) { return false; }
        let low_pk: [u8; 32] = match self.low_sig.public_key.as_slice().try_into() { Ok(p) => p, Err(_) => return false };
        let low_root = merkle::root_from_path(&low_pk, self.low_leaf as u64, &self.low_path);

        // 2. La feuille haute a signé cet arbre bas
        if !Wots::verify(&self.top_sig, &top_message(&low_root)) { return false; }
        let top_pk: [u8; 32] = match self.top_sig.public_key.as_slice().try_into() { Ok(p) => p, Err(_) => return false };

        // 3. Le chemin remonte jusqu'à R_i
        merkle::root_from_path(&top_pk, self.top_leaf as u64, &self.top_path) == *expected_root
    }
}

/// Clés d'UNE sous-adresse (côté wallet). L'arbre bas courant est gardé en cache.
pub struct HyperKeys {
    master: [u8; 32],
    sub_index: u64,
    top_levels: Vec<Vec<[u8; 32]>>,
    low_cache: Option<(u32, Vec<Vec<[u8; 32]>>)>,
}

impl HyperKeys {
    /// Construit l'arbre haut (64 clés : rapide, même sur téléphone)
    pub fn new(master: &[u8; 32], sub_index: u64) -> Self {
        let top_levels = merkle::build(&leaf_pks(&top_seed(master, sub_index), 1 << TOP_HEIGHT));
        Self { master: *master, sub_index, top_levels, low_cache: None }
    }

    /// R_i : ce qui va à la fin de l'adresse
    pub fn root(&self) -> [u8; 32] {
        merkle::root(&self.top_levels)
    }

    /// Clé à usage unique d'une feuille (pour vérifier si elle a déjà servi, sans signer)
    pub fn one_time_key(&self, leaf: u64) -> [u8; 32] {
        let top_leaf = (leaf >> LOW_HEIGHT) as u32;
        let low_leaf = leaf & ((1u64 << LOW_HEIGHT) - 1);
        Wots::public_key(&low_seed(&self.master, self.sub_index, top_leaf), low_leaf)
    }

    fn low_levels(&mut self, top_leaf: u32) -> &Vec<Vec<[u8; 32]>> {
        if self.low_cache.as_ref().map(|(t, _)| *t) != Some(top_leaf) {
            let seed = low_seed(&self.master, self.sub_index, top_leaf);
            self.low_cache = Some((top_leaf, merkle::build(&leaf_pks(&seed, 1 << LOW_HEIGHT))));
        }
        &self.low_cache.as_ref().unwrap().1
    }

    /// Signe avec la feuille `leaf` (0..65 535). Mêmes garde-fous que sign_for_utxo,
    /// indexés par la clé à usage unique : la réservation est écrite AVANT que la signature existe.
    pub fn sign<F, R>(
        &mut self,
        leaf: u64,
        msg: &[u8; 64],
        mut is_key_used: F,
        mut reserve_key: R,
    ) -> Result<HyperSignature, HyperError>
    where
        F: FnMut(&str) -> bool,
        R: FnMut(&str) -> bool,
    {
        if leaf >= LEAVES_PER_SUBADDRESS { return Err(HyperError::LeafOutOfRange(leaf)); }
        let top_leaf = (leaf >> LOW_HEIGHT) as u32;
        let low_leaf = (leaf & ((1u64 << LOW_HEIGHT) - 1)) as u32;

        // 1. Feuille basse : garde-fous puis signature de la transaction
        let (low_sk, low_pk) = Wots::generate_keypair(&low_seed(&self.master, self.sub_index, top_leaf), low_leaf as u64);
        let pk_hex = hex::encode(&low_pk);
        if is_key_used(&pk_hex) { return Err(HyperError::KeyUsedOnChainOrMempool(leaf)); }
        if !reserve_key(&pk_hex) { return Err(HyperError::KeyAlreadySignedLocally(leaf)); }
        let low_sig = Wots::sign(&low_sk, low_leaf as u64, msg, &low_pk);

        // 2. Chemin bas (l'arbre bas n'est construit qu'une fois toutes les 1024 dépenses)
        let low_levels = self.low_levels(top_leaf);
        let low_root = merkle::root(low_levels);
        let low_path = merkle::auth_path(low_levels, low_leaf as usize);

        // 3. Feuille haute : signe la racine basse (message identique à chaque fois → pas de réutilisation)
        let (top_sk, top_pk) = Wots::generate_keypair(&top_seed(&self.master, self.sub_index), top_leaf as u64);
        let top_sig = Wots::sign(&top_sk, top_leaf as u64, &top_message(&low_root), &top_pk);
        let top_path = merkle::auth_path(&self.top_levels, top_leaf as usize);

        Ok(HyperSignature { top_leaf, top_sig, top_path, low_leaf, low_sig, low_path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(b: u8) -> [u8; 64] { [b; 64] }

    #[test]
    fn signe_et_verifie() {
        let mut keys = HyperKeys::new(&[7u8; 32], 0);
        let root = keys.root();
        let sig = keys.sign(0, &msg(1), |_| false, |_| true).unwrap();
        assert!(sig.verify(&msg(1), &root));
        assert!(!sig.verify(&msg(2), &root));      // autre message
        assert!(!sig.verify(&msg(1), &[0u8; 32])); // autre racine
    }

    #[test]
    fn deuxieme_arbre_bas() {
        let mut keys = HyperKeys::new(&[7u8; 32], 0);
        let root = keys.root();
        let sig = keys.sign(1024 + 5, &msg(3), |_| false, |_| true).unwrap();
        assert_eq!((sig.top_leaf, sig.low_leaf), (1, 5));
        assert!(sig.verify(&msg(3), &root));
    }

    #[test]
    fn chemin_falsifie_rejete() {
        let mut keys = HyperKeys::new(&[7u8; 32], 0);
        let root = keys.root();
        let mut sig = keys.sign(3, &msg(4), |_| false, |_| true).unwrap();
        sig.low_path[0][0] ^= 1;
        assert!(!sig.verify(&msg(4), &root));
    }

    #[test]
    fn sous_adresses_independantes() {
        assert_ne!(HyperKeys::new(&[7u8; 32], 0).root(), HyperKeys::new(&[7u8; 32], 1).root());
    }

    #[test]
    fn garde_fous() {
        let mut keys = HyperKeys::new(&[7u8; 32], 0);
        assert_eq!(keys.sign(0, &msg(1), |_| true, |_| true), Err(HyperError::KeyUsedOnChainOrMempool(0)));
        assert_eq!(keys.sign(0, &msg(1), |_| false, |_| false), Err(HyperError::KeyAlreadySignedLocally(0)));
        assert_eq!(
            keys.sign(LEAVES_PER_SUBADDRESS, &msg(1), |_| false, |_| true),
            Err(HyperError::LeafOutOfRange(LEAVES_PER_SUBADDRESS))
        );
    }
	
	fn h512(data: &[u8]) -> [u8; 64] {
        let mut out = [0u8; 64];
        out.copy_from_slice(&Sha512::digest(data));
        out
    }

    #[test]
    fn wots_brut_signe_et_verifie() {
        let (sk, pk) = Wots::generate_keypair(b"ma_super_master_seed_secrete_12345", 0);
        let m = h512(b"Transaction data to protect post-quantically");
        let sig = Wots::sign(&sk, 0, &m, &pk);
        assert!(Wots::verify(&sig, &m), "La signature WOTS+ devrait être valide !");
    }

    #[test]
    fn wots_brut_autre_message_rejete() {
        let (sk, pk) = Wots::generate_keypair(b"ma_super_master_seed_secrete_12345", 1);
        let sig = Wots::sign(&sk, 1, &h512(b"Message 1"), &pk);
        assert!(!Wots::verify(&sig, &h512(b"Message 2")), "La signature ne doit pas valider un autre message !");
    }

    #[test]
    fn poids_signature_hypertree() {
        let mut keys = HyperKeys::new(&[9u8; 32], 0);
        let sig = keys.sign(0, &msg(5), |_| false, |_| true).unwrap();
        let wots_size = |s: &WotsSignature| 8 + s.public_key.len() + s.signature_bytes.len();
        let total = 4 + wots_size(&sig.top_sig) + 32 * sig.top_path.len()
                  + 4 + wots_size(&sig.low_sig) + 32 * sig.low_path.len();
        println!("\n--- Signature hypertree : {} octets ({:.2} Ko), UNE par transaction ---", total, total as f64 / 1024.0);
        assert_eq!(total, 4 + (8 + 32 + 2112) + 6 * 32 + 4 + (8 + 32 + 2112) + 10 * 32);
    }
	
	    #[test]
    fn cle_usage_unique() {
        let key = OneTimeKey::generate(&[3u8; 32], 0);
        let pk = key.public_key().to_vec();
        let sig = key.sign(&msg(6));
        assert_eq!(sig.public_key, pk);
        assert!(Wots::verify(&sig, &msg(6)));
        // key.sign(&msg(7)); // ← ne compile plus : la clé a été consommée
    }
}