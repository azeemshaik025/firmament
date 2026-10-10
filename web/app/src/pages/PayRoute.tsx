import { SolanaWalletProvider } from '../SolanaWalletProvider';
import { PayPage } from './PayPage';

export default function PayRoute() {
  return (
    <SolanaWalletProvider>
      <PayPage />
    </SolanaWalletProvider>
  );
}
