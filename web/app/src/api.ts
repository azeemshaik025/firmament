export type Asset = {
  id: string;
  symbol: string;
  mint: string;
  decimals: number;
  name?: string;
};

export type RfqRequest = {
  input_mint: string;
  output_mint: string;
  input_amount_raw: number;
  taker_wallet: string;
  expiry_seconds?: number;
};

export type RfqResponse =
  | {
      status: 'accepted';
      quote_id: string;
      quoted_output_amount_raw: number;
      spread_bps: number;
      expires_at: string;
      risk_checks: string[];
      htlc_terms: Record<string, unknown>;
    }
  | {
      status: 'rejected';
      reason: string;
      risk_check_details: string[];
    };

export type UnsignedWalletTransaction = {
  transaction_base64: string;
  recent_blockhash: string;
};

export type WalletSettlementResponse = {
  trade_id: string;
  quote_id: string;
  taker_lock_transaction: UnsignedWalletTransaction;
  expires_at?: string;
};

export type TradeStepResponse = {
  trade_id: string;
  settlement_status: string;
  maker_lock_signature?: string;
  maker_redeem_signature?: string;
  tx_signatures?: string[];
  taker_redeem_transaction?: UnsignedWalletTransaction;
};

export type AdminMe = {
  authenticated: boolean;
  username?: string;
  expires_at?: string;
};

export type AdminSummary = {
  inventory: unknown;
  risk: unknown;
  pnl: unknown;
  rfq: unknown;
  rebalance: unknown;
  gateway: unknown;
  recent_events: unknown[];
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
  const body = text ? JSON.parse(text) : null;

  if (!response.ok) {
    const message = body?.error?.message ?? body?.message ?? `Request failed with ${response.status}`;
    throw new Error(message);
  }

  return body as T;
}

export const api = {
  assets: () => request<Asset[]>('/v1/assets'),
  requestRfq: (payload: RfqRequest) => request<RfqResponse>('/v1/rfq', {
    method: 'POST',
    body: JSON.stringify(payload)
  }),
  walletSettlement: (quoteId: string, payload: { taker_wallet: string; secret_hash: string }) => request<WalletSettlementResponse>(`/v1/quotes/${quoteId}/wallet-settlement`, {
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
  adminLogin: (payload: { username: string; password: string }) => request<AdminMe>('/v1/admin/login', {
    method: 'POST',
    body: JSON.stringify(payload)
  }),
  adminLogout: () => request<AdminMe>('/v1/admin/logout', { method: 'POST' }),
  adminMe: () => request<AdminMe>('/v1/admin/me'),
  adminSummary: () => request<AdminSummary>('/v1/admin/summary'),
  rebalanceCheck: () => request<void>('/v1/admin/rebalance/check', { method: 'POST' }),
  gatewayRefillCheck: () => request<void>('/v1/admin/gateway/refill/check', { method: 'POST' })
};
