import './polyfills';
import ReactDOM from 'react-dom/client';
import { BrowserRouter, NavLink, Route, Routes } from 'react-router-dom';
import './styles.css';
import { RuntimePage } from './pages/RuntimePage';
import { SolanaWalletProvider } from './SolanaWalletProvider';
import { SwapPage } from './pages/SwapPage';

function Shell() {
  return (
    <div className="app-shell">
      <header className="topbar">
        <a className="brand" href="/app">
          <span className="brand-mark" aria-hidden="true">
            <span />
            <span />
            <span />
            <span />
          </span>
          <span className="brand-copy">
            <span>Firmament</span>
          </span>
        </a>
        <nav aria-label="Console navigation">
          <NavLink to="/" end>Swap</NavLink>
          <NavLink to="/runtime">Runtime</NavLink>
        </nav>
      </header>
      <div className="app-main">
        <Routes>
          <Route path="/" element={<SwapPage />} />
          <Route path="/runtime" element={<RuntimePage />} />
        </Routes>
      </div>
      <ConsoleFooter />
    </div>
  );
}

function ConsoleFooter() {
  const signals = [
    { code: '01', label: 'Risk-gated RFQs' },
    { code: '02', label: 'Jupiter + Gateway repair' },
    { code: '03', label: 'Ledger proof' }
  ];

  return (
    <footer className="console-footer" aria-label="Firmament runtime footer">
      <div className="footer-wordmark" aria-hidden="true">
        <span>Firm</span>
        <span>ament</span>
      </div>
      <div className="footer-content">
        <div className="footer-brand">
          <span className="brand-mark footer-mark" aria-hidden="true">
            <span />
            <span />
            <span />
            <span />
          </span>
          <div>
            <strong><span>Firm</span>ament</strong>
            <p>Execution infrastructure for Solana apps and treasuries.</p>
          </div>
        </div>
        <div className="footer-signals" aria-label="Runtime pillars">
          {signals.map((signal) => (
            <span key={signal.code}>
              <b>{signal.code}</b>
              {signal.label}
            </span>
          ))}
        </div>
      </div>
    </footer>
  );
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <SolanaWalletProvider>
    <BrowserRouter
      basename="/app"
      future={{
        v7_startTransition: true,
        v7_relativeSplatPath: true
      }}
    >
      <Shell />
    </BrowserRouter>
  </SolanaWalletProvider>
);
