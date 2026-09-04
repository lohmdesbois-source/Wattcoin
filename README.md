# ⚡ Wattcoin Network (WATT)

> **A Post-Quantum, UTXO-based, Privacy Layer 1 built from scratch in Rust.**
> *No Pre-mine. No ICO. No Smart Contract Honeypots. Pure Cypherpunk Engineering.*

![Rust](https://img.shields.io/badge/rust-v1.75+-blue.svg)
![License](https://img.shields.io/badge/license-GPLv3-blue.svg)
![Status](https://img.shields.io/badge/status-Experimental-orange.svg)

## 📖 Abstract

Wattcoin is an experimental Layer-1 blockchain designed to solve the critical vulnerabilities of modern account-based networks (like the EVM). Built from scratch in Rust, Wattcoin discards the complex and insecure smart contract model in favor of a strict **UTXO model**, native **HTLCs**, and **Lattice-based Post-Quantum Cryptography**.

## 🏗️ Architecture & Fonctionnalités

### 1. 🛡️ Confidentialité Post-Quantique (LWE Ring Signatures)
Wattcoin implémente la confidentialité nativement via la cryptographie sur les réseaux euclidiens (Learning With Errors).
* **Stealth Addresses:** Adresses masquées et intraçables.
* **LWE Commitments:** Les montants des transactions sont vérifiés de manière homomorphe.
* **PQ Ring Signatures:** Protection de l'expéditeur au niveau du protocole de base.

### 2. 🌊 DEX Natif On-Chain (Frequent Batch Auctions)
Pas de "Liquidity Pools" vulnérables ici. Le moteur d'échange est intégré au consensus.
* **Dark Pool Mempool:** Les ordres sont propagés via le réseau P2P.
* **Prix d'équilibre (On-Chain):** Les mineurs calculent le prix de vente exact lors de la création du bloc via une enchère par lots.
* **Settlement Trustless:** Les échanges atomiques (WATT/BTC) sont garantis par des contrats HTLC natifs.

### 3. ⛏️ PoW Résistant aux ASICs avec "Asynchronous Warm-Up"
Sécurisé par **RandomX**. Pour éviter les temps d'arrêt lors du changement d'époque, le dataset (2Go+) est pré-calculé en RAM sur un thread séparé (`tokio::task::spawn_blocking`) avant la transition. Le minage est ininterrompu.

### 4. 🧅 Mixnet PQC Intégré (Routage Oignon Natif)
Le nœud embarque son propre réseau Mixnet post-quantique. Tout le trafic P2P peut être routé via des paquets en oignon chiffrés (AES-256-GCM) et scellés par des capsules Kyber pour empêcher le traçage IP.

### 5. ⚡ Layer 2 (L2) Intégré au Core
Architecture d'un environnement d'exécution L2 directement dans le nœud, visant des **micro-blocs de 1 seconde** séquencés par le mineur gagnant pour des micro-transactions quasi instantanées.

### 6. 🤝 Minage P2Pool Natif (Anti-Fermes)
Le protocole intègre la coopérative de minage au cœur du consensus. Lorsqu'un bloc est trouvé, le "Finder" reçoit rigoureusement 20 % de la récompense, et les 80 % restants sont distribués de manière équitable aux autres mineurs ayant soumis des preuves de travail partielles. Les fermes de minage sont pénalisées algorithmiquement en cas de hashrate abusif (Effet Robin des Bois).

### 7. 🎰 Cyber-Jackpot L1 Intégré
Une taxe inaltérable de 1 % sur tous les frais du réseau (L1 et L2) alimente une cagnotte commune. Le protocole tire au sort un ticket (achetable on-chain) via un oracle VRF et redistribue les fonds de manière autonome via une transaction LotteryPayout générée par le consensus.

## 🚀 Installation & Lancement

### Prérequis
* Chaîne d'outils Rust (Edition 2021+)
* Minimum 4Go de RAM (8Go+ recommandés)

### Compilation

Pour le Nœud Core et le Wallet :
```bash
git clone [https://github.com/lohmdesbois-source/wattcoin.git](https://github.com/lohmdesbois-source/wattcoin.git)
#Pour le Nœud Core :
cd wattcoin/core-node
cargo build --release
# Compilation croisée pour Windows :
# cargo build --release --target x86_64-pc-windows-gnu

#Pour le Wallet :
cd ../wattcoin-mobile
cargo build --release
# Compilation croisée pour Windows :
# cargo build --release --target x86_64-pc-windows-gnu
