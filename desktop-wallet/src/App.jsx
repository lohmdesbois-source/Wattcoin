import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Toaster, toast } from "react-hot-toast";
import { Copy, Check, Lock, Unlock, Settings, ScrollText, ArrowRightLeft, Shield, Send, Zap, Bitcoin, Trash2, Download, EyeOff } from "lucide-react";
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
  const [showSeed, setShowSeed] = useState(false);
  const [copied, setCopied] = useState("");
  
  const [orderType, setOrderType] = useState("buy");
  const [orderAmount, setOrderAmount] = useState("");
  const [orderTotalBtc, setOrderTotalBtc] = useState("");
  const [darkPool, setDarkPool] = useState([]);
  const [pendingSwaps, setPendingSwaps] = useState([]);
  
  const [sendAddress, setSendAddress] = useState("");
  const [sendAmount, setSendAmount] = useState("");

  const [isProcessing, setIsProcessing] = useState(false);
  const [txHistory, setTxHistory] = useState([]);
  const [syncMessage, setSyncMessage] = useState("Établissement du tunnel Tor...");

  // 💡 NOUVEAU : Mémoire des swaps "verrouillés/cliqués" pour protéger le bouton cacher
  const [actionnedSwaps, setActionnedSwaps] = useState(new Set());

  const handleCopy = (e, text, type) => {
    e.stopPropagation(); 
    navigator.clipboard.writeText(text);
    setCopied(type);
    toast.success("Adresse copiée !");
    setTimeout(() => setCopied(""), 2000);
  };

  useEffect(() => {
    async function checkVault() {
      const exists = await invoke("vault_exists");
      if (exists) { setView("unlock"); } else { setView("onboarding"); }
    }
    checkVault();

    fetch("https://api.binance.com/api/v3/ticker/price?symbol=BTCUSDT")
      .then(res => res.json())
      .then(data => {
        if(data && data.price) setBtcUsdPrice(parseFloat(data.price));
      }).catch(err => console.warn("Erreur Prix USD:", err));
  }, []);

  useEffect(() => {
    if (view === "syncing" && walletData) {
      const performInitialSync = async () => {
        try {
          setSyncMessage("Connexion au réseau furtif en cours...");
          const info = await invoke("get_network_info");
          if(info.last_price_sats) setGlobalWattPriceSats(info.last_price_sats);

          setSyncMessage("Déchiffrement de la blockchain...");
          const balWATT = await invoke("get_watt_balance", { keys: walletData });
          setWattBalance(balWATT);

          const hist = await invoke("get_history", { keys: walletData });
          setTxHistory(hist);

          setView("dashboard");
          toast.success("Synchronisation terminée, coffre ouvert !");

          invoke("get_btc_balance", { masterSeedHex: walletData.master_seed_hex })
            .then(balBTC => setBtcBalance(balBTC))
            .catch(btcError => console.warn("Erreur BTC en arrière-plan :", btcError));

        } catch (e) {
          console.error("Erreur de sync initiale:", e);
          setSyncMessage("⚠️ Tor est très lent ou le Nœud est hors ligne. Nouvelle tentative...");
          setTimeout(performInitialSync, 5000);
        }
      };
      performInitialSync();
    }
  }, [view, walletData]);

  useEffect(() => {
    if (view !== "dex" && view !== "dashboard" && view !== "swaps" && view !== "history") return;

    const updateData = async () => {
      if (!walletData) return;
      try { const balWATT = await invoke("get_watt_balance", { keys: walletData }); setWattBalance(balWATT); } catch (e) {}
      try { const hist = await invoke("get_history", { keys: walletData }); setTxHistory(hist); } catch (e) {}
      try { const balBTC = await invoke("get_btc_balance", { masterSeedHex: walletData.master_seed_hex }); setBtcBalance(balBTC); } catch (e) {}
      
      if (view === "dex") {
        try { const pool = await invoke("get_dark_pool"); setDarkPool(pool); } catch (e) {}
      }

      try {
        const apiSwaps = await invoke("get_active_swaps", { btcAddress: walletData.btc_address, wattAddress: walletData.watt_address });
        
        // 🛡️ SAUVEGARDE LOCALE : On ne fait pas aveuglément confiance à l'API pour l'affichage !
        const cachedStr = localStorage.getItem('my_swaps');
        const cachedSwaps = cachedStr ? JSON.parse(cachedStr) : [];
        
        const combined = [...apiSwaps];
        cachedSwaps.forEach(cached => {
            // Si le swap est dans notre cache local mais que l'API l'a supprimé, 
            // on le garde à l'écran pour pouvoir finir nos actions (réclamer BTC ou Refund) !
            if (!combined.find(s => s.htlc_hash === cached.htlc_hash)) {
                combined.push(cached);
            }
        });
        
        localStorage.setItem('my_swaps', JSON.stringify(combined));
        setPendingSwaps(combined);

      } catch (e) {}
    };
    
    updateData();

    let unlisten;
    const setupListener = async () => {
      unlisten = await listen("network-update", () => {
        invoke("get_network_info")
            .then(data => { if(data.last_price_sats) setGlobalWattPriceSats(data.last_price_sats); })
            .catch(()=>{});
        updateData();
      });
    };
    setupListener();

    const timerDex = setInterval(async () => {
      if (view === "dex") {
        try { 
          const pool = await invoke("get_dark_pool"); 
          setDarkPool(pool); 
        } catch (e) {}
      }
    }, 5000); // Rafraîchit la piscine toutes les 5 secondes

    return () => { clearInterval(timerDex); if (unlisten) unlisten(); };
  }, [view, walletData]);

  const handleUnlock = async () => {
    setError("");
    try {
      const res = await invoke("unlock_vault", { password: password });
      setWalletData(res);
      setView("syncing");
    } catch (e) { 
      setError(e); 
      toast.error(e);
    }
  };

  const handleCreateWallet = async () => {
    try {
      const res = await invoke("generate_pro_wallet", { phraseOption: restorePhrase ? restorePhrase : null });
      await invoke("encrypt_vault", { password: password, keysJsonString: JSON.stringify(res) });
      setWalletData(res);
      setView("syncing");
    } catch (e) {
      toast.error("Erreur de création : " + e);
    }
  }; 

  const handleSubmitOrder = async () => {
    if (!orderAmount || !orderTotalBtc) return;
    if (isProcessing) return; 
    
    const amountWATT = parseFloat(orderAmount);
    const totalBTC = parseFloat(orderTotalBtc);

    if (amountWATT <= 0 || totalBTC <= 0) { toast.error("Les montants doivent être supérieurs à zéro."); return; }
    if (orderType === "buy" && totalBTC > btcBalance) { toast.error("Fonds BTC insuffisants !"); return; }
    if (orderType === "sell" && amountWATT > wattBalance) { toast.error("Fonds WATT insuffisants !"); return; }
    
    const unitPriceBtc = totalBTC / amountWATT;
    const loadingToast = toast.loading("Envoi de l'ordre via Tor...");
    setIsProcessing(true);

    try {
      await invoke("submit_order", {
        orderType: orderType, amount: amountWATT, price: unitPriceBtc, 
        btcAddress: walletData.btc_address, btcPubkey: walletData.btc_pubkey_hex, wattAddress: walletData.watt_address
      });
      setOrderAmount(""); setOrderTotalBtc("");
      const pool = await invoke("get_dark_pool");
      setDarkPool(pool);
      toast.success("Ordre ajouté au Dark Pool !", { id: loadingToast });
    } catch (e) { 
      toast.error(e, { id: loadingToast }); 
    } finally {
      setIsProcessing(false);
    }
  };
  
  const handleCancelOrder = async (id) => {
    const toastId = toast.loading("Annulation de l'ordre...");
    try {
      await invoke("cancel_order", { orderId: id });
      const pool = await invoke("get_dark_pool");
      setDarkPool(pool);
      toast.success("Ordre retiré avec succès !", { id: toastId });
    } catch (e) {
      toast.error("Erreur : " + e, { id: toastId });
    }
  };

  const handleSendTransaction = async () => {
    if (!sendAddress || !sendAmount) return;
    if (isProcessing) return;
    setIsProcessing(true);
    const loadingToast = toast.loading("Signature et routage Tor en cours...");

    try {
      if (activeCoinModal === "WATT") {
        const response = await invoke("send_wattcoin", {
          recipientKyberHex: sendAddress, amount: parseFloat(sendAmount),
          senderDilithiumSecretHex: walletData.dilithium_secret_hex, senderDilithiumPublicHex: walletData.dilithium_public_hex,
          senderKyberSecretHex: walletData.kyber_secret_hex, senderKyberPublicHex: walletData.watt_address,
          htlcHashHex: null, htlcTimeout: null  
        });
        toast.success(response, { id: loadingToast, duration: 5000 });
        setWattBalance(prev => prev - parseFloat(sendAmount));
        setActiveCoinModal(null); setSendAddress(""); setSendAmount(""); 
      } else if (activeCoinModal === "BTC") {
        const response = await invoke("send_btc_direct", {
          masterSeedHex: walletData.master_seed_hex, recipientAddress: sendAddress, amountBtc: parseFloat(sendAmount)
        });
        toast.success(response, { id: loadingToast, duration: 5000 });
        setActiveCoinModal(null); setSendAddress(""); setSendAmount("");
      }
    } catch (error) { 
      toast.error(error, { id: loadingToast }); 
    } finally { 
      setIsProcessing(false); 
    }
  };

  // --- ACTIONS DU SWAP ---

  const handleBobLockWatt = async (swap) => {
    const expectedPriceBtc = swap.btc_amount_sats / swap.watt_amount_flames;
    const userConfirmed = window.confirm(
        `🚨 VÉRIFICATION DE SÉCURITÉ 🚨\n\nLe Nœud Relais propose l'échange suivant :\n- Vous bloquez : ${swap.watt_amount_flames / 1000000000} WATT\n- Vous recevrez : ${(swap.btc_amount_sats / 100000000).toFixed(8)} BTC\nPrix unitaire : ${expectedPriceBtc.toFixed(8)} BTC/WATT\n\nSi ce prix ne correspond pas à votre ordre initial, ANNULEZ.`
    );
    if (!userConfirmed) return;

    if (isProcessing) return;
    setIsProcessing(true);
    const loadingToast = toast.loading("Verrouillage Post-Quantique en cours...");

    try {
      const response = await invoke("send_wattcoin", {
        recipientKyberHex: swap.seller_watt_address, amount: swap.watt_amount_flames / 1000000000,
        senderDilithiumSecretHex: walletData.dilithium_secret_hex, senderDilithiumPublicHex: walletData.dilithium_public_hex,
        senderKyberSecretHex: walletData.kyber_secret_hex, senderKyberPublicHex: walletData.watt_address,
        htlcHashHex: swap.htlc_hash, htlcTimeout: 144                 
      });
      toast.success(response, { id: loadingToast, duration: 5000 });
      setActionnedSwaps(prev => new Set(prev).add(swap.htlc_hash));
      setWattBalance(prev => prev - (swap.watt_amount_flames / 1000000000));
    } catch (error) { toast.error(error, { id: loadingToast }); } 
    finally { setIsProcessing(false); }
  };

  const handleRefundWatt = async (swap) => {
    if (isProcessing) return;
    setIsProcessing(true);
    const loadingToast = toast.loading("Demande de remboursement envoyée (Timelock check)...");

    try {
      const response = await invoke("refund_wattcoin_swap", {
        hash: swap.htlc_hash, 
        wattAddress: walletData.watt_address, 
        amount: swap.watt_amount_flames / 1000000000
      });
      toast.success(response, { id: loadingToast, duration: 5000 });
      toast.success("N'oubliez pas de miner un bloc pour récupérer vos WATT.");
      setActionnedSwaps(prev => new Set(prev).add(swap.htlc_hash));
    } catch (error) { toast.error(error.toString(), { id: loadingToast }); } 
    finally { setIsProcessing(false); }
  };

  const removeSwapFromCache = (hash) => {
      if (!actionnedSwaps.has(hash)) {
          toast.error("🚨 Action requise ! Effectuez une transaction (Réclamation ou Refund) avant d'effacer ce contrat.");
          return;
      }
      const cached = JSON.parse(localStorage.getItem('my_swaps') || '[]');
      const updated = cached.filter(s => s.htlc_hash !== hash);
      localStorage.setItem('my_swaps', JSON.stringify(updated));
      setPendingSwaps(updated);
      toast.success("Swap nettoyé de l'interface !");
  };

  // ================= UI COMPONENTS =================

  const Sidebar = ({ activeTab }) => (
    <nav className="sidebar">
      <h2 className="logo">WATTCOIN</h2>
      <ul className="nav-links">
        <li className={activeTab === "dashboard" ? "active" : ""} onClick={() => setView("dashboard")}><Lock size={18}/> Portefeuilles</li>
        <li className={activeTab === "dex" ? "active" : ""} onClick={() => setView("dex")}><ArrowRightLeft size={18}/> DEX (FBA)</li>
        <li className={activeTab === "swaps" ? "active" : ""} onClick={() => setView("swaps")}><Shield size={18}/> Atomic Swaps</li>
        <li className={activeTab === "history" ? "active" : ""} onClick={() => setView("history")}><ScrollText size={18}/> Historique</li>
        <li className={activeTab === "settings" ? "active" : ""} onClick={() => setView("settings")}><Settings size={18}/> Paramètres</li>
        <li onClick={() => { setWalletData(null); setView("unlock"); toast.success("Coffre verrouillé"); }} style={{marginTop: "auto", color: "#ef4444"}}><Lock size={18}/> Verrouiller</li>
      </ul>
    </nav>
  );

  // ================= CALCULS DE VALEURS (DASHBOARD) =================
  const wattBtcPrice = globalWattPriceSats / 100000000;
  const wattUsdPrice = wattBtcPrice * btcUsdPrice;
  const totalWattValueUsd = wattBalance * wattUsdPrice;
  const totalBtcValueUsd = btcBalance * btcUsdPrice;
  const grandTotalUsd = totalWattValueUsd + totalBtcValueUsd;

  // ================= MAIN RENDER =================

  return (
    <>
      <Toaster 
        position="bottom-right" 
        toastOptions={{ 
          // 💡 Configuration globale
          style: { background: '#1a1d24', color: '#fff', border: '1px solid #00F0FF', fontFamily: 'Inter' },
          
          // 💡 Temps d'affichage pour les SUCCÈS (5 secondes)
          success: { 
            duration: 5000,
            iconTheme: { primary: '#00F0FF', secondary: '#000' } 
          },
          
          // 💡 Temps d'affichage pour les ERREURS (8 secondes pour bien lire !)
          error: {
            duration: 8000,
            style: { border: '1px solid #ef4444' } // Bordure rouge pour les erreurs
          }
        }} 
      />

      {view === "loading" && <div className="onboarding-screen"><h1>Chargement...</h1></div>}

      {view === "syncing" && (
        <div className="onboarding-screen">
          <div className="card" style={{ maxWidth: "500px", margin: "0 auto", textAlign: "center" }}>
            <h2 style={{ color: "var(--primary)", marginBottom: "15px", display: "flex", alignItems: "center", justifyContent: "center", gap: "10px" }}><Shield /> Initialisation</h2>
            <div style={{ display: "flex", justifyContent: "center", marginBottom: "20px" }}><div className="spinner"></div></div>
            <p style={{ color: "var(--text-main)", fontWeight: "bold" }}>{syncMessage}</p>
            <p style={{ color: "var(--text-muted)", fontSize: "0.85rem", marginTop: "10px" }}>Routage asynchrone via le réseau Tor. L'anonymat prend quelques secondes.</p>
          </div>
        </div>
      )}

      {view === "onboarding" && (
        <div className="onboarding-screen">
          <h1 className="logo" style={{fontSize: "3rem"}}>WATTCOIN</h1>
          <div className="card" style={{ maxWidth: "500px", margin: "0 auto" }}>
            <h2 style={{display: "flex", alignItems: "center", gap: "10px", marginBottom: "10px"}}><Shield color="var(--primary)"/> Nouveau Sanctuaire</h2>
            <p style={{ color: "var(--text-muted)", marginBottom: "20px" }}>Créez un nouveau coffre cryptographique quantique.</p>
            <input type="password" placeholder="Nouveau mot de passe" value={password} onChange={(e) => setPassword(e.target.value)} style={{ marginBottom: "10px", width: "100%" }} />
            <input type="text" placeholder="Phrase de restauration (Laisser vide pour créer)" value={restorePhrase} onChange={(e) => setRestorePhrase(e.target.value)} style={{ marginBottom: "20px", width: "100%" }} />
            <button onClick={handleCreateWallet} className="btn-primary" style={{ width: "100%" }}>Créer / Restaurer le Coffre</button>
          </div>
        </div>
      )}
      
      {view === "unlock" && (
        <div className="onboarding-screen">
          <h1 className="logo" style={{fontSize: "3rem"}}>WATTCOIN</h1>
          <div className="card" style={{ maxWidth: "400px", margin: "0 auto" }}>
            <h2 style={{display: "flex", alignItems: "center", gap: "10px", marginBottom: "20px"}}><Unlock color="var(--primary)"/> Déchiffrement</h2>
            <input type="password" placeholder="Mot de passe" value={password} onChange={(e) => setPassword(e.target.value)} onKeyDown={(e) => e.key === 'Enter' && handleUnlock()} style={{width: "100%", marginBottom: "20px"}}/>
            <button onClick={handleUnlock} className="btn-primary" style={{ width: "100%" }}>Ouvrir le Coffre</button>
          </div>
        </div>
      )}

      {view === "dashboard" && (
        <div className="dashboard-layout"><Sidebar activeTab="dashboard" />
          <main className="main-content">
            <header style={{ marginBottom: "40px" }}>
              <p style={{ color: "var(--text-muted)", textTransform: "uppercase", fontSize: "0.8rem", letterSpacing: "1px", marginBottom: "5px" }}>Valeur totale du coffre</p>
              <h1 style={{ fontSize: "3.5rem", fontWeight: "900", color: "#FFF" }}>
                ${grandTotalUsd.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} 
                <span style={{ fontSize: "1.2rem", color: "var(--text-muted)", marginLeft: "10px" }}>USD</span>
              </h1>
            </header>

            <div className="networks-stack">
              
              <div className="network-card interactive-card" onClick={() => setActiveCoinModal('WATT')}>
                <div className="network-header">
                  <div style={{ display: "flex", flexDirection: "column" }}>
                    <h2 style={{ display: "flex", alignItems: "center", gap: "8px" }}><Zap color="var(--primary)"/> Wattcoin</h2>
                    <div style={{ color: "var(--primary)", fontSize: "0.85rem", marginTop: "4px", fontWeight: "600" }}>
                      Prix : ${wattUsdPrice.toFixed(4)} <span style={{ color: "var(--text-muted)", fontWeight: "normal" }}>/ WATT</span>
                    </div>
                  </div>
                  <span className="badge">Réseau L1 Furtif</span>
                </div>
                <div className="address-box">
                  <span className="mono" style={{ color: "#888", fontSize: "0.8rem" }}>{walletData.watt_address.substring(0, 16)}...</span>
                  <button onClick={(e) => handleCopy(e, walletData.watt_address, 'WATT')} style={{ background: "transparent", border: "none", color: "var(--text-muted)", cursor: "pointer" }}>
                    {copied === 'WATT' ? <Check size={16} color="var(--primary)"/> : <Copy size={16}/>}
                  </button>
                </div>
                <div style={{ marginTop: "20px", fontSize: "2.5rem", fontWeight: "900", color: "var(--text-main)" }}>
                  {wattBalance.toFixed(9)} <span style={{ fontSize: "1.2rem", color: "var(--primary)" }}>WATT</span>
                </div>
                <div style={{ fontSize: "1.1rem", color: "var(--text-muted)", marginTop: "5px" }}>
                  ≈ ${totalWattValueUsd.toLocaleString(undefined, { minimumFractionDigits: 2 })} USD
                </div>
              </div>

              <div className="network-card btc interactive-card" style={{ borderTopColor: "rgba(247, 147, 26, 0.4)" }} onClick={() => setActiveCoinModal('BTC')}>
                <div className="network-header">
                  <div style={{ display: "flex", flexDirection: "column" }}>
                    <h2 style={{ display: "flex", alignItems: "center", gap: "8px" }}><Bitcoin color="var(--btc-color)"/> Bitcoin</h2>
                    <div style={{ color: "var(--btc-color)", fontSize: "0.85rem", marginTop: "4px", fontWeight: "600" }}>
                      Prix : ${btcUsdPrice.toLocaleString(undefined, {maximumFractionDigits: 2})} <span style={{ color: "var(--text-muted)", fontWeight: "normal" }}>/ BTC</span>
                    </div>
                  </div>
                  <span className="badge" style={{ background: "rgba(247, 147, 26, 0.1)", color: "var(--btc-color)" }}>Testnet L1</span>
                </div>
                <div className="address-box">
                  <span className="mono" style={{ color: "#888", fontSize: "0.8rem" }}>{walletData.btc_address}</span>
                  <button onClick={(e) => handleCopy(e, walletData.btc_address, 'BTC')} style={{ background: "transparent", border: "none", color: "var(--text-muted)", cursor: "pointer" }}>
                    {copied === 'BTC' ? <Check size={16} color="var(--btc-color)"/> : <Copy size={16}/>}
                  </button>
                </div>
                <div style={{ marginTop: "20px", fontSize: "2.5rem", fontWeight: "900", color: "var(--text-main)" }}>
                  {btcBalance.toFixed(8)} <span style={{ fontSize: "1.2rem", color: "var(--btc-color)" }}>BTC</span>
                </div>
                <div style={{ fontSize: "1.1rem", color: "var(--text-muted)", marginTop: "5px" }}>
                  ≈ ${totalBtcValueUsd.toLocaleString(undefined, { minimumFractionDigits: 2 })} USD
                </div>
              </div>
              
            </div>

            {activeCoinModal && (
              <div className="modal-overlay" onClick={() => setActiveCoinModal(null)}>
                <div className="modal-content" onClick={(e) => e.stopPropagation()}>
                  <div className="modal-header">
                    <h2 style={{display: "flex", alignItems: "center", gap: "8px"}}><Send color={activeCoinModal === 'WATT' ? 'var(--primary)' : 'var(--btc-color)'} /> Envoyer {activeCoinModal}</h2>
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
                      style={{ width: "100%", padding: "12px", fontSize: "1.1rem", opacity: isProcessing ? 0.7 : 1, cursor: isProcessing ? "not-allowed" : "pointer", background: activeCoinModal === 'BTC' ? 'linear-gradient(135deg, #F7931A, #d97706)' : '' }}
                    >
                      {isProcessing ? "⏳ Chiffrement..." : "Signer & Envoyer"}
                    </button>
                    
                    {isProcessing && activeCoinModal === "WATT" && (
                      <div style={{ marginTop: "15px", color: "var(--primary)", textAlign: "center", fontSize: "0.9rem", fontWeight: "bold" }}>
                        ⚡ Création de la preuve ZKP Lattice en cours...
                      </div>
                    )}
                  </div>
                </div>
              </div>
            )}
          </main>
        </div>
      )}

      {view === "dex" && (
        <div className="dashboard-layout"><Sidebar activeTab="dex" />
          <main className="main-content">
            <div className="dex-header">
              <div className="trading-pair" style={{display: "flex", alignItems: "center", gap: "10px"}}><Zap color="var(--primary)"/> WATT / BTC <Bitcoin color="var(--btc-color)"/></div>
              <div className="batch-timer" style={{ fontSize: "0.9rem" }}>⏳ En attente du prochain bloc...</div>
            </div>
            
            <div className="dex-grid">
              <div className="order-form">
                <h3>Placer un ordre furtif</h3>
                <div className="form-tabs" style={{marginTop: "15px"}}>
                  <div className={`tab-btn buy ${orderType === "buy" ? "active" : ""}`} onClick={() => setOrderType("buy")}>Achat</div>
                  <div className={`tab-btn sell ${orderType === "sell" ? "active" : ""}`} onClick={() => setOrderType("sell")}>Vente</div>
                </div>
                
                <label style={{color: "var(--text-muted)", fontSize: "0.8rem", marginTop:"15px", display:"flex", justifyContent:"space-between"}}>
                  <span>Quantité (WATT)</span>
                  <span style={{color: "var(--primary)"}}>≈ ${orderAmount ? (parseFloat(orderAmount) * (globalWattPriceSats / 100000000) * btcUsdPrice).toFixed(2) : "0.00"}</span>
                </label>
                <input type="number" placeholder="Ex: 10" value={orderAmount} onChange={(e) => setOrderAmount(e.target.value)} style={{width: "100%"}}/>
                
                <label style={{color: "var(--text-muted)", fontSize: "0.8rem", marginTop:"10px", display:"flex", justifyContent:"space-between"}}>
                  <span>Total à {orderType === "buy" ? "payer" : "recevoir"} (BTC)</span>
                  <span style={{color: "var(--btc-color)"}}>≈ ${orderTotalBtc ? (parseFloat(orderTotalBtc) * btcUsdPrice).toFixed(2) : "0.00"}</span>
                </label>
                <input type="number" placeholder="Ex: 0.001" value={orderTotalBtc} onChange={(e) => setOrderTotalBtc(e.target.value)} style={{width: "100%"}}/>
                
                {orderAmount && orderTotalBtc && parseFloat(orderAmount) > 0 && (
                  <div style={{ background: "rgba(0,0,0,0.4)", padding: "15px", borderRadius: "8px", margin: "15px 0", border: "1px solid rgba(255,255,255,0.05)", textAlign: "center" }}>
                    <span style={{color: "var(--text-muted)", fontSize: "0.85rem"}}>Prix unitaire implicite :</span><br/>
                    <strong className="mono" style={{color: "#FFF", fontSize: "1.1rem"}}>
                      {(parseFloat(orderTotalBtc) / parseFloat(orderAmount)).toFixed(8)} BTC
                    </strong>
                  </div>
                )}

                <button className={`submit-order-btn ${orderType}`} style={{ width: "100%", padding: "12px", border: "none", borderRadius: "8px", fontWeight: "bold", cursor: "pointer" }} onClick={handleSubmitOrder}>
                  Signer et envoyer au Dark Pool
                </button>
              </div>
              <div className="dark-pool">
                <h3>🌊 Piscine d'ordres anonymes</h3>
                <table className="pool-table">
                  <thead><tr><th>Type</th><th>Quantité WATT</th><th>Prix BTC</th><th>Action</th></tr></thead>
                  <tbody>
                    {darkPool.map((o) => {
                      const isMyOrder = walletData && o.watt_address === walletData.watt_address;
                      return (
                      <tr key={o.id} className={o.order_type === "buy" ? "row-buy" : "row-sell"}>
                        <td>{o.order_type === "buy" ? "Achat" : "Vente"}</td>
                        <td>{o.amount_flames / 1000000000}</td>
                        <td>{(o.price_sats / 100000000).toFixed(8)}</td>
                        <td>
                          {isMyOrder ? (
                            <button onClick={() => handleCancelOrder(o.id)} style={{background: "transparent", color: "#ef4444", border: "1px solid #ef4444", borderRadius: "4px", padding: "4px 8px", cursor: "pointer", fontSize: "0.8rem"}}>
                              Annuler
                            </button>
                          ) : (
                            <span style={{color: "var(--text-muted)", fontSize: "0.8rem"}}>Anonyme</span>
                          )}
                        </td>
                      </tr>
                    )})}
                  </tbody>
                </table>
              </div>
            </div>
          </main>
        </div>
      )}
      
      {view === "swaps" && (
        <div className="dashboard-layout"><Sidebar activeTab="swaps" />
          <main className="main-content">
            <header>
              <h1>Exécution des Swaps (Cross-Chain)</h1>
              <p style={{ color: "var(--text-muted)" }}>Protocoles d'échanges atomiques sans intermédiaire</p>
            </header>

            <div className="dex-grid" style={{ gridTemplateColumns: "1fr" }}>
              <div className="dark-pool">
                <h3 style={{display: "flex", alignItems: "center", gap: "10px"}}><Lock color="var(--primary)"/> Contrats Matchés</h3>
                
                {pendingSwaps.length === 0 ? (
                  <p style={{ color: "var(--text-muted)", textAlign: "center", padding: "40px" }}>Aucun contrat en attente de votre signature.</p>
                ) : (
                  <table className="pool-table">
                    <thead><tr><th>Votre Rôle</th><th>Hash HTLC</th><th>WATT</th><th>BTC</th><th>Actions Requises (Dans l'ordre)</th></tr></thead>
                    <tbody>
                      {pendingSwaps.map((s, i) => {
                        const isAlice = s.buyer_btc_address === walletData.btc_address;
                        const isBob = s.seller_watt_address === walletData.watt_address;
                        const hasActioned = actionnedSwaps.has(s.htlc_hash);

                        return (
                        <tr key={i}>
                          <td>
                            {isAlice ? <span className="badge" style={{background: "rgba(16, 185, 129, 0.1)", color: "#10b981"}}>Acheteur WATT</span> : 
                             isBob ? <span className="badge" style={{background: "rgba(247, 147, 26, 0.1)", color: "var(--btc-color)"}}>Vendeur WATT</span> : 
                             <span className="badge">Observateur</span>}
                          </td>
                          <td className="mono" style={{ fontSize: "0.8rem", color: "var(--primary)" }}>{s.htlc_hash.substring(0, 16)}...</td>
                          <td style={{ fontWeight: "bold" }}>{s.watt_amount_flames / 1000000000}</td>
                          <td style={{ fontWeight: "bold" }}>
                            {s.btc_amount_sats / 100000000}
                          </td>
                          <td>
                            {isAlice && (
                              <div style={{ display: "flex", gap: "10px", flexDirection: "column" }}>
                                <button className="btn-secondary" disabled={isProcessing} style={{ padding: "8px 15px", fontSize: "0.85rem" }} onClick={async () => {
                                    const expectedPriceBtc = s.btc_amount_sats / s.watt_amount_flames;
                                    const userConfirmed = window.confirm(`🚨 VÉRIFICATION DE SÉCURITÉ 🚨\n\nLe Nœud Relais propose l'échange suivant :\n- Vous envoyez : ${(s.btc_amount_sats / 100000000).toFixed(8)} BTC\n- Vous recevrez : ${s.watt_amount_flames / 1000000000} WATT\nPrix unitaire : ${expectedPriceBtc.toFixed(8)} BTC/WATT\n\nEst-ce bien le prix que vous aviez demandé ?`);
                                    if (!userConfirmed) { toast.error("Échange annulé."); return; }

                                    setIsProcessing(true);
                                    const toastId = toast.loading("Création & Envoi au contrat BTC...");
                                    try {
                                        const addr = await invoke("create_btc_htlc", {
                                            buyerPubkeyHex: s.buyer_btc_pubkey, sellerPubkeyHex: s.seller_btc_pubkey, secretHex: s.htlc_secret, locktime: 144
                                        });
                                        await invoke("send_btc_to_htlc", {
                                            masterSeedHex: walletData.master_seed_hex, htlcAddress: addr, amountBtc: s.btc_amount_sats / 100000000
                                        });
                                        toast.success("BTC envoyés au contrat !", { id: toastId, duration: 5000 });
                                        setActionnedSwaps(prev => new Set(prev).add(s.htlc_hash));
                                    } catch(e) { toast.error(e.toString(), { id: toastId }); }
                                    finally { setIsProcessing(false); }
                                }}>
                                  1. Verrouiller BTC (L1)
                                </button>

                                <button className="btn-primary" disabled={isProcessing} style={{ padding: "8px 15px", fontSize: "0.85rem" }} onClick={async () => {
                                    setIsProcessing(true);
                                    const toastId = toast.loading("Réclamation des WATT...");
                                    try {
                                        const res = await invoke("claim_wattcoin_swap", {
                                            secret: s.htlc_secret, hash: s.htlc_hash, wattAddress: walletData.watt_address, amount: s.watt_amount_flames / 1000000000
                                        });
                                        toast.success(res, { id: toastId, duration: 5000 });
                                        setActionnedSwaps(prev => new Set(prev).add(s.htlc_hash));
                                    } catch(e) { toast.error(e.toString(), { id: toastId }); }
                                    finally { setIsProcessing(false); }
                                }}>
                                  4. Réclamer WATT
                                </button>
                                
                                <button 
                                  className="btn-danger" 
                                  style={{opacity: hasActioned ? 1 : 0.4, cursor: hasActioned ? "pointer" : "not-allowed"}}
                                  onClick={() => removeSwapFromCache(s.htlc_hash)}
                                >
                                  <EyeOff size={14}/> Cacher le Swap
                                </button>
                              </div>
                            )}

                            {isBob && (
                              <div style={{ display: "flex", gap: "10px", flexDirection: "column" }}>
                                <button className="btn-secondary" style={{ padding: "8px 15px", fontSize: "0.85rem" }} disabled={isProcessing} onClick={() => handleBobLockWatt(s)}>
                                  {isProcessing ? "⏳ Attente..." : "2. Verrouiller WATT"}
                                </button>

                                <button className="btn-primary" disabled={isProcessing} style={{ padding: "8px 15px", fontSize: "0.85rem", background: "linear-gradient(135deg, #F7931A, #d97706)" }} onClick={async () => {
                                    setIsProcessing(true);
                                    const toastId = toast.loading("Réclamation des BTC...");
                                    try {
                                        const addr = await invoke("create_btc_htlc", {
                                            buyerPubkeyHex: s.buyer_btc_pubkey, sellerPubkeyHex: s.seller_btc_pubkey, secretHex: s.htlc_secret, locktime: 144
                                        });
                                        const res = await invoke("claim_btc_swap", {
                                            masterSeedHex: walletData.master_seed_hex, htlcAddress: addr, secretHex: s.htlc_secret,
                                            buyerPubkeyHex: s.buyer_btc_pubkey, sellerPubkeyHex: s.seller_btc_pubkey 
                                        });
                                        toast.success(res, { id: toastId, duration: 5000 });
                                        setActionnedSwaps(prev => new Set(prev).add(s.htlc_hash));
                                    } catch(e) { toast.error(e.toString(), { id: toastId }); }
                                    finally { setIsProcessing(false); }
                                }}>
                                  3. Réclamer BTC (L1)
                                </button>

                                <div style={{display: "flex", gap: "5px"}}>
                                  <button className="btn-danger" style={{flex: 1}} disabled={isProcessing} onClick={() => handleRefundWatt(s)}>
                                    5. Refund WATT
                                  </button>
                                  <button 
                                      className="btn-danger" 
                                      style={{flex: 1, opacity: hasActioned ? 1 : 0.4, cursor: hasActioned ? "pointer" : "not-allowed"}}
                                      onClick={() => removeSwapFromCache(s.htlc_hash)}
                                  >
                                      Cacher
                                  </button>
                                </div>
                              </div>
                            )}
                          </td>
                        </tr>
                      )})}
                    </tbody>
                  </table>
                )}
              </div>
            </div>
          </main>
        </div>
      )}
      
      {view === "history" && (
        <div className="dashboard-layout">
          <Sidebar activeTab="history" />
          <main className="main-content">
            <header>
              <h1>Historique Cryptographique</h1>
              <p style={{ color: "var(--text-muted)" }}>Seules vos transactions déchiffrées par votre clé Kyber apparaissent ici.</p>
            </header>

            <div className="glass-panel" style={{ padding: "30px", maxWidth: "800px", marginTop: "20px" }}>
              <div style={{ marginTop: "10px" }}>
                {txHistory.length === 0 ? (
                  <div style={{ textAlign: "center", color: "var(--text-muted)", padding: "40px 0" }}>
                    Aucune transaction détectée sur la blockchain.
                  </div>
                ) : (
                  txHistory.map((tx, idx) => (
                    <div key={idx} style={{ display: "flex", justifyContent: "space-between", alignItems: "center", background: "rgba(0,0,0,0.4)", padding: "20px", borderRadius: "12px", marginBottom: "15px", borderLeft: "4px solid var(--primary)" }}>
                      <div>
                        <div style={{ color: "#FFF", fontWeight: "bold", fontSize: "1.1rem", display: "flex", alignItems: "center", gap: "8px" }}><Download size={18} color="var(--primary)"/> Reçu</div>
                        <div className="mono" style={{ color: "var(--text-muted)", fontSize: "0.85rem", marginTop: "5px" }}>{tx.date} • {tx.id}</div>
                      </div>
                      <div style={{ textAlign: "right" }}>
                        <div style={{ color: "var(--primary)", fontWeight: "bold", fontSize: "1.2rem" }}>+{tx.amount.toFixed(4)} {tx.coin}</div>
                        <div style={{ color: tx.status.includes("Dépensé") ? "#ef4444" : "var(--text-muted)", fontSize: "0.85rem", fontWeight: "bold", marginTop: "5px" }}>{tx.status}</div>
                      </div>
                    </div>
                  ))
                )}
              </div>
            </div>
          </main>
        </div>
      )}

      {view === "settings" && (
        <div className="dashboard-layout">
          <Sidebar activeTab="settings" />
          <main className="main-content">
            <header>
              <h1>Poste de Commandement</h1>
              <p style={{ color: "var(--text-muted)" }}>Gestion des clés quantiques et du matériel de minage</p>
            </header>

            <div className="glass-panel" style={{ padding: "40px", maxWidth: "800px", marginTop: "20px" }}>
              <div className="security-section">
                <h3 style={{ color: "var(--text-main)", display: "flex", alignItems: "center", gap: "10px" }}><Lock color="var(--primary)"/> Phrase de Récupération (Seed)</h3>
                <p style={{ color: "var(--text-muted)", marginTop: "10px" }}>Cette phrase de 24 mots est la clé maîtresse absolue. Ne l'exposez jamais sur internet.</p>
                
                {showSeed ? (
                  <div className="mono" style={{ background: "rgba(0,0,0,0.5)", padding: "25px", borderRadius: "12px", border: "1px solid var(--primary)", fontSize: "1.1rem", color: "#FFF", marginTop: "20px", lineHeight: "1.6" }}>
                    {walletData.mnemonic}
                  </div>
                ) : (
                  <div style={{ background: "rgba(0,0,0,0.3)", padding: "25px", borderRadius: "12px", border: "1px dashed rgba(255,255,255,0.1)", marginTop: "20px", display: "flex", justifyContent: "center" }}>
                    <button className="btn-secondary" onClick={() => setShowSeed(true)}>Déchiffrer la Phrase Secrète</button>
                  </div>
                )}
              </div>

              <div className="security-section" style={{ marginTop: "50px" }}>
                <h3 style={{ color: "var(--text-main)", display: "flex", alignItems: "center", gap: "10px" }}><Zap color="var(--primary)"/> Scripts de Minage</h3>
                <p style={{ color: "var(--text-muted)", marginTop: "10px" }}>Générez un lanceur pour votre Nœud Mineur pointant directement vers ce coffre.</p>
                <div style={{ display: "flex", gap: "15px", marginTop: "20px" }}>
                  <button className="btn-secondary" style={{ flex: 1 }} onClick={() => invoke("save_miner_script", { os: "linux", address: walletData.watt_address }).then(m => toast.success(m)).catch(e => toast.error(e))}>🐧 Générer start_miner.sh (Linux)</button>
                  <button className="btn-secondary" style={{ flex: 1 }} onClick={() => invoke("save_miner_script", { os: "windows", address: walletData.watt_address }).then(m => toast.success(m)).catch(e => toast.error(e))}>🪟 Générer start_miner.bat (Windows)</button>
                </div>
              </div>

              <div className="security-section" style={{ marginTop: "60px", borderTop: "1px solid rgba(239, 68, 68, 0.2)", paddingTop: "40px" }}>
                <h3 style={{ color: "#ef4444", display: "flex", alignItems: "center", gap: "10px" }}><Trash2 /> Protocole d'Autodestruction</h3>
                <p style={{ color: "var(--text-muted)", marginTop: "10px" }}>Efface définitivement le fichier crypté de cet ordinateur.</p>
                <button 
                  style={{ background: "rgba(239, 68, 68, 0.1)", color: "#ef4444", padding: "12px 25px", border: "1px solid #ef4444", borderRadius: "8px", cursor: "pointer", fontWeight: "bold", marginTop: "20px", transition: "0.3s" }}
                  onMouseEnter={(e) => e.target.style.background = "#ef4444"}
                  onMouseLeave={(e) => e.target.style.background = "rgba(239, 68, 68, 0.1)"}
                  onClick={async () => {
                    if(window.confirm("⚠️ Êtes-vous ABSOLUMENT certain de vouloir détruire ce coffre ?")) {
                      try { await invoke("destroy_vault"); setWalletData(null); setView('onboarding'); toast.success("Coffre détruit."); } 
                      catch (e) { toast.error("Erreur : " + e); }
                    }
                  }}>
                  Dynamiter le Coffre-Fort
                </button>
              </div>
            </div>
          </main>
        </div>
      )}
    </>
  );
}

export default App;