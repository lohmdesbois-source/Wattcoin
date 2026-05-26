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

### 4. 🧅 Routing Tor Intégré
Le nœud embarque `arti-client` (Rust native Tor). Tout le trafic P2P peut être routé via des circuits Onion pour empêcher le traçage IP.

### 5. ⚡ Layer 2 (L2) Intégré au Core
Architecture d'un environnement d'exécution L2 directement dans le nœud, visant des **blocs de 1 seconde** pour des micro-transactions instantanées.

## 🚀 Installation & Lancement

### Prérequis
* Chaîne d'outils Rust (Edition 2021+)
* Minimum 4Go de RAM (8Go+ recommandés)

### Compilation
```bash
git clone [https://github.com/lohmdesbois-source/wattcoin.git](https://github.com/lohmdesbois-source/wattcoin.git)
cd wattcoin
cargo build --release