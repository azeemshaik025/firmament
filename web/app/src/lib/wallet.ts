import { PublicKey, Transaction, type Connection } from '@solana/web3.js';
import type { Asset } from '../api';
import { bytesFromBase64, formatRawBigInt } from './format';

export const nativeSolMint = 'So11111111111111111111111111111111111111112';

export function isNativeAsset(asset: Asset) {
  return asset.kind === 'native' || asset.mint === nativeSolMint;
}

export async function walletBalanceRaw(connection: Connection, wallet: PublicKey, asset: Asset) {
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

export async function walletFundingIssue(
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

export function isWalletCancellation(error: unknown) {
  const message = error instanceof Error ? error.message : '';
  return message.toLowerCase().includes('reject') || message.toLowerCase().includes('cancel');
}

export function walletFailure(error: unknown, fallback: string) {
  if (isWalletCancellation(error)) {
    return 'Wallet action was cancelled.';
  }
  return fallback;
}

export async function signAndSubmit(
  connection: Connection,
  signTransaction: (transaction: Transaction) => Promise<Transaction>,
  transactionBase64: string
) {
  const transaction = Transaction.from(bytesFromBase64(transactionBase64));
  const signed = await signTransaction(transaction);
  return connection.sendRawTransaction(signed.serialize());
}
