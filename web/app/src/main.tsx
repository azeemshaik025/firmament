import './polyfills';
import { lazy, Suspense } from 'react';
import ReactDOM from 'react-dom/client';
import { BrowserRouter, NavLink, Route, Routes } from 'react-router-dom';
import './styles.css';

const PayRoute = lazy(() => import('./pages/PayRoute'));
const SwapRoute = lazy(() => import('./pages/SwapRoute'));
const RuntimePage = lazy(() => import('./pages/RuntimePage').then((module) => ({ default: module.RuntimePage })));

function Shell() {
  return (
    <div className="app-shell">
      <header className="topbar">
        <a className="brand" href="/app">
          <span className="brand-mark" aria-hidden="true" />
          <span className="brand-copy">Firmament</span>
        </a>
        <nav aria-label="Console navigation">
          <NavLink to="/" end>Pay</NavLink>
          <NavLink to="/swap">Swap</NavLink>
          <NavLink to="/runtime">Maker</NavLink>
        </nav>
      </header>
      <div className="app-main">
        <Suspense fallback={<div className="route-loading" role="status">Loading console</div>}>
          <Routes>
            <Route path="/" element={<PayRoute />} />
            <Route path="/swap" element={<SwapRoute />} />
            <Route path="/runtime" element={<RuntimePage />} />
          </Routes>
        </Suspense>
      </div>
      <ConsoleFooter />
    </div>
  );
}

function ConsoleFooter() {
  return (
    <footer className="console-footer">
      <div className="footer-content">
        <div className="footer-brand">
          <strong>Firmament</strong>
          <p>Firm FX for stablecoin payments on Solana.</p>
        </div>
        <div className="footer-signals">
          <span>Firm quotes</span>
          <span>Atomic settlement</span>
          <span>Ledger proof</span>
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
