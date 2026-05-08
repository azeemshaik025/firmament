import { SolanaWalletProvider } from '../SolanaWalletProvider';
import { SwapPage } from './SwapPage';

export default function SwapRoute() {
  return (
    <SolanaWalletProvider>
      <SwapPage />
    </SolanaWalletProvider>
  );
}
