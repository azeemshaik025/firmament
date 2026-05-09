import { FormEvent, KeyboardEvent as ReactKeyboardEvent, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useConnection, useWallet } from '@solana/wallet-adapter-react';
import { useWalletModal } from '@solana/wallet-adapter-react-ui';
import { PublicKey, Transaction, type Connection } from '@solana/web3.js';
import { api, Asset, RfqRejectedResponse, RfqResponse, RuntimeTrade, RuntimeTradesResponse, TradeSignature, TradeSignatureKind, TradeStepResponse, WalletSettlementResponse, WalletSettlementResumeResponse } from '../api';

type NoticeTone = 'info' | 'success' | 'warn' | 'error';

type Notice = {
  tone: NoticeTone;
  title: string;
  detail?: string;
};

type TerminalFailure = {
  kind: 'insufficient_funds' | 'missing_signer' | 'pre_lock_blocker';
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
  { id: 'USDC', symbol: 'USDC', name: 'USD Coin', mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v', decimals: 6, min_trade_amount: '1', max_trade_amount: '2' },
  { id: 'SOL', symbol: 'SOL', name: 'Solana', mint: 'So11111111111111111111111111111111111111112', decimals: 9, min_trade_amount: '0.01', max_trade_amount: '0.02' },
  { id: 'cbBTC', symbol: 'cbBTC', name: 'Coinbase Wrapped BTC', mint: 'cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij', decimals: 8, min_trade_amount: '0.00001', max_trade_amount: '0.00002' }
];

function parseDecimal(value: string | number | undefined): number | null {
  if (value === undefined) return null;
  const numeric = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(numeric) ? numeric : null;
}

function formatAssetAmount(value: number, asset: Asset) {
  const precision = Math.min(Math.max(asset.decimals, 2), 8);
  return value.toLocaleString(undefined, {
    minimumFractionDigits: 0,
    maximumFractionDigits: precision
  });
}

function formatRangeLabel(asset: Asset): string | null {
  const min = parseDecimal(asset.min_trade_amount);
  const max = parseDecimal(asset.max_trade_amount);
  if (min === null || max === null) return null;
  return `${formatAssetAmount(min, asset)}-${formatAssetAmount(max, asset)} ${asset.symbol}`;
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
  refund: TradeStepResponse | null;
  preimage: string | null;
  pending_lock_signature?: string | null;
  pending_redeem_signature?: string | null;
  pending_refund_signature?: string | null;
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
    min_trade_amount: asset.min_trade_amount ?? fallback.min_trade_amount,
    max_trade_amount: asset.max_trade_amount ?? fallback.max_trade_amount
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

function hexToBytes(value: string) {
  const normalized = value.trim();
  if (normalized.length % 2 !== 0) {
    throw new Error('Invalid recovery secret.');
  }
  const bytes = new Uint8Array(normalized.length / 2);
  for (let index = 0; index < bytes.length; index += 1) {
    const byte = Number.parseInt(normalized.slice(index * 2, index * 2 + 2), 16);
    if (Number.isNaN(byte)) {
      throw new Error('Invalid recovery secret.');
    }
    bytes[index] = byte;
  }
  return bytes;
}

async function hashHexSecret(value: string) {
  const digest = await crypto.subtle.digest('SHA-256', hexToBytes(value));
  return bytesToHex(new Uint8Array(digest));
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
    if (parsed.quote?.status === 'rejected') {
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
      refund: parsed.refund ?? null,
      preimage: typeof parsed.preimage === 'string' ? parsed.preimage : null,
      pending_lock_signature: typeof parsed.pending_lock_signature === 'string' ? parsed.pending_lock_signature : null,
      pending_redeem_signature: typeof parsed.pending_redeem_signature === 'string' ? parsed.pending_redeem_signature : null,
      pending_refund_signature: typeof parsed.pending_refund_signature === 'string' ? parsed.pending_refund_signature : null
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
  if (isWalletCancellation(error)) {
    return 'Wallet action was cancelled.';
  }
  return fallback;
}

function isWalletCancellation(error: unknown) {
  const message = error instanceof Error ? error.message : '';
  return message.toLowerCase().includes('reject') || message.toLowerCase().includes('cancel');
}

function signatureByKind(response: WalletSettlementResumeResponse, kind: TradeSignatureKind) {
  return response.tx_signature_kinds?.find((proof) => proof.kind === kind)?.signature ?? null;
}

function proofSignatures(response: WalletSettlementResumeResponse) {
  if (response.tx_signatures?.length) return response.tx_signatures;
  return response.tx_signature_kinds?.map((proof) => proof.signature) ?? [];
}

function tradeProofSignatures(trade: RuntimeTrade) {
  return trade.tx_signatures ?? [];
}

function tradeSignatureByKind(trade: RuntimeTrade, kind: TradeSignatureKind) {
  return trade.tx_signatures?.find((proof) => proof.kind === kind)?.signature ?? null;
}

function isTerminalTrade(trade: RuntimeTrade) {
  const status = trade.settlement_status.toLowerCase();
  return status === 'redeemed' || status === 'refunded' || status === 'failed';
}

function isTradeExpired(trade: RuntimeTrade) {
  if (!trade.expires_at) return false;
  const expiresAt = new Date(trade.expires_at).getTime();
  return Number.isFinite(expiresAt) && expiresAt <= Date.now();
}

function tradeStatusLabel(status: string) {
  const normalized = status.toLowerCase();
  if (normalized === 'redeemed') return 'Redeemed';
  if (normalized === 'refunded') return 'Refunded';
  if (normalized === 'failed') return 'Failed';
  if (normalized === 'initiated') return 'Locked';
  if (normalized === 'pending') return 'Pending';
  return normalized ? normalized.replace(/_/g, ' ') : 'Pending';
}

function historyAmount(amount: RuntimeTrade['input']) {
  return `${amount.display_amount || amount.amount_raw} ${amount.asset}`;
}

const emptyWalletTransaction: WalletSettlementResponse['taker_lock_transaction'] = {
  transaction_base64: '',
  recent_blockhash: ''
};

export function SwapPage() {
  const { connection } = useConnection();
  const { publicKey, signTransaction } = useWallet();
  const { setVisible: setWalletModalVisible } = useWalletModal();
  const [restoredSwap] = useState(() => readPersistedSwapState());
  const restoredFlowRef = useRef(Boolean(restoredSwap?.quote || restoredSwap?.settlement || restoredSwap?.lock || restoredSwap?.redeem || restoredSwap?.refund));
  const restoredQuoteRefreshRef = useRef(Boolean(restoredSwap?.quote && !restoredSwap?.settlement && !restoredSwap?.lock && !restoredSwap?.redeem && !restoredSwap?.refund));
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
  const [refund, setRefund] = useState<TradeStepResponse | null>(restoredSwap?.refund ?? null);
  const [preimage, setPreimage] = useState<string | null>(restoredSwap?.preimage ?? null);
  const [pendingLockSignature, setPendingLockSignature] = useState<string | null>(restoredSwap?.pending_lock_signature ?? null);
  const [pendingRedeemSignature, setPendingRedeemSignature] = useState<string | null>(restoredSwap?.pending_redeem_signature ?? null);
  const [pendingRefundSignature, setPendingRefundSignature] = useState<string | null>(restoredSwap?.pending_refund_signature ?? null);
  const [, bumpSettlementClock] = useState(0);
  const [walletHistoryOpen, setWalletHistoryOpen] = useState(false);
  const [walletHistory, setWalletHistory] = useState<RuntimeTradesResponse | null>(null);
  const [walletHistoryLoading, setWalletHistoryLoading] = useState(false);
  const [walletHistoryError, setWalletHistoryError] = useState<string | null>(null);
  const [selectedHistoryProofTrade, setSelectedHistoryProofTrade] = useState<RuntimeTrade | null>(null);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [terminalFailure, setTerminalFailure] = useState<TerminalFailure | null>(null);
  const noticeTimeoutRef = useRef<number | null>(null);
  const quoteTimerRef = useRef<number | null>(null);
  const quoteRequestSeqRef = useRef(0);
  const quoteInFlightRef = useRef(false);
  const resumedTradeRef = useRef<string | null>(null);

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
  const amountRangeValidation = useMemo(() => {
    if (amountValidation?.ok !== true) return null;
    const min = parseDecimal(sourceAsset.min_trade_amount);
    const max = parseDecimal(sourceAsset.max_trade_amount);
    if (min === null && max === null) return null;
    const numericAmount = Number(amount);
    if (!Number.isFinite(numericAmount)) return null;
    if (min !== null && numericAmount < min) {
      return { ok: false, message: `Below ${formatAssetAmount(min, sourceAsset)} ${sourceAsset.symbol} minimum.` } as const;
    }
    if (max !== null && numericAmount > max) {
      return { ok: false, message: `Above ${formatAssetAmount(max, sourceAsset)} ${sourceAsset.symbol} maximum.` } as const;
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
  const flowLocked = Boolean(settlement || lock || redeem || refund);
  const completedSwap = Boolean(redeem?.settlement_status || refund?.settlement_status);
  const refundAvailable = Boolean(
    settlement?.trade_id &&
    lock?.settlement_status &&
    !redeem?.settlement_status &&
    !refund?.settlement_status &&
    settlement.expires_at &&
    new Date(settlement.expires_at).getTime() <= Date.now()
  );
  const settlementPrelockExpired = Boolean(
    settlement?.trade_id &&
    !lock?.settlement_status &&
    settlement.expires_at &&
    new Date(settlement.expires_at).getTime() <= Date.now()
  );
  const terminalStartOver = Boolean(!lock && !redeem && (terminalFailure || quote?.status === 'rejected'));
  const amountRangeOk = amountRangeValidation?.ok !== false;
  const showSourceRangeError = Boolean(
    sourceRangeLabel &&
    amountValidation?.ok === true &&
    amountRangeValidation?.ok === false
  );
  const primaryLabel = primaryActionLabel({
    hasWallet: Boolean(walletAddress),
    hasDestination: Boolean(outputAsset),
    amount,
    amountValidation,
    amountRangeOk,
    quote,
    quoteExpired,
    settlement,
    lock,
    redeem,
    refund,
    refundAvailable,
    settlementPrelockExpired,
    terminalStartOver,
    busy
  });
  const primaryDisabled = primaryActionDisabled({
    hasWallet: Boolean(walletAddress),
    hasDestination: Boolean(outputAsset),
    amountValidation,
    amountRangeOk,
    quote,
    quoteExpired,
    settlement,
    lock,
    redeem,
    refund,
    refundAvailable,
    settlementPrelockExpired,
    terminalStartOver,
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

  const refreshWalletHistory = useCallback(async () => {
    if (!walletAddress) {
      setWalletHistory(null);
      return;
    }

    setWalletHistoryLoading(true);
    setWalletHistoryError(null);
    try {
      setWalletHistory(await api.walletTrades(walletAddress, 25));
    } catch (error) {
      setWalletHistoryError(error instanceof Error ? error.message : 'Unable to load wallet history.');
    } finally {
      setWalletHistoryLoading(false);
    }
  }, [walletAddress]);

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

  useEffect(() => {
    if (!walletAddress) {
      setWalletHistoryOpen(false);
      setWalletHistory(null);
      setSelectedHistoryProofTrade(null);
      return;
    }

    if (walletHistoryOpen) {
      void refreshWalletHistory();
    }
  }, [walletAddress, walletHistoryOpen, refreshWalletHistory]);

  useEffect(() => () => {
    quoteRequestSeqRef.current += 1;
    clearQuoteTimer();
  }, [clearQuoteTimer]);

  function resetFlow() {
    quoteRequestSeqRef.current += 1;
    clearQuoteTimer();
    restoredFlowRef.current = false;
    restoredQuoteRefreshRef.current = false;
    resumedTradeRef.current = null;
    setQuote(null);
    setQuoteExpired(false);
    setSettlement(null);
    setLock(null);
    setRedeem(null);
    setRefund(null);
    setPreimage(null);
    setPendingLockSignature(null);
    setPendingRedeemSignature(null);
    setPendingRefundSignature(null);
    setTerminalFailure(null);
    setBusy((currentBusy) => (currentBusy === 'quote' ? null : currentBusy));
  }

  function startNewSwap() {
    resetFlow();
    setAmount('');
    setOutputMint('');
    writePersistedSwapState(null);
    if (walletHistoryOpen) {
      void refreshWalletHistory();
    }
    showNotice({
      tone: 'success',
      title: 'New swap ready',
      detail: 'Pick a receive asset and enter a fresh amount.'
    });
  }

  async function startOverSwap() {
    if (settlement?.trade_id && !lock && !redeem && !refund) {
      if (!preimage) {
        showNotice({
          tone: 'warn',
          title: 'Recovery secret missing',
          detail: 'Use the original browser state, or let the unused settlement expire.'
        });
        return;
      }
      setBusy('abandon');
      showNotice({
        tone: 'info',
        title: 'Starting over',
        detail: 'Abandoning the unused settlement before any funds are locked.'
      }, { persist: true });
      try {
        await api.abandonSettlement(settlement.trade_id, {
          secret_hash: await hashHexSecret(preimage)
        });
        startNewSwap();
      } catch (error) {
        showNotice({
          tone: 'error',
          title: 'Cannot start over now',
          detail: error instanceof Error ? error.message : 'Continue this swap or refund after expiry.'
        });
      } finally {
        setBusy(null);
      }
      return;
    }

    startNewSwap();
  }

  useEffect(() => {
    if (
      restoredSwap?.quote ||
      restoredSwap?.settlement ||
      restoredSwap?.lock ||
      restoredSwap?.redeem ||
      restoredSwap?.refund ||
      restoredSwap?.pending_lock_signature ||
      restoredSwap?.pending_redeem_signature ||
      restoredSwap?.pending_refund_signature
    ) {
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

    if (quote || settlement || lock || redeem || refund || preimage) {
      writePersistedSwapState(null);
      resetFlow();
      showNotice({
        tone: 'warn',
        title: 'Restored swap cleared',
        detail: 'Connect the original wallet to continue an in-progress settlement.'
      });
    }
    persistedWalletRef.current = walletAddress;
  }, [walletAddress, quote, settlement, lock, redeem, refund, preimage, showNotice]);

  useEffect(() => {
    const hasStateToRestore = Boolean(
      amount.trim() ||
      outputMint ||
      quote ||
      settlement ||
      lock ||
      redeem ||
      refund ||
      preimage ||
      pendingLockSignature ||
      pendingRedeemSignature ||
      pendingRefundSignature
    );

    if (terminalFailure || quote?.status === 'rejected') {
      writePersistedSwapState(null);
      return;
    }

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
      refund,
      preimage,
      pending_lock_signature: pendingLockSignature,
      pending_redeem_signature: pendingRedeemSignature,
      pending_refund_signature: pendingRefundSignature
    });
  }, [walletAddress, inputMint, outputMint, amount, quote, quoteExpired, settlement, lock, redeem, refund, preimage, pendingLockSignature, pendingRedeemSignature, pendingRefundSignature, terminalFailure]);

  useEffect(() => {
    if (!restoredQuoteRefreshRef.current || !walletAddress || !outputAsset || flowLocked) {
      return;
    }

    restoredQuoteRefreshRef.current = false;
    if (quote?.status === 'accepted') {
      scheduleQuoteRefresh(quoteExpiryMs(quote) ?? quoteExpirySeconds * 1_000, 'expiry');
      return;
    }
  }, [walletAddress, outputAsset, flowLocked]);

  useEffect(() => {
    if (!settlement?.expires_at || !lock?.settlement_status || redeem?.settlement_status || refund?.settlement_status) {
      return undefined;
    }

    const expiresAt = new Date(settlement.expires_at).getTime();
    if (Number.isNaN(expiresAt)) return undefined;

    const delay = expiresAt - Date.now();
    if (delay <= 0) {
      bumpSettlementClock((tick) => tick + 1);
      return undefined;
    }

    const timer = window.setTimeout(() => {
      bumpSettlementClock((tick) => tick + 1);
    }, delay + 250);

    return () => window.clearTimeout(timer);
  }, [settlement?.expires_at, lock?.settlement_status, redeem?.settlement_status, refund?.settlement_status]);

  function applyResumeState(response: WalletSettlementResumeResponse) {
    setSettlement((currentSettlement) => {
      return {
        trade_id: response.trade_id,
        quote_id: response.quote_id,
        expires_at: response.expires_at ?? currentSettlement?.expires_at,
        taker_lock_transaction: response.taker_lock_transaction ?? currentSettlement?.taker_lock_transaction ?? emptyWalletTransaction
      };
    });

    const signatures = proofSignatures(response);
    const makerLock = signatureByKind(response, 'maker_lock');
    const takerLock = signatureByKind(response, 'taker_lock');
    if (takerLock || makerLock) {
      setLock({
        trade_id: response.trade_id,
        settlement_status: response.settlement_status,
        maker_lock_signature: makerLock ?? undefined,
        tx_signatures: signatures
      });
    } else {
      setLock(null);
    }

    const makerRedeem = signatureByKind(response, 'maker_redeem');
    const takerRedeem = signatureByKind(response, 'taker_redeem');
    if (takerRedeem || makerRedeem || response.settlement_status === 'redeemed') {
      setRedeem({
        trade_id: response.trade_id,
        settlement_status: response.settlement_status,
        maker_redeem_signature: makerRedeem ?? undefined,
        tx_signatures: signatures
      });
    } else {
      setRedeem(null);
    }

    const makerRefund = signatureByKind(response, 'maker_refund');
    const takerRefund = signatureByKind(response, 'taker_refund');
    if (takerRefund || makerRefund || response.settlement_status === 'refunded') {
      setRefund({
        trade_id: response.trade_id,
        settlement_status: response.settlement_status,
        maker_refund_signature: makerRefund ?? undefined,
        tx_signatures: signatures
      });
    } else {
      setRefund(null);
    }
  }

  useEffect(() => {
    if (!settlement?.trade_id || !walletAddress || redeem?.settlement_status || refund?.settlement_status) {
      return undefined;
    }
    if (resumedTradeRef.current === settlement.trade_id) {
      return undefined;
    }

    let cancelled = false;
    const tradeId = settlement.trade_id;
    resumedTradeRef.current = tradeId;

    async function resumeSettlement() {
      showNotice({
        tone: 'info',
        title: 'Checking settlement state',
        detail: 'Refreshing recovery state from the runtime.'
      }, { persist: true });

      try {
        const resume = await api.resumeSettlement(tradeId);
        if (cancelled) return;
        applyResumeState(resume);

        const hasTakerLock = Boolean(signatureByKind(resume, 'taker_lock'));
        const hasTakerRedeem = Boolean(signatureByKind(resume, 'taker_redeem'));
        const hasTakerRefund = Boolean(signatureByKind(resume, 'taker_refund'));

        if (pendingLockSignature && !hasTakerLock) {
          const nextLock = await api.takerLock(tradeId, { signature: pendingLockSignature });
          if (cancelled) return;
          setLock(nextLock);
          setPendingLockSignature(null);
          showNotice({ tone: 'success', title: 'Lock signature recovered', detail: 'The runtime accepted your submitted lock transaction.' });
          return;
        }

        if (pendingRedeemSignature && preimage && !hasTakerRedeem) {
          const nextRedeem = await api.takerRedeem(tradeId, { preimage, signature: pendingRedeemSignature });
          if (cancelled) return;
          setRedeem(nextRedeem);
          setPendingRedeemSignature(null);
          showNotice({ tone: 'success', title: 'Redeem signature recovered', detail: 'The swap is complete.' });
          return;
        }

        if (pendingRefundSignature && !hasTakerRefund) {
          const nextRefund = await api.takerRefund(tradeId, { signature: pendingRefundSignature });
          if (cancelled) return;
          setRefund(nextRefund);
          setPendingRefundSignature(null);
          showNotice({ tone: 'success', title: 'Refund signature recovered', detail: 'Expired funds were returned safely.' });
          return;
        }

        showNotice({
          tone: 'success',
          title: 'Swap state refreshed',
          detail: resume.taker_refund_transaction ? 'This expired swap can now be refunded.' : 'Continue from the latest persisted settlement step.'
        });
      } catch (error) {
        if (cancelled) return;
        showNotice({
          tone: 'warn',
          title: 'Could not refresh settlement',
          detail: error instanceof Error ? error.message : 'The runtime did not return a recovery state.'
        });
      }
    }

    void resumeSettlement();
    return () => {
      cancelled = true;
    };
  }, [
    settlement?.trade_id,
    walletAddress,
    redeem?.settlement_status,
    refund?.settlement_status,
    pendingLockSignature,
    pendingRedeemSignature,
    pendingRefundSignature,
    preimage,
    showNotice
  ]);

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
    // is obviously outside the per-asset denomination range.
    if (amountRangeValidation?.ok === false) {
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
  }, [walletAddress, outputMint, amount, sourceAsset.mint, sourceAsset.decimals, amountRangeValidation?.ok, flowLocked]);

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
    if (quoteInFlightRef.current || settlement || lock || redeem || refund || !walletAddress || !outputAsset) {
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
        clearQuoteTimer();
        showNotice({
          tone: 'warn',
          title: rejection.title,
          detail: rejection.detail
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
    setTerminalFailure(null);
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
      if (walletHistoryOpen) {
        void refreshWalletHistory();
      }
      showNotice({ tone: 'success', title: 'Secure settlement ready', detail: 'Your wallet can now lock funds for this swap.' });
    } catch (error) {
      const failure: TerminalFailure = {
        kind: 'pre_lock_blocker',
        title: 'Unable to prepare settlement',
        detail: error instanceof Error ? error.message : undefined
      };
      setTerminalFailure(failure);
      writePersistedSwapState(null);
      showNotice({ tone: 'error', title: failure.title, detail: failure.detail });
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
    setTerminalFailure(null);
    showNotice({ tone: 'info', title: 'Waiting for wallet signature', detail: 'Your wallet will lock funds for the swap.' }, { persist: true });
    let submittedSignature: string | null = null;
    try {
      if (!signTransaction) {
        const failure: TerminalFailure = {
          kind: 'missing_signer',
          title: 'Wallet cannot sign transactions',
          detail: 'Connect a Solana wallet that supports transaction signing.'
        };
        setTerminalFailure(failure);
        writePersistedSwapState(null);
        showNotice({ tone: 'error', title: failure.title, detail: failure.detail });
        return;
      }

      const fundingIssue = await walletFundingIssue(
        connection,
        publicKey,
        sourceAsset,
        quote.input.amount_raw
      );
      if (fundingIssue) {
        const failure: TerminalFailure = {
          kind: 'insufficient_funds',
          ...fundingIssue
        };
        setTerminalFailure(failure);
        writePersistedSwapState(null);
        showNotice({ tone: 'error', title: failure.title, detail: failure.detail });
        return;
      }

      const signature = pendingLockSignature ?? await signAndSubmit(settlement.taker_lock_transaction.transaction_base64);
      submittedSignature = signature;
      if (!pendingLockSignature) {
        setPendingLockSignature(signature);
      }
      setLock(await api.takerLock(settlement.trade_id, { signature }));
      setPendingLockSignature(null);
      if (walletHistoryOpen) {
        void refreshWalletHistory();
      }
      showNotice({ tone: 'success', title: 'Funds locked', detail: 'Maker liquidity is now being reserved for your swap.' });
    } catch (error) {
      const title = walletFailure(error, 'Unable to lock funds.');
      if (!submittedSignature && !isWalletCancellation(error)) {
        const failure: TerminalFailure = {
          kind: 'pre_lock_blocker',
          title,
          detail: error instanceof Error ? error.message : undefined
        };
        setTerminalFailure(failure);
        writePersistedSwapState(null);
        showNotice({ tone: 'error', title: failure.title, detail: failure.detail });
        return;
      }
      showNotice({ tone: 'error', title });
    } finally {
      setBusy(null);
    }
  }

  async function takerRedeem() {
    if (!settlement?.trade_id || !preimage) return;
    setBusy('redeem');
    showNotice({ tone: 'info', title: 'Completing swap', detail: 'Your wallet may need one final signature.' }, { persist: true });
    try {
      const prepared = pendingRedeemSignature
        ? null
        : await api.takerRedeem(settlement.trade_id, { preimage });
      if (prepared && !prepared.taker_redeem_transaction) {
        setRedeem(prepared);
        if (walletHistoryOpen) {
          void refreshWalletHistory();
        }
        showNotice({ tone: 'success', title: 'Swap complete', detail: 'The runtime has finalized this trade.' });
        return;
      }
      const transaction = prepared?.taker_redeem_transaction;
      if (!pendingRedeemSignature && !transaction) {
        throw new Error('Runtime did not return a redeem transaction.');
      }
      const signature = pendingRedeemSignature ?? await signAndSubmit(transaction!.transaction_base64);
      if (!pendingRedeemSignature) {
        setPendingRedeemSignature(signature);
      }
      setRedeem(await api.takerRedeem(settlement.trade_id, { preimage, signature }));
      setPendingRedeemSignature(null);
      if (walletHistoryOpen) {
        void refreshWalletHistory();
      }
      showNotice({ tone: 'success', title: 'Swap complete', detail: 'Funds exchanged successfully.' });
    } catch (error) {
      showNotice({ tone: 'error', title: walletFailure(error, 'Unable to complete swap.') });
    } finally {
      setBusy(null);
    }
  }

  async function takerRefund() {
    if (!settlement?.trade_id) return;
    setBusy('refund');
    showNotice({ tone: 'info', title: 'Preparing refund', detail: 'Your wallet will reclaim the expired locked funds.' }, { persist: true });
    try {
      const prepared = pendingRefundSignature
        ? null
        : await api.takerRefund(settlement.trade_id, {});
      if (prepared && !prepared.taker_refund_transaction) {
        setRefund(prepared);
        setPendingRefundSignature(null);
        showNotice({ tone: 'success', title: 'Refund complete', detail: 'Expired funds were returned safely.' });
        return;
      }
      const transaction = prepared?.taker_refund_transaction;
      if (!pendingRefundSignature && !transaction) {
        throw new Error('Runtime did not return a refund transaction.');
      }
      const signature = pendingRefundSignature ?? await signAndSubmit(transaction!.transaction_base64);
      if (!pendingRefundSignature) {
        setPendingRefundSignature(signature);
      }
      setRefund(await api.takerRefund(settlement.trade_id, { signature }));
      setPendingRefundSignature(null);
      if (walletHistoryOpen) {
        void refreshWalletHistory();
      }
      showNotice({ tone: 'success', title: 'Refund complete', detail: 'Expired funds were returned safely.' });
    } catch (error) {
      showNotice({ tone: 'error', title: walletFailure(error, 'Unable to refund locked funds.') });
    } finally {
      setBusy(null);
    }
  }

  function hydrateHistoryTrade(trade: RuntimeTrade, response: WalletSettlementResumeResponse) {
    const localPreimage = settlement?.trade_id === trade.trade_id ? preimage : null;
    const inputAsset = assets.find((asset) => asset.id === trade.input.asset || asset.symbol === trade.input.asset);
    const outputAssetForTrade = assets.find((asset) => asset.id === trade.output.asset || asset.symbol === trade.output.asset);

    resetFlow();
    if (inputAsset) setInputMint(inputAsset.mint);
    if (outputAssetForTrade) setOutputMint(outputAssetForTrade.mint);
    setAmount(trade.input.display_amount || trade.input.amount_raw);
    setPreimage(localPreimage);
    setQuote(null);
    setQuoteExpired(false);
    setTerminalFailure(null);
    persistedWalletRef.current = walletAddress;
    applyResumeState(response);
  }

  async function handleHistoryResume(trade: RuntimeTrade) {
    if (isTerminalTrade(trade)) {
      setSelectedHistoryProofTrade(trade);
      return;
    }

    const hasLocalPreimage = settlement?.trade_id === trade.trade_id && Boolean(preimage);
    if (!hasLocalPreimage && !isTradeExpired(trade)) {
      showNotice({
        tone: 'warn',
        title: 'Original browser needed',
        detail: 'This swap can continue only where the browser-local preimage is stored. Locked swaps can be refunded after expiry.'
      });
      return;
    }

    setBusy('resume');
    showNotice({ tone: 'info', title: 'Resuming swap', detail: 'Refreshing the latest settlement state.' }, { persist: true });
    try {
      const resume = await api.resumeSettlement(trade.trade_id);
      hydrateHistoryTrade(trade, resume);
      setWalletHistoryOpen(false);
      showNotice({
        tone: 'success',
        title: isTradeExpired(trade) ? 'Refund path ready' : 'Swap restored',
        detail: isTradeExpired(trade) ? 'The expired lock can now be refunded.' : 'Continue from the latest persisted step.'
      });
    } catch (error) {
      showNotice({
        tone: 'error',
        title: 'Unable to resume swap',
        detail: error instanceof Error ? error.message : undefined
      });
    } finally {
      setBusy(null);
    }
  }

  async function handleHistoryRefund(trade: RuntimeTrade) {
    setBusy('refund');
    showNotice({ tone: 'info', title: 'Preparing refund', detail: 'Your wallet will reclaim the expired locked funds.' }, { persist: true });
    try {
      const prepared = await api.takerRefund(trade.trade_id, {});
      if (!prepared.taker_refund_transaction) {
        setRefund(prepared);
        await refreshWalletHistory();
        showNotice({ tone: 'success', title: 'Refund complete', detail: 'Expired funds were returned safely.' });
        return;
      }
      const signature = await signAndSubmit(prepared.taker_refund_transaction.transaction_base64);
      const completedRefund = await api.takerRefund(trade.trade_id, { signature });
      if (settlement?.trade_id === trade.trade_id) {
        setRefund(completedRefund);
        setPendingRefundSignature(null);
      }
      await refreshWalletHistory();
      showNotice({ tone: 'success', title: 'Refund complete', detail: 'Expired funds were returned safely.' });
    } catch (error) {
      showNotice({
        tone: 'error',
        title: walletFailure(error, 'Unable to refund locked funds.'),
        detail: error instanceof Error && !isWalletCancellation(error) ? error.message : undefined
      });
    } finally {
      setBusy(null);
    }
  }

  async function handlePrimaryAction() {
    if (redeem?.settlement_status || refund?.settlement_status) return startNewSwap();
    if (settlementPrelockExpired) return startOverSwap();
    if (terminalStartOver) return startOverSwap();
    if (!walletAddress) return connectWallet();
    if (quote?.status === 'accepted' && !quoteExpired && !settlement) return startSettlement();
    if (settlement?.trade_id && !lock) return takerLock();
    if (refundAvailable) return takerRefund();
    if (lock?.settlement_status && !redeem) return takerRedeem();
    if (!outputAsset) {
      showNotice({ tone: 'warn', title: 'Choose a receive asset.' });
      return undefined;
    }
    if (amountValidation?.ok === false) {
      showNotice({ tone: 'error', title: amountValidation.message });
      return undefined;
    }
    if (amountRangeValidation?.ok === false) {
      showNotice({ tone: 'error', title: amountRangeValidation.message });
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
        <div className="wallet-actions">
          {walletAddress && (
            <button
              type="button"
              className="history-button"
              onClick={() => setWalletHistoryOpen((open) => !open)}
              aria-label="Open wallet swap history"
              title="Wallet history"
            >
              <span className="history-glyph" aria-hidden="true" />
            </button>
          )}
          <button className="wallet-button" onClick={connectWallet}>
            {walletAddress ? shortValue(walletAddress) : 'Connect wallet'}
          </button>
        </div>
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
          {terminalFailure && !lock && !redeem && (
            <p className="field-error">{terminalFailure.detail ?? terminalFailure.title}</p>
          )}

          <button
            className={completedSwap || terminalStartOver ? 'primary-swap-button primary-swap-button-reset' : 'primary-swap-button'}
            disabled={primaryDisabled}
          >
            {primaryLabel}
          </button>

          {settlement?.trade_id && !lock && !redeem && !refund && !terminalStartOver && (
            <button
              type="button"
              className="secondary-swap-button"
              onClick={() => void startOverSwap()}
              disabled={Boolean(busy)}
            >
              Start over
            </button>
          )}

          {quoteAccepted && !terminalStartOver && (
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
          refund={refund}
        />
      </section>

      {walletHistoryOpen && walletAddress && (
        <WalletHistoryDrawer
          walletAddress={walletAddress}
          history={walletHistory}
          loading={walletHistoryLoading}
          error={walletHistoryError}
          selectedProofTrade={selectedHistoryProofTrade}
          currentTradeId={settlement?.trade_id ?? null}
          hasCurrentPreimage={Boolean(preimage)}
          busy={busy}
          onClose={() => {
            setWalletHistoryOpen(false);
            setSelectedHistoryProofTrade(null);
          }}
          onRefresh={() => void refreshWalletHistory()}
          onResume={(trade) => void handleHistoryResume(trade)}
          onRefund={(trade) => void handleHistoryRefund(trade)}
          onProof={setSelectedHistoryProofTrade}
          onCloseProof={() => setSelectedHistoryProofTrade(null)}
        />
      )}
    </main>
  );
}

function primaryActionLabel({
  hasWallet,
  hasDestination,
  amount,
  amountValidation,
  amountRangeOk,
  quote,
  quoteExpired,
  settlement,
  lock,
  redeem,
  refund,
  refundAvailable,
  settlementPrelockExpired,
  terminalStartOver,
  busy
}: {
  hasWallet: boolean;
  hasDestination: boolean;
  amount: string;
  amountValidation: { ok: true; raw: number } | { ok: false; message: string } | null;
  amountRangeOk: boolean;
  quote: RfqResponse | null;
  quoteExpired: boolean;
  settlement: WalletSettlementResponse | null;
  lock: TradeStepResponse | null;
  redeem: TradeStepResponse | null;
  refund: TradeStepResponse | null;
  refundAvailable: boolean;
  settlementPrelockExpired: boolean;
  terminalStartOver: boolean;
  busy: string | null;
}) {
  if (busy === 'quote') return 'Finding quote...';
  if (busy === 'settlement') return 'Preparing settlement...';
  if (busy === 'lock') return 'Waiting for wallet...';
  if (busy === 'redeem') return 'Completing swap...';
  if (busy === 'refund') return 'Refunding funds...';
  if (busy === 'abandon') return 'Starting over...';
  if (busy === 'resume') return 'Resuming swap...';
  if (redeem?.settlement_status || refund?.settlement_status) return 'Start new swap';
  if (settlementPrelockExpired) return 'Start over';
  if (terminalStartOver) return 'Start over';
  if (!hasWallet) return 'Connect wallet';
  if (refundAvailable) return 'Refund funds';
  if (lock?.settlement_status) return 'Complete swap';
  if (settlement?.trade_id) return 'Lock funds';
  if (!hasDestination) return 'Choose receive asset';
  if (!amount.trim()) return 'Enter amount';
  if (amountValidation?.ok === false) return 'Fix amount';
  if (!amountRangeOk) return 'Adjust amount';
  if (quote?.status === 'accepted' && quoteExpired) return 'Refreshing quote...';
  if (quote?.status === 'accepted') return 'Continue';
  return 'Quote updates automatically';
}

function primaryActionDisabled({
  hasWallet,
  hasDestination,
  amountValidation,
  amountRangeOk,
  quote,
  quoteExpired,
  settlement,
  lock,
  redeem,
  refund,
  refundAvailable,
  settlementPrelockExpired,
  terminalStartOver,
  busy
}: {
  hasWallet: boolean;
  hasDestination: boolean;
  amountValidation: { ok: true; raw: number } | { ok: false; message: string } | null;
  amountRangeOk: boolean;
  quote: RfqResponse | null;
  quoteExpired: boolean;
  settlement: WalletSettlementResponse | null;
  lock: TradeStepResponse | null;
  redeem: TradeStepResponse | null;
  refund: TradeStepResponse | null;
  refundAvailable: boolean;
  settlementPrelockExpired: boolean;
  terminalStartOver: boolean;
  busy: string | null;
}) {
  if (Boolean(busy)) return true;
  if (redeem?.settlement_status || refund?.settlement_status) return false;
  if (settlementPrelockExpired) return false;
  if (terminalStartOver) return false;
  if (!hasWallet) return false;
  if (refundAvailable) return false;
  if (lock?.settlement_status || settlement?.trade_id) return false;
  if (quote?.status === 'accepted' && !quoteExpired) return false;
  if (!hasDestination || amountValidation?.ok !== true || !amountRangeOk) return true;
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

function WalletHistoryDrawer({
  walletAddress,
  history,
  loading,
  error,
  selectedProofTrade,
  currentTradeId,
  hasCurrentPreimage,
  busy,
  onClose,
  onRefresh,
  onResume,
  onRefund,
  onProof,
  onCloseProof
}: {
  walletAddress: string;
  history: RuntimeTradesResponse | null;
  loading: boolean;
  error: string | null;
  selectedProofTrade: RuntimeTrade | null;
  currentTradeId: string | null;
  hasCurrentPreimage: boolean;
  busy: string | null;
  onClose: () => void;
  onRefresh: () => void;
  onResume: (trade: RuntimeTrade) => void;
  onRefund: (trade: RuntimeTrade) => void;
  onProof: (trade: RuntimeTrade) => void;
  onCloseProof: () => void;
}) {
  const trades = history?.trades ?? [];

  return (
    <aside className="wallet-history-drawer" aria-label="Wallet swap history">
      <div className="wallet-history-head">
        <div>
          <p className="eyebrow">Your swaps</p>
          <h2>{shortValue(walletAddress)}</h2>
        </div>
        <div className="wallet-history-actions">
          <button type="button" onClick={onRefresh} disabled={loading || Boolean(busy)}>
            Refresh
          </button>
          <button type="button" onClick={onClose}>
            Close
          </button>
        </div>
      </div>

      {loading && !history ? (
        <div className="wallet-history-empty">Loading wallet history...</div>
      ) : error && !history ? (
        <div className="wallet-history-empty">{error}</div>
      ) : trades.length === 0 ? (
        <div className="wallet-history-empty">No swaps for this wallet yet.</div>
      ) : (
        <div className="wallet-history-list">
          {trades.map((trade) => (
            <WalletHistoryRow
              key={trade.trade_id}
              trade={trade}
              currentTradeId={currentTradeId}
              hasCurrentPreimage={hasCurrentPreimage}
              busy={busy}
              onResume={onResume}
              onRefund={onRefund}
              onProof={onProof}
            />
          ))}
        </div>
      )}

      {selectedProofTrade && (
        <WalletHistoryProof trade={selectedProofTrade} onClose={onCloseProof} />
      )}
    </aside>
  );
}

function WalletHistoryRow({
  trade,
  currentTradeId,
  hasCurrentPreimage,
  busy,
  onResume,
  onRefund,
  onProof
}: {
  trade: RuntimeTrade;
  currentTradeId: string | null;
  hasCurrentPreimage: boolean;
  busy: string | null;
  onResume: (trade: RuntimeTrade) => void;
  onRefund: (trade: RuntimeTrade) => void;
  onProof: (trade: RuntimeTrade) => void;
}) {
  const terminal = isTerminalTrade(trade);
  const expired = isTradeExpired(trade);
  const locked = Boolean(tradeSignatureByKind(trade, 'taker_lock')) || trade.settlement_status === 'initiated';
  const canResumeHere = currentTradeId === trade.trade_id && hasCurrentPreimage;
  const txCount = trade.tx_signatures?.length ?? 0;

  let action = (
    <button type="button" onClick={() => onResume(trade)} disabled={Boolean(busy)}>
      Resume
    </button>
  );
  let note: string | null = null;

  if (terminal) {
    action = (
      <button type="button" onClick={() => onProof(trade)}>
        Network proof
      </button>
    );
  } else if (locked && expired) {
    action = (
      <button type="button" onClick={() => onRefund(trade)} disabled={Boolean(busy)}>
        Refund
      </button>
    );
  } else if (!canResumeHere) {
    action = (
      <button type="button" onClick={() => onResume(trade)} disabled>
        Resume
      </button>
    );
    note = locked
      ? 'Complete in original browser. Refund after expiry.'
      : 'Resume from the browser that created this swap.';
  }

  return (
    <article className={terminal ? 'wallet-history-row' : 'wallet-history-row wallet-history-row-active'}>
      <div className="wallet-history-row-main">
        <strong>{historyAmount(trade.input)} {'->'} {historyAmount(trade.output)}</strong>
        <span>{tradeStatusLabel(trade.settlement_status)}</span>
      </div>
      <div className="wallet-history-row-meta">
        <span>{shortValue(trade.trade_id)}</span>
        <span>{txCount ? `${txCount} txs` : 'No tx yet'}</span>
        <span>{formatHistoryTime(trade.created_at)}</span>
      </div>
      {note && <p>{note}</p>}
      <div className="wallet-history-row-action">{action}</div>
    </article>
  );
}

function WalletHistoryProof({ trade, onClose }: { trade: RuntimeTrade; onClose: () => void }) {
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const copiedTimer = useRef<number | null>(null);
  const signatures = tradeProofSignatures(trade);

  useEffect(() => {
    return () => {
      if (copiedTimer.current !== null) {
        window.clearTimeout(copiedTimer.current);
      }
    };
  }, []);

  async function copySignature(proof: TradeSignature, index: number) {
    const copiedKey = `${proof.kind}-${index}`;
    await copyToClipboard(proof.signature);
    setCopiedId(copiedKey);
    if (copiedTimer.current !== null) {
      window.clearTimeout(copiedTimer.current);
    }
    copiedTimer.current = window.setTimeout(() => setCopiedId(null), 1_400);
  }

  return (
    <section className="wallet-history-proof" aria-label="Wallet trade network proof">
      <div className="wallet-history-proof-head">
        <div>
          <p className="eyebrow">Network proof</p>
          <h3>{historyAmount(trade.input)} {'->'} {historyAmount(trade.output)}</h3>
        </div>
        <button type="button" onClick={onClose}>Close</button>
      </div>
      <div className="wallet-history-proof-grid">
        <div>
          <span>Status</span>
          <strong>{tradeStatusLabel(trade.settlement_status)}</strong>
        </div>
        <div>
          <span>Trade</span>
          <strong title={trade.trade_id}>{shortValue(trade.trade_id)}</strong>
        </div>
        <div>
          <span>Quote</span>
          <strong title={trade.quote_id}>{shortValue(trade.quote_id)}</strong>
        </div>
      </div>
      {signatures.length === 0 ? (
        <p className="wallet-history-empty">No Solana signatures have landed for this swap yet.</p>
      ) : (
        <div className="wallet-history-proof-list">
          {signatures.map((proof, index) => {
            const copied = copiedId === `${proof.kind}-${index}`;
            return (
              <div className="wallet-history-proof-row" key={`${proof.kind}-${proof.signature}-${index}`}>
                <div>
                  <span>{tradeStatusLabel(proof.kind)}</span>
                  <a href={`https://solscan.io/tx/${proof.signature}`} target="_blank" rel="noreferrer">
                    {shortValue(proof.signature)}
                  </a>
                </div>
                <button
                  type="button"
                  className={copied ? 'copy-proof-button copied' : 'copy-proof-button'}
                  onClick={() => copySignature(proof, index)}
                  aria-label={`Copy ${proof.kind} transaction signature ${shortValue(proof.signature)}`}
                  title={copied ? 'Copied' : 'Copy signature'}
                >
                  <span className="copy-glyph" aria-hidden="true" />
                  <span className="copy-proof-label">{copied ? 'Copied' : 'Copy'}</span>
                </button>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );
}

function formatHistoryTime(value?: string) {
  if (!value) return 'Pending';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return 'Pending';
  return new Intl.DateTimeFormat(undefined, {
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit'
  }).format(date);
}

function BehindScenesPanel({
  quote,
  quoteExpired,
  settlement,
  lock,
  redeem,
  refund
}: {
  quote: RfqResponse | null;
  quoteExpired: boolean;
  settlement: WalletSettlementResponse | null;
  lock: TradeStepResponse | null;
  redeem: TradeStepResponse | null;
  refund: TradeStepResponse | null;
}) {
  const [copiedProofId, setCopiedProofId] = useState<string | null>(null);
  const copiedTimer = useRef<number | null>(null);
  const makerProof = lock?.maker_lock_signature ?? redeem?.maker_redeem_signature ?? refund?.maker_refund_signature;
  const networkProofs = Array.from(new Set([
    ...(lock?.tx_signatures ?? []),
    ...(redeem?.tx_signatures ?? []),
    ...(refund?.tx_signatures ?? []),
    ...(makerProof ? [makerProof] : [])
  ].filter(Boolean)));
  const terminalDone = Boolean(redeem?.settlement_status || refund?.settlement_status);
  const terminalDetail = refund?.settlement_status
    ? 'Expired funds were returned safely.'
    : 'Funds exchanged successfully.';

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
      <ProofRow done={terminalDone} title="Swap complete" detail={terminalDetail} />

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
