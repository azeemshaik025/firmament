import { FormEvent, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useConnection, useWallet } from '@solana/wallet-adapter-react';
import { useWalletModal } from '@solana/wallet-adapter-react-ui';
import { PublicKey } from '@solana/web3.js';
import { api, Asset, RfqAcceptedResponse, TradeStepResponse, WalletSettlementResumeResponse } from '../api';
import { bytesToHex, parseAmountRaw, shortValue } from '../lib/format';
import { isWalletCancellation, signAndSubmit, walletFailure, walletFundingIssue } from '../lib/wallet';

const payAsset = 'USDC';
const receiveAsset = 'EURC';
const quoteExpirySeconds = 30;
const quoteDebounceMs = 500;
const storageKey = 'firmament.pay.v1';

type Phase =
  | { kind: 'form' }
  | { kind: 'quoting' }
  | { kind: 'quoted'; quote: RfqAcceptedResponse; secondsLeft: number }
  | { kind: 'rejected'; message: string }
  | { kind: 'locking'; tradeId: string }
  | { kind: 'maker_locking'; tradeId: string }
  | { kind: 'redeeming'; tradeId: string }
  | { kind: 'paid'; tradeId: string; signatures: string[] }
  | { kind: 'expired'; tradeId: string }
  | { kind: 'refunding'; tradeId: string }
  | { kind: 'refunded'; tradeId: string; signatures: string[] }
  | { kind: 'failed'; message: string };

type StoredPayment = {
  version: 1;
  wallet: string;
  recipient: string;
  eurAmount: string;
  tradeId: string;
  preimage: string;
  lockSignature: string | null;
};

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function readStored(): StoredPayment | null {
  try {
    const raw = window.localStorage.getItem(storageKey);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (!isRecord(parsed)) return null;
    const { version, wallet, recipient, eurAmount, tradeId, preimage, lockSignature } = parsed;
    if (version !== 1 || typeof wallet !== 'string' || typeof tradeId !== 'string' || typeof preimage !== 'string') {
      return null;
    }
    return {
      version: 1,
      wallet,
      recipient: typeof recipient === 'string' ? recipient : '',
      eurAmount: typeof eurAmount === 'string' ? eurAmount : '',
      tradeId,
      preimage,
      lockSignature: typeof lockSignature === 'string' ? lockSignature : null
    };
  } catch {
    return null;
  }
}

function writeStored(value: StoredPayment | null) {
  try {
    if (value) window.localStorage.setItem(storageKey, JSON.stringify(value));
    else window.localStorage.removeItem(storageKey);
  } catch {
    // Private mode: the payment still completes, it just cannot resume after a refresh.
  }
}

function isValidAddress(value: string) {
  try {
    new PublicKey(value.trim());
    return true;
  } catch {
    return false;
  }
}

function solscan(signature: string) {
  return `https://solscan.io/tx/${signature}`;
}

function signaturesOf(step: TradeStepResponse) {
  return step.tx_signatures ?? [];
}

function hasKind(resume: WalletSettlementResumeResponse, kind: string) {
  return (resume.tx_signature_kinds ?? []).some((entry) => entry.kind === kind);
}

const settlingPhases = new Set<Phase['kind']>(['locking', 'maker_locking', 'redeeming', 'paid', 'expired', 'refunding', 'refunded', 'failed']);

const steps: Array<{ label: string; doneAt: Phase['kind'][]; nowAt: Phase['kind'][] }> = [
  { label: 'Quote locked at a firm rate', doneAt: ['quoted', 'locking', 'maker_locking', 'redeeming', 'paid'], nowAt: ['quoting'] },
  { label: 'Your USDC locked in escrow', doneAt: ['maker_locking', 'redeeming', 'paid'], nowAt: ['locking'] },
  { label: 'Maker locked the EURC for the recipient', doneAt: ['redeeming', 'paid'], nowAt: ['maker_locking'] },
  { label: 'Delivered', doneAt: ['paid'], nowAt: ['redeeming'] }
];

export function PayPage() {
  const { connection } = useConnection();
  const { publicKey, signTransaction } = useWallet();
  const { setVisible: setWalletModalVisible } = useWalletModal();
  const [assets, setAssets] = useState<Asset[]>([]);
  const [eurAmount, setEurAmount] = useState('');
  const [recipient, setRecipient] = useState('');
  const [phase, setPhase] = useState<Phase>({ kind: 'form' });
  const [preimage, setPreimage] = useState<string | null>(null);
  const [quoteNonce, setQuoteNonce] = useState(0);
  const quoteSeq = useRef(0);
  const resumedRef = useRef(false);
  const walletAddress = publicKey?.toBase58() ?? '';

  const eurc = useMemo(() => assets.find((asset) => asset.id === receiveAsset), [assets]);
  const usdc = useMemo(() => assets.find((asset) => asset.id === payAsset), [assets]);
  const amountCheck = useMemo(() => (eurAmount.trim() ? parseAmountRaw(eurAmount, eurc?.decimals ?? 6) : null), [eurAmount, eurc]);
  const amountOk = amountCheck?.ok === true;
  const recipientOk = recipient.trim() === '' || isValidAddress(recipient);
  const inFlight = settlingPhases.has(phase.kind);

  useEffect(() => {
    api.assets().then(setAssets).catch(() => setAssets([]));
  }, []);

  // Quote whenever the amount changes and nothing is settling.
  useEffect(() => {
    if (inFlight) return undefined;
    if (!walletAddress || !amountOk) {
      setPhase((current) => (current.kind === 'quoted' || current.kind === 'quoting' || current.kind === 'rejected' ? { kind: 'form' } : current));
      return undefined;
    }
    const seq = quoteSeq.current + 1;
    quoteSeq.current = seq;
    setPhase({ kind: 'quoting' });
    const timer = window.setTimeout(async () => {
      try {
        const response = await api.requestRfq({
          input_asset: payAsset,
          output_asset: receiveAsset,
          output_amount: eurAmount.trim(),
          taker_wallet: walletAddress,
          expiry_seconds: quoteExpirySeconds
        });
        if (quoteSeq.current !== seq) return;
        if (response.status === 'accepted') {
          setPhase({ kind: 'quoted', quote: response, secondsLeft: quoteExpirySeconds });
        } else {
          setPhase({ kind: 'rejected', message: response.message || response.reason });
        }
      } catch (error) {
        if (quoteSeq.current !== seq) return;
        setPhase({ kind: 'rejected', message: error instanceof Error ? error.message : 'Quote unavailable.' });
      }
    }, quoteDebounceMs);
    return () => window.clearTimeout(timer);
  }, [eurAmount, walletAddress, amountOk, inFlight, quoteNonce]);

  // Countdown; when the quote expires, ask for a fresh one.
  const quotedExpiresAt = phase.kind === 'quoted' ? phase.quote.expires_at : null;
  useEffect(() => {
    if (!quotedExpiresAt) return undefined;
    const expiresAt = new Date(quotedExpiresAt).getTime();
    const tick = window.setInterval(() => {
      const left = Math.max(0, Math.ceil((expiresAt - Date.now()) / 1000));
      if (left === 0) {
        quoteSeq.current += 1;
        setPhase({ kind: 'form' });
        setQuoteNonce((nonce) => nonce + 1);
      } else {
        setPhase((current) => (current.kind === 'quoted' ? { ...current, secondsLeft: left } : current));
      }
    }, 1000);
    return () => window.clearInterval(tick);
  }, [quotedExpiresAt]);

  const redeem = useCallback(async (tradeId: string, secret: string) => {
    if (!signTransaction) throw new Error('Connected wallet cannot sign transactions.');
    setPhase({ kind: 'redeeming', tradeId });
    const prepared = await api.takerRedeem(tradeId, { preimage: secret });
    if (!prepared.taker_redeem_transaction) {
      writeStored(null);
      setPhase({ kind: 'paid', tradeId, signatures: signaturesOf(prepared) });
      return;
    }
    const signature = await signAndSubmit(connection, signTransaction, prepared.taker_redeem_transaction.transaction_base64);
    const done = await api.takerRedeem(tradeId, { preimage: secret, signature });
    writeStored(null);
    setPhase({ kind: 'paid', tradeId, signatures: signaturesOf(done) });
  }, [connection, signTransaction]);

  // Resume an interrupted payment once the paying wallet reconnects.
  useEffect(() => {
    if (resumedRef.current || !walletAddress) return;
    const stored = readStored();
    if (!stored) return;
    resumedRef.current = true;
    if (stored.wallet !== walletAddress) {
      writeStored(null);
      return;
    }
    setRecipient(stored.recipient);
    setEurAmount(stored.eurAmount);
    setPreimage(stored.preimage);
    void (async () => {
      try {
        const resume = await api.resumeSettlement(stored.tradeId);
        if (hasKind(resume, 'taker_redeem') || resume.settlement_status === 'redeemed') {
          writeStored(null);
          setPhase({ kind: 'paid', tradeId: stored.tradeId, signatures: resume.tx_signatures ?? [] });
          return;
        }
        if (hasKind(resume, 'taker_refund') || resume.settlement_status === 'refunded') {
          writeStored(null);
          setPhase({ kind: 'refunded', tradeId: stored.tradeId, signatures: resume.tx_signatures ?? [] });
          return;
        }
        if (resume.taker_refund_transaction) {
          setPhase({ kind: 'expired', tradeId: stored.tradeId });
          return;
        }
        if (hasKind(resume, 'maker_lock')) {
          await redeem(stored.tradeId, stored.preimage);
          return;
        }
        if (stored.lockSignature) {
          setPhase({ kind: 'maker_locking', tradeId: stored.tradeId });
          await api.takerLock(stored.tradeId, { signature: stored.lockSignature });
          await redeem(stored.tradeId, stored.preimage);
          return;
        }
        writeStored(null);
        setPhase({ kind: 'form' });
      } catch (error) {
        setPhase({ kind: 'failed', message: walletFailure(error, 'Could not resume the payment.') });
      }
    })();
  }, [walletAddress, redeem]);

  async function pay() {
    if (phase.kind !== 'quoted' || !publicKey || !signTransaction || !usdc) return;
    const quote = phase.quote;
    if (recipient.trim() && !isValidAddress(recipient)) return;
    quoteSeq.current += 1;
    try {
      const funding = await walletFundingIssue(connection, publicKey, usdc, quote.input.amount_raw);
      if (funding) {
        setPhase({ kind: 'failed', message: funding.detail });
        return;
      }
      const secretBytes = crypto.getRandomValues(new Uint8Array(32));
      const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', secretBytes));
      const secret = bytesToHex(secretBytes);
      setPreimage(secret);
      const settlement = await api.walletSettlement(quote.quote_id, {
        taker_wallet: publicKey.toBase58(),
        secret_hash: bytesToHex(hash),
        ...(recipient.trim() ? { recipient: recipient.trim() } : {})
      });
      const stored: StoredPayment = {
        version: 1,
        wallet: publicKey.toBase58(),
        recipient: recipient.trim(),
        eurAmount,
        tradeId: settlement.trade_id,
        preimage: secret,
        lockSignature: null
      };
      writeStored(stored);
      setPhase({ kind: 'locking', tradeId: settlement.trade_id });
      const lockSignature = await signAndSubmit(connection, signTransaction, settlement.taker_lock_transaction.transaction_base64);
      writeStored({ ...stored, lockSignature });
      setPhase({ kind: 'maker_locking', tradeId: settlement.trade_id });
      await api.takerLock(settlement.trade_id, { signature: lockSignature });
      await redeem(settlement.trade_id, secret);
    } catch (error) {
      if (isWalletCancellation(error)) {
        writeStored(null);
        setPhase({ kind: 'form' });
        setQuoteNonce((nonce) => nonce + 1);
        return;
      }
      setPhase({ kind: 'failed', message: walletFailure(error, error instanceof Error ? error.message : 'Payment failed.') });
    }
  }

  async function refund() {
    if (phase.kind !== 'expired' || !signTransaction) return;
    const tradeId = phase.tradeId;
    setPhase({ kind: 'refunding', tradeId });
    try {
      const prepared = await api.takerRefund(tradeId, {});
      if (!prepared.taker_refund_transaction) {
        writeStored(null);
        setPhase({ kind: 'refunded', tradeId, signatures: signaturesOf(prepared) });
        return;
      }
      const signature = await signAndSubmit(connection, signTransaction, prepared.taker_refund_transaction.transaction_base64);
      const done = await api.takerRefund(tradeId, { signature });
      writeStored(null);
      setPhase({ kind: 'refunded', tradeId, signatures: signaturesOf(done) });
    } catch (error) {
      setPhase({ kind: 'failed', message: walletFailure(error, 'Refund failed.') });
    }
  }

  function startOver() {
    writeStored(null);
    setPreimage(null);
    setEurAmount('');
    setRecipient('');
    setPhase({ kind: 'form' });
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (!walletAddress) {
      setWalletModalVisible(true);
      return;
    }
    void pay();
  }

  const cost = phase.kind === 'quoted' ? phase.quote.input.amount : null;
  const rate = phase.kind === 'quoted'
    ? (Number(phase.quote.output.amount) / Number(phase.quote.input.amount)).toFixed(4)
    : null;
  const lockFraction = phase.kind === 'quoted' ? phase.secondsLeft / quoteExpirySeconds : 1;
  const terminal = phase.kind === 'paid' || phase.kind === 'refunded' || phase.kind === 'failed';
  const primaryLabel = !walletAddress
    ? 'Connect wallet'
    : phase.kind === 'locking'
      ? 'Confirm in wallet…'
      : phase.kind === 'maker_locking'
        ? 'Maker locking EURC…'
        : phase.kind === 'redeeming'
          ? 'Delivering…'
          : cost
            ? `Pay ${cost} USDC`
            : 'Pay';

  return (
    <main className="pay-page">
      <header className="page-head">
        <div>
          <p className="page-kicker">Firm FX</p>
          <h1>Pay in euros.</h1>
          <p className="lede">The rate you see is the rate they get. Held for {quoteExpirySeconds} seconds, settled atomically.</p>
        </div>
        <div className="wallet-actions">
          <button className="wallet-button" onClick={() => setWalletModalVisible(true)} type="button">
            {walletAddress ? shortValue(walletAddress) : 'Connect wallet'}
          </button>
        </div>
      </header>

      <section className="pay-desk">
        <form className="pay-form" onSubmit={submit}>
          <h2>Send EURC</h2>

          <div className="pay-amount">
            <span>Recipient gets</span>
            <div className="pay-amount-row">
              <input
                aria-label="Amount in EURC"
                inputMode="decimal"
                placeholder="89.30"
                value={eurAmount}
                disabled={inFlight}
                onChange={(event) => setEurAmount(event.target.value)}
              />
              <b>EURC</b>
            </div>
          </div>
          {amountCheck && !amountCheck.ok && <p className="field-error">{amountCheck.message}</p>}

          <label className="pay-field">
            <span>Recipient address</span>
            <input
              placeholder="Leave empty to receive it yourself"
              value={recipient}
              disabled={inFlight}
              onChange={(event) => setRecipient(event.target.value)}
            />
          </label>
          {!recipientOk && <p className="field-error">That is not a Solana address.</p>}

          <div className="pay-quote">
            <span>You pay</span>
            <strong className="is-cost">{cost ? `${cost} USDC` : phase.kind === 'quoting' ? 'Quoting…' : '—'}</strong>
            <span>Rate</span>
            <strong>{rate ? `1 USDC = ${rate} EURC` : '—'}</strong>
            {phase.kind === 'quoted' && (
              <div className="pay-lock" role="timer" style={{ '--lock': lockFraction }} aria-label={`Rate held for ${phase.secondsLeft} more seconds`}>
                <span>Held</span>
                <div className="pay-lock-track">
                  <div className="pay-lock-fill" />
                  <div className="pay-lock-point" />
                </div>
                <span className="pay-lock-time">{phase.secondsLeft}s</span>
              </div>
            )}
          </div>

          {phase.kind === 'rejected' && <p className="field-error">{phase.message}</p>}
          {phase.kind === 'failed' && <p className="field-error">{phase.message}</p>}

          {terminal ? (
            <button type="button" className="pay-submit is-reset" onClick={startOver}>New payment</button>
          ) : phase.kind === 'expired' ? (
            <button type="button" className="pay-submit" onClick={() => void refund()}>Lock expired, refund my USDC</button>
          ) : (
            <button
              className="pay-submit"
              disabled={inFlight || (walletAddress !== '' && (phase.kind !== 'quoted' || !recipientOk))}
            >
              {primaryLabel}
            </button>
          )}
        </form>

        <aside className="pay-rail" aria-label="Settlement progress">
          <h2>Settlement</h2>
          <ol className="pay-steps">
            {steps.map((step) => (
              <li
                key={step.label}
                className={step.doneAt.includes(phase.kind) ? 'done' : step.nowAt.includes(phase.kind) ? 'now' : ''}
              >
                {step.label === 'Delivered' && eurAmount ? `Recipient received exactly ${eurAmount} EURC` : step.label}
              </li>
            ))}
          </ol>

          {phase.kind === 'paid' && (
            <div className="pay-delivered">
              <strong>{eurAmount} EURC</strong>
              <span>delivered{recipient.trim() ? ` to ${shortValue(recipient.trim())}` : ''}</span>
            </div>
          )}

          {(phase.kind === 'paid' || phase.kind === 'refunded') && (
            <ul className="pay-receipt" aria-label="Transactions">
              {phase.signatures.map((signature) => (
                <li key={signature}><a href={solscan(signature)} target="_blank" rel="noreferrer">{shortValue(signature)}</a></li>
              ))}
            </ul>
          )}

          {preimage && inFlight && phase.kind !== 'paid' && (
            <p className="locked-note">Keep this tab open until delivery completes; it holds the secret that releases the EURC.</p>
          )}
          <p className="locked-note">
            {eurc ? `This maker quotes up to ${eurc.max_trade_amount ?? '?'} EURC per payment.` : 'EURC is not enabled on this maker.'}
          </p>
        </aside>
      </section>
    </main>
  );
}
