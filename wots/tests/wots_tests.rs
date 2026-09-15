#[cfg(test)]
mod tests {
    use wots::Wots;
    use sha2::{Sha256, Digest};

    #[test]
    fn test_wots_sign_and_verify() {
        let master_seed = b"ma_super_master_seed_secrete_12345";
        let index = 0u64;

        let (sk, pk) = Wots::generate_keypair(master_seed, index);

        let message = b"Transaction data to protect post-quantically";
        let message_hash: [u8; 32] = Sha256::digest(message).into();

        let signature = Wots::sign(&sk, index, &message_hash, &pk);

        assert!(Wots::verify(&signature, &message_hash), "La signature WOTS+ devrait être valide !");
    }

    #[test]
    fn test_wots_prevent_reuse_or_tampering() {
        let master_seed = b"ma_super_master_seed_secrete_12345";
        let (sk, pk) = Wots::generate_keypair(master_seed, 1);

        let msg1: [u8; 32] = Sha256::digest(b"Message 1").into();
        let msg2: [u8; 32] = Sha256::digest(b"Message 2").into();

        let signature = Wots::sign(&sk, 1, &msg1, &pk);

        assert!(!Wots::verify(&signature, &msg2), "La signature ne doit pas valider un autre message !");
    }

    #[test]
    fn test_wots_benchmark_weights() {
        let master_seed = b"benchmark_seed";
        let utxo_counts = [1, 4, 8, 16, 32, 64];

        println!("\n--- BENCHMARK DES POIDS DES SIGNATURES WOTS+ ---");
        for &count in &utxo_counts {
            let mut total_size_bytes = 0;

            for i in 0..count {
                let (sk, pk) = Wots::generate_keypair(master_seed, i as u64);
                let msg: [u8; 32] = Sha256::digest(b"dummy tx data").into();
                let sig = Wots::sign(&sk, i as u64, &msg, &pk);

                // Poids d'une signature = index (8) + clé publique (1088) + signature (1088)
                let sig_size = 8 + sig.public_key.len() + sig.signature_bytes.len();
                total_size_bytes += sig_size;
            }

            println!(
                "Tx avec {:2} UTXOs : {} octets (soit {:.2} Ko)",
                count,
                total_size_bytes,
                total_size_bytes as f64 / 1024.0
            );
        }
        println!("------------------------------------------------\n");
    }
}