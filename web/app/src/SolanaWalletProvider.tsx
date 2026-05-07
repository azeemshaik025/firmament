import { ComponentType, ReactNode, useMemo } from 'react';
import { Adapter } from '@solana/wallet-adapter-base';
import { ConnectionProvider, WalletProvider } from '@solana/wallet-adapter-react';
import { WalletModalProvider } from '@solana/wallet-adapter-react-ui';
import '@solana/wallet-adapter-react-ui/styles.css';

type SolanaWalletProviderProps = {
  children: ReactNode;
};

declare global {
  interface Window {
    __FIRMAMENT_CONFIG__?: {
      solanaRpcUrl?: string;
    };
  }
}

const runtimeRpcEndpoint = window.__FIRMAMENT_CONFIG__?.solanaRpcUrl?.trim();
const rpcEndpoint = runtimeRpcEndpoint || import.meta.env.VITE_SOLANA_RPC_URL || 'https://api.mainnet-beta.solana.com';
const AppConnectionProvider = ConnectionProvider as unknown as ComponentType<{
  endpoint: string;
  children: ReactNode;
}>;
const AppWalletProvider = WalletProvider as unknown as ComponentType<{
  wallets: Adapter[];
  autoConnect?: boolean;
  children: ReactNode;
}>;
const AppWalletModalProvider = WalletModalProvider as unknown as ComponentType<{
  children: ReactNode;
}>;

export function SolanaWalletProvider({ children }: SolanaWalletProviderProps) {
  const wallets = useMemo<Adapter[]>(() => [], []);

  return (
    <AppConnectionProvider endpoint={rpcEndpoint}>
      <AppWalletProvider wallets={wallets} autoConnect>
        <AppWalletModalProvider>{children}</AppWalletModalProvider>
      </AppWalletProvider>
    </AppConnectionProvider>
  );
}
