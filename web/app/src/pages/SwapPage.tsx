import { FormEvent, KeyboardEvent as ReactKeyboardEvent, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useConnection, useWallet } from '@solana/wallet-adapter-react';
import { useWalletModal } from '@solana/wallet-adapter-react-ui';
import { PublicKey, Transaction, type Connection } from '@solana/web3.js';
import { api, Asset, RfqRejectedResponse, RfqResponse, TradeStepResponse, WalletSettlementResponse } from '../api';

type NoticeTone = 'info' | 'success' | 'warn' | 'error';

type Notice = {
  tone: NoticeTone;
  title: string;
  detail?: string;
};

const noticeTimeoutMs: Record<NoticeTone, number> = {
  info: 4_000,
  success: 4_000,
  warn: 7_000,
  error: 7_000
};

const fallbackAssets: Asset[] = [
  { id: 'USDC', symbol: 'USDC', name: 'USD Coin', mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v', decimals: 6, min_trade_notional_usd: '1', max_trade_notional_usd: '2' },
  { id: 'SOL', symbol: 'SOL', name: 'Solana', mint: 'So11111111111111111111111111111111111111112', decimals: 9, min_trade_notional_usd: '1', max_trade_notional_usd: '2' },
  { id: 'cbBTC', symbol: 'cbBTC', name: 'Coinbase Wrapped BTC', mint: 'cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij', decimals: 8, min_trade_notional_usd: '1', max_trade_notional_usd: '5' }
];

// Conservative fallback USD prices used when the runtime hasn't surfaced a
// fresh oracle quote yet. The backend is the source of truth — these are only
// used for client-side range hints to avoid sending an obviously-out-of-range
// RFQ to the runtime.
const fallbackUsdPriceBySymbol: Record<string, number> = {
  USDC: 1,
  SOL: 150,
  cbBTC: 60_000
};

function parseDecimal(value: string | number | undefined): number | null {
  if (value === undefined) return null;
  const numeric = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(numeric) ? numeric : null;
}

function assetUsdPrice(asset: Asset): number | null {
  const fallback = fallbackUsdPriceBySymbol[asset.symbol] ?? fallbackUsdPriceBySymbol[asset.id];
  return fallback ?? null;
}

function inputUsdValue(asset: Asset, amount: string): number | null {
  const price = assetUsdPrice(asset);
  const numeric = Number(amount);
  if (!Number.isFinite(numeric) || price === null) return null;
  return numeric * price;
}

function formatUsd(value: number) {
  if (value < 1) {
    return `$${value.toFixed(2)}`;
  }
  return `$${value.toFixed(value < 100 ? 2 : 0)}`;
}

function formatRangeLabel(asset: Asset): string | null {
  const min = parseDecimal(asset.min_trade_notional_usd);
  const max = parseDecimal(asset.max_trade_notional_usd);
  if (min === null || max === null) return null;
  return `Min ${formatUsd(min)}, Max ${formatUsd(max)}`;
}

const defaultSourceSymbol = 'SOL';
const quoteExpirySeconds = 45;
const quoteRetryMs = 5 * 60 * 1_000;
const quoteInputDebounceMs = 600;
const swapStateStorageKey = 'firmament.swap.v1';
const swapStateMaxAgeMs = 12 * 60 * 60 * 1_000;
const nativeSolMint = 'So11111111111111111111111111111111111111112';
const assetLogoUrls: Record<string, string> = {
  sol: 'https://garden.imgix.net/chain_images/solana.png',
  usdc: 'https://garden.imgix.net/token-images/usdc.svg',
  cbbtc: 'https://garden.imgix.net/token-images/cbBTC.svg'
};

type PersistedSwapState = {
  version: 1;
  saved_at: number;
  wallet_address: string;
  input_mint: string;
  output_mint: string;
  amount: string;
  quote: RfqResponse | null;
  quote_expired: boolean;
  settlement: WalletSettlementResponse | null;
  lock: TradeStepResponse | null;
  redeem: TradeStepResponse | null;
  preimage: string | null;
};

function assetBySymbol(assets: Asset[], symbol: string) {
  return assets.find((asset) => asset.symbol === symbol || asset.id === symbol);
}

function fallbackAssetFor(asset: Asset) {
  return fallbackAssets.find((fallback) => (
    fallback.mint === asset.mint ||
    fallback.symbol === asset.symbol ||
    fallback.id === asset.id
  ));
}

function withFallbackLimits(asset: Asset): Asset {
  const fallback = fallbackAssetFor(asset);
  if (!fallback) return asset;
  return {
    ...fallback,
    ...asset,
    name: asset.name ?? fallback.name,
    min_trade_notional_usd: asset.min_trade_notional_usd ?? fallback.min_trade_notional_usd,
    max_trade_notional_usd: asset.max_trade_notional_usd ?? fallback.max_trade_notional_usd
  };
}

function parseAmountRaw(amount: string, decimals: number): { ok: true; raw: number } | { ok: false; message: string } {
  const value = amount.trim();
  if (!/^\d+(\.\d+)?$/.test(value)) {
    return { ok: false, message: 'Enter a valid amount.' };
  }

  const [whole, fraction = ''] = value.split('.');
  if (fraction.length > decimals) {
    return { ok: false, message: `Use ${decimals} or fewer decimal places for this asset.` };
  }

  const raw = BigInt(whole || '0') * 10n ** BigInt(decimals) + BigInt(`${fraction}${'0'.repeat(decimals)}`.slice(0, decimals) || '0');
  if (raw <= 0n) {
    return { ok: false, message: 'Enter an amount greater than zero.' };
  }
  if (raw > BigInt(Number.MAX_SAFE_INTEGER)) {
    return { ok: false, message: 'Use a smaller amount for this demo swap.' };
  }

  return { ok: true, raw: Number(raw) };
}

function formatRawAmount(raw: number, decimals: number) {
  const rawText = BigInt(Math.trunc(raw)).toString().padStart(decimals + 1, '0');
  const whole = rawText.slice(0, -decimals) || '0';
  const fraction = rawText.slice(-decimals).replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : whole;
}

function formatRawBigInt(raw: bigint, decimals: number) {
  const rawText = raw.toString().padStart(decimals + 1, '0');
  const whole = rawText.slice(0, -decimals) || '0';
  const fraction = rawText.slice(-decimals).replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : whole;
}

function bytesToHex(bytes: Uint8Array) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

function bytesFromBase64(value: string) {
  return Uint8Array.from(atob(value), (char) => char.charCodeAt(0));
}

function shortValue(value?: string) {
  if (!value) return '';
  return value.length > 14 ? `${value.slice(0, 7)}...${value.slice(-5)}` : value;
}

async function copyToClipboard(value: string) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value);
    return;
  }

  const textarea = document.createElement('textarea');
  textarea.value = value;
  textarea.setAttribute('readonly', '');
  textarea.style.position = 'fixed';
  textarea.style.opacity = '0';
  document.body.appendChild(textarea);
  textarea.select();
  document.execCommand('copy');
  document.body.removeChild(textarea);
}

function friendlyQuoteError(error: unknown) {
  const message = error instanceof Error ? error.message : '';
  if (message.includes('Failed to fetch') || message.toLowerCase().includes('unavailable') || message.toLowerCase().includes('not attached')) {
    return {
      title: 'Runtime is not ready',
      detail: 'Start the Firmament backend, wait for it to warm up, then try again.'
    };
  }

  return {
    title: 'No liquidity sources found',
    detail: 'Try a smaller amount or a different pair.'
  };
}

function quoteRejectionCopy(rejection: RfqRejectedResponse) {
  const title = rejection.message?.replace(/\.$/, '') || 'Quote rejected';
  const detail = rejection.risk_check_details.find((item) => item.trim().length > 0)
    ?? rejection.suggested_action
    ?? 'The runtime rejected this quote.';
  return { title, detail };
}

function isNativeAsset(asset: Asset) {
  return asset.kind === 'native' || asset.mint === nativeSolMint;
}

async function walletBalanceRaw(connection: Connection, wallet: PublicKey, asset: Asset) {
  if (isNativeAsset(asset)) {
    return BigInt(await connection.getBalance(wallet, 'confirmed'));
  }

  const accounts = await connection.getParsedTokenAccountsByOwner(
    wallet,
    { mint: new PublicKey(asset.mint) },
    'confirmed'
  );
  return accounts.value.reduce((total, { account }) => {
    const amount = account.data.parsed.info.tokenAmount.amount;
    return total + BigInt(typeof amount === 'string' ? amount : '0');
  }, 0n);
}

async function walletFundingIssue(
  connection: Connection,
  wallet: PublicKey,
  asset: Asset,
  requiredRaw: string
) {
  const required = BigInt(requiredRaw);
  const available = await walletBalanceRaw(connection, wallet, asset);
  if (available >= required) return null;

  return {
    title: `Insufficient ${asset.symbol} balance`,
    detail: `Wallet has ${formatRawBigInt(available, asset.decimals)} ${asset.symbol}; this quote needs ${formatRawBigInt(required, asset.decimals)} ${asset.symbol}.`
  };
}

function formatExpiry(value?: string) {
  if (!value) return 'Expiry unavailable';
  const expiry = new Date(value);
  if (Number.isNaN(expiry.getTime())) return 'Expiry unavailable';

  return new Intl.DateTimeFormat(undefined, {
    hour: 'numeric',
    minute: '2-digit',
    second: '2-digit'
  }).format(expiry);
}

function quoteExpiryMs(quote: RfqResponse | null) {
  if (quote?.status !== 'accepted') return null;
  const expiresAt = new Date(quote.expires_at).getTime();
  return Number.isNaN(expiresAt) ? null : expiresAt - Date.now();
}

function isQuoteExpired(quote: RfqResponse | null) {
  const expiresInMs = quoteExpiryMs(quote);
  return expiresInMs !== null && expiresInMs <= 0;
}

function readPersistedSwapState(): PersistedSwapState | null {
  if (typeof window === 'undefined') return null;

  try {
    const stored = window.localStorage.getItem(swapStateStorageKey);
    if (!stored) return null;

    const parsed = JSON.parse(stored) as Partial<PersistedSwapState>;
    if (parsed.version !== 1 || typeof parsed.saved_at !== 'number') return null;
    if (Date.now() - parsed.saved_at > swapStateMaxAgeMs) {
      window.localStorage.removeItem(swapStateStorageKey);
      return null;
    }

    return {
      version: 1,
      saved_at: parsed.saved_at,
      wallet_address: typeof parsed.wallet_address === 'string' ? parsed.wallet_address : '',
      input_mint: typeof parsed.input_mint === 'string' ? parsed.input_mint : '',
      output_mint: typeof parsed.output_mint === 'string' ? parsed.output_mint : '',
      amount: typeof parsed.amount === 'string' ? parsed.amount : '',
      quote: parsed.quote ?? null,
      quote_expired: Boolean(parsed.quote_expired) || isQuoteExpired(parsed.quote ?? null),
      settlement: parsed.settlement ?? null,
      lock: parsed.lock ?? null,
      redeem: parsed.redeem ?? null,
      preimage: typeof parsed.preimage === 'string' ? parsed.preimage : null
    };
  } catch {
    window.localStorage.removeItem(swapStateStorageKey);
    return null;
  }
}

function writePersistedSwapState(snapshot: PersistedSwapState | null) {
  if (typeof window === 'undefined') return;

  if (!snapshot) {
    window.localStorage.removeItem(swapStateStorageKey);
    return;
  }

  window.localStorage.setItem(swapStateStorageKey, JSON.stringify(snapshot));
}

function walletFailure(error: unknown, fallback: string) {
  const message = error instanceof Error ? error.message : '';
  if (message.toLowerCase().includes('reject') || message.toLowerCase().includes('cancel')) {
    return 'Wallet action was cancelled.';
  }
  return fallback;
}

export function SwapPage() {
  const { connection } = useConnection();
  const { publicKey, signTransaction } = useWallet();
  const { setVisible: setWalletModalVisible } = useWalletModal();
  const [restoredSwap] = useState(() => readPersistedSwapState());
  const restoredFlowRef = useRef(Boolean(restoredSwap?.quote || restoredSwap?.settlement || restoredSwap?.lock || restoredSwap?.redeem));
  const restoredQuoteRefreshRef = useRef(Boolean(restoredSwap?.quote && !restoredSwap?.settlement && !restoredSwap?.lock && !restoredSwap?.redeem));
  const persistedWalletRef = useRef(restoredSwap?.wallet_address ?? '');
  const [assets, setAssets] = useState<Asset[]>(fallbackAssets);
  const [inputMint, setInputMint] = useState(restoredSwap?.input_mint || assetBySymbol(fallbackAssets, defaultSourceSymbol)?.mint || fallbackAssets[0].mint);
  const [outputMint, setOutputMint] = useState(restoredSwap?.output_mint ?? '');
  const [amount, setAmount] = useState(restoredSwap?.amount ?? '');
  const [quote, setQuote] = useState<RfqResponse | null>(restoredSwap?.quote ?? null);
  const [quoteExpired, setQuoteExpired] = useState(Boolean(restoredSwap?.quote_expired));
  const [settlement, setSettlement] = useState<WalletSettlementResponse | null>(restoredSwap?.settlement ?? null);
  const [lock, setLock] = useState<TradeStepResponse | null>(restoredSwap?.lock ?? null);
  const [redeem, setRedeem] = useState<TradeStepResponse | null>(restoredSwap?.redeem ?? null);
  const [preimage, setPreimage] = useState<string | null>(restoredSwap?.preimage ?? null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const noticeTimeoutRef = useRef<number | null>(null);
  const quoteTimerRef = useRef<number | null>(null);
  const quoteRequestSeqRef = useRef(0);
  const quoteInFlightRef = useRef(false);

  const sourceAsset = useMemo(
    () => assets.find((asset) => asset.mint === inputMint) ?? assetBySymbol(assets, defaultSourceSymbol) ?? assetBySymbol(fallbackAssets, defaultSourceSymbol) ?? fallbackAssets[0],
    [assets, inputMint]
  );
  const destinationAssets = useMemo(
    () => assets.filter((asset) => asset.mint !== sourceAsset.mint),
    [assets, sourceAsset.mint]
  );
  const outputAsset = useMemo(
    () => destinationAssets.find((asset) => asset.mint === outputMint),
    [destinationAssets, outputMint]
  );
  const walletAddress = publicKey?.toBase58() ?? '';
  const amountValidation = useMemo(
    () => (amount.trim() ? parseAmountRaw(amount, sourceAsset.decimals) : null),
    [amount, sourceAsset.decimals]
  );
  const sourceRangeLabel = useMemo(() => formatRangeLabel(sourceAsset), [sourceAsset]);
  const notionalValidation = useMemo(() => {
    if (amountValidation?.ok !== true) return null;
    const min = parseDecimal(sourceAsset.min_trade_notional_usd);
    const max = parseDecimal(sourceAsset.max_trade_notional_usd);
    if (min === null && max === null) return null;
    const usd = inputUsdValue(sourceAsset, amount);
    if (usd === null) return null;
    if (min !== null && usd < min) {
      return { ok: false, message: `Below ${formatUsd(min)} minimum for ${sourceAsset.symbol}.` } as const;
    }
    if (max !== null && usd > max) {
      return { ok: false, message: `Above ${formatUsd(max)} maximum for ${sourceAsset.symbol}.` } as const;
    }
    return { ok: true } as const;
  }, [amount, amountValidation, sourceAsset]);
  const quoteAccepted = quote?.status === 'accepted';
  const receiveAmount = quoteAccepted && outputAsset
    ? (quote.output?.amount ?? formatRawAmount(quote.quoted_output_amount_raw, outputAsset.decimals))
    : null;
  const quoteRejection = useMemo(
    () => (quote?.status === 'rejected' ? quoteRejectionCopy(quote) : null),
    [quote]
  );
  const flowLocked = Boolean(settlement || lock || redeem);
  const completedSwap = Boolean(redeem?.settlement_status);
  const notionalOk = notionalValidation?.ok !== false;
  const showSourceRangeError = Boolean(
    sourceRangeLabel &&
    amountValidation?.ok === true &&
    notionalValidation?.ok === false
  );
  const primaryLabel = primaryActionLabel({
    hasWallet: Boolean(walletAddress),
    hasDestination: Boolean(outputAsset),
    amount,
    amountValidation,
    notionalOk,
    quote,
    quoteExpired,
    settlement,
    lock,
    redeem,
    busy
  });
  const primaryDisabled = primaryActionDisabled({
    hasWallet: Boolean(walletAddress),
    hasDestination: Boolean(outputAsset),
    amountValidation,
    notionalOk,
    quote,
    quoteExpired,
    settlement,
    lock,
    redeem,
    busy
  });

  const clearNoticeTimer = useCallback(() => {
    if (noticeTimeoutRef.current === null) return;
    window.clearTimeout(noticeTimeoutRef.current);
    noticeTimeoutRef.current = null;
  }, []);

  const clearQuoteTimer = useCallback(() => {
    if (quoteTimerRef.current === null) return;
    window.clearTimeout(quoteTimerRef.current);
    quoteTimerRef.current = null;
  }, []);

  const showNotice = useCallback((nextNotice: Notice, options?: { persist?: boolean }) => {
    clearNoticeTimer();
    setNotice(nextNotice);

    if (options?.persist) return;

    noticeTimeoutRef.current = window.setTimeout(() => {
      setNotice((currentNotice) => (currentNotice === nextNotice ? null : currentNotice));
      noticeTimeoutRef.current = null;
    }, noticeTimeoutMs[nextNotice.tone]);
  }, [clearNoticeTimer]);

  useEffect(() => {
    api.assets()
      .then((nextAssets) => {
        if (Array.isArray(nextAssets) && nextAssets.length > 0) {
          const nextAssetsWithLimits = nextAssets.map(withFallbackLimits);
          const nextSource = assetBySymbol(nextAssetsWithLimits, defaultSourceSymbol) ?? assetBySymbol(fallbackAssets, defaultSourceSymbol) ?? fallbackAssets[0];
          setAssets(nextAssetsWithLimits);
          setInputMint((currentMint) => (
            nextAssetsWithLimits.some((asset) => asset.mint === currentMint)
              ? currentMint
              : nextSource.mint
          ));
          setOutputMint((currentMint) => (
            nextAssetsWithLimits.some((asset) => asset.mint === currentMint && asset.mint !== nextSource.mint)
              ? currentMint
              : ''
          ));
        }
      })
      .catch(() => setAssets(fallbackAssets));
  }, []);

  useEffect(() => {
    if (!outputMint) return;
    if (!destinationAssets.some((asset) => asset.mint === outputMint)) {
      setOutputMint('');
    }
  }, [destinationAssets, outputMint]);

  useEffect(() => clearNoticeTimer, [clearNoticeTimer]);

  useEffect(() => () => {
    quoteRequestSeqRef.current += 1;
    clearQuoteTimer();
  }, [clearQuoteTimer]);

  function resetFlow() {
    quoteRequestSeqRef.current += 1;
    clearQuoteTimer();
    restoredFlowRef.current = false;
    restoredQuoteRefreshRef.current = false;
    setQuote(null);
    setQuoteExpired(false);
    setSettlement(null);
    setLock(null);
    setRedeem(null);
    setPreimage(null);
    setBusy((currentBusy) => (currentBusy === 'quote' ? null : currentBusy));
  }

  function startNewSwap() {
    resetFlow();
    setAmount('');
    setOutputMint('');
    writePersistedSwapState(null);
    showNotice({
      tone: 'success',
      title: 'New swap ready',
      detail: 'Pick a receive asset and enter a fresh amount.'
    });
  }

  useEffect(() => {
    if (restoredSwap?.quote || restoredSwap?.settlement || restoredSwap?.lock || restoredSwap?.redeem) {
      showNotice({
        tone: 'info',
        title: 'Swap restored',
        detail: 'Your in-progress swap was restored from this browser.'
      });
    }
  }, [restoredSwap, showNotice]);

  useEffect(() => {
    if (!walletAddress) return;
    if (!persistedWalletRef.current) {
      persistedWalletRef.current = walletAddress;
      return;
    }
    if (walletAddress === persistedWalletRef.current) return;

    if (quote || settlement || lock || redeem || preimage) {
      writePersistedSwapState(null);
      resetFlow();
      showNotice({
        tone: 'warn',
        title: 'Restored swap cleared',
        detail: 'Connect the original wallet to continue an in-progress settlement.'
      });
    }
    persistedWalletRef.current = walletAddress;
  }, [walletAddress, quote, settlement, lock, redeem, preimage, showNotice]);

  useEffect(() => {
    const hasStateToRestore = Boolean(
      amount.trim() ||
      outputMint ||
      quote ||
      settlement ||
      lock ||
      redeem ||
      preimage
    );

    if (!hasStateToRestore) {
      writePersistedSwapState(null);
      return;
    }

    if (walletAddress) {
      persistedWalletRef.current = walletAddress;
    }

    writePersistedSwapState({
      version: 1,
      saved_at: Date.now(),
      wallet_address: persistedWalletRef.current,
      input_mint: inputMint,
      output_mint: outputMint,
      amount,
      quote,
      quote_expired: quoteExpired || isQuoteExpired(quote),
      settlement,
      lock,
      redeem,
      preimage
    });
  }, [walletAddress, inputMint, outputMint, amount, quote, quoteExpired, settlement, lock, redeem, preimage]);

  useEffect(() => {
    if (!restoredQuoteRefreshRef.current || !walletAddress || !outputAsset || flowLocked) {
      return;
    }

    restoredQuoteRefreshRef.current = false;
    if (quote?.status === 'accepted') {
      scheduleQuoteRefresh(quoteExpiryMs(quote) ?? quoteExpirySeconds * 1_000, 'expiry');
      return;
    }
    if (quote?.status === 'rejected') {
      scheduleQuoteRefresh(quoteRetryMs, 'retry');
    }
  }, [walletAddress, outputAsset, flowLocked]);

  useEffect(() => {
    if (restoredFlowRef.current) {
      restoredFlowRef.current = false;
      if (quote || flowLocked) {
        return undefined;
      }
    }

    if (flowLocked) {
      return undefined;
    }

    resetFlow();

    if (!walletAddress || !outputAsset || amountValidation?.ok !== true) {
      return undefined;
    }

    // Backend is the source of truth, but skip the round-trip when the input
    // is obviously outside the per-asset min/max range.
    if (notionalValidation?.ok === false) {
      return undefined;
    }

    const timer = window.setTimeout(() => {
      void requestQuote('auto');
    }, quoteInputDebounceMs);
    quoteTimerRef.current = timer;

    return () => {
      if (quoteTimerRef.current === timer) {
        clearQuoteTimer();
      }
    };
  }, [walletAddress, outputMint, amount, sourceAsset.mint, sourceAsset.decimals, notionalValidation?.ok, flowLocked]);

  function scheduleQuoteRefresh(delayMs: number, reason: 'expiry' | 'retry') {
    clearQuoteTimer();
    quoteTimerRef.current = window.setTimeout(() => {
      if (reason === 'expiry') {
        setQuoteExpired(true);
      }
      void requestQuote(reason === 'expiry' ? 'refresh' : 'retry');
    }, Math.max(0, delayMs));
  }

  function selectInput(mint: string) {
    setInputMint(mint);
    if (mint === outputMint) {
      setOutputMint('');
    }
  }

  function selectOutput(mint: string) {
    setOutputMint(mint);
  }

  function flipPair() {
    if (!outputAsset || flowLocked) return;
    resetFlow();
    setInputMint(outputAsset.mint);
    setOutputMint(sourceAsset.mint);
  }

  function connectWallet() {
    setWalletModalVisible(true);
  }

  async function requestQuote(reason: 'auto' | 'refresh' | 'retry') {
    if (quoteInFlightRef.current || settlement || lock || redeem || !walletAddress || !outputAsset) {
      return;
    }

    const parsed = parseAmountRaw(amount, sourceAsset.decimals);
    if (!parsed.ok) {
      return;
    }

    const requestSeq = quoteRequestSeqRef.current + 1;
    quoteRequestSeqRef.current = requestSeq;
    quoteInFlightRef.current = true;
    setBusy('quote');
    showNotice({
      tone: 'info',
      title: reason === 'refresh' ? 'Refreshing quote' : 'Finding a firm quote',
      detail: 'Firmament is checking live price, inventory, and risk limits.'
    }, { persist: true });

    try {
      const nextQuote = await api.requestRfq({
        input_asset: sourceAsset.symbol || sourceAsset.id,
        output_asset: outputAsset.symbol || outputAsset.id,
        amount: amount.trim(),
        taker_wallet: walletAddress,
        expiry_seconds: quoteExpirySeconds
      });

      if (quoteRequestSeqRef.current !== requestSeq) return;

      setQuote(nextQuote);
      setQuoteExpired(false);
      if (nextQuote.status === 'accepted') {
        const expiresAt = new Date(nextQuote.expires_at).getTime();
        const refreshDelay = Number.isNaN(expiresAt)
          ? quoteExpirySeconds * 1_000
          : Math.max(0, expiresAt - Date.now());
        scheduleQuoteRefresh(refreshDelay, 'expiry');
        showNotice({
          tone: 'success',
          title: reason === 'refresh' ? 'Quote refreshed' : 'Quote locked',
          detail: `Fresh ${sourceAsset.symbol}->${outputAsset.symbol} terms expire at ${formatExpiry(nextQuote.expires_at)}.`
        });
      } else {
        const rejection = quoteRejectionCopy(nextQuote);
        scheduleQuoteRefresh(quoteRetryMs, 'retry');
        showNotice({
          tone: 'warn',
          title: rejection.title,
          detail: `${rejection.detail} It will retry automatically.`
        });
      }
    } catch (error) {
      if (quoteRequestSeqRef.current !== requestSeq) return;

      const friendly = friendlyQuoteError(error);
      setQuote(null);
      setQuoteExpired(false);
      scheduleQuoteRefresh(quoteRetryMs, 'retry');
      showNotice({
        tone: 'error',
        title: friendly.title,
        detail: `${friendly.detail} It will retry automatically.`
      });
    } finally {
      quoteInFlightRef.current = false;
      if (quoteRequestSeqRef.current === requestSeq) {
        setBusy(null);
      }
    }
  }

  async function startSettlement() {
    if (quote?.status !== 'accepted' || quoteExpired || !publicKey) return;
    clearQuoteTimer();
    setBusy('settlement');
    showNotice({ tone: 'info', title: 'Preparing secure settlement', detail: 'Your wallet will be asked to sign the next step.' }, { persist: true });
    try {
      const secret = crypto.getRandomValues(new Uint8Array(32));
      const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', secret));
      setPreimage(bytesToHex(secret));
      setSettlement(await api.walletSettlement(quote.quote_id, {
        taker_wallet: publicKey.toBase58(),
        secret_hash: bytesToHex(hash)
      }));
      showNotice({ tone: 'success', title: 'Secure settlement ready', detail: 'Your wallet can now lock funds for this swap.' });
    } catch (error) {
      showNotice({ tone: 'error', title: 'Unable to prepare settlement', detail: error instanceof Error ? error.message : undefined });
    } finally {
      setBusy(null);
    }
  }

  async function signAndSubmit(transactionBase64: string) {
    if (!signTransaction) {
      throw new Error('Connected wallet cannot sign Solana transactions.');
    }
    const transaction = Transaction.from(bytesFromBase64(transactionBase64));
    const signed = await signTransaction(transaction);
    return connection.sendRawTransaction(signed.serialize());
  }

  async function takerLock() {
    if (!settlement?.trade_id || quote?.status !== 'accepted' || !publicKey) return;
    setBusy('lock');
    showNotice({ tone: 'info', title: 'Waiting for wallet signature', detail: 'Your wallet will lock funds for the swap.' }, { persist: true });
    try {
      const fundingIssue = await walletFundingIssue(
        connection,
        publicKey,
        sourceAsset,
        quote.input.amount_raw
      );
      if (fundingIssue) {
        showNotice({ tone: 'error', ...fundingIssue });
        return;
      }

      const signature = await signAndSubmit(settlement.taker_lock_transaction.transaction_base64);
      setLock(await api.takerLock(settlement.trade_id, { signature }));
      showNotice({ tone: 'success', title: 'Funds locked', detail: 'Maker liquidity is now being reserved for your swap.' });
    } catch (error) {
      showNotice({ tone: 'error', title: walletFailure(error, 'Unable to lock funds.') });
    } finally {
      setBusy(null);
    }
  }

  async function takerRedeem() {
    if (!settlement?.trade_id || !preimage) return;
    setBusy('redeem');
    showNotice({ tone: 'info', title: 'Completing swap', detail: 'Your wallet may need one final signature.' }, { persist: true });
    try {
      const prepared = await api.takerRedeem(settlement.trade_id, { preimage });
      if (!prepared.taker_redeem_transaction) {
        setRedeem(prepared);
        showNotice({ tone: 'success', title: 'Swap complete', detail: 'The runtime has finalized this trade.' });
        return;
      }
      const signature = await signAndSubmit(prepared.taker_redeem_transaction.transaction_base64);
      setRedeem(await api.takerRedeem(settlement.trade_id, { preimage, signature }));
      showNotice({ tone: 'success', title: 'Swap complete', detail: 'Funds exchanged successfully.' });
    } catch (error) {
      showNotice({ tone: 'error', title: walletFailure(error, 'Unable to complete swap.') });
    } finally {
      setBusy(null);
    }
  }

  async function handlePrimaryAction() {
    if (redeem?.settlement_status) return startNewSwap();
    if (!walletAddress) return connectWallet();
    if (quote?.status === 'accepted' && !quoteExpired && !settlement) return startSettlement();
    if (settlement?.trade_id && !lock) return takerLock();
    if (lock?.settlement_status && !redeem) return takerRedeem();
    if (!outputAsset) {
      showNotice({ tone: 'warn', title: 'Choose a receive asset.' });
      return undefined;
    }
    if (amountValidation?.ok === false) {
      showNotice({ tone: 'error', title: amountValidation.message });
      return undefined;
    }
    if (notionalValidation?.ok === false) {
      showNotice({ tone: 'error', title: notionalValidation.message });
    }
    return undefined;
  }

  function submitForm(event: FormEvent) {
    event.preventDefault();
    void handlePrimaryAction();
  }

  return (
    <main className="swap-page">
      <section className="swap-page-header">
        <div className="hero-brand-stack">
          <p className="eyebrow">Maker Runtime</p>
          <h1>Firmament</h1>
          <p className="lede">Firm quotes. Managed inventory. Live Solana settlement.</p>
        </div>
        <button className="wallet-button" onClick={connectWallet}>
          {walletAddress ? shortValue(walletAddress) : 'Connect wallet'}
        </button>
      </section>

      {notice && <NotificationBar notice={notice} />}

      <section className="swap-layout">
        <form className="swap-card" onSubmit={submitForm}>
          <div className="swap-card-heading">
            <div>
              <h2>Swap</h2>
            </div>
            <QuotePill quote={quote} quoteExpired={quoteExpired} busy={busy} />
          </div>

          <TokenRow
            label="You pay"
            amount={amount}
            assetMint={sourceAsset.mint}
            assets={assets}
            disabled={!outputAsset || flowLocked}
            selectDisabled={flowLocked}
            placeholder="0.00"
            onAmountChange={setAmount}
            onAssetChange={selectInput}
          />

          {showSourceRangeError && (
            <p className="field-error">
              {sourceAsset.symbol} trade range: {sourceRangeLabel}
            </p>
          )}

          <div className="flip-row">
            <button
              type="button"
              className="flip-button"
              onClick={flipPair}
              disabled={!outputAsset || flowLocked}
              aria-label="Flip pay and receive assets"
              title="Flip pay and receive assets"
            >
              <span className="flip-glyph" aria-hidden="true" />
              <span>Flip</span>
            </button>
          </div>

          <TokenRow
            label="You receive"
            amount={receiveAmount ?? ''}
            assetMint={outputMint}
            assets={destinationAssets}
            onAmountChange={() => undefined}
            onAssetChange={selectOutput}
            readOnly
            selectDisabled={flowLocked}
            placeholder="Estimated output"
            selectPlaceholder="Select"
            loading={busy === 'quote'}
          />

          {!outputAsset && <p className="locked-note">Choose a receive asset.</p>}
          {outputAsset && amountValidation?.ok === false && <p className="field-error">{amountValidation.message}</p>}
          {quoteRejection && <p className="field-error">{quoteRejection.detail}</p>}

          <button
            className={completedSwap ? 'primary-swap-button primary-swap-button-reset' : 'primary-swap-button'}
            disabled={primaryDisabled}
          >
            {primaryLabel}
          </button>

          {quoteAccepted && (
            <div className={quoteExpired ? 'quote-summary quote-summary-expired' : 'quote-summary'}>
              <span>{quoteExpired ? 'Refreshing expired quote' : 'Rate reserved until'}</span>
              <strong>{formatExpiry(quote.expires_at)}</strong>
              <span>Solver fee/spread</span>
              <strong>{quote.spread_bps} bps</strong>
            </div>
          )}
        </form>

        <BehindScenesPanel
          quote={quote}
          quoteExpired={quoteExpired}
          settlement={settlement}
          lock={lock}
          redeem={redeem}
        />
      </section>
    </main>
  );
}

function primaryActionLabel({
  hasWallet,
  hasDestination,
  amount,
  amountValidation,
  notionalOk,
  quote,
  quoteExpired,
  settlement,
  lock,
  redeem,
  busy
}: {
  hasWallet: boolean;
  hasDestination: boolean;
  amount: string;
  amountValidation: { ok: true; raw: number } | { ok: false; message: string } | null;
  notionalOk: boolean;
  quote: RfqResponse | null;
  quoteExpired: boolean;
  settlement: WalletSettlementResponse | null;
  lock: TradeStepResponse | null;
  redeem: TradeStepResponse | null;
  busy: string | null;
}) {
  if (busy === 'quote') return 'Finding quote...';
  if (busy === 'settlement') return 'Preparing settlement...';
  if (busy === 'lock') return 'Waiting for wallet...';
  if (busy === 'redeem') return 'Completing swap...';
  if (redeem?.settlement_status) return 'Start new swap';
  if (!hasWallet) return 'Connect wallet';
  if (lock?.settlement_status) return 'Complete swap';
  if (settlement?.trade_id) return 'Lock funds';
  if (!hasDestination) return 'Choose receive asset';
  if (!amount.trim()) return 'Enter amount';
  if (amountValidation?.ok === false) return 'Fix amount';
  if (!notionalOk) return 'Adjust amount';
  if (quote?.status === 'accepted' && quoteExpired) return 'Refreshing quote...';
  if (quote?.status === 'accepted') return 'Continue';
  if (quote?.status === 'rejected') return 'Retrying quote...';
  return 'Quote updates automatically';
}

function primaryActionDisabled({
  hasWallet,
  hasDestination,
  amountValidation,
  notionalOk,
  quote,
  quoteExpired,
  settlement,
  lock,
  redeem,
  busy
}: {
  hasWallet: boolean;
  hasDestination: boolean;
  amountValidation: { ok: true; raw: number } | { ok: false; message: string } | null;
  notionalOk: boolean;
  quote: RfqResponse | null;
  quoteExpired: boolean;
  settlement: WalletSettlementResponse | null;
  lock: TradeStepResponse | null;
  redeem: TradeStepResponse | null;
  busy: string | null;
}) {
  if (Boolean(busy)) return true;
  if (redeem?.settlement_status) return false;
  if (!hasWallet) return false;
  if (lock?.settlement_status || settlement?.trade_id) return false;
  if (quote?.status === 'accepted' && !quoteExpired) return false;
  if (!hasDestination || amountValidation?.ok !== true || !notionalOk) return true;
  return true;
}

function TokenRow({
  label,
  amount,
  assetMint,
  assets,
  readOnly = false,
  disabled = false,
  selectDisabled = false,
  loading = false,
  placeholder,
  selectPlaceholder,
  onAmountChange,
  onAssetChange
}: {
  label: string;
  amount: string;
  assetMint: string;
  assets: Asset[];
  readOnly?: boolean;
  disabled?: boolean;
  selectDisabled?: boolean;
  loading?: boolean;
  placeholder?: string;
  selectPlaceholder?: string;
  onAmountChange: (amount: string) => void;
  onAssetChange: (mint: string) => void;
}) {
  return (
    <div className={loading ? 'token-row token-row-loading' : 'token-row'} aria-busy={loading || undefined}>
      <span>{label}</span>
      <div className="token-row-controls">
        <input
          aria-label={`${label} amount`}
          value={amount}
          onChange={(event) => onAmountChange(event.target.value)}
          inputMode="decimal"
          placeholder={placeholder ?? '0.00'}
          readOnly={readOnly}
          disabled={disabled}
        />
        <AssetSelect
          label={label}
          assetMint={assetMint}
          assets={assets}
          disabled={selectDisabled}
          placeholder={selectPlaceholder}
          onAssetChange={onAssetChange}
        />
      </div>
    </div>
  );
}

function AssetSelect({
  label,
  assetMint,
  assets,
  disabled,
  placeholder,
  onAssetChange
}: {
  label: string;
  assetMint: string;
  assets: Asset[];
  disabled: boolean;
  placeholder?: string;
  onAssetChange: (mint: string) => void;
}) {
  const selectedAsset = assets.find((asset) => asset.mint === assetMint);
  const symbol = selectedAsset?.symbol ?? '';
  const logoUrl = assetLogoUrls[symbol.toLowerCase()];
  const [open, setOpen] = useState(false);
  const selectRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (!open) return undefined;

    function closeOnOutsideClick(event: MouseEvent) {
      if (!selectRef.current?.contains(event.target as Node)) {
        setOpen(false);
      }
    }

    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === 'Escape') {
        setOpen(false);
      }
    }

    document.addEventListener('mousedown', closeOnOutsideClick);
    document.addEventListener('keydown', closeOnEscape);
    return () => {
      document.removeEventListener('mousedown', closeOnOutsideClick);
      document.removeEventListener('keydown', closeOnEscape);
    };
  }, [open]);

  function chooseAsset(mint: string) {
    onAssetChange(mint);
    setOpen(false);
  }

  function handleKeyDown(event: ReactKeyboardEvent<HTMLButtonElement>) {
    if (event.key === 'ArrowDown') {
      event.preventDefault();
      if (!disabled) setOpen(true);
    }
  }

  return (
    <div className="asset-select-shell" ref={selectRef}>
      <button
        type="button"
        className={disabled ? 'asset-select asset-select-disabled' : 'asset-select'}
        onClick={() => setOpen((currentOpen) => !currentOpen)}
        onKeyDown={handleKeyDown}
        disabled={disabled}
        aria-label={`${label} asset`}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <AssetMark symbol={symbol} logoUrl={logoUrl} priority />
        <span className="asset-select-copy">
          <strong>{selectedAsset?.symbol ?? placeholder ?? 'Select'}</strong>
        </span>
        <span className="asset-select-arrow" aria-hidden="true" />
      </button>

      {open && (
        <div className="asset-menu" role="listbox" aria-label={`${label} asset options`}>
          <div className="asset-menu-heading">
            <strong>Select</strong>
            <span>{assets.length} assets</span>
          </div>
          {assets.map((asset) => (
            <button
              key={asset.mint}
              type="button"
              className={asset.mint === assetMint ? 'asset-option asset-option-active' : 'asset-option'}
              onClick={() => chooseAsset(asset.mint)}
              role="option"
              aria-selected={asset.mint === assetMint}
            >
              <AssetMark symbol={asset.symbol} logoUrl={assetLogoUrls[asset.symbol.toLowerCase()]} />
              <span className="asset-option-copy">
                <strong>{asset.symbol}</strong>
                <small>{asset.name}</small>
              </span>
              {asset.mint === assetMint && <span className="asset-option-check" aria-hidden="true" />}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function AssetMark({ symbol, logoUrl, priority = false }: { symbol: string; logoUrl?: string; priority?: boolean }) {
  const [failed, setFailed] = useState(false);
  const fallback = symbol.slice(0, 2).toUpperCase() || '--';

  useEffect(() => {
    setFailed(false);
  }, [logoUrl]);

  return (
    <span className={logoUrl && !failed ? 'asset-mark asset-mark-logo' : 'asset-mark asset-mark-fallback'} aria-hidden="true">
      {logoUrl && !failed ? (
        <img
          src={logoUrl}
          alt=""
          loading={priority ? 'eager' : 'lazy'}
          decoding="async"
          onError={() => setFailed(true)}
        />
      ) : (
        fallback
      )}
    </span>
  );
}

function QuotePill({ quote, quoteExpired, busy }: { quote: RfqResponse | null; quoteExpired: boolean; busy: string | null }) {
  if (busy === 'quote') return <span className="quote-pill quote-pill-refreshing">Updating quote</span>;
  if (quote?.status === 'accepted' && quoteExpired) return <span className="quote-pill quote-pill-expired">Refreshing</span>;
  if (quote?.status === 'accepted') return <span className="quote-pill">Quote locked</span>;
  return null;
}

function NotificationBar({ notice }: { notice: Notice }) {
  return (
    <section className={`notice-bar notice-${notice.tone}`} role="status" aria-live="polite">
      <strong>{notice.title}</strong>
      {notice.detail && <span>{notice.detail}</span>}
    </section>
  );
}

function BehindScenesPanel({
  quote,
  quoteExpired,
  settlement,
  lock,
  redeem
}: {
  quote: RfqResponse | null;
  quoteExpired: boolean;
  settlement: WalletSettlementResponse | null;
  lock: TradeStepResponse | null;
  redeem: TradeStepResponse | null;
}) {
  const [copiedProofId, setCopiedProofId] = useState<string | null>(null);
  const copiedTimer = useRef<number | null>(null);
  const makerProof = lock?.maker_lock_signature ?? redeem?.maker_redeem_signature;
  const networkProofs = [
    ...(lock?.tx_signatures ?? []),
    ...(redeem?.tx_signatures ?? []),
    ...(makerProof ? [makerProof] : [])
  ].filter(Boolean);

  useEffect(() => {
    return () => {
      if (copiedTimer.current !== null) {
        window.clearTimeout(copiedTimer.current);
      }
    };
  }, []);

  async function copyProof(signature: string, proofId: string) {
    await copyToClipboard(signature);
    setCopiedProofId(proofId);
    if (copiedTimer.current !== null) {
      window.clearTimeout(copiedTimer.current);
    }
    copiedTimer.current = window.setTimeout(() => setCopiedProofId(null), 1_400);
  }

  return (
    <aside className="proof-panel" aria-label="What happened behind the scenes">
      <div className="proof-heading">
        <p className="eyebrow">Behind the scenes</p>
        <h2>Runtime proof</h2>
      </div>

      <ProofRow done={quote?.status === 'accepted' && !quoteExpired} title="Quote locked" detail="This rate is reserved for a short time." />
      <ProofRow done={Boolean(settlement?.trade_id)} title="Secure settlement ready" detail="Your wallet signs each fund movement." />
      <ProofRow done={Boolean(lock?.maker_lock_signature)} title="Liquidity reserved" detail="Maker liquidity is committed for this swap." />
      <ProofRow done={Boolean(redeem?.settlement_status)} title="Swap complete" detail="Funds exchanged successfully." />

      <div className="network-proof">
        <span>Network proof</span>
        {networkProofs.length === 0 ? (
          <p>Proof appears after wallet signatures land on Solana.</p>
        ) : (
          networkProofs.slice(0, 3).map((signature, index) => {
            const proofId = `${signature}-${index}`;
            const copied = copiedProofId === proofId;
            return (
              <div className="network-proof-item" key={proofId}>
                <a href={`https://solscan.io/tx/${signature}`} target="_blank" rel="noreferrer">
                  {shortValue(signature)}
                </a>
                <button
                  className={copied ? 'copy-proof-button copied' : 'copy-proof-button'}
                  type="button"
                  onClick={() => copyProof(signature, proofId)}
                  aria-label={`Copy transaction signature ${shortValue(signature)}`}
                  title={copied ? 'Copied' : 'Copy signature'}
                >
                  <span className="copy-glyph" aria-hidden="true" />
                  <span className="copy-proof-label">{copied ? 'Copied' : 'Copy'}</span>
                </button>
              </div>
            );
          })
        )}
      </div>
    </aside>
  );
}

function ProofRow({ done, title, detail }: { done: boolean; title: string; detail: string }) {
  return (
    <div className={done ? 'proof-row proof-done' : 'proof-row'}>
      <span>{done ? 'Done' : 'Waiting'}</span>
      <div>
        <strong>{title}</strong>
        <p>{detail}</p>
      </div>
    </div>
  );
}
