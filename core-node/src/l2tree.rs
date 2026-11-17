// src/l2tree.rs
// =====================================================================
// ARBRE DE L'ARGENT D'UN L2 : Merkle-somme INDEXÉ, trié par identifiant
//
// - Les feuilles sont rangées par ordre d'arrivée (index 0, 1, 2…) dans un
//   arbre de profondeur fixe TREE_DEPTH. Un index n'est JAMAIS réutilisé.
// - Le tri par identifiant passe par un lien : chaque feuille connaît
//   l'identifiant suivant (next_id). La feuille 0 est une sentinelle (id "").
//   → absence de x prouvée par UNE feuille : id < x < next_id
// - Chaque nœud porte la SOMME des montants en dessous : la racine engage
//   le total du L2 (compteur 1:1 structurel).
//
// Deux usages, une seule définition :
//   * L2Tree      : l'arbre complet (séquenceur, pairs, wallet)
//   * apply_*()   : la même mise à jour, refaite SANS l'arbre, à partir de
//                   chemins (le nœud L1 juge une étape contestée)
// Un test vérifie que les deux donnent toujours la même racine.
// =====================================================================
use serde::{Serialize, Deserialize};
use sha2::{Digest, Sha512};
use std::collections::{BTreeMap, HashMap};
use crate::transaction::TransactionOutput;

/// Profondeur de l'arbre : 2^32 sorties au maximum sur toute la vie d'un L2
pub const TREE_DEPTH: usize = 32;

type Hash = [u8; 64];

/// Racine d'un arbre : hash, somme des montants (= total du L2), prochain index libre
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeRoot {
    pub hash: String, // hex
    pub sum: u64,
    pub next_index: u64,
}

/// Une feuille : la sortie (son identifiant = kyber_capsule) et l'identifiant suivant dans l'ordre trié
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Leaf {
    pub output: TransactionOutput,
    pub next_id: String, // "" = pas de suivant (fin de liste)
}

impl Leaf {
    pub fn id(&self) -> &str { &self.output.kyber_capsule }
}

/// Un frère sur le chemin : son hash (hex) et sa somme
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sibling {
    pub hash: String,
    pub sum: u64,
}

/// Chemin d'une case (index) jusqu'à la racine : TREE_DEPTH frères, du bas vers le haut
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Path {
    pub index: u64,
    pub siblings: Vec<Sibling>,
}

// ----------------------------- Hachage -----------------------------

fn put(h: &mut Sha512, s: &str) {
    h.update((s.len() as u64).to_be_bytes());
    h.update(s.as_bytes());
}

fn finish(h: Sha512) -> Hash {
    let mut out = [0u8; 64];
    out.copy_from_slice(&h.finalize());
    out
}

/// Hash d'une case : None = case vide
fn leaf_hash(leaf: Option<&Leaf>) -> (Hash, u64) {
    let mut h = Sha512::new();
    match leaf {
        None => {
            h.update(b"wattcoin/l2/empty");
            (finish(h), 0)
        }
        Some(l) => {
            h.update(b"wattcoin/l2/leaf");
            put(&mut h, &l.output.kyber_capsule);
            put(&mut h, &l.output.stealth_address);
            h.update(l.output.amount.to_be_bytes());
            let mut v = Sha512::new();
            v.update(l.output.aes_vault.as_bytes());
            h.update(finish(v));
            put(&mut h, &l.next_id);
            (finish(h), l.output.amount)
        }
    }
}

fn node_hash(left: &(Hash, u64), right: &(Hash, u64)) -> Option<(Hash, u64)> {
    let sum = left.1.checked_add(right.1)?; // jamais de débordement silencieux
    let mut h = Sha512::new();
    h.update(b"wattcoin/l2/node");
    h.update(left.0);
    h.update(left.1.to_be_bytes());
    h.update(right.0);
    h.update(right.1.to_be_bytes());
    Some((finish(h), sum))
}

/// Hash d'un sous-arbre entièrement vide, pour chaque niveau (0 = case)
fn empty_hashes() -> Vec<(Hash, u64)> {
    let mut z = vec![leaf_hash(None)];
    for lvl in 0..TREE_DEPTH {
        let n = node_hash(&z[lvl], &z[lvl]).expect("somme nulle");
        z.push(n);
    }
    z
}

fn decode_hash(s: &str) -> Option<Hash> {
    hex::decode(s).ok()?.try_into().ok()
}

// ------------------- Vérification sans l'arbre (nœud L1) -------------------

/// Remonte de la case (leaf) jusqu'à la racine par le chemin. None = chemin mal formé ou somme qui déborde.
fn climb(leaf: Option<&Leaf>, path: &Path) -> Option<(Hash, u64)> {
    if path.siblings.len() != TREE_DEPTH || path.index >= (1u64 << TREE_DEPTH) { return None; }
    let mut cur = leaf_hash(leaf);
    for (lvl, sib) in path.siblings.iter().enumerate() {
        let s = (decode_hash(&sib.hash)?, sib.sum);
        cur = if (path.index >> lvl) & 1 == 0 { node_hash(&cur, &s)? } else { node_hash(&s, &cur)? };
    }
    Some(cur)
}

/// La case `path.index` de cette racine contient-elle exactement `leaf` (None = case vide) ?
pub fn verify(root: &TreeRoot, leaf: Option<&Leaf>, path: &Path) -> bool {
    match (climb(leaf, path), decode_hash(&root.hash)) {
        (Some((h, s)), Some(rh)) => h == rh && s == root.sum,
        _ => false,
    }
}

/// `id` est-il présent ? (la feuille est fournie avec son chemin)
pub fn verify_member(root: &TreeRoot, leaf: &Leaf, path: &Path) -> bool {
    !leaf.id().is_empty() && path.index < root.next_index && verify(root, Some(leaf), path)
}

/// `id` est-il ABSENT ? Preuve : la feuille basse `low`, avec low.id < id < low.next_id
pub fn verify_absent(root: &TreeRoot, id: &str, low: &Leaf, low_path: &Path) -> bool {
    !id.is_empty()
        && low.id() < id
        && (low.next_id.is_empty() || id < low.next_id.as_str())
        && path_ok_for(root, low, low_path)
}

/// Feuille présente (la sentinelle 0 comprise)
fn path_ok_for(root: &TreeRoot, leaf: &Leaf, path: &Path) -> bool {
    path.index < root.next_index && verify(root, Some(leaf), path)
}

fn rebuild(leaf: Option<&Leaf>, path: &Path, next_index: u64) -> Option<TreeRoot> {
    let (h, s) = climb(leaf, path)?;
    Some(TreeRoot { hash: hex::encode(h), sum: s, next_index })
}

/// INSERTION refaite sans l'arbre. Témoins, dans cet ordre :
///   1. `low` + `low_path` : la feuille basse dans `root` (prouve l'absence du nouvel id)
///   2. `slot_path`        : la case vide `root.next_index`, dans l'arbre APRÈS la mise à jour de `low`
pub fn apply_insert(root: &TreeRoot, output: &TransactionOutput, low: &Leaf, low_path: &Path, slot_path: &Path) -> Option<TreeRoot> {
    let id = output.kyber_capsule.as_str();
    if !verify_absent(root, id, low, low_path) { return None; }
    let next_index = root.next_index.checked_add(1).filter(|n| *n <= (1u64 << TREE_DEPTH))?;

    // 1. La feuille basse pointe désormais vers le nouvel id
    let new_low = Leaf { output: low.output.clone(), next_id: id.to_string() };
    let r1 = rebuild(Some(&new_low), low_path, root.next_index)?;

    // 2. Le nouvel id prend la case libre et hérite de l'ancien suivant
    if slot_path.index != root.next_index || !verify(&r1, None, slot_path) { return None; }
    let leaf = Leaf { output: output.clone(), next_id: low.next_id.clone() };
    rebuild(Some(&leaf), slot_path, next_index)
}

/// SUPPRESSION (dépense) refaite sans l'arbre. Témoins, dans cet ordre :
///   1. `leaf` + `leaf_path` : la feuille dépensée dans `root`
///   2. `pred` + `pred_path` : son prédécesseur trié, dans l'arbre APRÈS que la case a été vidée
pub fn apply_remove(root: &TreeRoot, leaf: &Leaf, leaf_path: &Path, pred: &Leaf, pred_path: &Path) -> Option<TreeRoot> {
    if !verify_member(root, leaf, leaf_path) || pred.next_id != leaf.id() { return None; }

    // 1. La case est vidée (son index ne servira plus jamais)
    let r1 = rebuild(None, leaf_path, root.next_index)?;

    // 2. Le prédécesseur saute la feuille dépensée
    if !path_ok_for(&r1, pred, pred_path) { return None; }
    let new_pred = Leaf { output: pred.output.clone(), next_id: leaf.next_id.clone() };
    rebuild(Some(&new_pred), pred_path, root.next_index)
}

// ------------------- L'arbre complet (séquenceur, pairs) -------------------

/// Témoins d'une insertion (ce que `apply_insert` attend)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsertWitness { pub low: Leaf, pub low_path: Path, pub slot_path: Path }

/// Témoins d'une suppression (ce que `apply_remove` attend)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemoveWitness { pub leaf: Leaf, pub leaf_path: Path, pub pred: Leaf, pub pred_path: Path }

#[derive(Clone)]
pub struct L2Tree {
    leaves: HashMap<u64, Leaf>,          // cases occupées
    index_of: BTreeMap<String, u64>,     // id → case, TRIÉ (sentinelle "" comprise)
    nodes: HashMap<(usize, u64), (Hash, u64)>, // nœuds non vides (niveau, position)
    empty: Vec<(Hash, u64)>,
    next_index: u64,
}

impl L2Tree {
    /// Arbre neuf : la sentinelle (id "", montant 0) occupe la case 0
    pub fn new() -> Self {
        let mut t = L2Tree { leaves: HashMap::new(), index_of: BTreeMap::new(), nodes: HashMap::new(), empty: empty_hashes(), next_index: 1 };
        let sentinel = Leaf {
            output: TransactionOutput { stealth_address: String::new(), kyber_capsule: String::new(), aes_vault: String::new(), amount: 0 },
            next_id: String::new(),
        };
        t.index_of.insert(String::new(), 0);
        t.set(0, Some(sentinel));
        t
    }

    fn node(&self, lvl: usize, pos: u64) -> (Hash, u64) {
        self.nodes.get(&(lvl, pos)).copied().unwrap_or(self.empty[lvl])
    }

    /// Écrit une case et remonte jusqu'à la racine (TREE_DEPTH hachages)
    fn set(&mut self, index: u64, leaf: Option<Leaf>) {
        let h = leaf_hash(leaf.as_ref());
        match leaf { Some(l) => { self.leaves.insert(index, l); } None => { self.leaves.remove(&index); } }
        self.nodes.insert((0, index), h);
        let mut pos = index;
        for lvl in 0..TREE_DEPTH {
            let (l, r) = (self.node(lvl, pos & !1), self.node(lvl, pos | 1));
            pos >>= 1;
            let n = node_hash(&l, &r).expect("le total d'un L2 ne dépasse jamais u64");
            self.nodes.insert((lvl + 1, pos), n);
        }
    }

    pub fn root(&self) -> TreeRoot {
        let (h, s) = self.node(TREE_DEPTH, 0);
        TreeRoot { hash: hex::encode(h), sum: s, next_index: self.next_index }
    }

    pub fn path(&self, index: u64) -> Path {
        let siblings = (0..TREE_DEPTH).map(|lvl| {
            let (h, s) = self.node(lvl, (index >> lvl) ^ 1);
            Sibling { hash: hex::encode(h), sum: s }
        }).collect();
        Path { index, siblings }
    }

    pub fn get(&self, id: &str) -> Option<&TransactionOutput> {
        self.index_of.get(id).and_then(|i| self.leaves.get(i)).map(|l| &l.output).filter(|_| !id.is_empty())
    }

    /// Preuve de présence : (feuille, chemin)
    pub fn prove_member(&self, id: &str) -> Option<(Leaf, Path)> {
        if id.is_empty() { return None; }
        let i = *self.index_of.get(id)?;
        Some((self.leaves.get(&i)?.clone(), self.path(i)))
    }

    /// Preuve d'absence : la feuille basse (plus grand id < id) et son chemin
    pub fn prove_absent(&self, id: &str) -> Option<(Leaf, Path)> {
        if id.is_empty() || self.index_of.contains_key(id) { return None; }
        let (_, &i) = self.index_of.range(..id.to_string()).next_back()?;
        Some((self.leaves.get(&i)?.clone(), self.path(i)))
    }

    /// Ajoute une sortie. Renvoie les témoins qui permettent au L1 de refaire la même mise à jour.
    pub fn insert(&mut self, output: TransactionOutput) -> Result<InsertWitness, String> {
        let id = output.kyber_capsule.clone();
        if id.is_empty() { return Err("identifiant de sortie vide".into()); }
        if self.next_index >= (1u64 << TREE_DEPTH) { return Err("arbre plein".into()); }
        let (low, low_path) = self.prove_absent(&id).ok_or("identifiant déjà présent")?;
        if self.root().sum.checked_add(output.amount).is_none() { return Err("total du L2 hors limites".into()); }

        let low_index = low_path.index;
        self.set(low_index, Some(Leaf { output: low.output.clone(), next_id: id.clone() }));
        let slot = self.next_index;
        let slot_path = self.path(slot);
        self.set(slot, Some(Leaf { output, next_id: low.next_id.clone() }));
        self.index_of.insert(id, slot);
        self.next_index += 1;
        Ok(InsertWitness { low, low_path, slot_path })
    }

    /// Dépense une sortie. Renvoie la sortie et les témoins pour le L1.
    pub fn remove(&mut self, id: &str) -> Result<(TransactionOutput, RemoveWitness), String> {
        let (leaf, leaf_path) = self.prove_member(id).ok_or("sortie inexistante")?;
        self.set(leaf_path.index, None);
        self.index_of.remove(id);

        let (_, &pi) = self.index_of.range(..id.to_string()).next_back().expect("la sentinelle précède tout");
        let pred = self.leaves.get(&pi).expect("prédécesseur présent").clone();
        let pred_path = self.path(pi);
        self.set(pi, Some(Leaf { output: pred.output.clone(), next_id: leaf.next_id.clone() }));
        Ok((leaf.output.clone(), RemoveWitness { leaf, leaf_path, pred, pred_path }))
    }
}

impl Default for L2Tree {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(id: &str, amount: u64) -> TransactionOutput {
        TransactionOutput { stealth_address: format!("kyber.{}", id), kyber_capsule: id.to_string(), aes_vault: String::new(), amount }
    }

    #[test]
    fn insertion_suppression_et_sommes() {
        let mut t = L2Tree::new();
        let r0 = t.root();
        assert_eq!((r0.sum, r0.next_index), (0, 1));
        for (id, a) in [("m", 50), ("c", 20), ("x", 30)] { t.insert(out(id, a)).unwrap(); }
        assert_eq!(t.root().sum, 100);                                  // la racine porte le total
        assert!(t.insert(out("c", 1)).is_err());                        // pas deux fois le même id
        assert_eq!(t.remove("c").unwrap().0.amount, 20);
        assert_eq!(t.root().sum, 80);
        assert!(t.remove("c").is_err());                                // pas de double dépense
        assert!(t.get("c").is_none() && t.get("m").is_some());
        assert_eq!(t.root().next_index, 4);                             // un index n'est jamais réutilisé
    }

    #[test]
    fn presence_et_absence() {
        let mut t = L2Tree::new();
        for id in ["b", "d", "f"] { t.insert(out(id, 1)).unwrap(); }
        let root = t.root();

        let (leaf, path) = t.prove_member("d").unwrap();
        assert!(verify_member(&root, &leaf, &path));
        let mut faux = leaf.clone(); faux.output.amount = 999;           // montant falsifié
        assert!(!verify_member(&root, &faux, &path));

        let (low, low_path) = t.prove_absent("e").unwrap();              // d < e < f
        assert_eq!(low.id(), "d");
        assert!(verify_absent(&root, "e", &low, &low_path));
        assert!(!verify_absent(&root, "d", &low, &low_path));            // présent : pas d'absence
        assert!(!verify_absent(&root, "g", &low, &low_path));            // mauvaise feuille basse
        let (low, low_path) = t.prove_absent("a").unwrap();              // avant tout : la sentinelle
        assert!(low.id().is_empty() && verify_absent(&root, "a", &low, &low_path));
        let (low, low_path) = t.prove_absent("z").unwrap();              // après tout : la dernière
        assert!(verify_absent(&root, "z", &low, &low_path));
    }

    #[test]
    fn le_l1_refait_la_meme_racine_sans_l_arbre() {
        let mut t = L2Tree::new();
        let mut ids: Vec<String> = Vec::new();
        // suite déterministe d'insertions et de dépenses mélangées
        for i in 0..60u64 {
            let before = t.root();
            if i % 3 == 2 && !ids.is_empty() {
                let id = ids.remove(((i * 7) % ids.len() as u64) as usize);
                let (_, w) = t.remove(&id).unwrap();
                let after = apply_remove(&before, &w.leaf, &w.leaf_path, &w.pred, &w.pred_path);
                assert_eq!(after.as_ref(), Some(&t.root()), "suppression {}", id);
            } else {
                let id = format!("id{:03}", (i * 37) % 101);
                if ids.contains(&id) { continue; }
                let o = out(&id, i + 1);
                let w = t.insert(o.clone()).unwrap();
                let after = apply_insert(&before, &o, &w.low, &w.low_path, &w.slot_path);
                assert_eq!(after.as_ref(), Some(&t.root()), "insertion {}", id);
                ids.push(id);
            }
        }
    }

    #[test]
    fn le_l1_refuse_les_temoins_truques() {
        let mut t = L2Tree::new();
        for id in ["b", "d"] { t.insert(out(id, 5)).unwrap(); }
        let before = t.root();

        // Insertion d'un id DÉJÀ présent : aucune feuille basse ne le prouve absent
        let (low, low_path) = t.prove_member("b").unwrap();
        let slot = t.path(before.next_index);
        assert!(apply_insert(&before, &out("d", 7), &low, &low_path, &slot).is_none());

        // Insertion dans une case déjà occupée (pas la case libre)
        let mut t2 = L2Tree::new();
        for id in ["b", "d"] { t2.insert(out(id, 5)).unwrap(); }
        let (low, low_path) = t2.prove_absent("c").unwrap();
        let occupied = t2.path(1);
        assert!(apply_insert(&before, &out("c", 7), &low, &low_path, &occupied).is_none());

        // Dépense d'une sortie inexistante (feuille inventée)
        let (pred, pred_path) = t.prove_member("b").unwrap();
        let ghost = Leaf { output: out("c", 1_000_000), next_id: "d".into() };
        let ghost_path = t.path(9);
        assert!(apply_remove(&before, &ghost, &ghost_path, &pred, &pred_path).is_none());

        // Racine annoncée avec une somme gonflée : plus rien ne se vérifie
        let mut menteuse = before.clone(); menteuse.sum += 1;
        let (leaf, path) = t.prove_member("b").unwrap();
        assert!(!verify_member(&menteuse, &leaf, &path));
    }
}
