import { FormEvent, KeyboardEvent as ReactKeyboardEvent, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useConnection, useWallet } from '@solana/wallet-adapter-react';
import { useWalletModal } from '@solana/wallet-adapter-react-ui';
import { Transaction } from '@solana/web3.js';
import { api, Asset, RfqResponse, TradeStepResponse, WalletSettlementResponse } from '../api';

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
  { id: 'USDC', symbol: 'USDC', name: 'USD Coin', mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v', decimals: 6 },
  { id: 'SOL', symbol: 'SOL', name: 'Solana', mint: 'So11111111111111111111111111111111111111112', decimals: 9 },
  { id: 'cbBTC', symbol: 'cbBTC', name: 'Coinbase Wrapped BTC', mint: 'cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij', decimals: 8 }
];

const defaultSourceSymbol = 'SOL';
const quoteExpirySeconds = 45;
const quoteRetryMs = 5 * 60 * 1_000;
const quoteInputDebounceMs = 600;
const assetLogoUrls: Record<string, string> = {
  sol: 'https://garden.imgix.net/chain_images/solana.png',
  usdc: 'https://garden.imgix.net/token-images/usdc.svg',
  cbbtc: 'https://garden.imgix.net/token-images/cbBTC.svg'
};

function assetBySymbol(assets: Asset[], symbol: string) {
  return assets.find((asset) => asset.symbol === symbol || asset.id === symbol);
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
  const [assets, setAssets] = useState<Asset[]>(fallbackAssets);
  const [inputMint, setInputMint] = useState(assetBySymbol(fallbackAssets, defaultSourceSymbol)?.mint ?? fallbackAssets[0].mint);
  const [outputMint, setOutputMint] = useState('');
  const [amount, setAmount] = useState('');
  const [quote, setQuote] = useState<RfqResponse | null>(null);
  const [quoteExpired, setQuoteExpired] = useState(false);
  const [settlement, setSettlement] = useState<WalletSettlementResponse | null>(null);
  const [lock, setLock] = useState<TradeStepResponse | null>(null);
  const [redeem, setRedeem] = useState<TradeStepResponse | null>(null);
  const [preimage, setPreimage] = useState<string | null>(null);
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
  const quoteAccepted = quote?.status === 'accepted';
  const receiveAmount = quoteAccepted && outputAsset ? formatRawAmount(quote.quoted_output_amount_raw, outputAsset.decimals) : null;
  const flowLocked = Boolean(settlement || lock || redeem);
  const primaryLabel = primaryActionLabel({
    hasWallet: Boolean(walletAddress),
    hasDestination: Boolean(outputAsset),
    amount,
    amountValidation,
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
          const nextSource = assetBySymbol(nextAssets, defaultSourceSymbol) ?? assetBySymbol(fallbackAssets, defaultSourceSymbol) ?? fallbackAssets[0];
          setAssets(nextAssets);
          setInputMint((currentMint) => (
            nextAssets.some((asset) => asset.mint === currentMint)
              ? currentMint
              : nextSource.mint
          ));
          setOutputMint((currentMint) => (
            nextAssets.some((asset) => asset.mint === currentMint && asset.mint !== nextSource.mint)
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
    setQuote(null);
    setQuoteExpired(false);
    setSettlement(null);
    setLock(null);
    setRedeem(null);
    setPreimage(null);
    setBusy((currentBusy) => (currentBusy === 'quote' ? null : currentBusy));
  }

  useEffect(() => {
    resetFlow();

    if (!walletAddress || !outputAsset || amountValidation?.ok !== true) {
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
  }, [walletAddress, outputMint, amount, sourceAsset.mint, sourceAsset.decimals]);

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
        input_mint: sourceAsset.mint,
        output_mint: outputAsset.mint,
        input_amount_raw: parsed.raw,
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
        scheduleQuoteRefresh(quoteRetryMs, 'retry');
        showNotice({
          tone: 'warn',
          title: 'No liquidity sources found',
          detail: 'Try a smaller amount or a different pair. It will retry automatically.'
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
    if (!settlement?.trade_id) return;
    setBusy('lock');
    showNotice({ tone: 'info', title: 'Waiting for wallet signature', detail: 'Your wallet will lock funds for the swap.' }, { persist: true });
    try {
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
          {quote?.status === 'rejected' && <p className="field-error">No liquidity sources found</p>}

          <button className="primary-swap-button" disabled={primaryDisabled}>
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
  if (!hasWallet) return 'Connect wallet';
  if (redeem?.settlement_status) return 'Swap complete';
  if (lock?.settlement_status) return 'Complete swap';
  if (settlement?.trade_id) return 'Lock funds';
  if (!hasDestination) return 'Choose receive asset';
  if (!amount.trim()) return 'Enter amount';
  if (amountValidation?.ok === false) return 'Fix amount';
  if (quote?.status === 'accepted' && quoteExpired) return 'Refreshing quote...';
  if (quote?.status === 'accepted') return 'Continue';
  if (quote?.status === 'rejected') return 'Retrying quote...';
  return 'Quote updates automatically';
}

function primaryActionDisabled({
  hasWallet,
  hasDestination,
  amountValidation,
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
  quote: RfqResponse | null;
  quoteExpired: boolean;
  settlement: WalletSettlementResponse | null;
  lock: TradeStepResponse | null;
  redeem: TradeStepResponse | null;
  busy: string | null;
}) {
  if (!hasWallet) return false;
  if (Boolean(busy)) return true;
  if (redeem?.settlement_status) return true;
  if (lock?.settlement_status || settlement?.trade_id) return false;
  if (quote?.status === 'accepted' && !quoteExpired) return false;
  if (!hasDestination || amountValidation?.ok !== true) return true;
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
  const makerProof = lock?.maker_lock_signature ?? redeem?.maker_redeem_signature;
  const networkProofs = [
    ...(lock?.tx_signatures ?? []),
    ...(redeem?.tx_signatures ?? []),
    ...(makerProof ? [makerProof] : [])
  ].filter(Boolean);

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
          networkProofs.slice(0, 3).map((signature) => (
            <a key={signature} href={`https://solscan.io/tx/${signature}`} target="_blank" rel="noreferrer">
              {shortValue(signature)}
            </a>
          ))
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
