import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Toaster, toast } from "react-hot-toast";
import { QRCodeSVG } from "qrcode.react"; 
import { scan, checkPermissions, requestPermissions, Format } from '@tauri-apps/plugin-barcode-scanner';
import { Copy, Check, Lock, Unlock, Settings, ScrollText, ArrowRightLeft, Shield, Send, Zap, Bitcoin, Trash2, Download, EyeOff, Dices, QrCode } from "lucide-react"; 
import "./App.css";
import logoWattcoin from "./assets/logo_wattcoin.png";


function App() {
  const [view, setView] = useState("loading"); 
  const [walletData, setWalletData] = useState(null);
  const [password, setPassword] = useState("");
  const [restorePhrase, setRestorePhrase] = useState("");
  const [error, setError] = useState("");
  
  const [wattBalanceL1, setWattBalanceL1] = useState(0.0);
  const [wattBalanceL2, setWattBalanceL2] = useState(0.0);
  const [btcBalance, setBtcBalance] = useState(0.0);
  const [btcUsdPrice, setBtcUsdPrice] = useState(0.0); 
  const [globalWattPriceSats, setGlobalWattPriceSats] = useState(0);
  
  const [totalSupply, setTotalSupply] = useState(0);
  const [currentJackpot, setCurrentJackpot] = useState(0);
  const [receiveModal, setReceiveModal] = useState(null); // 'WATT_L1', 'WATT_L2', ou 'BTC'
  const [receiveAmount, setReceiveAmount] = useState("");

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

  const [btcLockedStatus, setBtcLockedStatus] = useState({}); // { htlc_hash: true/false }
  const [swapUiTrigger, setSwapUiTrigger] = useState(0);
  
  const [appVersion, setAppVersion] = useState("");

  const handleCopy = (e, text, type) => {
    e.stopPropagation(); 
    navigator.clipboard.writeText(text);
    setCopied(type);
    toast.success("Copié dans le presse-papier !");
    setTimeout(() => setCopied(""), 2000);
  };
  
	// ==================== GESTION D'ÉTAT CENTRALISÉ ====================
	const getSwapStates = () => {
	  try {
		return JSON.parse(localStorage.getItem('swapStates') || '{}');
	  } catch {
		return {};
	  }
	};

	const getSwapState = (hash) => {
	  const states = getSwapStates();
	  return states[hash] || {
		role: null,
		btcLocked: false,
		wattLocked: false,
		claimSent: false,
		wattClaimed: false,
		btcClaimed: false,
		secret: null,
		lastChecked: 0
	  };
	};

	const updateSwapState = (hash, updates) => {
		const states = getSwapStates();
		states[hash] = {
			...getSwapState(hash),
			...updates,
			lastChecked: Date.now()
		};
		localStorage.setItem('swapStates', JSON.stringify(states));
		setSwapUiTrigger(prev => prev + 1); // 💡 FORCE REACT À RAFRAÎCHIR L'UI
	};

	const removeSwapState = (hash) => {
	  const states = getSwapStates();
	  delete states[hash];
	  localStorage.setItem('swapStates', JSON.stringify(states));
	};
	
	// ==================== NETTOYAGE DES ANCIENS FLAGS ====================
	const cleanupOldSwapFlags = (hash) => {
	  try {
		localStorage.removeItem(`btc_locked_${hash}`);
		localStorage.removeItem(`secret_${hash}`);
		localStorage.removeItem(`claim_sent_${hash}`);
		localStorage.removeItem(`watt_claimed_${hash}`);
		localStorage.removeItem(`btc_claimed_${hash}`);
		console.log(`[Cleanup] Anciens flags supprimés pour ${hash.substring(0, 10)}...`);
	  } catch (e) {
		console.warn("Erreur lors du nettoyage des flags:", e);
	  }
	};

  useEffect(() => {
    async function checkVault() {
      const exists = await invoke("vault_exists");
      if (exists) { setView("unlock"); } else { setView("onboarding"); }
    }
    checkVault();

    // Récupération de la version
    invoke("get_version")
      .then(ver => { setAppVersion(ver); })
      .catch(e => console.warn("Impossible de lire la version", e));

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
			if (info.last_price_sats) setGlobalWattPriceSats(info.last_price_sats);

			setSyncMessage("Déchiffrement de la blockchain...");
			const balances = await invoke("get_balances", { keys: walletData });
			setWattBalanceL1(balances.l1);
			setWattBalanceL2(balances.l2);

			const hist = await invoke("get_history", { keys: walletData });
			setTxHistory(hist);

			const supply = await invoke("get_total_supply").catch(() => 0);
			setTotalSupply(supply);
			const jackpot = await invoke("get_current_jackpot").catch(() => 0);
			setCurrentJackpot(jackpot);

			// FORCE BTC (défensif)
			const balBTC = await invoke("get_btc_balance", { 
			  masterSeedHex: walletData.master_seed_hex, 
			  btc_address: walletData.btc_address 
			}).catch(() => 0);
			setBtcBalance(balBTC);

			setView("dashboard");
			toast.success("Synchronisation terminée, coffre ouvert !");

		  } catch (e) {
			console.error("Erreur critique pendant la sync :", e);
			setSyncMessage("❌ Erreur de synchronisation. Redémarre l'application.");
			toast.error("Impossible d'ouvrir le dashboard : " + e.toString());
			setView("dashboard"); // Fallback sécurité
		  }
		};

		performInitialSync();
	  }
	}, [view, walletData]);

  useEffect(() => {
    if (view !== "dex" && view !== "dashboard" && view !== "swaps" && view !== "history" && view !== "casino") return;

    let isRefreshing = false;

    const updateData = async () => {
      if (!walletData || isRefreshing) return;
      isRefreshing = true;

      try {
        const balances = await invoke("get_balances", { keys: walletData });
		setWattBalanceL1(balances.l1);
		setWattBalanceL2(balances.l2);

        const hist = await invoke("get_history", { keys: walletData });
        setTxHistory(hist);

        const balBTC = await invoke("get_btc_balance", { 
            masterSeedHex: walletData.master_seed_hex, 
            btc_address: walletData.btc_address
        });
        setBtcBalance(balBTC);

        const pool = await invoke("get_dark_pool");
        setDarkPool(pool);

        const apiSwaps = await invoke("get_active_swaps", { btcAddress: walletData.btc_address, wattAddress: walletData.watt_address });
        setPendingSwaps(prev => {
			const completedToKeep = prev.filter(s => {
				const st = getSwapState(s.htlc_hash);
				const isFinished = st.wattClaimed || st.btcClaimed;
				const isMissingFromNode = !apiSwaps.find(a => a.htlc_hash === s.htlc_hash);
				return isFinished && isMissingFromNode;
			});
			return [...apiSwaps, ...completedToKeep];
		});
        localStorage.setItem('my_swaps', JSON.stringify(apiSwaps));

        const supply = await invoke("get_total_supply");
        setTotalSupply(supply);

        const jackpot = await invoke("get_current_jackpot");
        setCurrentJackpot(jackpot);

        const info = await invoke("get_network_info");
        if(info.last_price_sats) setGlobalWattPriceSats(info.last_price_sats);

      } catch (e) {
        console.error("Erreur de sync temps réel :", e);
      } finally {
        isRefreshing = false; 
      }
    };
    
    updateData();

    const unlistenPromise = listen("network-update", () => {
      updateData();
    });

    const timerDex = setInterval(() => {
      if (view === "dex" && !isRefreshing) {
        invoke("get_dark_pool").then(setDarkPool).catch(()=>{});
      }
    }, 10000);

    return () => { 
      clearInterval(timerDex); 
      unlistenPromise.then(unlisten => unlisten()); 
    };
  }, [view, walletData]);
  
	// ==================== LE WATCHDOG ====================
	useEffect(() => {
	  if (view !== "swaps" || !walletData) return;

	  const watchdogTimer = setInterval(async () => {
		if (!pendingSwaps || pendingSwaps.length === 0) return;

		for (const swap of pendingSwaps) {
		  const hash = swap.htlc_hash;
		  const isAlice = swap.buyer_btc_address === walletData.btc_address;
		  const isBob = swap.seller_watt_address === walletData.watt_address;
		  
		  if (!isAlice && !isBob) continue;

		  const state = getSwapState(hash);

			if (state.wattClaimed || state.btcClaimed) {
				continue; 
			}

		  // --- Logique ALICE ---
		  if (isAlice) {
			  try {
				const isWattLocked = await invoke("check_watt_lock_exists", { hash });
				if (isWattLocked && !state.wattClaimed) {
				  if (!state.claimSent) {
					await invoke("claim_wattcoin_swap", {
					  secret: state.secret,
					  hash: hash,
					  amountFlames: swap.watt_amount_flames,
					  wattAddress: walletData.watt_address
					});
					updateSwapState(hash, { claimSent: true, wattClaimed: true });
					toast.success("🔥 WATT Réclamés ! (Accepté par le node)", { icon: '✅' });
				  }
				}
			  } catch (e) {}
			}

		  // --- Logique BOB ---
		  if (isBob && !state.btcClaimed) {
			try {
			   const secret = await invoke("get_revealed_secret", { htlcHash: hash });
			   if (secret) {
				 const res = await invoke("auto_claim_btc_swap", { htlcHash: hash, htlcAddress: swap.buyer_btc_address });
				 updateSwapState(hash, { btcClaimed: true });
				 toast.success("⚡ Claim BTC réussi : " + res);
			   }
			} catch (e) {}
		  }

		  const currentState = getSwapState(hash);
		  if (currentState.wattClaimed || currentState.btcClaimed) {
			cleanupOldSwapFlags(hash);
		  }
		}
	  }, 6000);

	  return () => clearInterval(watchdogTimer);
	}, [view, walletData, pendingSwaps]);
  
	// ==================== BTC LOCK STATUS ====================
	useEffect(() => {
	  if (!walletData || pendingSwaps.length === 0) return;

	  const checkAllBtcLocks = async () => {
		const newStatus = {};
		for (const swap of pendingSwaps) {
		  try {
			const exists = await invoke("check_btc_contract_exists", { htlcHash: swap.htlc_hash });
			newStatus[swap.htlc_hash] = exists;
		  } catch (e) {
			newStatus[swap.htlc_hash] = false;
		  }
		}
		setBtcLockedStatus(newStatus);
	  };

	  checkAllBtcLocks();
	  const interval = setInterval(checkAllBtcLocks, 5000);
	  return () => clearInterval(interval);
	}, [pendingSwaps, walletData]);
  
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
      const res = await invoke("generate_pro_wallet", { 
			phraseOption: restorePhrase ? restorePhrase : null,
			password: password // 🔒 On passe le mot de passe au backend  pour l'ajouter au sel de la seed !
		});
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
      let hashToSend = null;
      
      // L'acheteur génère le verrou AVANT l'envoi
      if (orderType === "buy") {
		  const cryptoData = await invoke("create_swap_secret");
		  hashToSend = cryptoData.hash;

		  updateSwapState(hashToSend, {
			role: "alice",
			secret: cryptoData.secret
		  });
	  }

      await invoke("submit_order", {
		  orderType: orderType, 
		  amount: amountWATT, 
		  price: unitPriceBtc, 
		  btcAddress: walletData.btc_address, 
		  btcPubkey: walletData.btc_pubkey_hex, 
		  wattAddress: walletData.watt_address,
		  htlcHash: hashToSend || null
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
  
  const handleScanQR = async () => {
    try {
      // Demande la permission pour la caméra
      let perm = await checkPermissions();
      if (perm === 'prompt') perm = await requestPermissions();
      if (perm !== 'granted') {
        toast.error("Permission caméra refusée.");
        return;
      }

      // Lance le scan
      const result = await scan({ formats: [Format.QRCode] });
      
      if (result && result.content) {
        const text = result.content;
        
        // Décryptage du lien "wattcoin:ADRESSE?amount=MONTANT"
        if (text.startsWith("wattcoin:") || text.startsWith("bitcoin:")) {
          const scheme = text.startsWith("wattcoin:") ? "wattcoin:" : "bitcoin:";
          let data = text.replace(scheme, "");
          
          if (data.includes("?")) {
            const [addr, query] = data.split("?");
            setSendAddress(addr); // Remplit l'adresse
            const params = new URLSearchParams(query);
            if (params.has("amount")) {
                setSendAmount(params.get("amount")); // Pré-remplit le montant !
            }
          } else {
            setSendAddress(data);
            setSendAmount(""); // Laisse vide pour que le payeur choisisse
          }
          toast.success("QR Code décodé avec succès !");
        } else {
          // Si on scanne un QR code "brut" sans le préfixe
          setSendAddress(text);
          toast.success("Adresse scannée !");
        }
      }
    } catch (e) {
      console.error(e);
      toast.error("Scan annulé ou impossible à démarrer.");
    }
  };

  const handleSendTransaction = async () => {
    if (!sendAddress || !sendAmount) return;
    if (isProcessing) return;
    setIsProcessing(true);

    // 💡 NOUVEAU : Nettoyage absolu de l'adresse (au cas où l'utilisateur colle le lien complet)
    let cleanAddress = sendAddress.trim();
    if (cleanAddress.startsWith("wattcoin:")) cleanAddress = cleanAddress.replace("wattcoin:", "");
    if (cleanAddress.startsWith("bitcoin:")) cleanAddress = cleanAddress.replace("bitcoin:", "");
    if (cleanAddress.includes("?")) cleanAddress = cleanAddress.split("?")[0]; // Retire le ?amount= si présent

    const loadingToast = toast.loading("Signature et routage Tor en cours...");

    try {
      if (activeCoinModal.startsWith("WATT")) {
        const isL2Source = activeCoinModal === "WATT_L2";
        const isL2Dest = cleanAddress.startsWith("L2_WATT_"); // On utilise cleanAddress !

        const response = await invoke("send_wattcoin", {
          recipientKyberHex: cleanAddress, // On envoie l'adresse propre
          amount: parseFloat(sendAmount),
          senderKyberSecretHex: walletData.kyber_secret_hex, 
          senderKyberPublicHex: walletData.watt_address,
          htlcHashHex: null, 
          htlcTimeout: null,
          spendFromL2: isL2Source, 
          sendToL2: isL2Dest    
        });

        toast.success(response, { id: loadingToast, duration: 5000 });
        
        if (isL2Source) setWattBalanceL2(prev => prev - parseFloat(sendAmount));
        else setWattBalanceL1(prev => prev - parseFloat(sendAmount));
        
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

	const handleBobLockWatt = async (swap) => {
	  if (isProcessing) return;
	  const confirmed = window.confirm(`🔒 Voulez-vous vraiment verrouiller ${swap.watt_amount_flames / 1e9} WATT dans le HTLC ?`);
	  if (!confirmed) return;

	  setIsProcessing(true);
	  const toastId = toast.loading("🔒 Envoi du HTLCLock WATT...");

	  try {
		const res = await invoke("send_wattcoin", {
		  recipientKyberHex: swap.buyer_watt_address,
		  amount: swap.watt_amount_flames / 1e9,
		  senderKyberSecretHex: walletData.kyber_secret_hex,
		  senderKyberPublicHex: walletData.watt_address,
		  htlcHashHex: swap.htlc_hash,
		  htlcTimeout: 144
		});
		toast.success("✅ WATT verrouillés ! Alice peut maintenant réclamer.", { id: toastId });
		updateSwapState(swap.htlc_hash, { role: "bob", wattLocked: true });
	  } catch (e) {
		toast.error("❌ " + e, { id: toastId });
	  } finally {
		setIsProcessing(false);
	  }
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
    } catch (error) { toast.error(error.toString(), { id: loadingToast }); } 
    finally { setIsProcessing(false); }
  };

  const removeSwapFromCache = (hash) => {
		const state = getSwapState(hash);

		if (!state.wattLocked && !state.btcClaimed && !state.wattClaimed) {
		  toast.error("🚨 Action requise ! Effectuez une transaction (Lock, Claim ou Refund) avant d'effacer ce contrat.");
		  return;
		}

	  removeSwapState(hash);
	  cleanupOldSwapFlags(hash);

	  const cached = JSON.parse(localStorage.getItem('my_swaps') || '[]');
	  const updated = cached.filter(s => s.htlc_hash !== hash);
	  localStorage.setItem('my_swaps', JSON.stringify(updated));
	  setPendingSwaps(updated);

	  toast.success("Swap nettoyé et purgé complètement !");
	};

  // ================= UI COMPONENTS =================

  const Sidebar = ({ activeTab }) => (
    <nav className="sidebar">
		<div className="sidebar-logo">
		  <div className="logo-container">
			<Zap size={26} color="var(--primary)" fill="var(--primary)" />
			<h2 className="logo">WATTCOIN</h2>
		  </div>
		  
		  <span className="mono" style={{ 
			fontSize: "0.73rem", 
			color: "var(--text-muted)", 
			marginTop: "7px",
			letterSpacing: "1.5px"
		  }}>
			v{appVersion}
		  </span>
		</div>

      <ul className="nav-links">
        <li className={activeTab === "dashboard" ? "active" : ""} onClick={() => setView("dashboard")}><Lock size={18}/> Portefeuilles</li>
        <li className={activeTab === "dex" ? "active" : ""} onClick={() => setView("dex")}><ArrowRightLeft size={18}/> DEX (FBA)</li>
        <li className={activeTab === "swaps" ? "active" : ""} onClick={() => setView("swaps")}><Shield size={18}/> Atomic Swaps</li>
        <li className={activeTab === "casino" ? "active" : ""} onClick={() => setView("casino")}><Dices size={18}/> Casino L1</li>
        <li className={activeTab === "history" ? "active" : ""} onClick={() => setView("history")}><ScrollText size={18}/> Historique</li>
        <li className={activeTab === "settings" ? "active" : ""} onClick={() => setView("settings")}><Settings size={18}/> Paramètres</li>
        <li onClick={() => { setWalletData(null); setView("unlock"); toast.success("Coffre verrouillé"); }} style={{marginTop: "auto", color: "#ef4444"}}><Lock size={18}/> Verrouiller</li>
      </ul>
    </nav>
  );
  
  // ================= CALCULS DE VALEURS =================
  const wattBtcPrice = globalWattPriceSats / 100000000;
  const wattUsdPrice = wattBtcPrice * btcUsdPrice;
  const totalWattValueUsd = (wattBalanceL1 + wattBalanceL2) * wattUsdPrice; // ⚡ L1 + L2
  const totalBtcValueUsd = btcBalance * btcUsdPrice;
  const grandTotalUsd = totalWattValueUsd + totalBtcValueUsd;
  
  const displayTotalSupply = (totalSupply / 1000000000).toLocaleString(undefined, { minimumFractionDigits: 9, maximumFractionDigits: 9 });
  const displayJackpot = (currentJackpot / 1000000000).toLocaleString(undefined, { minimumFractionDigits: 9, maximumFractionDigits: 9 });
  const jackpotUsd = ((currentJackpot / 1000000000) * wattUsdPrice).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 });
  
  const hasWonJackpot = txHistory.some(tx => tx.status.includes("Jackpot gagné"));

  // ================= MAIN RENDER =================

  return (
    <>
      <Toaster 
        position="bottom-right" 
        toastOptions={{ 
          style: { background: '#1a1d24', color: '#fff', border: '1px solid #00F0FF', fontFamily: 'Inter' },
          success: { duration: 5000, iconTheme: { primary: '#00F0FF', secondary: '#000' } },
          error: { duration: 8000, style: { border: '1px solid #ef4444' } }
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
          {/* ⚡ LOGO GÉANT AU MILIEU */}
          <img src={logoWattcoin} alt="Wattcoin" style={{ width: "120px", height: "120px", borderRadius: "20px", boxShadow: "0 0 30px rgba(0, 240, 255, 0.4)", marginBottom: "20px" }} />
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
          {/* ⚡ LOGO GÉANT AU MILIEU */}
          <img src={logoWattcoin} alt="Wattcoin" style={{ width: "120px", height: "120px", borderRadius: "20px", boxShadow: "0 0 30px rgba(0, 240, 255, 0.4)", marginBottom: "20px" }} />
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
            
            {/* 💡 J'ai changé alignItems à "center" pour que le logo soit bien centré en hauteur */}
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: "40px" }}>
                <header>
                  <p style={{ color: "var(--text-muted)", textTransform: "uppercase", fontSize: "0.8rem", letterSpacing: "1px", marginBottom: "5px" }}>Valeur totale du coffre</p>
                  <h1 style={{ fontSize: "3.5rem", fontWeight: "900", color: "#FFF", lineHeight: "1" }}>
                    ${grandTotalUsd.toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} 
                    <span style={{ fontSize: "1.2rem", color: "var(--text-muted)", marginLeft: "10px" }}>USD</span>
                  </h1>
                </header>

                {/* ⚡ TON LOGO EST PLACÉ ICI, EXACTEMENT OÙ TU AS FAIT LE CERCLE ROUGE */}
                <div style={{ flex: 1, display: "flex", justifyContent: "center" }}>
                   <img 
                      src={logoWattcoin} 
                      alt="Wattcoin Logo" 
                      style={{ 
                          width: "130px", 
                          height: "130px", 
                          borderRadius: "50%", 
                          objectFit: "cover",
                          boxShadow: "0 0 40px rgba(0, 240, 255, 0.4)", // Un bel effet de lumière néon
                          border: "2px solid rgba(0, 240, 255, 0.6)"
                      }} 
                   />
                </div>

                <div className="glass-panel" style={{ padding: "15px 20px", border: "1px solid rgba(255,255,255,0.1)", textAlign: "right", display: "flex", flexDirection: "column", gap: "10px", minWidth: "260px" }}>
                    <div>
                        <div style={{ fontSize: "0.8rem", color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: "1px" }}>Supply Totale</div>
                        <div className="mono" style={{ fontSize: "1.15rem", fontWeight: "bold", color: "var(--primary)", marginTop: "4px" }}>
                            {displayTotalSupply} <span style={{fontSize: "0.8rem"}}>WATT</span>
                        </div>
                    </div>
                    <div style={{ borderTop: "1px solid rgba(255,255,255,0.1)", paddingTop: "10px" }}>
                        <div style={{ fontSize: "0.8rem", color: "var(--text-muted)", textTransform: "uppercase", letterSpacing: "1px", display: "flex", alignItems: "center", justifyContent: "flex-end", gap: "6px" }}>
                            <Dices size={14} /> Jackpot L1
                        </div>
                        <div className="mono" style={{ fontSize: "1.15rem", fontWeight: "bold", color: "#f59e0b", marginTop: "4px" }}>
                            {displayJackpot} <span style={{fontSize: "0.8rem"}}>WATT</span>
                        </div>
                        <div style={{ fontSize: "0.8rem", color: "var(--text-muted)", marginTop: "2px" }}>
                            ≈ ${jackpotUsd} USD
                        </div>
                    </div>
                </div>
            </div>

            <div className="networks-stack">
              
              {/* CARTE WATTCOIN L1 */}
              <div className="network-card interactive-card" onClick={() => setActiveCoinModal('WATT_L1')}>
                <div className="network-header">
                  <div style={{ display: "flex", flexDirection: "column" }}>
                    <h2 style={{ display: "flex", alignItems: "center", gap: "8px" }}><Lock color="var(--primary)"/> Wattcoin L1</h2>
                  </div>
                  <span className="badge">⚡ Coffre-fort (Testnet L1)</span>
                </div>
                <div className="address-box">
					<span className="mono" style={{ color: "#888", fontSize: "0.8rem" }}>
					  {walletData?.watt_address ? walletData.watt_address.substring(0, 16) + "..." : "—"}
					</span>
					<div style={{ display: "flex", gap: "10px" }}>
						<button onClick={(e) => { e.stopPropagation(); setReceiveModal('WATT_L1'); }} style={{ background: "transparent", border: "none", color: "var(--text-muted)", cursor: "pointer" }}>
							<QrCode size={16} />
						</button>
						<button onClick={(e) => handleCopy(e, walletData.watt_address, 'WATT_L1')} style={{ background: "transparent", border: "none", color: "var(--text-muted)", cursor: "pointer" }}>
							{copied === 'WATT_L1' ? <Check size={16} color="var(--primary)"/> : <Copy size={16}/>}
						</button>
					</div>
				</div>
                <div style={{ marginTop: "20px", fontSize: "2.5rem", fontWeight: "900", color: "var(--text-main)" }}>
                  {wattBalanceL1.toFixed(9)} <span style={{ fontSize: "1.2rem", color: "var(--primary)" }}>WATT</span>
                </div>
                {/* Prix: */}
                <div style={{ fontSize: "1.1rem", color: "var(--text-muted)", marginTop: "5px" }}>
                  ≈ ${(wattBalanceL1 * wattUsdPrice).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} USD
                </div>
              </div>

              {/* ⚡ CARTE WATTCOIN L2 ⚡ */}
              <div className="network-card interactive-card" style={{ borderTopColor: "rgba(0, 240, 255, 0.4)" }} onClick={() => setActiveCoinModal('WATT_L2')}>
                <div className="network-header">
                  <div style={{ display: "flex", flexDirection: "column" }}>
                    <h2 style={{ display: "flex", alignItems: "center", gap: "8px" }}><Zap color="#00F0FF"/> Wattcoin L2</h2>
                  </div>
                  <span className="badge" style={{ background: "rgba(0, 240, 255, 0.1)", color: "#00F0FF" }}>⚡ Instantané (Testnet L2)</span>
                </div>
                <div className="address-box">
					<span className="mono" style={{ color: "#888", fontSize: "0.8rem" }}>
					  {walletData?.watt_address ? "L2_WATT_" + walletData.watt_address.substring(0, 8) + "..." : "—"}
					</span>
					<div style={{ display: "flex", gap: "10px" }}>
						<button onClick={(e) => { e.stopPropagation(); setReceiveModal('WATT_L2'); }} style={{ background: "transparent", border: "none", color: "var(--text-muted)", cursor: "pointer" }}>
							<QrCode size={16} />
						</button>
						  <button onClick={(e) => handleCopy(e, "L2_WATT_" + walletData.watt_address, 'WATT_L2')} style={{ background: "transparent", border: "none", color: "var(--text-muted)", cursor: "pointer" }}>
							{copied === 'WATT_L2' ? <Check size={16} color="var(--primary)"/> : <Copy size={16}/>}
						  </button>
					</div>
                </div>
                <div style={{ marginTop: "20px", fontSize: "2.5rem", fontWeight: "900", color: "var(--text-main)" }}>
                  {wattBalanceL2.toFixed(9)} <span style={{ fontSize: "1.2rem", color: "#00F0FF" }}>WATT</span>
                </div>
                {/* Prix: */}
                <div style={{ fontSize: "1.1rem", color: "var(--text-muted)", marginTop: "5px" }}>
                  ≈ ${(wattBalanceL2 * wattUsdPrice).toLocaleString(undefined, { minimumFractionDigits: 2, maximumFractionDigits: 2 })} USD
                </div>
              </div>

              {/* ⚡ CARTE BITCOIN L1 ⚡ */}
			  <div className="network-card btc interactive-card" style={{ borderTopColor: "rgba(247, 147, 26, 0.4)" }} onClick={() => setActiveCoinModal('BTC')}>
                <div className="network-header">
                  <div style={{ display: "flex", flexDirection: "column" }}>
                    <h2 style={{ display: "flex", alignItems: "center", gap: "8px" }}><Bitcoin color="var(--btc-color)"/> Bitcoin</h2>
                    <div style={{ color: "var(--btc-color)", fontSize: "0.85rem", marginTop: "4px", fontWeight: "600" }}>
                      Prix : ${btcUsdPrice.toLocaleString(undefined, {maximumFractionDigits: 2})} <span style={{ color: "var(--text-muted)", fontWeight: "normal" }}>/ BTC</span>
                    </div>
                  </div>
                  <span className="badge" style={{ background: "rgba(247, 147, 26, 0.1)", color: "var(--btc-color)" }}>💲 Coffre-fort (Testnet L1)</span>
                </div>
                <div className="address-box">
					<span className="mono" style={{ color: "#888", fontSize: "0.8rem" }}>
					  {walletData?.btc_address || "—"}
					</span>
					<div style={{ display: "flex", gap: "10px" }}>
						<button onClick={(e) => { e.stopPropagation(); setReceiveModal('BTC'); }} style={{ background: "transparent", border: "none", color: "var(--text-muted)", cursor: "pointer" }}>
							<QrCode size={16} />
						</button>
						<button onClick={(e) => handleCopy(e, walletData.btc_address, 'BTC')} style={{ background: "transparent", border: "none", color: "var(--text-muted)", cursor: "pointer" }}>
							{copied === 'BTC' ? <Check size={16} color="var(--btc-color)"/> : <Copy size={16}/>}
						</button>
					</div>
                </div>
                <div style={{ marginTop: "20px", fontSize: "2.5rem", fontWeight: "900", color: "var(--text-main)" }}>
                  {btcBalance.toFixed(8)} <span style={{ fontSize: "1.2rem", color: "var(--btc-color)" }}>BTC</span>
                </div>
                <div style={{ fontSize: "1.1rem", color: "var(--text-muted)", marginTop: "5px" }}>
                  ≈ ${totalBtcValueUsd.toLocaleString(undefined, { minimumFractionDigits: 2 })} USD
                </div>
              </div>
              
            </div>

            {/* 💡 MODALES DE PAIEMENT */}
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
                            {activeCoinModal === 'WATT_L1' ? wattBalanceL1.toFixed(9) : activeCoinModal === 'WATT_L2' ? wattBalanceL2.toFixed(9) : btcBalance.toFixed(8)}
                        </span> {activeCoinModal}
                        </div>
                        <div style={{ display: "flex", gap: "10px", marginBottom: "15px" }}>
							<input 
								type="text" 
								placeholder={`Adresse ${activeCoinModal} du destinataire`} 
								value={sendAddress} 
								onChange={(e) => setSendAddress(e.target.value)} 
								style={{ width: "100%", margin: 0 }} 
							/>
							{/* 💡 BOUTON SCANNER */}
							<button 
								className="btn-secondary" 
								onClick={handleScanQR} 
								style={{ padding: "0 15px", display: "flex", alignItems: "center", justifyContent: "center", cursor: "pointer" }}
								title="Scanner un QR Code"
							>
								<QrCode size={20} />
							</button>
						</div>
                        <input type="number" placeholder={`Montant en ${activeCoinModal}`} value={sendAmount} onChange={(e) => setSendAmount(e.target.value)} style={{ width: "100%", marginBottom: "25px" }} />
                        {activeCoinModal.startsWith('WATT') && (
                          <div style={{ fontSize: "0.85rem", color: "var(--text-muted)", marginBottom: "15px", textAlign: "center" }}>
                            Frais réseau prévus : <span style={{ color: "var(--primary)", fontWeight: "bold" }}>
                              {activeCoinModal === 'WATT_L2' && sendAddress.startsWith('L2_WATT_') ? "100 Flames (Pur L2)" : "1000 Flames (Routage L1)"}
                            </span>
                          </div>
                        )}
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
			{/* 💡 MODALE DE RÉCEPTION (QR CODE) */}
			{receiveModal && (() => {
			  // Construction dynamique de l'URI (Adresse + Montant optionnel)
			  const baseAddress = receiveModal === 'WATT_L1' ? walletData.watt_address :
								  receiveModal === 'WATT_L2' ? `L2_WATT_${walletData.watt_address}` :
								  walletData.btc_address;
			  const scheme = receiveModal.startsWith('WATT') ? 'wattcoin:' : 'bitcoin:';
			  let qrUri = `${scheme}${baseAddress}`;
			  if (receiveAmount > 0) {
				  qrUri += `?amount=${receiveAmount}`;
			  }

			  return (
			  <div className="modal-overlay" onClick={() => { setReceiveModal(null); setReceiveAmount(""); }}>
				<div className="modal-content" onClick={(e) => e.stopPropagation()} style={{ textAlign: "center", maxWidth: "450px", maxHeight: "90vh", overflowY: "auto" }}>
					<div className="modal-header">
						<h2 style={{display: "flex", alignItems: "center", gap: "8px"}}>
							<QrCode color={receiveModal.startsWith('WATT') ? 'var(--primary)' : 'var(--btc-color)'} /> 
							Recevoir {receiveModal.replace('_', ' ')}
						</h2>
						<button className="close-btn" onClick={() => { setReceiveModal(null); setReceiveAmount(""); }}>✖</button>
					</div>
					
					<div style={{ background: "#FFF", padding: "20px", borderRadius: "12px", display: "inline-block", marginTop: "20px", marginBottom: "15px" }}>
						<div style={{ background: "#FFF", padding: "15px", borderRadius: "12px", display: "inline-block", marginTop: "20px", marginBottom: "15px", maxWidth: "100%", boxSizing: "border-box" }}>
							<QRCodeSVG 
								value={qrUri} 
								size={380} // 👈 Garde une haute résolution interne
								style={{ width: "100%", height: "auto", maxWidth: "300px" }} // 👈 MAGIE CSS : Rétrécit sur mobile !
								level={"L"} 
								includeMargin={true}
							/>
						</div>
					</div>

					{/* 💡 NOUVEAU : Champ pour demander un montant spécifique */}
					<input 
						type="number" 
						placeholder="Montant demandé (Optionnel)" 
						value={receiveAmount} 
						onChange={(e) => setReceiveAmount(e.target.value)} 
						style={{ width: "100%", marginBottom: "15px", textAlign: "center" }} 
					/>

					<div className="mono" style={{ background: "rgba(0,0,0,0.4)", padding: "12px", borderRadius: "8px", fontSize: "0.75rem", color: "#888", wordBreak: "break-all" }}>
						{baseAddress.substring(0, 16)}...
					</div>
					
					<button 
						className="btn-secondary" 
						style={{ width: "100%", marginTop: "15px" }}
						onClick={(e) => handleCopy(e, qrUri, receiveModal)}
					>
						{copied === receiveModal ? "URI Copiée !" : "Copier le lien de paiement"}
					</button>
				</div>
			  </div>
			  );
			})()}
          </main>
        </div>
      )}

      {view === "casino" && (
        <div className="dashboard-layout"><Sidebar activeTab="casino" />
          <main className="main-content">
            <header>
              <h1 style={{ display: "flex", alignItems: "center", gap: "15px" }}><Dices /> Cyber-Jackpot L1</h1>
              <p style={{ color: "var(--text-muted)" }}>La loterie inviolable ancrée dans le consensus. Tirage tous les 10 blocs.</p>
            </header>

            <div className="glass-panel" style={{ padding: "50px", maxWidth: "700px", marginTop: "40px", margin: "40px auto", textAlign: "center", border: "1px solid var(--primary)", boxShadow: "0 0 40px rgba(0, 240, 255, 0.1)" }}>
              
              {hasWonJackpot && (
                <div style={{ background: "rgba(16, 185, 129, 0.2)", color: "#10b981", padding: "15px", borderRadius: "8px", border: "1px solid #10b981", marginBottom: "30px", fontWeight: "bold", display: "flex", alignItems: "center", justifyContent: "center", gap: "10px" }}>
                  🎉 FÉLICITATIONS ! Vous avez remporté le Jackpot ! Les fonds sont dans votre coffre.
                </div>
              )}

              <h2 style={{ fontSize: "1.2rem", textTransform: "uppercase", letterSpacing: "2px", color: "var(--text-muted)" }}>À gagner au prochain bloc</h2>
              <div style={{ fontSize: "4rem", fontWeight: "900", color: "var(--primary)", marginTop: "20px", textShadow: "0 0 20px rgba(0, 240, 255, 0.4)" }}>
                  {displayJackpot} <span style={{fontSize: "1.5rem"}}>WATT</span>
              </div>
              <div style={{ fontSize: "1.2rem", color: "var(--text-muted)", marginBottom: "20px" }}>
                  ≈ ${jackpotUsd} USD
              </div>
              <p style={{ color: "#aaa", marginBottom: "40px", fontSize: "0.9rem" }}>La cagnotte est alimentée par une taxe de 1% sur tous les frais du réseau, plus les tickets des joueurs.</p>
              
              <button 
                className="btn-primary" 
                style={{ width: "100%", padding: "18px", fontSize: "1.2rem", fontWeight: "bold", background: "linear-gradient(135deg, var(--primary), #0088ff)" }}
                disabled={isProcessing}
                onClick={async () => {
                  if (wattBalanceL1 < 10) { toast.error("Fonds insuffisants ! Il vous faut 10 WATT."); return; }
                  setIsProcessing(true);
                  const toastId = toast.loading("Verrouillage du ticket dans le contrat...");
                  try {
					  const res = await invoke("buy_lottery_ticket", {
						senderKyberSecretHex: walletData.kyber_secret_hex, senderKyberPublicHex: walletData.watt_address
					  });
					  toast.success(res, { id: toastId, duration: 6000 });
					  setWattBalanceL1(prev => prev - 10); 
					  setCurrentJackpot(prev => prev + 10000000000); 
					} catch (error) { toast.error(error.toString(), { id: toastId }); }
                  finally { setIsProcessing(false); }
                }}
              >
                {isProcessing ? "⏳ Frappe du ticket..." : "🎟️ Acheter un ticket (10 WATT)"}
              </button>
              <p style={{ marginTop: "20px", fontSize: "0.8rem", color: "var(--text-muted)" }}>Si vous gagnez, les fonds apparaîtront automatiquement sur ce portefeuille.</p>
            </div>
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
			
			<div style={{margin: "10px 0", display:"flex", gap:"10px", justifyContent:"center"}}>
			  <button 
				className="btn-secondary"
				onClick={async () => {
				  const res = await invoke("get_active_swaps", {
					btcAddress: walletData.btc_address,
					wattAddress: walletData.watt_address
				  });
				  setPendingSwaps(res);
				  toast.success(`✅ ${res.length} swaps reçus du node`);
				}}
			  >
				🔄 Forcer refresh Swaps + voir dans console
			  </button>
			  <button onClick={() => { setPendingSwaps([]); toast("Cache vidé") }}>🗑️ Vider cache</button>
			</div>

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

                        return (
                        <tr key={i}>
                          <td>
                            {isAlice ? <span className="badge" style={{background: "rgba(16, 185, 129, 0.1)", color: "#10b981"}}>Acheteur WATT</span> : 
                             isBob ? <span className="badge" style={{background: "rgba(247, 147, 26, 0.1)", color: "var(--btc-color)"}}>Vendeur WATT</span> : 
                             <span className="badge">Observateur</span>}
                          </td>
                          <td className="mono" style={{ fontSize: "0.8rem", color: "var(--primary)" }}>
							  <div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
								<span>{s.htlc_hash.substring(0, 16)}...</span>
								
								<button
								  onClick={(e) => {
									e.stopPropagation();
									navigator.clipboard.writeText(s.htlc_hash);
									toast.success("Hash HTLC copié ! (prêt pour explorer)");
								  }}
								  style={{
									background: "transparent",
									border: "1px solid #444",
									borderRadius: "4px",
									padding: "2px 6px",
									cursor: "pointer",
									color: "#888",
									fontSize: "0.7rem"
								  }}
								  title="Copier le hash complet pour explorer BTC"
								>
								  📋
								</button>
							  </div>
							</td>
                          <td style={{ fontWeight: "bold" }}>{s.watt_amount_flames / 1000000000}</td>
                          <td style={{ fontWeight: "bold" }}>
                            {s.btc_amount_sats / 100000000}
                          </td>
                          <td>
							  {isAlice ? (
								  <div style={{ display: "flex", flexDirection: "column", gap: "10px", alignItems: "flex-start" }}>
									<button
									  className="btn-secondary"
									  disabled={getSwapState(s.htlc_hash).btcLocked}
									  onClick={async () => {
										if (isProcessing) return;

										const confirmed = window.confirm(
										  `🔒 Verrouiller ${s.btc_amount_sats / 100000000} BTC dans le HTLC ?`
										);
										if (!confirmed) return;

										setIsProcessing(true);
										const toastId = toast.loading("🔄 Création HTLC BTC + verrouillage...");

										try {
										  const secretHex = getSwapState(s.htlc_hash).secret;
										  if (!secretHex) {
											throw new Error("Secret introuvable ! Refais l’ordre.");
										  }

										  const createRes = await invoke("create_btc_htlc", {
											buyerPubkeyHex: s.buyer_btc_pubkey,
											sellerPubkeyHex: s.seller_btc_pubkey,
											secretHex: secretHex,
											locktime: 144
										  });

										  const parsed = typeof createRes === "string" ? JSON.parse(createRes) : createRes;
										  const htlcAddress = parsed.htlc_address || parsed;

										  await invoke("send_btc_to_htlc", {
											htlcAddress: htlcAddress,
											amountBtc: s.btc_amount_sats / 100000000,
											rawTx: null
										  });

										  updateSwapState(s.htlc_hash, { 
											  btcLocked: true, 
											  role: "alice",
											  secret: secretHex 
											});

										  toast.success(`✅ BTC verrouillés dans le HTLC !`, { id: toastId });

										} catch (e) {
										  console.error("Erreur bouton 1 Alice:", e);
										  toast.error("❌ " + (e.message || e), { id: toastId });
										} finally {
										  setIsProcessing(false);
										}
									  }}
									>
									  {getSwapState(s.htlc_hash).btcLocked
										? "🔒 1. BTC Verrouillés"
										: "1. Lock BTC"}
									</button>

									{getSwapState(s.htlc_hash).btcLocked && (
									  <div style={{ 
										background: "rgba(0,0,0,0.4)", 
										padding: "10px", 
										borderRadius: "6px", 
										border: "1px solid var(--primary)", 
										width: "100%" 
									  }}>
										{getSwapState(s.htlc_hash).wattClaimed ? (
										  <div style={{ color: "#10b981", fontWeight: "bold", textAlign: "center" }}>
											✅ WATT Récupérés ! Secret Révélé.
										  </div>
										) : (
										  <div style={{ 
											display: "flex", 
											alignItems: "center", 
											justifyContent: "center", 
											gap: "8px", 
											color: "var(--primary)", 
											fontSize: "0.85rem" 
										  }}>
											<div className="spinner" style={{ 
											  width: "14px", 
											  height: "14px", 
											  borderTopColor: "var(--primary)", 
											  borderRightColor: "transparent" 
											}}></div>
											Watchdog : En attente du Lock WATT de Bob...
										  </div>
										)}
									  </div>
									)}

									<button className="btn-danger" onClick={() => removeSwapFromCache(s.htlc_hash)}>
									  🗑️ Cacher
									</button>
								  </div>
							  ) : isBob ? (
								<div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
								  <button
									className="btn-secondary"
									onClick={() => handleBobLockWatt(s)}
									disabled={
									  getSwapState(s.htlc_hash).wattLocked || 
									  !btcLockedStatus[s.htlc_hash]
									}
								  >
									{getSwapState(s.htlc_hash).wattLocked 
									  ? "🔒 2. WATT Verrouillés" 
									  : "2. Lock WATT"}
								  </button>

								  {!btcLockedStatus[s.htlc_hash] && !getSwapState(s.htlc_hash).wattLocked && (
									<p className="text-xs text-amber-400 mt-1">
									  ⏳ En attente du verrouillage BTC d’Alice...
									</p>
								  )}

								  <button 
									className="btn-danger" 
									onClick={() => handleRefundWatt(s)}
								  >
									Refund WATT (Si Alice annule)
								  </button>
								</div>
							  ) : (
								"Observateur"
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
                        <div style={{ color: "var(--primary)", fontWeight: "bold", fontSize: "1.2rem" }}>+{tx.amount.toFixed(9)} {tx.coin}</div>
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
                <p style={{ color: "var(--text-muted)", marginTop: "10px" }}>Cette phrase de 48 mots est la clé maîtresse absolue (512 bits d'entropie). Ne l'exposez jamais sur internet.</p>
                
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