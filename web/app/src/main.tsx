import ReactDOM from 'react-dom/client';
import { BrowserRouter, NavLink, Route, Routes } from 'react-router-dom';
import './styles.css';
import { AdminPage } from './pages/AdminPage';
import { SwapPage } from './pages/SwapPage';

function Shell() {
  return (
    <div className="app-shell">
      <header className="topbar">
        <a className="brand" href="/">
          <span>Firmament</span>
          <small>RFQ console</small>
        </a>
        <nav aria-label="Console navigation">
          <NavLink to="/" end>Swap</NavLink>
          <NavLink to="/admin">Admin</NavLink>
        </nav>
      </header>
      <Routes>
        <Route path="/" element={<SwapPage />} />
        <Route path="/admin" element={<AdminPage />} />
      </Routes>
    </div>
  );
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <BrowserRouter basename="/app">
    <Shell />
  </BrowserRouter>
);
