import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./App.css";

function App() {
  const [view, setView] = useState("loading"); 
  const [walletData, setWalletData] = useState(null);
  const [password, setPassword] = useState("");
  const [restorePhrase, setRestorePhrase] = useState("");
  const [error, setError] = useState("");
  
  const [wattBalance, setWattBalance] = useState(0.0);
  const [btcBalance, setBtcBalance] = useState(0.0);
  const [btcUsdPrice, setBtcUsdPrice] = useState(0.0); 
  const [globalWattPriceSats, setGlobalWattPriceSats] = useState(0);
  
  const [activeCoinModal, setActiveCoinModal] = useState(null); 
  const [activeContractAddress, setActiveContractAddress] = useState(null);
  
  const [swapProgress, setSwapProgress] = useState(0);
  const [showSeed, setShowSeed] = useState(false);
  const [copied, setCopied] = useState("");
  
  const [orderType, setOrderType] = useState("buy");
  const [orderAmount, setOrderAmount] = useState("");
  const [orderTotalBtc, setOrderTotalBtc] = useState("");
  const [countdown, setCountdown] = useState(120); 
  const [darkPool, setDarkPool] = useState([]);
  const [pendingSwaps, setPendingSwaps] = useState([]);
  
  const [sendAddress, setSendAddress] = useState("");
  const [sendAmount, setSendAmount] = useState("");

  const [manualSwapHash, setManualSwapHash] = useState(""); // 💡 NOUVEAU : Pour que Bob s'aligne
  const [manualSwapAmount, setManualSwapAmount] = useState("");
  const [isProcessing, setIsProcessing] = useState(false);
  const [txHistory, setTxHistory] = useState([]);

  const handleCopy = (e, text, type) => {
    e.stopPropagation(); 
    navigator.clipboard.writeText(text);
    setCopied(type);
    setTimeout(() => setCopied(""), 2000);
  };

  useEffect(() => {
    async function checkVault() {
      const exists = await invoke("vault_exists");
      if (exists) { setView("unlock"); } else { setView("onboarding"); }
    }
    checkVault();

    // 💡 NOUVEAU : Récupération du prix du Bitcoin en USD (CoinGecko API Libre)
    fetch("https://api.coingecko.com/api/v3/simple/price?ids=bitcoin&vs_currencies=usd")
      .then(res => res.json())
      .then(data => {
        if(data && data.bitcoin) setBtcUsdPrice(data.bitcoin.usd);
      }).catch(err => console.warn("Erreur Prix USD:", err));
  }, []);

  useEffect(() => {
    if (view !== "dex" && view !== "dashboard" && view !== "swaps") return;

    const updateData = async () => {
      if (!walletData) return;
      try {
        const balWATT = await invoke("get_watt_balance", { keys: walletData });
        setWattBalance(balWATT);
      } catch (e) { console.error(e); }
	  
	  try {
        const hist = await invoke("get_history", { keys: walletData });
        setTxHistory(hist);
      } catch (e) { console.error("Erreur historique:", e); }
      
      try {
        const balBTC = await invoke("get_btc_balance", { masterSeedHex: walletData.master_seed_hex });
        setBtcBalance(balBTC);
      } catch (e) { console.error(e); }
      
      if (view === "dex") {
        try {
          const pool = await invoke("get_dark_pool");
          setDarkPool(pool);
        } catch (e) { console.error(e); }
      }

      try {
        const swaps = await invoke("get_active_swaps", { 
          btcAddress: walletData.btc_address, 
          wattAddress: walletData.watt_address 
        });
        setPendingSwaps(swaps);
      } catch (e) { console.error("Erreur Swaps:", e); }
    };

    updateData();

    let unlisten;
    const setupListener = async () => {
      unlisten = await listen("network-update", () => {
        // 💡 FETCH DU PRIX GLOBAL LORS D'UN NOUVEAU BLOC
        fetch("http://80.78.26.243:8100/info")
          .then(res => res.json())
          .then(data => {
             if(data.last_price_sats) setGlobalWattPriceSats(data.last_price_sats);
          }).catch(()=>{});

        updateData();
      });
    };
    setupListener();

    // 💡 FETCH DU PRIX INITIAL
    fetch("http://80.78.26.243:8100/info").then(res => res.json()).then(data => {
        if(data.last_price_sats) setGlobalWattPriceSats(data.last_price_sats);
    }).catch(()=>{});

    const timerDex = setInterval(async () => {
      if (view === "dex") {
        setCountdown((prev) => {
          if (prev <= 1) {
            invoke("resolve_batch").then(result => {
               if(result.success) {
                 setPendingSwaps(result.swaps);
                 // 💡 L'alerte agaçante a été supprimée ici !
                 setGlobalWattPriceSats(result.clearing_price_sats); // MAJ INSTANTANÉE DU PRIX
               }
            });
            return 120;
          }
          return prev - 1;
        });
      }
    }, 1000);

    return () => { clearInterval(timerDex); if (unlisten) unlisten(); };
  }, [view, walletData]);

  const handleUnlock = async () => {
    setError("");
    try {
      const res = await invoke("unlock_vault", { password: password });
      setWalletData(res);
      setView("dashboard"); 
    } catch (e) { 
      setError(e); 
    }
  };

  const handleSubmitOrder = async () => {
    if (!orderAmount || !orderTotalBtc) return;
    
    const amountWATT = parseFloat(orderAmount);
    const totalBTC = parseFloat(orderTotalBtc);

    if (amountWATT <= 0 || totalBTC <= 0) {
      alert("Les montants doivent être supérieurs à zéro.");
      return;
    }

    if (orderType === "buy" && totalBTC > btcBalance) {
      alert(`❌ Fonds insuffisants ! Il vous faut ${totalBTC.toFixed(8)} BTC, mais vous n'avez que ${btcBalance.toFixed(8)} BTC.`);
      return;
    }
    
    // 💡 Le backend Rust attend le prix UNITAIRE en BTC. On le calcule ici en silence !
    const unitPriceBtc = totalBTC / amountWATT;

    await invoke("submit_order", {
      orderType: orderType, 
      amount: amountWATT, 
      price: unitPriceBtc, // Envoi du prix unitaire calculé
      btcAddress: walletData.btc_address, 
      btcPubkey: walletData.btc_pubkey_hex, 
      wattAddress: walletData.watt_address
    });
    
    setOrderAmount(""); setOrderTotalBtc("");
    try {
      const pool = await invoke("get_dark_pool");
      setDarkPool(pool);
    } catch (e) { console.error(e); }
  };

  const Sidebar = ({ activeTab }) => (
    <nav className="sidebar">
      <h2 className="logo">WATTCOIN</h2>
      <ul className="nav-links">
        <li className={activeTab === "dashboard" ? "active" : ""} onClick={() => setView("dashboard")}>🔑 Portefeuilles</li>
        <li className={activeTab === "dex" ? "active" : ""} onClick={() => setView("dex")}>⚖️ DEX (FBA)</li>
        <li className={activeTab === "swaps" ? "active" : ""} onClick={() => setView("swaps")}>🔗 Atomic Swaps</li>
        <li className={activeTab === "history" ? "active" : ""} onClick={() => setView("history")}>📜 Historique</li>
        <li className={activeTab === "settings" ? "active" : ""} onClick={() => setView("settings")}>⚙️ Paramètres</li>
        <li onClick={() => { setWalletData(null); setView("unlock"); }}>🔒 Verrouiller</li>
      </ul>
    </nav>
  );
  
  const handleSendTransaction = async () => {
    if (!sendAddress || !sendAmount) {
      alert("Veuillez remplir l'adresse et le montant.");
      return;
    }
    
    if (isProcessing) return;
    setIsProcessing(true);

    try {
      if (activeCoinModal === "WATT") {
        const response = await invoke("send_wattcoin", {
          recipientKyberHex: sendAddress, 
          amount: parseFloat(sendAmount),
          senderDilithiumSecretHex: walletData.dilithium_secret_hex, 
          senderDilithiumPublicHex: walletData.dilithium_public_hex,
          senderKyberSecretHex: walletData.kyber_secret_hex,
          senderKyberPublicHex: walletData.watt_address,
          htlcHashHex: null, 
          htlcTimeout: null  
        });
        alert(response);
        
        setWattBalance(prev => prev - parseFloat(sendAmount));
        setActiveCoinModal(null); 
        setSendAddress(""); setSendAmount(""); 
      } else if (activeCoinModal === "BTC") {
        // 💡 NOUVEAU : Envoi Direct de Bitcoin !
        const response = await invoke("send_btc_direct", {
          masterSeedHex: walletData.master_seed_hex,
          recipientAddress: sendAddress,
          amountBtc: parseFloat(sendAmount)
        });
        alert(response);
        
        setActiveCoinModal(null);
        setSendAddress(""); setSendAmount("");
      }
    } catch (error) {
      alert(error);
    } finally {
      setIsProcessing(false);
    }
  };
  
  const handleFundSwap = async (swap) => {
    try {
      const contractAddress = await invoke("create_btc_htlc", {
        buyerPubkeyHex: swap.buyer_btc_pubkey,
        sellerPubkeyHex: swap.seller_btc_pubkey,
        secretHex: swap.htlc_secret, // 💡 CORRIGÉ : On passe le secret
        locktime: 144 
      });
      setActiveContractAddress(contractAddress);
    } catch (error) {
      alert("Erreur lors de la création du contrat : " + error);
    }
  };

  // 💡 Bob verrouille ses WATT en utilisant directement le contrat matché par le DEX
  const handleBobLockWatt = async (swap) => {
    if (isProcessing) return; // Anti double-clic !
    setIsProcessing(true);
    try {
      await invoke("send_wattcoin", {
        recipientKyberHex: walletData.watt_address, 
        amount: swap.watt_amount_flames / 1000000000,
        senderDilithiumSecretHex: walletData.dilithium_secret_hex,
        senderDilithiumPublicHex: walletData.dilithium_public_hex,
        senderKyberSecretHex: walletData.kyber_secret_hex,
        senderKyberPublicHex: walletData.watt_address,
        htlcHashHex: swap.htlc_hash, 
        htlcTimeout: 144
      });
      alert("🔒 WATT verrouillés sur la blockchain WATT ! Alice peut maintenant réclamer.");
    } catch (e) { 
      alert("Erreur Lock WATT : " + e); 
    } finally {
      setIsProcessing(false); // On libère le bouton
    }
  };

  if (view === "loading") return <div className="onboarding-screen"><h1>Chargement...</h1></div>;

  if (view === "onboarding") { /* ... Reste identique ... */
    const handleCreateWallet = async () => {
      try {
        const res = await invoke("generate_pro_wallet", { phraseOption: restorePhrase ? restorePhrase : null });
        await invoke("encrypt_vault", { password: password, keysJsonString: JSON.stringify(res) });
        setWalletData(res);
        setView("dashboard"); 
      } catch (e) {
        alert("Erreur de création : " + e);
      }
    }; 
      
    return (
      <div className="onboarding-screen">
        <h1 className="logo">WATTCOIN</h1>
        <div className="card" style={{ maxWidth: "500px", margin: "0 auto" }}>
          <h2>🏴‍☠️ Nouveau Portefeuille</h2>
          <p style={{ color: "var(--text-muted)", marginBottom: "20px" }}>Créez un nouveau coffre ou restaurez-en un.</p>
          <input type="password" placeholder="Nouveau mot de passe" value={password} onChange={(e) => setPassword(e.target.value)} style={{ marginBottom: "10px", width: "100%" }} />
          <input type="text" placeholder="Phrase de restauration (Laisser vide pour créer)" value={restorePhrase} onChange={(e) => setRestorePhrase(e.target.value)} style={{ marginBottom: "20px", width: "100%" }} />
          <button onClick={handleCreateWallet} className="btn-primary" style={{ width: "100%" }}>Créer / Restaurer le Coffre</button>
        </div>
      </div>
    );
  }
  
  if (view === "unlock") {
    return (
      <div className="onboarding-screen">
        <h1 className="logo">WATTCOIN</h1>
        <div className="card" style={{ maxWidth: "400px", margin: "0 auto" }}>
          <h2>🔓 Déverrouiller</h2>
          {error && <div style={{ color: "#ff4d4d", marginBottom: "15px", textAlign: "center", fontWeight: "bold" }}>{error}</div>}
          <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && handleUnlock()} />
          <button onClick={handleUnlock} style={{ width: "100%" }}>Ouvrir</button>
        </div>
      </div>
    );
  }

  if (view === "dashboard") {
    // Si le prix est à zéro (début de chaîne), le WATT ne vaut rien en USD.
    const wattBtcPrice = globalWattPriceSats / 100000000;
    const wattUsdPrice = wattBtcPrice * btcUsdPrice;

    return (
      <div className="dashboard-layout"><Sidebar activeTab="dashboard" />
        <main className="main-content">
          <header><h1>Votre Trésorerie</h1></header>
          <div className="networks-stack">
            
            {/* CARTE WATTCOIN */}
            <div className="network-card watt interactive-card" onClick={() => setActiveCoinModal('WATT')}>
              <div className="network-header">
                <h2><span style={{ color: "var(--primary)" }}>⚡</span> Wattcoin</h2>
                <span className="badge">Réseau Testnet Furtif (L1)</span>
              </div>
              <div className="address-box" style={{ fontSize: "1rem", display: "flex", justifyContent: "space-between", alignItems: "center", background: "rgba(0,0,0,0.4)" }}>
                <span style={{ fontFamily: "monospace", color: "#ccc" }}>
                  {walletData.watt_address.substring(0, 12)}...{walletData.watt_address.substring(walletData.watt_address.length - 12)}
                </span>
                <button onClick={(e) => handleCopy(e, walletData.watt_address, 'WATT')} style={{ background: "transparent", border: "none", cursor: "pointer", fontSize: "1.2rem" }} title="Copier l'adresse">
                  {copied === 'WATT' ? '✅' : '📋'}
                </button>
              </div>
              <div style={{ marginTop: "20px", fontSize: "2.5rem", fontWeight: "bold", color: "var(--text-main)" }}>
                {wattBalance.toFixed(9)} <span style={{ fontSize: "1.2rem", color: "var(--primary)" }}>WATT</span>
              </div>
              <div style={{ fontSize: "1rem", color: "#888", marginTop: "5px" }}>
                ≈ ${(wattBalance * wattUsdPrice).toFixed(2)} USD
              </div>
            </div>

            {/* CARTE BITCOIN */}
            <div className="network-card btc interactive-card" onClick={() => setActiveCoinModal('BTC')}>
              <div className="network-header">
                <h2><span style={{ color: "#F7931A" }}>₿</span> Bitcoin</h2>
                <span className="badge" style={{ background: "rgba(247, 147, 26, 0.2)", color: "#F7931A" }}>Testnet</span>
              </div>
              <div className="address-box" style={{ fontSize: "1rem", display: "flex", justifyContent: "space-between", alignItems: "center" }}>
                <span style={{ fontFamily: "monospace", color: "#ccc" }}>
                  {walletData.btc_address.substring(0, 12)}...{walletData.btc_address.substring(walletData.btc_address.length - 12)}
                </span>
                <button onClick={(e) => handleCopy(e, walletData.btc_address, 'BTC')} style={{ background: "transparent", border: "none", cursor: "pointer", fontSize: "1.2rem" }} title="Copier l'adresse">
                  {copied === 'BTC' ? '✅' : '📋'}
                </button>
              </div>
              <div style={{ marginTop: "20px", fontSize: "2.5rem", fontWeight: "bold", color: "var(--text-main)" }}>
                {btcBalance.toFixed(8)} <span style={{ fontSize: "1.2rem", color: "#F7931A" }}>BTC</span>
              </div>
              <div style={{ fontSize: "1rem", color: "#888", marginTop: "5px" }}>
                ≈ ${(btcBalance * btcUsdPrice).toFixed(2)} USD
              </div>
            </div>
            
          </div>

          {activeCoinModal && (
            <div className="modal-overlay" onClick={() => setActiveCoinModal(null)}>
              <div className="modal-content" onClick={(e) => e.stopPropagation()}>
                <div className="modal-header">
                  <h2>Envoyer du {activeCoinModal}</h2>
                  <button className="close-btn" onClick={() => setActiveCoinModal(null)}>✖</button>
                </div>
                <div className="send-form" style={{ marginTop: "10px" }}>
                  <div style={{ fontSize: "0.9rem", color: "var(--text-muted)", marginBottom: "15px", textAlign: "right" }}>
                    Solde : <span style={{ color: "var(--text-main)", fontWeight: "bold" }}>
                      {activeCoinModal === 'WATT' ? wattBalance.toFixed(9) : btcBalance.toFixed(8)}
                    </span> {activeCoinModal}
                  </div>
                  <input type="text" placeholder={`Adresse ${activeCoinModal} du destinataire`} value={sendAddress} onChange={(e) => setSendAddress(e.target.value)} style={{ width: "100%", marginBottom: "15px" }} />
                  <input type="number" placeholder={`Montant en ${activeCoinModal}`} value={sendAmount} onChange={(e) => setSendAmount(e.target.value)} style={{ width: "100%", marginBottom: "25px" }} />
                  <button 
                    className="btn-primary" 
                    disabled={isProcessing} 
                    onClick={handleSendTransaction} 
                    style={{ width: "100%", padding: "12px", fontSize: "1.1rem", opacity: isProcessing ? 0.7 : 1, cursor: isProcessing ? "not-allowed" : "pointer" }}
                  >
                    {isProcessing ? "⏳ Transaction en cours..." : "Signer & Envoyer"}
                  </button>
                  
                  {/* 💡 NOUVEAU : Le message d'attente stylé */}
                  {isProcessing && activeCoinModal === "BTC" && (
                    <div style={{ marginTop: "15px", color: "#F7931A", textAlign: "center", fontSize: "0.9rem", fontWeight: "bold" }}>
                      📡 BDK synchronise la blockchain Bitcoin...
                    </div>
                  )}
                  {isProcessing && activeCoinModal === "WATT" && (
                    <div style={{ marginTop: "15px", color: "var(--primary)", textAlign: "center", fontSize: "0.9rem", fontWeight: "bold" }}>
                      ⚡ Création de la preuve ZKP en cours...
                    </div>
                  )}
                </div>
              </div>
            </div>
          )}
        </main>
      </div>
    );
  }

  if (view === "dex") {
    const minutes = Math.floor(countdown / 60).toString().padStart(2, '0');
    const seconds = (countdown % 60).toString().padStart(2, '0');
    return (
      <div className="dashboard-layout"><Sidebar activeTab="dex" />
        <main className="main-content">
          <div className="dex-header">
            <div className="trading-pair">WATT / BTC</div>
            <div className="batch-timer">{minutes}:{seconds}</div>
          </div>
          <div style={{ marginBottom: "20px" }}>
             <button onClick={() => setCountdown(1)} className="btn-secondary">⏩ Forcer Résolution du Batch</button>
          </div>
          <div className="dex-grid">
            <div className="order-form">
              <h3>Placer un ordre</h3>
              <div className="form-tabs">
                <div className={`tab-btn buy ${orderType === "buy" ? "active" : ""}`} onClick={() => setOrderType("buy")}>Achat</div>
                <div className={`tab-btn sell ${orderType === "sell" ? "active" : ""}`} onClick={() => setOrderType("sell")}>Vente</div>
              </div>
              
              {/* 💡 CHAMP QUANTITÉ AVEC USD ESTIMÉ (basé sur le prix global actuel) */}
              <label style={{color: "#888", fontSize: "0.8rem", marginTop:"15px", display:"flex", justifyContent:"space-between"}}>
                <span>Quantité (WATT)</span>
                <span style={{color: "var(--primary)"}}>
                  ≈ ${orderAmount ? (parseFloat(orderAmount) * (globalWattPriceSats / 100000000) * btcUsdPrice).toFixed(2) : "0.00"}
                </span>
              </label>
              <input type="number" placeholder="Ex: 10" value={orderAmount} onChange={(e) => setOrderAmount(e.target.value)} />
              
              {/* 💡 CHAMP TOTAL BTC AVEC USD ESTIMÉ */}
              <label style={{color: "#888", fontSize: "0.8rem", marginTop:"10px", display:"flex", justifyContent:"space-between"}}>
                <span>Total à {orderType === "buy" ? "payer" : "recevoir"} (BTC)</span>
                <span style={{color: "#F7931A"}}>
                  ≈ ${orderTotalBtc ? (parseFloat(orderTotalBtc) * btcUsdPrice).toFixed(2) : "0.00"}
                </span>
              </label>
              <input type="number" placeholder="Ex: 0.001" value={orderTotalBtc} onChange={(e) => setOrderTotalBtc(e.target.value)} />
              
              {/* 💡 RÉTRO-AFFICHAGE DU PRIX UNITAIRE IMPLICITE (BTC et USD) */}
              {orderAmount && orderTotalBtc && parseFloat(orderAmount) > 0 && (
                <div style={{ background: "rgba(0,0,0,0.4)", padding: "10px", borderRadius: "5px", marginBottom: "15px", border: "1px solid #444", textAlign: "center" }}>
                  <span style={{color: "#888", fontSize: "0.85rem"}}>Prix unitaire implicite :</span><br/>
                  <strong style={{color: "#ccc", fontSize: "1.1rem"}}>
                    {(parseFloat(orderTotalBtc) / parseFloat(orderAmount)).toFixed(8)} BTC/WATT
                  </strong>
                  <br/>
                  <span style={{color: "#28a745", fontSize: "0.9rem"}}>
                    ≈ ${((parseFloat(orderTotalBtc) / parseFloat(orderAmount)) * btcUsdPrice).toFixed(2)} USD/WATT
                  </span>
                </div>
              )}

              <button className={`submit-order-btn ${orderType}`} style={{ width: "100%", marginTop: "10px" }} onClick={handleSubmitOrder}>
                Envoyer au Dark Pool
              </button>
            </div>
            <div className="dark-pool">
              <h3>🌊 Piscine d'ordres</h3>
              <table className="pool-table">
                <thead><tr><th>Type</th><th>WATT</th><th>BTC</th></tr></thead>
                <tbody>
                  {darkPool.map((o) => (
                    <tr key={o.id} className={o.order_type === "buy" ? "row-buy" : "row-sell"}>
                      <td>{o.order_type === "buy" ? "Achat" : "Vente"}</td>
                      <td>{o.amount_flames / 1000000000}</td>
                      <td>{(o.price_sats / 100000000).toFixed(8)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              {pendingSwaps.length > 0 && (
                <div style={{ marginTop: "20px", padding: "10px", background: "rgba(16, 185, 129, 0.1)", border: "1px solid var(--primary)", borderRadius: "8px" }}>
                  <h4 style={{ margin: 0 }}>✅ Swaps Générés pour Vous</h4>
                  {pendingSwaps.map((s, i) => <div key={i} style={{ fontSize: "0.8rem", userSelect: "all" }}>HASH: {s.htlc_hash}</div>)}
                </div>
              )}
            </div>
          </div>
        </main>
      </div>
    );
  }
  
  if (view === "swaps") {
    return (
      <div className="dashboard-layout"><Sidebar activeTab="swaps" />
        <main className="main-content">
          <header>
            <h1>Exécution des Atomic Swaps</h1>
            <p style={{ color: "var(--text-muted)" }}>Gérez vos échanges cross-chain automatiques</p>
          </header>

          <div className="dex-grid" style={{ gridTemplateColumns: "1fr" }}>
            <div className="dark-pool">
              <h3>🔒 Contrats Matchés par le DEX</h3>
              
              {pendingSwaps.length === 0 ? (
                <p style={{ color: "var(--text-muted)", textAlign: "center", padding: "20px" }}>Aucun swap en cours vous concernant.</p>
              ) : (
                <table className="pool-table">
                  <thead><tr><th>Votre Rôle</th><th>Hash HTLC</th><th>WATT</th><th>BTC</th><th>Action Requise</th></tr></thead>
                  <tbody>
                    {pendingSwaps.map((s, i) => {
                      const isAlice = s.buyer_btc_address === walletData.btc_address;
                      const isBob = s.seller_watt_address === walletData.watt_address;

                      return (
                      <tr key={i}>
                        <td>
                          {isAlice ? <span className="badge" style={{background: "#10b981"}}>Acheteur WATT</span> : 
                           isBob ? <span className="badge" style={{background: "#F7931A"}}>Vendeur WATT</span> : 
                           <span className="badge">Observateur</span>}
                        </td>
                        <td style={{ fontFamily: "monospace", fontSize: "0.8rem", color: "var(--primary)" }}>
                          {s.htlc_hash.substring(0, 16)}...
                        </td>
                        <td style={{ fontWeight: "bold" }}>{s.watt_amount_flames / 1000000000}</td>
                        <td style={{ fontWeight: "bold" }}>
                          {s.btc_amount_sats / 100000000}
                          {/* 💡 NOUVEAU : Affichage de l'estimation en USD */}
                          <div style={{fontSize: "0.8rem", color: "#aaa", fontWeight: "normal"}}>
                            ≈ ${((s.btc_amount_sats / 100000000) * btcUsdPrice).toFixed(2)}
                          </div>
                        </td>
                        <td>
                          {isAlice && (
                            <button className="btn-primary" style={{ padding: "5px 15px", fontSize: "0.9rem" }} onClick={() => handleFundSwap(s)}>
                              1️⃣ Initialiser Contrat BTC
                            </button>
                          )}
                          {isBob && (
                            <div style={{ display: "flex", gap: "5px", flexDirection: "column" }}>
                              <button 
                                className="btn-secondary" 
                                style={{ padding: "5px 15px", fontSize: "0.9rem" }} 
                                disabled={isProcessing}
                                onClick={() => handleBobLockWatt(s)}
                              >
                                {isProcessing ? "⏳ Verrouillage..." : "🔒 1. Verrouiller mes WATT"}
                              </button>

                              {/* 💡 Bouton Réclamation BTC (Avec correction appel API) */}
                              <button 
                                className="btn-primary" 
                                style={{ padding: "5px 15px", fontSize: "0.9rem" }} 
                                disabled={isProcessing}
                                onClick={async () => {
                                  try {
                                    setIsProcessing(true);
                                    
                                    // 1. On recrée l'adresse du contrat localement
                                    const contractAddress = await invoke("create_btc_htlc", {
                                      buyerPubkeyHex: s.buyer_btc_pubkey,
                                      sellerPubkeyHex: s.seller_btc_pubkey,
                                      secretHex: s.htlc_secret, // 💡 CORRIGÉ : On passe le secret
                                      locktime: 144 
                                    });

                                    // 2. ON APPELLE LE CLAIM AVEC *TOUS* LES PARAMÈTRES !
                                    const res = await invoke("claim_btc_swap", {
                                      masterSeedHex: walletData.master_seed_hex,
                                      htlcAddress: contractAddress,
                                      secretHex: s.htlc_secret,
                                      buyerPubkeyHex: s.buyer_btc_pubkey,
                                      sellerPubkeyHex: s.seller_btc_pubkey // 💡 AJOUTÉ
                                    });
                                    
                                    alert(res);
                                  } catch (e) { alert("Erreur Claim BTC : " + e); }
                                  finally { setIsProcessing(false); }
                                }}
                              >
                                {isProcessing ? "⏳ Signature L1 en cours..." : "💰 2. Réclamer les BTC (Secret)"}
                              </button>
                            </div>
                          )}
                        </td>
                      </tr>
                    )})}
                  </tbody>
                </table>
              )}

              {/* Console de commandement d'Alice (N'apparaît que si Alice a initialisé le contrat) */}
              {activeContractAddress && (
                <div style={{ marginTop: "30px", padding: "20px", background: "rgba(247, 147, 26, 0.1)", border: "1px solid #F7931A", borderRadius: "8px" }}>
                  <h3 style={{ color: "#F7931A", textAlign: "center", margin: "0 0 15px 0" }}>🔐 Coffre HTLC Bitcoin L1 (Pour Alice)</h3>
                  <div className="address-box" style={{ fontSize: "1rem", fontWeight: "bold", background: "rgba(0,0,0,0.5)", padding: "10px", wordBreak: "break-all", textAlign: "center" }}>
                    {activeContractAddress}
                  </div>

                  <div style={{ textAlign: "center", marginTop: "20px", display: "flex", gap: "10px", justifyContent: "center" }}>
					<button className="btn-primary" disabled={swapProgress > 0 || isProcessing} onClick={async () => {
                      try {
                        setIsProcessing(true); // 💡 On verrouille
                        setSwapProgress(1);
                        await invoke("send_btc_to_htlc", {
                          masterSeedHex: walletData.master_seed_hex,
                          htlcAddress: activeContractAddress,
                          amountBtc: pendingSwaps[0].btc_amount_sats / 100000000
                        });
                        setSwapProgress(2);
                        alert("✅ BTC envoyés sur le Testnet ! Attendez que le vendeur verrouille ses WATT.");
                      } catch (error) { alert("Erreur L1 : " + error); setSwapProgress(0); }
                      finally { setIsProcessing(false); } // 💡 On libère
                    }}>
                      {isProcessing && swapProgress === 1 ? "⏳ Sync BDK en cours..." : "2️⃣ Envoyer les BTC"}
                    </button>
                    <button className="btn-secondary" disabled={swapProgress < 2 || swapProgress === 4} onClick={async () => {
                      try {
                        setSwapProgress(3);
                        await invoke("claim_wattcoin_swap", { 
                          secret: pendingSwaps[0].htlc_secret, 
                          hash: pendingSwaps[0].htlc_hash,
                          wattAddress: walletData.watt_address,
                          amount: pendingSwaps[0].watt_amount_flames / 1000000000
                        });
                        setSwapProgress(4);
                      } catch (e) { 
                        alert("Erreur Claim : Le Nœud a rejeté. Le vendeur a-t-il bien verrouillé les WATT ?"); 
                        setSwapProgress(2); 
                      }
                    }}>
                      3️⃣ Réclamer les WATT
                    </button>
                  </div>

                  {swapProgress === 4 && (
                    <div className="success-banner" style={{ marginTop: "20px" }}>
                      🎉 ATOMIC SWAP RÉUSSI !<br/>
                      <span style={{fontSize: "0.9rem", color: "white"}}>Vos WATT sont débloqués sur votre portefeuille.</span>
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>
        </main>
      </div>
    );
  }
  
  if (view === "history") {
    return (
      <div className="dashboard-layout">
        <Sidebar activeTab="history" />
        <main className="main-content">
          <header>
            <h1>Historique des Transactions</h1>
            <p style={{ color: "var(--text-muted)" }}>Scanner cryptographique activé. Seuls vos fonds déchiffrés apparaissent ici.</p>
          </header>

          <div className="glass-panel" style={{ padding: "30px", maxWidth: "800px" }}>
            <div style={{ marginTop: "10px" }}>
              
              {txHistory.length === 0 ? (
                <div style={{ textAlign: "center", color: "#888", padding: "40px 0" }}>
                  Aucune transaction détectée sur la blockchain.
                </div>
              ) : (
                txHistory.map((tx, idx) => (
                  <div key={idx} style={{ display: "flex", justifyContent: "space-between", alignItems: "center", background: "#111", padding: "15px", borderRadius: "8px", marginBottom: "10px", borderLeft: "4px solid #00FF00" }}>
                    <div>
                      <div style={{ color: "#FFF", fontWeight: "bold", fontSize: "1.1rem" }}>
                        ⬇️ Reçu
                      </div>
                      <div style={{ color: "#888", fontSize: "0.9rem" }}>{tx.date} • {tx.id}</div>
                    </div>
                    <div style={{ textAlign: "right" }}>
                      <div style={{ color: "#00FF00", fontWeight: "bold", fontSize: "1.2rem" }}>
                        +{tx.amount.toFixed(4)} {tx.coin}
                      </div>
                      <div style={{ color: tx.status.includes("Dépensé") ? "#FF3333" : "#888", fontSize: "0.8rem", fontWeight: "bold" }}>
                        {tx.status}
                      </div>
                    </div>
                  </div>
                ))
              )}

            </div>
          </div>
        </main>
      </div>
    );
  }

  if (view === "settings") {
    
    // 💡 NOUVEAU : Appel du backend Rust pour créer le fichier
    const handleDownloadMinerScript = async (os) => {
      try {
        const address = walletData.watt_address;
        const message = await invoke("save_miner_script", { os: os, address: address });
        alert("✅ " + message);
      } catch (error) {
        alert("❌ Erreur : " + error);
      }
    };

    return (
      <div className="dashboard-layout">
        <Sidebar activeTab="settings" />
        <main className="main-content">
          <header>
            <h1>Paramètres de Sécurité</h1>
            <p style={{ color: "var(--text-muted)" }}>Gestion du coffre-fort et des sauvegardes</p>
          </header>

          <div className="glass-panel" style={{ padding: "30px", maxWidth: "800px" }}>
            <div className="security-section">
              <h3 style={{ color: "var(--primary)" }}>🔑 Phrase de Récupération (Seed)</h3>
              <p style={{ color: "#AAA" }}>Cette phrase de 24 mots est la clé maîtresse absolue de tous vos fonds. Ne la montrez jamais à personne.</p>
              
              {showSeed ? (
                <div style={{ background: "#111", padding: "20px", borderRadius: "10px", border: "1px solid var(--primary)", fontFamily: "monospace", fontSize: "1.2rem", color: "#FFF", marginTop: "15px" }}>
                  {walletData.mnemonic}
                </div>
              ) : (
                <div style={{ background: "#111", padding: "20px", borderRadius: "10px", border: "1px solid #333", marginTop: "15px", display: "flex", justifyContent: "center" }}>
                  <button className="btn-secondary" onClick={() => setShowSeed(true)}>Afficher la Phrase Secrète</button>
                </div>
              )}
            </div>

            {/* 💡 NOUVEAU : SECTION SCRIPTS DE MINAGE */}
            <div className="security-section" style={{ marginTop: "40px" }}>
              <h3 style={{ color: "#10b981" }}>⛏️ Scripts de Minage</h3>
              <p style={{ color: "#AAA" }}>Téléchargez un script pré-configuré avec votre adresse de réception (3000 caractères) pour lancer votre propre nœud de minage sans faire d'erreur.</p>
              
              <div style={{ display: "flex", gap: "15px", marginTop: "15px" }}>
                <button 
                  className="btn-secondary" 
                  style={{ flex: 1, padding: "12px", border: "1px solid #10b981", color: "#10b981", background: "rgba(16, 185, 129, 0.1)" }}
                  onClick={() => handleDownloadMinerScript("linux")}
                >
                  🐧 Générer start_miner.sh (Linux/Mac)
                </button>
                <button 
                  className="btn-secondary" 
                  style={{ flex: 1, padding: "12px", border: "1px solid #3b82f6", color: "#3b82f6", background: "rgba(59, 130, 246, 0.1)" }}
                  onClick={() => handleDownloadMinerScript("windows")}
                >
                  🪟 Générer start_miner.bat (Windows)
                </button>
              </div>
            </div>

            <div className="security-section" style={{ marginTop: "50px", borderTop: "1px solid #444", paddingTop: "30px" }}>
              <h3 style={{ color: "#FF3333" }}>🚨 Zone de Danger</h3>
              <p style={{ color: "#AAA" }}>Détruire le coffre supprimera le fichier chiffré de cet ordinateur.</p>
              
              <button 
                style={{ background: "#FF3333", color: "#FFF", padding: "10px 20px", border: "none", borderRadius: "5px", cursor: "pointer", fontWeight: "bold", marginTop: "10px" }}
                onClick={async () => {
                  if(window.confirm("⚠️ Êtes-vous ABSOLUMENT certain de vouloir détruire ce coffre ?")) {
                    try {
                      await invoke("destroy_vault");
                      setWalletData(null); 
                      setView('onboarding'); 
                      alert("Coffre détruit avec succès.");
                    } catch (e) {
                      alert("Erreur lors de la destruction : " + e);
                    }
                  }
                }}
              >
                🔥 Détruire le Coffre-Fort
              </button>
            </div>
          </div>
        </main>
      </div>
    );
  }
  
  return null;
}

export default App;