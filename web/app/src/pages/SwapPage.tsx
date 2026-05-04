import { FormEvent, useEffect, useMemo, useState } from 'react';
import { Connection, PublicKey, Transaction } from '@solana/web3.js';
import { api, Asset, RfqResponse, TradeStepResponse, WalletSettlementResponse } from '../api';

type BrowserSolanaProvider = {
  isPhantom?: boolean;
  isSolflare?: boolean;
  publicKey?: PublicKey;
  connect: () => Promise<{ publicKey: PublicKey }>;
  disconnect?: () => Promise<void>;
  signTransaction: (transaction: Transaction) => Promise<Transaction>;
};

declare global {
  interface Window {
    solana?: BrowserSolanaProvider;
    solflare?: BrowserSolanaProvider;
  }
}

const fallbackAssets: Asset[] = [
  { id: 'USDC', symbol: 'USDC', name: 'USD Coin', mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v', decimals: 6 },
  { id: 'SOL', symbol: 'SOL', name: 'Solana', mint: 'So11111111111111111111111111111111111111112', decimals: 9 },
  { id: 'cbBTC', symbol: 'cbBTC', name: 'Coinbase Wrapped BTC', mint: 'cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij', decimals: 8 }
];

function rawAmount(amount: string, decimals: number) {
  const [whole, fraction = ''] = amount.trim().split('.');
  const padded = `${fraction}${'0'.repeat(decimals)}`.slice(0, decimals);
  return Number(`${whole || '0'}${padded}`.replace(/^0+(?=\d)/, '') || '0');
}

function bytesToHex(bytes: Uint8Array) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

function bytesFromBase64(value: string) {
  return Uint8Array.from(atob(value), (char) => char.charCodeAt(0));
}

export function SwapPage() {
  const endpoint = import.meta.env.VITE_SOLANA_RPC_URL ?? 'https://api.mainnet-beta.solana.com';
  const [provider, setProvider] = useState<BrowserSolanaProvider | null>(null);
  const [publicKey, setPublicKey] = useState<PublicKey | null>(null);
  const [assets, setAssets] = useState<Asset[]>(fallbackAssets);
  const [inputMint, setInputMint] = useState(fallbackAssets[0].mint);
  const [outputMint, setOutputMint] = useState(fallbackAssets[1].mint);
  const [amount, setAmount] = useState('1.00');
  const [quote, setQuote] = useState<RfqResponse | null>(null);
  const [settlement, setSettlement] = useState<WalletSettlementResponse | null>(null);
  const [lock, setLock] = useState<TradeStepResponse | null>(null);
  const [redeem, setRedeem] = useState<TradeStepResponse | null>(null);
  const [preimage, setPreimage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    setProvider(window.solana ?? window.solflare ?? null);
    api.assets()
      .then((nextAssets) => {
        if (Array.isArray(nextAssets) && nextAssets.length > 0) {
          setAssets(nextAssets);
          setInputMint(nextAssets[0].mint);
          setOutputMint(nextAssets[1]?.mint ?? nextAssets[0].mint);
        }
      })
      .catch(() => setAssets(fallbackAssets));
  }, []);

  const inputAsset = useMemo(() => assets.find((asset) => asset.mint === inputMint) ?? assets[0], [assets, inputMint]);
  const outputAsset = useMemo(() => assets.find((asset) => asset.mint === outputMint) ?? assets[1] ?? assets[0], [assets, outputMint]);

  async function submitRfq(event: FormEvent) {
    event.preventDefault();
    setError(null);
    setQuote(null);
    setSettlement(null);
    setLock(null);
    setRedeem(null);
    setPreimage(null);

    if (!publicKey) {
      setError('Connect a Solana wallet before requesting a firm quote.');
      return;
    }

    setBusy('quote');
    try {
      const nextQuote = await api.requestRfq({
        input_mint: inputMint,
        output_mint: outputMint,
        input_amount_raw: rawAmount(amount, inputAsset.decimals),
        taker_wallet: publicKey.toBase58(),
        expiry_seconds: 45
      });
      setQuote(nextQuote);
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : 'Unable to request RFQ.');
    } finally {
      setBusy(null);
    }
  }

  async function startSettlement() {
    if (quote?.status !== 'accepted' || !publicKey) return;
    setBusy('settlement');
    setError(null);
    try {
      const secret = crypto.getRandomValues(new Uint8Array(32));
      const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', secret));
      setPreimage(bytesToHex(secret));
      setSettlement(await api.walletSettlement(quote.quote_id, {
        taker_wallet: publicKey.toBase58(),
        secret_hash: bytesToHex(hash)
      }));
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : 'Unable to start wallet settlement.');
    } finally {
      setBusy(null);
    }
  }

  async function signAndSubmit(transactionBase64: string) {
    if (!provider) {
      throw new Error('No browser Solana wallet provider found.');
    }
    const transaction = Transaction.from(bytesFromBase64(transactionBase64));
    const signed = await provider.signTransaction(transaction);
    const connection = new Connection(endpoint, 'confirmed');
    return connection.sendRawTransaction(signed.serialize());
  }

  async function connectWallet() {
    setError(null);
    const nextProvider = window.solana ?? window.solflare ?? null;
    if (!nextProvider) {
      setError('Install or enable a Solana browser wallet such as Phantom or Solflare.');
      return;
    }
    const response = await nextProvider.connect();
    setProvider(nextProvider);
    setPublicKey(response.publicKey);
  }

  async function takerLock() {
    if (!settlement?.trade_id) return;
    setBusy('lock');
    setError(null);
    try {
      const signature = await signAndSubmit(settlement.taker_lock_transaction.transaction_base64);
      setLock(await api.takerLock(settlement.trade_id, { signature }));
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : 'Unable to lock taker funds.');
    } finally {
      setBusy(null);
    }
  }

  async function takerRedeem() {
    if (!settlement?.trade_id || !preimage) return;
    setBusy('redeem');
    setError(null);
    try {
      const prepared = await api.takerRedeem(settlement.trade_id, { preimage });
      if (!prepared.taker_redeem_transaction) {
        setRedeem(prepared);
        return;
      }
      const signature = await signAndSubmit(prepared.taker_redeem_transaction.transaction_base64);
      setRedeem(await api.takerRedeem(settlement.trade_id, { preimage, signature }));
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : 'Unable to redeem taker settlement.');
    } finally {
      setBusy(null);
    }
  }

  return (
    <main className="panel-grid">
      <section className="hero-panel">
        <p className="eyebrow">Wallet RFQ lane</p>
        <h1>Ask the maker for a firm tiny quote.</h1>
        <p className="lede">Connect a Solana wallet, choose inventory pair, request an RFQ, then walk through the placeholder wallet settlement flow exactly as the runtime contract expects.</p>
        <button className="wallet-button" onClick={connectWallet}>
          {publicKey ? `${publicKey.toBase58().slice(0, 6)}...${publicKey.toBase58().slice(-4)}` : 'Connect wallet'}
        </button>
      </section>

      <section className="trade-card" aria-label="RFQ form">
        <form onSubmit={submitRfq}>
          <label>
            Pay asset
            <select value={inputMint} onChange={(event) => setInputMint(event.target.value)}>
              {assets.map((asset) => <option key={asset.mint} value={asset.mint}>{asset.symbol}</option>)}
            </select>
          </label>
          <label>
            Receive asset
            <select value={outputMint} onChange={(event) => setOutputMint(event.target.value)}>
              {assets.map((asset) => <option key={asset.mint} value={asset.mint}>{asset.symbol}</option>)}
            </select>
          </label>
          <label>
            Amount
            <input value={amount} onChange={(event) => setAmount(event.target.value)} inputMode="decimal" placeholder="1.00" />
          </label>
          <button className="action-button" disabled={!publicKey || busy === 'quote'}>
            {busy === 'quote' ? 'Requesting...' : 'Request RFQ'}
          </button>
        </form>
        <div className="pair-note">
          <span>{inputAsset?.symbol}</span>
          <b>to</b>
          <span>{outputAsset?.symbol}</span>
        </div>
      </section>

      <section className="flow-card">
        <div className="step-row done">
          <span>1</span>
          <div>
            <h2>Wallet connected</h2>
            <p>{publicKey ? publicKey.toBase58() : 'Waiting for taker wallet.'}</p>
          </div>
        </div>
        <div className={quote?.status === 'accepted' ? 'step-row done' : 'step-row'}>
          <span>2</span>
          <div>
            <h2>RFQ response</h2>
            <p>{quote?.status === 'accepted' ? `Quote ${quote.quote_id}, spread ${quote.spread_bps} bps` : quote?.status === 'rejected' ? `${quote.reason}: ${quote.risk_check_details.join(', ')}` : 'No quote yet.'}</p>
            {quote?.status === 'accepted' && <button onClick={startSettlement} disabled={busy === 'settlement'}>Start wallet settlement</button>}
          </div>
        </div>
        <div className={settlement?.trade_id ? 'step-row done' : 'step-row'}>
          <span>3</span>
          <div>
            <h2>Settlement terms</h2>
            <p>{settlement?.trade_id ? `Trade ${settlement.trade_id}` : 'POST /v1/quotes/{quote_id}/wallet-settlement'}</p>
            {settlement?.trade_id && <button onClick={takerLock} disabled={busy === 'lock'}>Taker lock</button>}
          </div>
        </div>
        <div className={lock?.settlement_status ? 'step-row done' : 'step-row'}>
          <span>4</span>
          <div>
            <h2>Taker lock</h2>
            <p>{lock?.maker_lock_signature ?? 'POST /v1/trades/{trade_id}/taker-lock'}</p>
            {settlement?.trade_id && <button onClick={takerRedeem} disabled={busy === 'redeem'}>Taker redeem</button>}
          </div>
        </div>
        <div className={redeem?.settlement_status ? 'step-row done' : 'step-row'}>
          <span>5</span>
          <div>
            <h2>Taker redeem</h2>
            <p>{redeem?.maker_redeem_signature ?? redeem?.settlement_status ?? 'POST /v1/trades/{trade_id}/taker-redeem'}</p>
          </div>
        </div>
        {error && <p className="error-box">{error}</p>}
      </section>
    </main>
  );
}
