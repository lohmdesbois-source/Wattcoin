🛡️ PHASE 1 : La Forteresse Cryptographique (L1 Security) - [Priorité Actuelle]
On a un bel emballage, il faut maintenant le remplir avec du béton armé.

Ring Signatures (Cercle de signatures) : Implémenter le vrai mélange mathématique pour cacher l'expéditeur parmi les leurres (les fameux decoys qu'on a rebranchés).

Stealth Addresses strictes : S'assurer que le calcul Diffie-Hellman avec Kyber génère bien une adresse unique indéchiffrable par le reste du réseau.

Bulletproofs / ZK-SNARKs (Confidentialité des montants) : Actuellement, le montant est caché dans le coffre AES, mais le réseau a besoin de vérifier que tu ne crées pas d'argent à partir de rien (Lattice Commitments réels).



🤝 PHASE 2 : Le DEX 100% Trustless (L1 Decentralization)
Fini les bots d'injection, place au monde réel.

Retirer la génération automatique d'ordres.

Lancer deux Wallets séparés sur la même machine.

Faire un vrai dépôt de liquidité, placer un ordre d'achat sur le Wallet A, un ordre de vente sur le Wallet B, et laisser le Nœud Relais jouer les entremetteurs.

Vérifier le bon déroulement de l'Atomic Swap L1 (Bitcoin <-> Wattcoin) entre les deux entités.



⚡ PHASE 3 : Le "Lightning" Wattcoin (L2 Scalability)
Une fois que le L1 est inattaquable et que les swaps fonctionnent, on passe à la vitesse supérieure.

Création de canaux de paiement (State Channels) hors-chaîne.

Transactions instantanées et gratuites en contournant le minage RandomX.

Fermeture du canal et règlement final sur la blockchain (L1).