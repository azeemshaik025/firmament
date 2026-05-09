export type Asset = {
  id: string;
  symbol: string;
  mint: string;
  decimals: number;
  name?: string;
  // serde rust_decimal::Decimal serializes as a JSON string (e.g. "1.50").
  // Marked optional so older runtimes without the field still type-check.
  min_trade_amount?: string;
  max_trade_amount?: string;
  aliases?: string[];
  kind?: 'native' | 'spl';
  network?: string;
  supported_outputs?: string[];
  quoteable_threshold_raw?: string;
  quoteable_threshold?: string;
};

export type Pair = {
  input_asset: string;
  output_asset: string;
  input_mint: string;
  output_mint: string;
  input_decimals: number;
  output_decimals: number;
  max_quote_notional_usd: string;
  min_quote_notional_usd: string;
  min_input_trade_amount?: string;
  max_input_trade_amount?: string;
  default_expiry_seconds: number;
};

export type PairsResponse = { pairs: Pair[] };

export type FriendlyRfqRequest = {
  input_asset: string;
  output_asset: string;
  amount: string;
  taker_wallet: string;
  expiry_seconds?: number;
};

export type LegacyRfqRequest = {
  input_mint: string;
  output_mint: string;
  input_amount_raw: number;
  taker_wallet: string;
  expiry_seconds?: number;
};

export type RfqRequest = FriendlyRfqRequest | LegacyRfqRequest;

export type AmountView = {
  asset: string;
  amount: string;
  amount_raw: string;
  decimals: number;
  mint?: string;
};

export type NextAction = {
  type: string;
  method: string;
  path: string;
};

export type RfqAcceptedResponse = {
  status: 'accepted';
  quote_id: string;
  quoted_output_amount_raw: number;
  spread_bps: number;
  expires_at: string;
  risk_checks: string[];
  htlc_terms: Record<string, unknown>;
  pair: { input_asset: string; output_asset: string };
  input: AmountView;
  output: AmountView;
  next_action: NextAction;
};

export type RfqRejectedResponse = {
  status: 'rejected';
  reason: string;
  risk_check_details: string[];
  message: string;
  suggested_action: string;
};

export type RfqResponse = RfqAcceptedResponse | RfqRejectedResponse;

export type UnsignedWalletTransaction = {
  transaction_base64: string;
  recent_blockhash: string;
};

export type WalletSettlementResponse = {
  trade_id: string;
  quote_id: string;
  taker_lock_transaction: UnsignedWalletTransaction;
  expires_at?: string;
  next_action?: NextAction;
};

export type WalletSettlementResumeResponse = {
  trade_id: string;
  quote_id: string;
  run_id: string;
  settlement_phase: string;
  settlement_status: string;
  taker_lock_transaction?: UnsignedWalletTransaction | null;
  taker_redeem_transaction?: UnsignedWalletTransaction | null;
  taker_refund_transaction?: UnsignedWalletTransaction | null;
  tx_signatures?: string[];
  tx_signature_kinds?: TradeSignature[];
  expires_at?: string;
  next_action?: NextAction | null;
};

export type TradeStepResponse = {
  trade_id: string;
  settlement_status: string;
  maker_lock_signature?: string;
  maker_redeem_signature?: string;
  maker_refund_signature?: string;
  tx_signatures?: string[];
  taker_redeem_transaction?: UnsignedWalletTransaction;
  taker_refund_transaction?: UnsignedWalletTransaction;
  next_action?: NextAction;
};

export type TokenAmount = {
  asset: string;
  amount_raw: number | string;
};

export type RuntimeEvent = {
  category: string;
  event?: {
    type?: string;
    metadata?: {
      occurred_at?: RuntimeTimestamp;
    };
    [key: string]: unknown;
  };
};

export type RuntimeTimestamp = string | number[];

export type RuntimeStateResponse = {
  state: {
    run_id: string;
    started_at: RuntimeTimestamp;
  };
};

export type RuntimeEventsResponse = {
  count: number;
  events: RuntimeEvent[];
};

export type LedgerAccountType =
  | 'working_custody'
  | 'reserved'
  | 'pending_dex_spend'
  | 'receivable'
  | 'htlc_escrow'
  | 'pending_escrow'
  | 'gateway'
  | 'gateway_reserved'
  | 'pending_gateway_deposit'
  | 'rebalance'
  | 'fees'
  | 'trading'
  | 'external';

export type RuntimeLedgerBalance = {
  account_type: LedgerAccountType;
  asset: string;
  qualifier?: string | null;
  balance_raw: string;
  decimals: number;
  display_amount: string;
};

export type RuntimeLedgerResponse = {
  healthy: boolean;
  entry_count: number;
  balances: RuntimeLedgerBalance[];
};

export type TradeSignatureKind =
  | 'taker_lock'
  | 'taker_redeem'
  | 'taker_refund'
  | 'maker_lock'
  | 'maker_redeem'
  | 'maker_refund'
  | 'gateway_burn'
  | 'gateway_mint'
  | 'jupiter_swap';

export type TradeSignature = {
  kind: TradeSignatureKind;
  signature: string;
};

export type TradeAmount = {
  asset: string;
  amount_raw: string;
  decimals: number;
  display_amount: string;
};

export type RuntimeTrade = {
  trade_id: string;
  quote_id: string;
  run_id?: string;
  current_run?: boolean;
  taker_wallet?: string;
  expires_at?: string;
  created_at?: string;
  settlement_status: string;
  input: TradeAmount;
  output: TradeAmount;
  tx_signatures: TradeSignature[];
};

export type RuntimeTradesResponse = {
  total_count: number;
  successful_count: number;
  active_count?: number;
  refunded_count?: number;
  failed_count?: number;
  trades: RuntimeTrade[];
};

export type HealthResponse = {
  status?: string;
  ok?: boolean;
  [key: string]: unknown;
};

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    credentials: 'include',
    ...init,
    headers: {
      'Content-Type': 'application/json',
      ...(init?.headers ?? {})
    }
  });

  const text = await response.text();
  const body = parseResponseBody(text);

  if (!response.ok) {
    const message = body?.error?.message ?? body?.message ?? `Request failed with ${response.status}`;
    throw new Error(message);
  }

  return body as T;
}

function parseResponseBody(text: string) {
  if (!text) return null;
  try {
    return JSON.parse(text);
  } catch {
    return { status: text };
  }
}

export const api = {
  assets: () => request<Asset[]>('/v1/assets'),
  pairs: () => request<PairsResponse>('/v1/pairs'),
  health: () => request<HealthResponse>('/health'),
  runtimeState: () => request<RuntimeStateResponse>('/v1/runtime/state'),
  runtimeEvents: (limit = 30) => request<RuntimeEventsResponse>(`/v1/runtime/events?limit=${limit}`),
  runtimeLedger: (accountType?: LedgerAccountType) => {
    const query = accountType ? `?account_type=${encodeURIComponent(accountType)}` : '';
    return request<RuntimeLedgerResponse>(`/v1/runtime/ledger${query}`);
  },
  runtimeTrades: (limit = 10, wallet?: string) => {
    const params = new URLSearchParams({ limit: String(limit) });
    if (wallet) params.set('wallet', wallet);
    return request<RuntimeTradesResponse>(`/v1/runtime/trades?${params.toString()}`);
  },
  walletTrades: (wallet: string, limit = 25) => {
    const params = new URLSearchParams({ limit: String(limit), wallet });
    return request<RuntimeTradesResponse>(`/v1/runtime/trades?${params.toString()}`);
  },
  requestRfq: (payload: RfqRequest) => request<RfqResponse>('/v1/rfq', {
    method: 'POST',
    body: JSON.stringify(payload)
  }),
  walletSettlement: (quoteId: string, payload: { taker_wallet: string; secret_hash: string }) => request<WalletSettlementResponse>(`/v1/quotes/${quoteId}/wallet-settlement`, {
    method: 'POST',
    body: JSON.stringify(payload)
  }),
  resumeSettlement: (tradeId: string) => request<WalletSettlementResumeResponse>(`/v1/trades/${tradeId}/resume`, {
    method: 'POST'
  }),
  abandonSettlement: (tradeId: string, payload: { secret_hash: string }) => request<unknown>(`/v1/trades/${tradeId}/abandon`, {
    method: 'POST',
    body: JSON.stringify(payload)
  }),
  takerLock: (tradeId: string, payload: { signature: string }) => request<TradeStepResponse>(`/v1/trades/${tradeId}/taker-lock`, {
    method: 'POST',
    body: JSON.stringify(payload)
  }),
  takerRedeem: (tradeId: string, payload: { preimage: string; signature?: string }) => request<TradeStepResponse>(`/v1/trades/${tradeId}/taker-redeem`, {
    method: 'POST',
    body: JSON.stringify(payload)
  }),
  takerRefund: (tradeId: string, payload: { signature?: string }) => request<TradeStepResponse>(`/v1/trades/${tradeId}/taker-refund`, {
    method: 'POST',
    body: JSON.stringify(payload)
  })
};
