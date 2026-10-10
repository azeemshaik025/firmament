import {
  BaseSignerWalletAdapter,
  WalletLoadError,
  WalletName,
  WalletNotConnectedError,
  WalletReadyState,
  isVersionedTransaction
} from '@solana/wallet-adapter-base';
import type { TransactionVersion } from '@solana/web3.js';
import { Keypair, Transaction, VersionedTransaction } from '@solana/web3.js';

// A dev-only signer for recordings and local runs: the Vite dev server serves a keypair from
// FIRMAMENT_DEMO_SIGNER_PATH at /__demo-signer and this adapter signs with it, no popup.
export const demoWalletName = 'Demo wallet' as WalletName<'Demo wallet'>;

const icon =
  'data:image/svg+xml;base64,' +
  btoa(
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="7" fill="#f6f5f1"/><rect x="5" y="21" width="22" height="1.6" rx="0.8" fill="#1d2130"/><circle cx="19" cy="16.5" r="3.4" fill="#e3b04b"/></svg>'
  );

function parseSecretKey(value: unknown): Uint8Array {
  if (!Array.isArray(value) || value.length !== 64 || !value.every((byte) => Number.isInteger(byte) && byte >= 0 && byte <= 255)) {
    throw new WalletLoadError('The demo signer endpoint did not return a 64-byte secret key.');
  }
  return Uint8Array.from(value);
}

export class DemoWalletAdapter extends BaseSignerWalletAdapter {
  name = demoWalletName;
  url = 'https://github.com/azeemshaik025/firmament';
  icon = icon;
  supportedTransactionVersions: ReadonlySet<TransactionVersion> = new Set(['legacy', 0]);
  private keypair: Keypair | null = null;
  private loading = false;

  get connecting() {
    return this.loading;
  }

  get publicKey() {
    return this.keypair?.publicKey ?? null;
  }

  get readyState() {
    return WalletReadyState.Loadable;
  }

  async connect(): Promise<void> {
    if (this.keypair || this.loading) return;
    this.loading = true;
    try {
      const response = await fetch('/__demo-signer');
      if (!response.ok) {
        throw new WalletLoadError('Start the dev server with FIRMAMENT_DEMO_SIGNER_PATH to use the demo wallet.');
      }
      const body: unknown = await response.json();
      this.keypair = Keypair.fromSecretKey(parseSecretKey(body));
    } catch (error) {
      this.emit('error', error instanceof WalletLoadError ? error : new WalletLoadError(String(error)));
      throw error;
    } finally {
      this.loading = false;
    }
    this.emit('connect', this.keypair.publicKey);
  }

  async disconnect(): Promise<void> {
    this.keypair = null;
    this.emit('disconnect');
  }

  async signTransaction<T extends Transaction | VersionedTransaction>(transaction: T): Promise<T> {
    if (!this.keypair) throw new WalletNotConnectedError();
    if (isVersionedTransaction(transaction)) {
      transaction.sign([this.keypair]);
    } else {
      transaction.partialSign(this.keypair);
    }
    return transaction;
  }
}
