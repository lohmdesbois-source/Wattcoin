// src/l2trace.rs
// =====================================================================
// TRACE D'UN BLOC L2 : la suite des étapes qui ont fait bouger l'ARGENT
//
// Règle universelle (tous les L2, internes ou externes) : l'argent ne bouge
// QUE par des transactions au format du L1, jugées par les règles du L1.
//   - Tx      : une `Transaction` Standard, valide (is_valid), fee = 0.
//               Ses entrées sont dépensées DANS L'ORDRE, puis ses sorties
//               ajoutées DANS L'ORDRE. Une sortie "EXIT_<kyber>.<R>" n'entre
//               pas dans l'arbre : c'est un retrait vers le L1.
//   - Deposit : le k-ième dépôt L1 → L2 (L2BridgeLock), qui crée sa sortie.
//
// Chaque étape engage la racine de l'arbre avant / après et deux compteurs
// (dépôts, retraits). Les étapes forment un arbre de Merkle : trace_root.
//
// Une seule définition, deux usages :
//   * TraceBuilder : le séquenceur construit les étapes ET leurs témoins
//   * verify_step  : le nœud L1 rejuge UNE étape contestée, sans l'arbre
// =====================================================================
use serde::{Serialize, Deserialize};
use sha2::{Digest, Sha512};
use crate::transaction::{can_spend, Transaction, TransactionOutput, TransactionType};
use crate::l2tree::{self, InsertWitness, L2Tree, Leaf, Path, RemoveWitness, TreeRoot};

/// Adresse d'une sortie qui QUITTE le L2 : "EXIT_<kyber>.<R>" (payée sur le L1)
pub const EXIT_PREFIX: &str = "EXIT_";
/// Identifiant réservé aux sorties créées par un dépôt
pub const DEPOSIT_PREFIX: &str = "DEPOSIT_";
/// Identifiants qu'une transaction L2 ne peut JAMAIS créer
const RESERVED_IDS: [&str; 2] = [DEPOSIT_PREFIX, "UNPEG_"];

/// Identifiant de la sortie du k-ième dépôt vers ce L2
pub fn deposit_id(l2_name: &str, k: u64) -> String {
    format!("{}{}_{}", DEPOSIT_PREFIX, l2_name, k)
}

pub fn is_exit(o: &TransactionOutput) -> bool {
    o.stealth_address.starts_with(EXIT_PREFIX)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StepKind {
    Tx(Transaction),
    Deposit { k: u64, output: TransactionOutput },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub index: u64,
    pub kind: StepKind,
    pub root_before: TreeRoot,
    pub root_after: TreeRoot,
    pub deposits_before: u64, // dépôts déjà inclus (depuis la création du L2)
    pub exits_before: u64,    // retraits déjà faits DANS CET ANCRAGE
}

impl Step {
    pub fn hash(&self) -> [u8; 64] {
        let mut h = Sha512::new();
        h.update(b"wattcoin/l2/step");
        h.update(bincode::serialize(self).unwrap_or_default()); // tout, signatures comprises
        let mut out = [0u8; 64];
        out.copy_from_slice(&h.finalize());
        out
    }

    /// Les sorties qui quittent le L2, dans l'ordre
    pub fn exits(&self) -> Vec<&TransactionOutput> {
        match &self.kind {
            StepKind::Tx(tx) => tx.outputs.iter().filter(|o| is_exit(o)).collect(),
            StepKind::Deposit { .. } => vec![],
        }
    }

    pub fn deposits_after(&self) -> u64 {
        self.deposits_before + matches!(self.kind, StepKind::Deposit { .. }) as u64
    }

    pub fn exits_after(&self) -> u64 {
        self.exits_before + self.exits().len() as u64
    }
}

// ------------------------- Les règles d'une étape -------------------------

/// Règles qui se jugent SANS l'arbre (sur la seule transaction)
fn tx_rules(tx: &Transaction) -> Result<(), String> {
    if tx.tx_type != TransactionType::Standard { return Err("transaction L2 non Standard".into()); }
    if tx.fee != 0 { return Err("frais ≠ 0 sur le L2 (on paie le séquenceur par une sortie)".into()); }
    if !tx.is_valid() { return Err("transaction invalide (signatures ou montants)".into()); }
    let mut seen = std::collections::HashSet::new();
    for i in &tx.inputs {
        if i.utxo_id.is_empty() || !seen.insert(i.utxo_id.as_str()) { return Err("entrée vide ou en double".into()); }
    }
    let mut ids = std::collections::HashSet::new();
    for o in tx.outputs.iter().filter(|o| !is_exit(o)) {
        let id = o.kyber_capsule.as_str();
        if id.is_empty() || RESERVED_IDS.iter().any(|p| id.starts_with(p)) { return Err("identifiant de sortie vide ou réservé".into()); }
        if !ids.insert(id) { return Err("identifiant de sortie en double".into()); }
    }
    Ok(())
}

fn static_rules(step: &Step) -> Result<(), String> {
    match &step.kind {
        StepKind::Tx(tx) => tx_rules(tx),
        StepKind::Deposit { k, output } => {
            if *k != step.deposits_before { return Err("dépôt hors de l'ordre".into()); }
            if !output.kyber_capsule.starts_with(DEPOSIT_PREFIX) || is_exit(output) { return Err("sortie de dépôt mal formée".into()); }
            Ok(())
        }
    }
}

/// Le témoin d'UNE opération sur l'arbre, relatif à la racine du moment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OpWitness {
    Remove(RemoveWitness),                // l'entrée existe : on la dépense
    Insert(InsertWitness),                // la sortie est neuve : on l'ajoute
    Absent { low: Leaf, low_path: Path }, // preuve de FRAUDE : l'entrée n'existe pas
    Present { leaf: Leaf, path: Path },   // preuve de FRAUDE : la sortie existe déjà
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Valid,
    Fraud(String),      // l'étape viole une règle : prouvé
    BadWitness(String), // les témoins ne prouvent rien : la contestation est rejetée
}

/// Ajoute une sortie à la racine du moment (ou prouve qu'elle existait déjà). None = c'est bon, on continue.
fn insert_op(cur: &mut TreeRoot, out: &TransactionOutput, wit: Option<&OpWitness>) -> Option<Verdict> {
    match wit {
        Some(OpWitness::Insert(iw)) => match l2tree::apply_insert(cur, out, &iw.low, &iw.low_path, &iw.slot_path) {
            Some(next) => { *cur = next; None }
            None => Some(Verdict::BadWitness("insertion non prouvée".into())),
        },
        Some(OpWitness::Present { leaf, path }) if leaf.id() == out.kyber_capsule && l2tree::verify_member(cur, leaf, path) =>
            Some(Verdict::Fraud("sortie déjà existante".into())),
        _ => Some(Verdict::BadWitness("témoin d'insertion attendu".into())),
    }
}

/// Rejuge une étape à partir de ses témoins (un par entrée, puis un par sortie gardée).
pub fn verify_step(step: &Step, witnesses: &[OpWitness]) -> Verdict {
    use Verdict::*;
    if let Err(e) = static_rules(step) { return Fraud(e); }
    let mut cur = step.root_before.clone();
    let mut w = witnesses.iter();

    match &step.kind {
        StepKind::Tx(tx) => {
            for input in &tx.inputs {
                match w.next() {
                    Some(OpWitness::Remove(r)) => {
                        if r.leaf.id() != input.utxo_id { return BadWitness("témoin d'une autre entrée".into()); }
                        let Some(next) = l2tree::apply_remove(&cur, &r.leaf, &r.leaf_path, &r.pred, &r.pred_path) else {
                            return BadWitness("dépense non prouvée".into());
                        };
                        if r.leaf.output.amount != input.amount { return Fraud("montant d'entrée falsifié".into()); }
                        let Some(owner) = tx.owners.get(input.owner as usize) else { return Fraud("entrée sans preuve de propriété".into()); };
                        if !can_spend(&r.leaf.output, &owner.root, &input.owner_otp) { return Fraud("entrée dépensée par un autre que son propriétaire".into()); }
                        cur = next;
                    }
                    Some(OpWitness::Absent { low, low_path }) if l2tree::verify_absent(&cur, &input.utxo_id, low, low_path) =>
                        return Fraud("entrée inexistante (argent créé de rien)".into()),
                    _ => return BadWitness("témoin de dépense attendu".into()),
                }
            }
            for out in tx.outputs.iter().filter(|o| !is_exit(o)) {
                if let Some(v) = insert_op(&mut cur, out, w.next()) { return v; }
            }
        }
        StepKind::Deposit { output, .. } => {
            if let Some(v) = insert_op(&mut cur, output, w.next()) { return v; }
        }
    }

    if w.next().is_some() { return BadWitness("témoins en trop".into()); }
    if cur == step.root_after { Valid } else { Fraud("racine d'arrivée fausse".into()) }
}

/// Deux étapes voisines se suivent-elles ? (racine, compteurs, index)
pub fn check_link(a: &Step, b: &Step) -> Result<(), String> {
    if b.index != a.index + 1 { return Err("étapes non consécutives".into()); }
    if b.root_before != a.root_after { return Err("racine discontinue".into()); }
    if b.deposits_before != a.deposits_after() { return Err("compteur de dépôts discontinu".into()); }
    if b.exits_before != a.exits_after() { return Err("compteur de retraits discontinu".into()); }
    Ok(())
}

/// La première étape part-elle de l'ancrage précédent ?
pub fn check_first(step: &Step, prev_root: &TreeRoot, prev_deposits: u64) -> Result<(), String> {
    if step.index != 0 { return Err("ce n'est pas la première étape".into()); }
    if &step.root_before != prev_root { return Err("ne part pas de l'ancrage précédent".into()); }
    if step.deposits_before != prev_deposits || step.exits_before != 0 { return Err("compteurs de départ faux".into()); }
    Ok(())
}

/// La dernière étape arrive-t-elle à l'ancrage ?
pub fn check_last(step: &Step, trace_len: u64, utxo_root: &TreeRoot, deposit_count: u64, exit_count: u64) -> Result<(), String> {
    if step.index + 1 != trace_len { return Err("ce n'est pas la dernière étape".into()); }
    if &step.root_after != utxo_root { return Err("n'arrive pas à la racine ancrée".into()); }
    if step.deposits_after() != deposit_count || step.exits_after() != exit_count { return Err("compteurs d'arrivée faux".into()); }
    Ok(())
}

// --------------------- L'arbre de Merkle de la trace ---------------------

/// Racine de la trace : engage le NOMBRE d'étapes et leur arbre
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceRoot {
    pub hash: String, // hex
    pub len: u64,
}

fn depth(len: u64) -> usize {
    if len <= 1 { 0 } else { (64 - (len - 1).leading_zeros()) as usize }
}

fn tnode(l: &[u8; 64], r: &[u8; 64]) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(b"wattcoin/l2/tnode");
    h.update(l);
    h.update(r);
    let mut out = [0u8; 64];
    out.copy_from_slice(&h.finalize());
    out
}

fn tempty() -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(b"wattcoin/l2/tempty");
    let mut out = [0u8; 64];
    out.copy_from_slice(&h.finalize());
    out
}

fn seal(len: u64, top: &[u8; 64]) -> String {
    let mut h = Sha512::new();
    h.update(b"wattcoin/l2/trace");
    h.update(len.to_be_bytes());
    h.update(top);
    hex::encode(h.finalize())
}

fn levels(steps: &[Step]) -> Vec<Vec<[u8; 64]>> {
    let d = depth(steps.len() as u64);
    let mut lvl: Vec<[u8; 64]> = steps.iter().map(|s| s.hash()).collect();
    lvl.resize(1usize << d, tempty());
    let mut all = vec![lvl];
    while all.last().unwrap().len() > 1 {
        let next = all.last().unwrap().chunks(2).map(|c| tnode(&c[0], &c[1])).collect();
        all.push(next);
    }
    all
}

pub fn trace_root(steps: &[Step]) -> TraceRoot {
    let all = levels(steps);
    TraceRoot { hash: seal(steps.len() as u64, &all.last().unwrap()[0]), len: steps.len() as u64 }
}

/// Chemin de l'étape i dans la trace (frères en hex, du bas vers le haut)
pub fn trace_path(steps: &[Step], i: u64) -> Vec<String> {
    let all = levels(steps);
    (0..all.len() - 1).map(|lvl| hex::encode(all[lvl][((i >> lvl) ^ 1) as usize])).collect()
}

/// L'étape fait-elle partie de cette trace, à SON index ?
pub fn verify_trace(root: &TraceRoot, step: &Step, path: &[String]) -> bool {
    if step.index >= root.len || path.len() != depth(root.len) { return false; }
    let mut cur = step.hash();
    for (lvl, sib) in path.iter().enumerate() {
        let Some(s) = hex::decode(sib).ok().and_then(|v| <[u8; 64]>::try_from(v).ok()) else { return false };
        cur = if (step.index >> lvl) & 1 == 0 { tnode(&cur, &s) } else { tnode(&s, &cur) };
    }
    seal(root.len, &cur) == root.hash
}

// ------------------- Construction (séquenceur, pairs) -------------------

/// Applique dépôts et transactions à l'arbre en notant chaque étape et ses témoins.
/// Une transaction refusée ne touche PAS à l'arbre.
pub struct TraceBuilder<'a> {
    tree: &'a mut L2Tree,
    pub steps: Vec<Step>,
    pub witnesses: Vec<Vec<OpWitness>>,
    pub exits: Vec<TransactionOutput>,
    deposits: u64,
}

impl<'a> TraceBuilder<'a> {
    /// `deposits_before` : nombre de dépôts déjà inclus au dernier ancrage
    pub fn new(tree: &'a mut L2Tree, deposits_before: u64) -> Self {
        TraceBuilder { tree, steps: vec![], witnesses: vec![], exits: vec![], deposits: deposits_before }
    }

    fn push(&mut self, kind: StepKind, root_before: TreeRoot, wit: Vec<OpWitness>) {
        let step = Step {
            index: self.steps.len() as u64, kind, root_before, root_after: self.tree.root(),
            deposits_before: self.deposits, exits_before: self.exits.len() as u64,
        };
        self.deposits = step.deposits_after();
        self.exits.extend(step.exits().into_iter().cloned());
        self.steps.push(step);
        self.witnesses.push(wit);
    }

    /// Le dépôt suivant (sa sortie porte l'identifiant `deposit_id(l2, k)`)
    pub fn deposit(&mut self, output: TransactionOutput) -> Result<(), String> {
        if !output.kyber_capsule.starts_with(DEPOSIT_PREFIX) || is_exit(&output) { return Err("sortie de dépôt mal formée".into()); }
        if self.tree.get(&output.kyber_capsule).is_some() { return Err("dépôt déjà inclus".into()); }
        let before = self.tree.root();
        let w = self.tree.insert(output.clone())?;
        let k = self.deposits;
        self.push(StepKind::Deposit { k, output }, before, vec![OpWitness::Insert(w)]);
        Ok(())
    }

    /// Une transaction d'argent L2 : refusée (arbre intact) si elle viole une règle
    pub fn tx(&mut self, tx: Transaction) -> Result<(), String> {
        tx_rules(&tx)?;
        for input in &tx.inputs {
            let o = self.tree.get(&input.utxo_id).ok_or("entrée inexistante")?;
            if o.amount != input.amount { return Err("montant d'entrée faux".into()); }
            let owner = tx.owners.get(input.owner as usize).ok_or("entrée sans preuve de propriété")?;
            if !can_spend(o, &owner.root, &input.owner_otp) { return Err("entrée qui n'appartient pas au signataire".into()); }
        }
        if tx.outputs.iter().filter(|o| !is_exit(o)).any(|o| self.tree.get(&o.kyber_capsule).is_some()) {
            return Err("sortie déjà existante".into());
        }

        let before = self.tree.root();
        let mut wit = Vec::new();
        for input in &tx.inputs { wit.push(OpWitness::Remove(self.tree.remove(&input.utxo_id)?.1)); }
        for out in tx.outputs.iter().filter(|o| !is_exit(o)) { wit.push(OpWitness::Insert(self.tree.insert(out.clone())?)); }
        self.push(StepKind::Tx(tx), before, wit);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction::{OwnerProof, TransactionInput};

    /// Sous-adresse de test : "R1" → graine [b'1'; 32]
    fn keys(name: &str) -> wots::HyperKeys { wots::HyperKeys::new(&[name.as_bytes()[1]; 32], 0) }
    fn root(name: &str) -> String { hex::encode(keys(name).root()) }

    fn out(id: &str, owner: &str, amount: u64) -> TransactionOutput {
        TransactionOutput { stealth_address: format!("kyber.{}", root(owner)), kyber_capsule: id.into(), aes_vault: String::new(), amount }
    }

    fn exit(id: &str, owner: &str, amount: u64) -> TransactionOutput {
        TransactionOutput { stealth_address: format!("{}kyber.{}", EXIT_PREFIX, root(owner)), kyber_capsule: id.into(), aes_vault: String::new(), amount }
    }

    /// Transaction VRAIMENT signée (hypertree) par la sous-adresse `owner`
    fn tx(owner: &str, inputs: &[(&str, u64)], outputs: Vec<TransactionOutput>) -> Transaction {
        let mut k = keys(owner);
        let mut t = Transaction {
            tx_type: TransactionType::Standard,
            inputs: inputs.iter().map(|(id, a)| TransactionInput { utxo_id: id.to_string(), amount: *a, source_height: 0, owner_otp: String::new(), owner: 0 }).collect(),
            outputs, fee: 0, wots_signature: None, public_key: String::new(),
            owners: vec![OwnerProof { root: hex::encode(k.root()), signature: None }],
        };
        let msg = t.hash_data();
        t.owners[0].signature = Some(k.sign(0, &msg, |_| false, |_| true).unwrap());
        t
    }

    /// Un L2 jouet : 2 dépôts, un paiement, un retrait vers le L1
    fn scenario(tree: &mut L2Tree) -> TraceBuilder<'_> {
        let mut b = TraceBuilder::new(tree, 0);
        b.deposit(out(&deposit_id("WNS", 0), "R1", 100)).unwrap();
        b.deposit(out(&deposit_id("WNS", 1), "R2", 50)).unwrap();
        b.tx(tx("R1", &[("DEPOSIT_WNS_0", 100)], vec![out("a", "R2", 60), out("b", "R1", 40)])).unwrap();
        b.tx(tx("R2", &[("DEPOSIT_WNS_1", 50)], vec![exit("x", "R2", 30), out("c", "R2", 20)])).unwrap();
        b
    }

    #[test]
    fn chaque_etape_construite_est_valide_et_enchainee() {
        let mut tree = L2Tree::new();
        let genesis = tree.root();
        let b = scenario(&mut tree);
        assert_eq!(b.steps.len(), 4);
        for (s, w) in b.steps.iter().zip(&b.witnesses) { assert_eq!(verify_step(s, w), Verdict::Valid); }
        for p in b.steps.windows(2) { check_link(&p[0], &p[1]).unwrap(); }
        check_first(&b.steps[0], &genesis, 0).unwrap();
        let last = b.steps.last().unwrap();
        check_last(last, 4, &last.root_after, 2, 1).unwrap();
        assert_eq!(last.root_after.sum, 150 - 30);                       // le retrait a quitté le L2
        assert_eq!(b.exits.len(), 1);
        assert_eq!(b.exits[0].amount, 30);

        let root = trace_root(&b.steps);
        for s in &b.steps { assert!(verify_trace(&root, s, &trace_path(&b.steps, s.index))); }
        let mut faux = b.steps[2].clone(); faux.exits_before = 9;         // étape modifiée après coup
        assert!(!verify_trace(&root, &faux, &trace_path(&b.steps, 2)));
    }

    #[test]
    fn le_constructeur_refuse_sans_toucher_l_arbre() {
        let mut tree = L2Tree::new();
        let mut b = scenario(&mut tree);
        let before = b.tree.root();
        assert!(b.tx(tx("R1", &[("zzz", 10)], vec![out("y", "R1", 10)])).is_err());          // argent de rien
        assert!(b.tx(tx("R1", &[("a", 60)], vec![out("y", "R1", 60)])).is_err());            // "a" est à R2
        assert!(b.tx(tx("R2", &[("a", 61)], vec![out("y", "R2", 61)])).is_err());            // montant gonflé
        assert!(b.tx(tx("R2", &[("a", 60)], vec![out("DEPOSIT_WNS_9", "R2", 60)])).is_err()); // id réservé
        let mut f = tx("R2", &[("a", 60)], vec![out("y", "R2", 59)]); f.fee = 1;
        assert!(b.tx(f).is_err());                                                            // frais sur le L2
        assert_eq!(b.tree.root(), before);
    }

    /// Construit l'étape d'un séquenceur TRICHEUR (sans les contrôles du constructeur)
    fn cheat(tree: &mut L2Tree, t: Transaction, index: u64) -> (Step, Vec<OpWitness>) {
        let before = tree.root();
        let mut wit = Vec::new();
        for i in &t.inputs { wit.push(OpWitness::Remove(tree.remove(&i.utxo_id).unwrap().1)); }
        for o in t.outputs.iter().filter(|o| !is_exit(o)) { wit.push(OpWitness::Insert(tree.insert(o.clone()).unwrap())); }
        let step = Step { index, kind: StepKind::Tx(t), root_before: before, root_after: tree.root(), deposits_before: 2, exits_before: 1 };
        (step, wit)
    }

    #[test]
    fn le_l1_prouve_chaque_fraude() {
        let mut tree = L2Tree::new();
        let b = scenario(&mut tree);
        let (steps, wits) = (b.steps.clone(), b.witnesses.clone());
        drop(b);
        let honest = (steps[2].clone(), wits[2].clone());

        // 1. Vol : R1 dépense la sortie "a" de R2 (l'arbre l'accepte, la règle non)
        let (s, w) = cheat(&mut tree.clone(), tx("R1", &[("a", 60)], vec![out("vol", "R1", 60)]), 4);
        assert!(matches!(verify_step(&s, &w), Verdict::Fraud(_)));

        // 2. Argent de rien : une entrée qui n'existe pas, prouvée absente
        let t = tx("R1", &[("fantome", 1_000)], vec![out("riche", "R1", 1_000)]);
        let (low, low_path) = tree.prove_absent("fantome").unwrap();
        let s = Step { index: 4, kind: StepKind::Tx(t), root_before: tree.root(), root_after: tree.root(), deposits_before: 2, exits_before: 1 };
        assert!(matches!(verify_step(&s, &[OpWitness::Absent { low, low_path }]), Verdict::Fraud(_)));

        // 3. Racine d'arrivée falsifiée
        let mut s = honest.0.clone(); s.root_after.sum += 1;
        assert_eq!(verify_step(&s, &honest.1), Verdict::Fraud("racine d'arrivée fausse".into()));

        // 4. Frais sur le L2 : jugé sans même regarder l'arbre
        let mut s = honest.0.clone();
        if let StepKind::Tx(t) = &mut s.kind { t.fee = 1; t.outputs[0].amount -= 1; }
        assert!(matches!(verify_step(&s, &honest.1), Verdict::Fraud(_)));

        // 5. Contestation bidon d'une étape honnête : témoins d'une autre étape → rejetée
        assert!(matches!(verify_step(&honest.0, &wits[3]), Verdict::BadWitness(_)));
        assert!(matches!(verify_step(&honest.0, &[]), Verdict::BadWitness(_)));
    }
}
