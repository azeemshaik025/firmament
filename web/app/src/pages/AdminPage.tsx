import { FormEvent, useEffect, useState } from 'react';
import { api, AdminMe, AdminSummary } from '../api';

export function AdminPage() {
  const [username, setUsername] = useState('admin');
  const [password, setPassword] = useState('');
  const [me, setMe] = useState<AdminMe | null>(null);
  const [summary, setSummary] = useState<AdminSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function loadDashboard() {
    setBusy(true);
    setError(null);
    try {
      const [nextMe, nextSummary] = await Promise.all([api.adminMe(), api.adminSummary()]);
      setMe(nextMe);
      setSummary(nextSummary);
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : 'Unable to load admin dashboard.');
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    void loadDashboard();
  }, []);

  async function submitLogin(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      setMe(await api.adminLogin({ username, password }));
      setPassword('');
      await loadDashboard();
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : 'Unable to login.');
    } finally {
      setBusy(false);
    }
  }

  async function runAction(action: 'rebalance' | 'gateway') {
    setBusy(true);
    setError(null);
    try {
      if (action === 'rebalance') {
        await api.rebalanceCheck();
      } else {
        await api.gatewayRefillCheck();
      }
      await loadDashboard();
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : 'Unable to run admin action.');
    } finally {
      setBusy(false);
    }
  }

  const authenticated = Boolean(me?.authenticated);

  return (
    <main className="admin-layout">
      <section className="admin-login">
        <p className="eyebrow">Operator access</p>
        <h1>Admin cockpit</h1>
        <p className="lede">Password-hash admin login with an HttpOnly session cookie. Dashboard data comes from the same-origin runtime endpoints.</p>
        <form onSubmit={submitLogin}>
          <label>
            Username
            <input value={username} onChange={(event) => setUsername(event.target.value)} placeholder="admin" />
          </label>
          <label>
            Password
            <input type="password" value={password} onChange={(event) => setPassword(event.target.value)} placeholder="Configured admin password" />
          </label>
          <button className="action-button" disabled={!username || !password || busy}>{busy ? 'Loading...' : 'Login and load dashboard'}</button>
        </form>
      </section>

      <section className="dashboard-board" aria-live="polite">
        <div className="board-heading">
          <div>
            <p className="eyebrow">Runtime summary</p>
            <h2>{authenticated ? 'Liquidity supervision' : 'Locked'}</h2>
          </div>
          <button onClick={loadDashboard} disabled={!authenticated || busy}>Refresh</button>
        </div>
        <div className="admin-actions">
          <button onClick={() => runAction('rebalance')} disabled={!authenticated || busy}>Run rebalance check</button>
          <button onClick={() => runAction('gateway')} disabled={!authenticated || busy}>Run Gateway refill check</button>
        </div>
        {error && <p className="error-box">{error}</p>}
        <div className="summary-grid">
          <DashboardTile title="Operator" value={me?.username ?? 'pending'} detail={me?.expires_at ?? 'session unknown'} />
          <DashboardTile title="Inventory" value={summary?.inventory ? 'online' : 'waiting'} detail={summary?.inventory} />
          <DashboardTile title="Risk" value={summary?.risk ? 'loaded' : 'waiting'} detail={summary?.risk} />
          <DashboardTile title="P&L" value={summary?.pnl ? 'projected' : 'waiting'} detail={summary?.pnl} />
          <DashboardTile title="RFQs" value={summary?.rfq ? 'tracked' : 'waiting'} detail={summary?.rfq} />
          <DashboardTile title="Rebalance" value={summary?.rebalance ? 'visible' : 'waiting'} detail={summary?.rebalance} />
          <DashboardTile title="Gateway" value={summary?.gateway ? 'visible' : 'waiting'} detail={summary?.gateway} />
          <DashboardTile title="Events" value={String(summary?.recent_events?.length ?? 0)} detail={summary?.recent_events} />
        </div>
      </section>
    </main>
  );
}

function DashboardTile({ title, value, detail }: { title: string; value: string; detail?: unknown }) {
  return (
    <article className="summary-tile">
      <span>{title}</span>
      <strong>{value}</strong>
      <pre>{detail === undefined ? 'No data yet' : JSON.stringify(detail, null, 2)}</pre>
    </article>
  );
}
