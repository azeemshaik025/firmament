import { useEffect, useMemo, useRef, useState } from 'react';
import {
  api,
  Asset,
  LedgerAccountType,
  RuntimeEventsResponse,
  RuntimeLedgerResponse,
  RuntimeStateResponse,
  RuntimeTimestamp,
  RuntimeTrade,
  RuntimeTradesResponse
} from '../api';

type LoadState<T> = {
  data: T | null;
  error: string | null;
  loading: boolean;
  updatedAt: number | null;
};

type HealthPayload = { status?: string; ok?: boolean; [key: string]: unknown };

const pollMs = 5_000;
const visibleAccounts: LedgerAccountType[] = ['working_custody', 'gateway', 'htlc_escrow'];
const assetOrder = ['SOL', 'USDC', 'cbBTC'];
const fallbackAssets: Asset[] = [
  { id: 'SOL', symbol: 'SOL', name: 'Solana', mint: 'So11111111111111111111111111111111111111112', decimals: 9 },
  { id: 'USDC', symbol: 'USDC', name: 'USD Coin', mint: 'EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v', decimals: 6 },
  { id: 'cbBTC', symbol: 'cbBTC', name: 'Coinbase Wrapped BTC', mint: 'cbbtcf3aa214zXHbiAZQwf4122FBYbraNdFqgw4iMij', decimals: 8 }
];

const emptyState = <T,>(): LoadState<T> => ({
  data: null,
  error: null,
  loading: true,
  updatedAt: null
});

export function RuntimePage() {
  const [assets, setAssets] = useState<Asset[]>(fallbackAssets);
  const [health, setHealth] = useState<LoadState<HealthPayload>>(emptyState);
  const [runtimeState, setRuntimeState] = useState<LoadState<RuntimeStateResponse>>(emptyState);
  const [events, setEvents] = useState<LoadState<RuntimeEventsResponse>>(emptyState);
  const [ledger, setLedger] = useState<LoadState<RuntimeLedgerResponse>>(emptyState);
  const [trades, setTrades] = useState<LoadState<RuntimeTradesResponse>>(emptyState);
  const [showAllTrades, setShowAllTrades] = useState(false);
  const [selectedProofTradeId, setSelectedProofTradeId] = useState<string | null>(null);

  useEffect(() => {
    let active = true;

    api.assets()
      .then((nextAssets) => {
        if (active && Array.isArray(nextAssets) && nextAssets.length > 0) {
          setAssets(nextAssets);
        }
      })
      .catch(() => {
        if (active) setAssets(fallbackAssets);
      });

    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    let active = true;
    let timer: number | null = null;

    async function refresh() {
      const [nextHealth, nextRuntimeState, nextEvents, nextLedger, nextTrades] = await Promise.all([
        resolve(api.health()),
        resolve(api.runtimeState()),
        resolve(api.runtimeEvents(30)),
        resolve(api.runtimeLedger()),
        resolve(api.runtimeTrades(10))
      ]);

      if (!active) return;

      setHealth((current) => mergeLoadState(current, nextHealth));
      setRuntimeState((current) => mergeLoadState(current, nextRuntimeState));
      setEvents((current) => mergeLoadState(current, nextEvents));
      setLedger((current) => mergeLoadState(current, nextLedger));
      setTrades((current) => mergeLoadState(current, nextTrades));

      timer = window.setTimeout(refresh, pollMs);
    }

    void refresh();

    return () => {
      active = false;
      if (timer !== null) window.clearTimeout(timer);
    };
  }, []);

  const sortedAssets = useMemo(() => sortAssets(assets), [assets]);
  const status = runtimeStatus(health, runtimeState);
  const lastEventAt = latestEventTimestamp(events.data);
  const recentTrades = trades.data?.trades ?? [];
  const visibleTrades = showAllTrades ? recentTrades : recentTrades.slice(0, 4);
  const canShowMore = recentTrades.length > visibleTrades.length;
  const selectedProofTrade = useMemo(
    () => recentTrades.find((trade) => trade.trade_id === selectedProofTradeId) ?? null,
    [recentTrades, selectedProofTradeId]
  );
  const ledgerLoading = ledger.loading && !ledger.data;
  const tradesLoading = trades.loading && !trades.data;

  return (
    <main className="runtime-page">
      <section className="runtime-hero">
        <div>
          <p className="eyebrow">Public Runtime</p>
          <h1>Live Runtime</h1>
          <p className="lede">Ledger-backed proof for Firmament quotes, custody, Gateway balances, and user-facing trades.</p>
        </div>
        <RuntimeStatusBadge status={status} />
      </section>

      <section className="runtime-topline" aria-label="Runtime status and trade counters">
        <StatusPanel
          status={status}
          runId={runtimeState.data?.state.run_id}
          startedAt={runtimeState.data?.state.started_at}
          lastEventAt={lastEventAt}
          updatedAt={latestUpdate([health, runtimeState, events])}
          loading={!runtimeState.data && (runtimeState.loading || events.loading)}
        />
        <CounterTile
          label="Total trades"
          value={trades.data ? String(trades.data.total_count) : 'Unavailable'}
          loading={tradesLoading}
          unavailable={Boolean(trades.error && !trades.data)}
        />
        <CounterTile
          label="Successful trades"
          value={trades.data ? String(trades.data.successful_count) : 'Unavailable'}
          loading={tradesLoading}
          unavailable={Boolean(trades.error && !trades.data)}
        />
      </section>

      <section className="runtime-board">
        <div className="runtime-board-heading">
          <div>
            <p className="eyebrow">Custody Ledger</p>
            <h2>Ledger Balances</h2>
          </div>
          {ledger.data && <LedgerHealth healthy={ledger.data.healthy} entryCount={ledger.data.entry_count} />}
        </div>
        {ledgerLoading ? (
          <LedgerSkeleton assets={sortedAssets} />
        ) : ledger.error && !ledger.data ? (
          <UnavailableState title="Ledger unavailable" detail="Balances will appear once ledger reads are online." />
        ) : (
          <LedgerMatrix assets={sortedAssets} ledger={ledger.data} />
        )}
      </section>

      <section className="runtime-board">
        <div className="runtime-board-heading">
          <div>
            <p className="eyebrow">Trade Flow</p>
            <h2>Recent Trades</h2>
          </div>
        </div>
        {tradesLoading ? (
          <TradeSkeletonRail />
        ) : trades.error && !trades.data ? (
          <UnavailableState title="Trades unavailable" detail="Trade history will appear once runtime trade reads are online." />
        ) : visibleTrades.length > 0 ? (
          <>
            <div className="trade-rail" aria-label="Recent user-facing trades">
              {visibleTrades.map((trade) => (
                <TradeCard
                  key={trade.trade_id}
                  trade={trade}
                  assets={sortedAssets}
                  selected={trade.trade_id === selectedProofTradeId}
                  onOpenProof={() => setSelectedProofTradeId(trade.trade_id)}
                />
              ))}
            </div>
            {selectedProofTrade && (
              <NetworkProofDrawer
                trade={selectedProofTrade}
                assets={sortedAssets}
                onClose={() => setSelectedProofTradeId(null)}
              />
            )}
            {canShowMore && (
              <button className="runtime-link-button" onClick={() => setShowAllTrades(true)}>
                Show more
              </button>
            )}
          </>
        ) : (
          <p className="runtime-empty">No completed user-facing trades yet.</p>
        )}
      </section>
    </main>
  );
}

function RuntimeStatusBadge({ status }: { status: ReturnType<typeof runtimeStatus> }) {
  return <span className={`runtime-status-badge runtime-status-${status.tone}`}>{status.label}</span>;
}

function StatusPanel({
  status,
  runId,
  startedAt,
  lastEventAt,
  updatedAt,
  loading
}: {
  status: ReturnType<typeof runtimeStatus>;
  runId?: string;
  startedAt?: RuntimeTimestamp;
  lastEventAt: RuntimeTimestamp | null;
  updatedAt: number | null;
  loading: boolean;
}) {
  return (
    <article className="runtime-status-panel">
      <div>
        <span>Status</span>
        <strong>{status.label}</strong>
      </div>
      <dl>
        <div className={loading ? 'runtime-stat-skeleton' : undefined}>
          <dt>Uptime</dt>
          <dd>{loading ? <SkeletonText /> : startedAt ? formatUptime(startedAt) : 'Unavailable'}</dd>
        </div>
        <div className={loading ? 'runtime-stat-skeleton' : undefined}>
          <dt>Last event</dt>
          <dd>{loading ? <SkeletonText /> : lastEventAt ? formatTimestamp(lastEventAt) : 'Unavailable'}</dd>
        </div>
        <div className={loading ? 'runtime-stat-skeleton' : undefined}>
          <dt>Run</dt>
          <dd>{loading ? <SkeletonText /> : shortValue(runId) || 'Unavailable'}</dd>
        </div>
        <div className={loading ? 'runtime-stat-skeleton' : undefined}>
          <dt>Updated</dt>
          <dd>{loading ? <SkeletonText /> : updatedAt ? formatRelative(updatedAt) : 'Pending'}</dd>
        </div>
      </dl>
    </article>
  );
}

function CounterTile({
  label,
  value,
  loading = false,
  unavailable = false
}: {
  label: string;
  value: string;
  loading?: boolean;
  unavailable?: boolean;
}) {
  return (
    <article className={unavailable ? 'runtime-counter runtime-counter-unavailable' : 'runtime-counter'} aria-busy={loading || undefined}>
      <span>{label}</span>
      <strong>{loading ? <SkeletonText wide /> : value}</strong>
    </article>
  );
}

function LedgerHealth({ healthy, entryCount }: { healthy: boolean; entryCount: number }) {
  return (
    <div className={healthy ? 'ledger-health ledger-health-good' : 'ledger-health ledger-health-warn'}>
      <strong>{healthy ? 'Balanced' : 'Needs reconciliation'}</strong>
      <span>{entryCount} entries</span>
    </div>
  );
}

function LedgerMatrix({ assets, ledger }: { assets: Asset[]; ledger: RuntimeLedgerResponse | null }) {
  const balances = new Map(
    (ledger?.balances ?? []).map((balance) => [
      balanceKey(balance.account_type, balance.asset),
      balance.display_amount || '0'
    ])
  );

  return (
    <div className="ledger-matrix" role="table" aria-label="Ledger balances">
      <div className="ledger-matrix-row ledger-matrix-head" role="row">
        <span role="columnheader">Asset</span>
        {visibleAccounts.map((accountType) => (
          <span key={accountType} role="columnheader">{accountLabel(accountType)}</span>
        ))}
      </div>
      {assets.map((asset) => (
        <div className="ledger-matrix-row" role="row" key={asset.id}>
          <strong role="rowheader">{asset.symbol}</strong>
          {visibleAccounts.map((accountType) => (
            <span key={accountType} role="cell">
              {balances.get(balanceKey(accountType, asset.id)) ?? balances.get(balanceKey(accountType, asset.symbol)) ?? '0'}
            </span>
          ))}
        </div>
      ))}
    </div>
  );
}

function LedgerSkeleton({ assets }: { assets: Asset[] }) {
  return (
    <div className="ledger-matrix ledger-matrix-loading" role="table" aria-label="Loading ledger balances" aria-busy="true">
      <div className="ledger-matrix-row ledger-matrix-head" role="row">
        <span role="columnheader">Asset</span>
        {visibleAccounts.map((accountType) => (
          <span key={accountType} role="columnheader">{accountLabel(accountType)}</span>
        ))}
      </div>
      {assets.map((asset) => (
        <div className="ledger-matrix-row" role="row" key={asset.id}>
          <strong role="rowheader">{asset.symbol}</strong>
          {visibleAccounts.map((accountType) => (
            <span key={accountType} role="cell">
              <SkeletonText />
            </span>
          ))}
        </div>
      ))}
    </div>
  );
}

function TradeCard({
  trade,
  assets,
  selected,
  onOpenProof
}: {
  trade: RuntimeTrade;
  assets: Asset[];
  selected: boolean;
  onOpenProof: () => void;
}) {
  const input = formatTradeAmount(trade.input, assets);
  const output = formatTradeAmount(trade.output, assets);
  const status = tradeStatus(trade.settlement_status);
  const txCount = trade.tx_signatures?.length ?? 0;

  return (
    <article className={selected ? `trade-card trade-card-${status.tone} trade-card-selected` : `trade-card trade-card-${status.tone}`}>
      <div className="trade-card-main">
        <strong>{input} {'->'} {output}</strong>
        <span>{status.label}</span>
      </div>
      <div className="trade-card-meta">
        <span>{shortValue(trade.trade_id)}</span>
        <span>{txCount ? `${txCount} txs` : 'No tx yet'}</span>
      </div>
      <button className="trade-proof-button" type="button" onClick={onOpenProof}>
        Network proof
      </button>
    </article>
  );
}

function NetworkProofDrawer({
  trade,
  assets,
  onClose
}: {
  trade: RuntimeTrade;
  assets: Asset[];
  onClose: () => void;
}) {
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const copiedTimer = useRef<number | null>(null);
  const input = formatTradeAmount(trade.input, assets);
  const output = formatTradeAmount(trade.output, assets);
  const status = tradeStatus(trade.settlement_status);
  const signatures = trade.tx_signatures ?? [];

  useEffect(() => {
    return () => {
      if (copiedTimer.current !== null) {
        window.clearTimeout(copiedTimer.current);
      }
    };
  }, []);

  async function copySignature(signature: string, signatureId: string) {
    await copyToClipboard(signature);
    setCopiedId(signatureId);
    if (copiedTimer.current !== null) {
      window.clearTimeout(copiedTimer.current);
    }
    copiedTimer.current = window.setTimeout(() => setCopiedId(null), 1_400);
  }

  return (
    <section className="network-proof-drawer" aria-label="Network proof drawer">
      <div className="network-proof-drawer-head">
        <div>
          <p className="eyebrow">Network proof</p>
          <h3>{input} {'->'} {output}</h3>
          <p>Match the amounts, trade id, quote id, and transaction roles in Solscan to verify this swap.</p>
        </div>
        <button className="network-proof-close" type="button" onClick={onClose} aria-label="Close network proof">
          Close
        </button>
      </div>

      <div className="network-proof-summary" aria-label="Trade verification details">
        <div>
          <span>Status</span>
          <strong>{status.label}</strong>
        </div>
        <div>
          <span>Trade id</span>
          <strong title={trade.trade_id}>{shortValue(trade.trade_id)}</strong>
        </div>
        <div>
          <span>Quote id</span>
          <strong title={trade.quote_id}>{shortValue(trade.quote_id)}</strong>
        </div>
      </div>

      {signatures.length > 0 ? (
        <div className="network-proof-list">
          {signatures.map((proof, index) => {
            const signatureId = `${proof.kind}-${proof.signature}-${index}`;
            const copied = copiedId === signatureId;
            return (
              <div className="network-proof-row" key={signatureId}>
                <div>
                  <span>{tradeSignatureLabel(proof.kind)}</span>
                  <a href={`https://solscan.io/tx/${proof.signature}`} target="_blank" rel="noreferrer">
                    {shortValue(proof.signature)}
                  </a>
                </div>
                <button
                  className={copied ? 'copy-proof-button copied' : 'copy-proof-button'}
                  type="button"
                  onClick={() => copySignature(proof.signature, signatureId)}
                  aria-label={`Copy ${tradeSignatureLabel(proof.kind)} transaction signature ${shortValue(proof.signature)}`}
                  title={copied ? 'Copied' : 'Copy signature'}
                >
                  <span className="copy-glyph" aria-hidden="true" />
                  <span className="copy-proof-label">{copied ? 'Copied' : 'Copy'}</span>
                </button>
              </div>
            );
          })}
        </div>
      ) : (
        <p className="network-proof-empty">No Solana transaction signatures have landed for this trade yet.</p>
      )}
    </section>
  );
}

function UnavailableState({ title, detail }: { title: string; detail: string }) {
  return (
    <div className="runtime-unavailable">
      <strong>{title}</strong>
      <p>{detail}</p>
    </div>
  );
}

function TradeSkeletonRail() {
  return (
    <div className="trade-rail" aria-label="Loading recent trades" aria-busy="true">
      {[0, 1, 2].map((index) => (
        <article className="trade-card trade-card-loading" key={index}>
          <div className="trade-card-main">
            <SkeletonText wide />
            <SkeletonText />
          </div>
          <div className="trade-card-meta">
            <SkeletonText />
            <SkeletonText />
          </div>
        </article>
      ))}
    </div>
  );
}

function SkeletonText({ wide = false }: { wide?: boolean }) {
  return <span className={wide ? 'runtime-skeleton runtime-skeleton-wide' : 'runtime-skeleton'} aria-hidden="true" />;
}

async function resolve<T>(promise: Promise<T>): Promise<{ ok: true; data: T } | { ok: false; error: string }> {
  try {
    return { ok: true, data: await promise };
  } catch (error) {
    return { ok: false, error: error instanceof Error ? error.message : 'Request failed' };
  }
}

function mergeLoadState<T>(
  current: LoadState<T>,
  result: { ok: true; data: T } | { ok: false; error: string }
): LoadState<T> {
  if (result.ok) {
    return {
      data: result.data,
      error: null,
      loading: false,
      updatedAt: Date.now()
    };
  }

  return {
    data: current.data,
    error: result.error,
    loading: false,
    updatedAt: current.updatedAt
  };
}

function runtimeStatus(
  health: LoadState<HealthPayload>,
  runtimeState: LoadState<RuntimeStateResponse>
) {
  if (health.loading || runtimeState.loading) {
    return { label: 'Starting', tone: 'warn' as const };
  }
  if (health.error) {
    return { label: 'Offline', tone: 'error' as const };
  }
  if (runtimeState.error) {
    return { label: 'Starting', tone: 'warn' as const };
  }
  return { label: 'Running', tone: 'good' as const };
}

function sortAssets(assets: Asset[]) {
  const bySymbol = new Map(assets.map((asset) => [asset.symbol, asset]));
  const ordered = assetOrder
    .map((symbol) => bySymbol.get(symbol) ?? assets.find((asset) => asset.id === symbol))
    .filter((asset): asset is Asset => Boolean(asset));
  const remaining = assets.filter((asset) => !ordered.some((orderedAsset) => orderedAsset.id === asset.id));
  return [...ordered, ...remaining];
}

function balanceKey(accountType: LedgerAccountType, asset: string) {
  return `${accountType}:${asset.toLowerCase()}`;
}

function accountLabel(accountType: LedgerAccountType) {
  switch (accountType) {
    case 'working_custody':
      return 'Working custody';
    case 'gateway':
      return 'Gateway';
    case 'htlc_escrow':
      return 'HTLC escrow';
    default:
      return humanize(accountType);
  }
}

function latestEventTimestamp(events: RuntimeEventsResponse | null) {
  const timestamps = (events?.events ?? [])
    .map((event) => event.event?.metadata?.occurred_at)
    .filter((value): value is RuntimeTimestamp => Boolean(value))
    .sort((left, right) => (runtimeDate(left)?.getTime() ?? 0) - (runtimeDate(right)?.getTime() ?? 0));
  return timestamps.length ? timestamps[timestamps.length - 1] : null;
}

function latestUpdate(states: Array<LoadState<unknown>>) {
  return states
    .map((state) => state.updatedAt)
    .filter((value): value is number => Boolean(value))
    .sort((a, b) => b - a)[0] ?? null;
}

function formatTradeAmount(amount: RuntimeTrade['input'], assets: Asset[]) {
  if (!amount) return 'Unknown';
  const asset = assets.find((candidate) => candidate.id === amount.asset || candidate.symbol === amount.asset);
  const display = formatRawAmount(amount.amount_raw, asset?.decimals ?? 0);
  return `${display} ${asset?.symbol ?? amount.asset}`;
}

function formatRawAmount(value: number | string, decimals: number) {
  const raw = BigInt(typeof value === 'number' ? Math.trunc(value).toString() : value);
  const negative = raw < 0n;
  const absolute = negative ? -raw : raw;
  if (decimals === 0) return `${negative ? '-' : ''}${absolute.toString()}`;

  const rawText = absolute.toString().padStart(decimals + 1, '0');
  const whole = rawText.slice(0, -decimals) || '0';
  const fraction = rawText.slice(-decimals).replace(/0+$/, '');
  return `${negative ? '-' : ''}${fraction ? `${whole}.${fraction}` : whole}`;
}

function tradeStatus(value: string) {
  const status = value.toLowerCase();
  if (status === 'redeemed' || status === 'completed' || status === 'success') {
    return { label: 'Redeemed', tone: 'success' as const };
  }
  if (status === 'failed' || status === 'refunded') {
    return { label: humanize(status), tone: 'failed' as const };
  }
  return { label: status ? humanize(status) : 'In progress', tone: 'pending' as const };
}

function formatUptime(value: RuntimeTimestamp) {
  const startedAt = runtimeDate(value);
  if (!startedAt) return 'Unavailable';
  const seconds = Math.max(0, Math.floor((Date.now() - startedAt.getTime()) / 1_000));
  const days = Math.floor(seconds / 86_400);
  const hours = Math.floor((seconds % 86_400) / 3_600);
  const minutes = Math.floor((seconds % 3_600) / 60);
  if (days > 0) return `${days}d ${hours}h`;
  if (hours > 0) return `${hours}h ${minutes}m`;
  return `${minutes}m`;
}

function formatRelative(value: number) {
  const seconds = Math.max(0, Math.floor((Date.now() - value) / 1_000));
  if (seconds < 5) return 'just now';
  if (seconds < 60) return `${seconds}s ago`;
  return `${Math.floor(seconds / 60)}m ago`;
}

function formatTimestamp(value: RuntimeTimestamp) {
  const timestamp = runtimeDate(value);
  if (!timestamp) return 'Unavailable';
  return new Intl.DateTimeFormat(undefined, {
    hour: 'numeric',
    minute: '2-digit',
    second: '2-digit'
  }).format(timestamp);
}

function runtimeDate(value: RuntimeTimestamp) {
  if (typeof value === 'string') {
    const timestamp = new Date(value);
    return Number.isNaN(timestamp.getTime()) ? null : timestamp;
  }

  if (!Array.isArray(value) || value.length < 6) return null;
  const [year, ordinalDay, hour, minute, second, nanosecond] = value;
  if (![year, ordinalDay, hour, minute, second, nanosecond].every(Number.isFinite)) return null;

  const timestamp = new Date(Date.UTC(
    year,
    0,
    1,
    hour,
    minute,
    second,
    Math.floor(nanosecond / 1_000_000)
  ));
  timestamp.setUTCDate(ordinalDay);
  return timestamp;
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

function tradeSignatureLabel(kind: RuntimeTrade['tx_signatures'][number]['kind']) {
  switch (kind) {
    case 'taker_lock':
      return 'Taker lock';
    case 'taker_redeem':
      return 'Taker redeem';
    case 'taker_refund':
      return 'Taker refund';
    case 'maker_lock':
      return 'Maker lock';
    case 'maker_redeem':
      return 'Maker redeem';
    case 'maker_refund':
      return 'Maker refund';
    case 'gateway_burn':
      return 'Gateway burn';
    case 'gateway_mint':
      return 'Gateway mint';
    case 'jupiter_swap':
      return 'Jupiter swap';
    default:
      return humanize(kind);
  }
}

function humanize(value: string) {
  return value.split('_').join(' ');
}
