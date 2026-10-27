import { useEffect, useState } from 'react';
import { describeError, preferencesGet, preferencesSet } from '../ipc';

export function SettingsPage() {
  const [maxSteps, setMaxSteps] = useState(30);
  const [maxMinutes, setMaxMinutes] = useState(30);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    preferencesGet().then((prefs) => {
      if (!active) return;
      setMaxSteps(prefs.taskMaxSteps);
      setMaxMinutes(prefs.taskMaxMinutes);
    }).catch((err: unknown) => active && setError(describeError(err)));
    return () => { active = false; };
  }, []);

  async function saveLimits() {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const prefs = await preferencesSet({ taskMaxSteps: maxSteps, taskMaxMinutes: maxMinutes });
      setMaxSteps(prefs.taskMaxSteps);
      setMaxMinutes(prefs.taskMaxMinutes);
      setNotice(`Task budget saved: ${prefs.taskMaxSteps} steps or ${prefs.taskMaxMinutes} minutes.`);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section aria-label="Settings" className="page settings-page">
      <header className="page-head">
        <div className="grow">
          <h1>Settings</h1>
          <p>Configure task budgets and vault security for this Selai workspace.</p>
        </div>
      </header>

      <section className="card" aria-labelledby="settings-task-budget">
        <h2 id="settings-task-budget">AI task budget</h2>
        <p className="muted">A task stops when it reaches either limit. Changes apply to tasks started after saving.</p>
        <div className="form-row">
          <label className="field">
            Maximum steps
            <input type="number" min={1} max={300} value={maxSteps} disabled={busy}
              onChange={(e) => setMaxSteps(Number(e.target.value))} />
          </label>
          <label className="field">
            Time limit (minutes)
            <input type="number" min={1} max={300} value={maxMinutes} disabled={busy}
              onChange={(e) => setMaxMinutes(Number(e.target.value))} />
          </label>
          <button type="button" className="btn" disabled={busy} onClick={() => void saveLimits()}>
            {busy ? 'Saving…' : 'Save task budget'}
          </button>
        </div>
      </section>

      <section className="card" aria-labelledby="settings-security">
        <h2 id="settings-security">Security</h2>
        <p className="muted">Manage vault auto-lock, passphrase, recovery key, and encrypted backups.</p>
        <button type="button" className="btn" onClick={() => window.dispatchEvent(new Event('selai:security'))}>
          Open vault security
        </button>
      </section>

      {error && <p className="banner" role="alert" data-tone="danger">{error}</p>}
      {notice && <p className="banner" role="status" data-tone="success">{notice}</p>}
    </section>
  );
}
