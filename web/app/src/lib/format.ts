import type { Asset, RfqResponse } from '../api';

export function parseDecimal(value: string | number | undefined): number | null {
  if (value === undefined) return null;
  const numeric = typeof value === 'number' ? value : Number(value);
  return Number.isFinite(numeric) ? numeric : null;
}

export function formatAssetAmount(value: number, asset: Asset) {
  const precision = Math.min(Math.max(asset.decimals, 2), 8);
  return value.toLocaleString(undefined, {
    minimumFractionDigits: 0,
    maximumFractionDigits: precision
  });
}

export function formatRangeLabel(asset: Asset): string | null {
  const min = parseDecimal(asset.min_trade_amount);
  const max = parseDecimal(asset.max_trade_amount);
  if (min === null || max === null) return null;
  return `${formatAssetAmount(min, asset)}-${formatAssetAmount(max, asset)} ${asset.symbol}`;
}

export function parseAmountRaw(amount: string, decimals: number): { ok: true; raw: number } | { ok: false; message: string } {
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

export function formatRawAmount(raw: number, decimals: number) {
  const rawText = BigInt(Math.trunc(raw)).toString().padStart(decimals + 1, '0');
  const whole = rawText.slice(0, -decimals) || '0';
  const fraction = rawText.slice(-decimals).replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : whole;
}

export function formatRawBigInt(raw: bigint, decimals: number) {
  const rawText = raw.toString().padStart(decimals + 1, '0');
  const whole = rawText.slice(0, -decimals) || '0';
  const fraction = rawText.slice(-decimals).replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : whole;
}

export function bytesToHex(bytes: Uint8Array) {
  return [...bytes].map((byte) => byte.toString(16).padStart(2, '0')).join('');
}

export function hexToBytes(value: string) {
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

export function bytesFromBase64(value: string) {
  return Uint8Array.from(atob(value), (char) => char.charCodeAt(0));
}

export function shortValue(value?: string) {
  if (!value) return '';
  return value.length > 14 ? `${value.slice(0, 7)}...${value.slice(-5)}` : value;
}

export function formatExpiry(value?: string) {
  if (!value) return 'Expiry unavailable';
  const expiry = new Date(value);
  if (Number.isNaN(expiry.getTime())) return 'Expiry unavailable';

  return new Intl.DateTimeFormat(undefined, {
    hour: 'numeric',
    minute: '2-digit',
    second: '2-digit'
  }).format(expiry);
}

export function quoteExpiryMs(quote: RfqResponse | null) {
  if (quote?.status !== 'accepted') return null;
  const expiresAt = new Date(quote.expires_at).getTime();
  return Number.isNaN(expiresAt) ? null : expiresAt - Date.now();
}

export function isQuoteExpired(quote: RfqResponse | null) {
  const expiresInMs = quoteExpiryMs(quote);
  return expiresInMs !== null && expiresInMs <= 0;
}
