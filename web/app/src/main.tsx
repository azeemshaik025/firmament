import './polyfills';
import { lazy, Suspense } from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter, NavLink, Route, Routes } from 'react-router-dom';
import './styles.css';

const SwapRoute = lazy(() => import('./pages/SwapRoute'));
const RuntimePage = lazy(() => import('./pages/RuntimePage').then((module) => ({ default: module.RuntimePage })));

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
        <Suspense fallback={<div className="route-loading" role="status">Loading console</div>}>
          <Routes>
            <Route path="/" element={<SwapRoute />} />
            <Route path="/runtime" element={<RuntimePage />} />
          </Routes>
        </Suspense>
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

async function loadRuntimeEnv() {
  try {
    await import(/* @vite-ignore */ `${import.meta.env.BASE_URL}runtime-env.js`);
  } catch {
    // VITE_SOLANA_RPC_URL and the public mainnet endpoint remain fallback paths.
  }
}

function renderApp() {
  ReactDOM.createRoot(document.getElementById('root')!).render(
    <BrowserRouter
      basename="/app"
      future={{
        v7_startTransition: true,
        v7_relativeSplatPath: true
      }}
    >
      <Shell />
    </BrowserRouter>
  );
}

void loadRuntimeEnv().finally(renderApp);
