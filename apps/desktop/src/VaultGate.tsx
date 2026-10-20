import { useCallback, useEffect, useRef, useState, type ReactNode, type FormEvent } from 'react';
import { describeError } from './ipc';
import { flushVaultViews, vaultAction, vaultActivity, vaultLock, vaultStatus, vaultWatch, vaultFlushComplete, type VaultStatus } from './vault';
import { SelaiLogo } from './ui/SelaiLogo';
import { ThemedSelect } from './ui/ThemedSelect';
import { VaultWorkspacePhase } from './vaultWorkspace';
import { WindowControls } from './App';
import { UpdateButton } from './updates';
import './vault.css';

export function VaultGate({ children }: { children: ReactNode }) {
  const [status, setStatus] = useState<VaultStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [phrase, setPhrase] = useState('');
  const [confirm, setConfirm] = useState('');
  const [recoveryInput, setRecoveryInput] = useState('');
  const [recovery, setRecovery] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [show, setShow] = useState(false);
  const [mode, setMode] = useState<'unlock' | 'recover' | 'restore' | 'restoreRecovery'>('unlock');
  const [settings, setSettings] = useState(false);
  const [lockChoice, setLockChoice] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const lastActivity = useRef(0);
  const statusRevision = useRef(0);
  const workspaceOpened = useRef(false);
  const acceptStatus = useCallback((next: VaultStatus) => {
    if (next.revision !== undefined && next.revision < statusRevision.current) return;
    statusRevision.current = next.revision ?? statusRevision.current;
    setStatus(next);
  }, []);
  const refresh = useCallback(async () => {
    try { const next = await vaultStatus(); acceptStatus(next); }
    catch (e) { setError(describeError(e)); }
  }, [acceptStatus]);

  useEffect(() => {
    let live = true;
    let receivedEvent = false;
    let timer: number | undefined;
    const visibility = () => { if (!document.hidden) void refresh(); };
    void vaultWatch((next) => { if (live) { receivedEvent = true; acceptStatus(next); } })
      .then((next) => {
        if (!live) return;
        if (!receivedEvent) acceptStatus(next);
        // Sparse recovery polling, not two IPC round trips per second at idle.
        timer = window.setInterval(() => { void refresh(); }, 10000);
      })
      .catch(() => {
        if (!live) return;
        void refresh();
        timer = window.setInterval(() => { void refresh(); }, 500);
      });
    document.addEventListener('visibilitychange', visibility);
    return () => { live = false; window.clearInterval(timer); document.removeEventListener('visibilitychange', visibility); };
  }, [refresh, acceptStatus]);
  useEffect(() => {
    if (status?.phase !== 'unlocked' || recovery) return;
    const activity = () => {
      if (Date.now() - lastActivity.current < 1000) return;
      lastActivity.current = Date.now();
      void vaultActivity().catch(() => refresh());
    };
    window.addEventListener('pointerdown', activity, true);
    window.addEventListener('keydown', activity, true);
    window.addEventListener('wheel', activity, { passive: true, capture: true });
    return () => {
      window.removeEventListener('pointerdown', activity, true);
      window.removeEventListener('keydown', activity, true);
      window.removeEventListener('wheel', activity, true);
    };
  }, [status?.phase, recovery, refresh]);
  useEffect(() => {
    if (status?.phase !== 'locking' && status?.phase !== 'locked') return;
    setPhrase(''); setConfirm(''); setRecovery(null); setRecoveryInput(''); setSettings(false); setLockChoice(false);
    setNotice(null); setError(null); setShow(false);
    if (status.phase !== 'locking') return;
    const timer = window.setInterval(() => { void flushVaultViews().catch(() => undefined); }, 150);
    return () => window.clearInterval(timer);
  }, [status?.phase]);
  useEffect(() => {
    if (status?.phase === 'locking' && status.flushRequired) void flushVaultViews().then(vaultFlushComplete).catch(() => undefined);
  }, [status?.phase, status?.flushRequired]);

  async function run(action: string) {
    setError(null); setNotice(null); setBusy(true);
    try {
      const recovering=action==='recover'||action==='restoreRecovery';
      if (['setup', 'recover', 'restoreRecovery', 'change'].includes(action) && phrase !== confirm) throw new Error('Passphrases do not match.');
      const result = await vaultAction(action, recovering ? recoveryInput : phrase, recovering || action === 'change' ? phrase : '');
      if (action === 'setup' || recovering || action === 'rotateRecovery') { setRecovery(result); setSaved(false); }
      else if (result) setNotice(result);
      setPhrase(''); setConfirm(''); setRecoveryInput('');
      if (['setup', 'unlock', 'recover', 'restore', 'restoreRecovery'].includes(action)) setMode('unlock');
      await refresh();
    } catch (e) { setError(describeError(e)); }
    finally { setBusy(false); }
  }
  async function lock(force: boolean) {
    setError(null); setBusy(true);
    try {
      let timer: number | undefined;
      try { await Promise.race([flushVaultViews(), new Promise<never>((_, reject) => { timer = window.setTimeout(() => reject(new Error('save timeout')), 2000); })]); }
      catch { setError('Some recent chat text could not be saved. Locking will continue.'); }
      finally { window.clearTimeout(timer); }
      await vaultLock(force); setSettings(false); setLockChoice(false); await refresh();
    }
    catch (e) { setError(describeError(e)); }
    finally { setBusy(false); }
  }
  async function saveRecovery() {
    if (!recovery) return;
    setBusy(true); setError(null);
    try { const path = await vaultAction('saveRecovery', recovery); if (path) { setSaved(true); setNotice(`Recovery key saved to ${path}. Keep it separate from your backup.`); } }
    catch (e) { setError(describeError(e)); }
    finally { setBusy(false); }
  }
  const requestLock = async () => {
    try { const current = await vaultStatus(); acceptStatus(current); if (current.activeTasks) setLockChoice(true); else await lock(false); }
    catch (e) { setError(describeError(e)); }
  };
  useEffect(() => {
    const security = () => { setSettings(true); setPhrase(''); setError(null); setNotice(null); };
    const lockRequest = () => { void requestLock(); };
    window.addEventListener('selai:security', security);
    window.addEventListener('selai:lock', lockRequest);
    return () => { window.removeEventListener('selai:security', security); window.removeEventListener('selai:lock', lockRequest); };
  });
  const setup = status?.phase === 'setup' && !mode.startsWith('restore');
  const recovering=mode==='recover'||mode==='restoreRecovery';
  const unlocked = status?.phase === 'unlocked';
  const locking = status?.phase === 'locking';
  if (unlocked && !recovery) workspaceOpened.current = true;
  const standby = status?.phase === 'locked' && status.terminals > 0 && workspaceOpened.current;
  const secretInput = (label: string, value: string, onChange: (value: string) => void, newPhrase = false) => (
    <label>{label}<input aria-label={label} type={show ? 'text' : 'password'} autoComplete={newPhrase ? 'new-password' : 'current-password'} value={value} maxLength={1024} onChange={(e) => onChange(e.target.value)} required minLength={newPhrase ? 12 : undefined} autoCapitalize="none" spellCheck={false} /></label>
  );
  const feedback = <>{error && <p role="alert" className="vault-error">{error}</p>}{notice && <p role="status" className="vault-notice">{notice}</p>}</>;
  return <>
    {(unlocked || locking || standby) && !recovery && <VaultWorkspacePhase.Provider value={unlocked ? 'unlocked' : locking ? 'locking' : 'locked'}><div hidden={!unlocked} inert={!unlocked} className="vault-workspace">{children}</div></VaultWorkspacePhase.Provider>}
    {unlocked && status.cleanupPending && !recovery && <button className="vault-cleanup-banner" onClick={() => setSettings(true)}>Legacy data cleanup needs attention · Open Security</button>}
    {(!unlocked || recovery) && <div className="vault-screen">
      <header className="workspace-head" data-tauri-drag-region><SelaiLogo size={27} /><span data-tauri-drag-region>Selai</span><span className="grow" data-tauri-drag-region /><UpdateButton /><WindowControls /></header>
      <main className="vault-center"><section className="vault-card">
        <SelaiLogo size={56} />
        {!status ? <><h1>Opening Selai</h1>{feedback}<button onClick={() => void refresh()}>Retry</button></> : recovery ? <>
          <h1>Save your recovery key</h1><p>This key can unlock your entire vault. Store it in a separate password manager or print it. It is shown only now.</p>
          <textarea aria-label="Recovery key" readOnly value={recovery} rows={3} spellCheck={false} />
          <button type="button" disabled={busy} onClick={() => void saveRecovery()}>Save recovery key</button>
          {feedback}
          <label className="vault-check"><input type="checkbox" checked={saved} onChange={(e) => setSaved(e.target.checked)} />I saved this key somewhere safe.</label>
          <button className="primary" disabled={!saved || busy} onClick={() => { setRecovery(null); setSettings(false); }}>Open workspace</button>
        </> : locking ? <><h1>Locking your vault</h1><p>The workspace is hidden. Terminal sessions stay connected while Selai finishes the current AI command and releases the vault key.</p><div className="vault-spinner" role="status" aria-label="Locking" /><p className="muted">{status.activeTasks} active task(s). No new commands will start.</p><button disabled={busy} onClick={() => void lock(true)}>Cancel work and lock now</button><p className="muted">A cancelled AI command may keep running on the server. Check its outcome before retrying.</p>{feedback}</> : <>
          <h1>{setup ? 'Protect your workspace' : mode === 'recover' ? 'Recover your vault' : mode.startsWith('restore') ? 'Restore an encrypted backup' : 'Unlock Selai'}</h1>
          <p>{setup ? status.legacyFound ? 'Your existing hosts, credentials, chat, and history will be migrated into one encrypted vault. Close all older Selai windows before continuing.' : 'One passphrase protects your hosts, credentials, chat, and history on this computer.' : mode === 'recover' ? 'Use your recovery key to set a new passphrase. A new recovery key will replace the old one.' : mode.startsWith('restore') ? 'Choose a .selai backup after entering its passphrase or recovery key. Restore requires a fresh data directory.' : 'Enter your passphrase to open your workspace.'}</p>
          {mode==='restoreRecovery' && <p>Enter the recovery key saved with this backup and choose a new passphrase.</p>}
          {status.phase === 'locked' && status.terminals > 0 && mode === 'unlock' && <p className="muted">{status.terminals} terminal session{status.terminals === 1 ? '' : 's'} on standby. Unlock to resume.</p>}
          <form onSubmit={(e: FormEvent) => { e.preventDefault(); void run(setup ? 'setup' : mode); }}>
            {recovering && secretInput('Recovery key', recoveryInput, setRecoveryInput)}
            {secretInput(setup || recovering ? 'New passphrase' : 'Passphrase', phrase, setPhrase, setup || recovering)}
            {(setup || recovering) && <>{secretInput('Confirm passphrase', confirm, setConfirm, true)}<p className="muted">Use at least 12 characters. Several unrelated words are easier to remember. Paste is supported.</p></>}
            <label className="vault-check"><input type="checkbox" checked={show} onChange={(e) => setShow(e.target.checked)} />Show passphrase</label>
            {feedback}
            <button className="primary" disabled={busy || status.retryAfterMs > 0}>{busy ? 'Please wait…' : setup ? 'Create encrypted vault' : mode === 'recover' ? 'Recover vault' : mode.startsWith('restore') ? 'Choose backup and restore' : 'Unlock'}</button>
            {status.retryAfterMs > 0 && <p role="status">Try again in {Math.ceil(status.retryAfterMs / 1000)} seconds.</p>}
          </form>
          {setup && <p className="muted">You will receive a recovery key. If you lose both it and your passphrase, the data cannot be recovered.</p>}
          {!setup && mode !== 'unlock' && <button className="vault-link" onClick={() => { setMode('unlock'); setError(null); setPhrase(''); }}>Back</button>}
          {status.phase === 'locked' && mode === 'unlock' && <button className="vault-link" onClick={() => { setMode('recover'); setPhrase(''); setError(null); }}>Use recovery key</button>}
          {status.phase === 'setup' && !status.legacyFound && mode === 'unlock' && <button className="vault-link" onClick={() => { setMode('restore'); setError(null); }}>Restore an existing backup</button>}
          {mode === 'restore' && <button className="vault-link" onClick={() => { setMode('restoreRecovery'); setPhrase(''); setConfirm(''); setError(null); }}>Use the backup recovery key instead</button>}
          {mode === 'restoreRecovery' && <button className="vault-link" onClick={() => { setMode('restore'); setPhrase(''); setConfirm(''); setRecoveryInput(''); setError(null); }}>Use the backup passphrase instead</button>}
        </>}
      </section></main>
    </div>}
    {settings && unlocked && !recovery && <SecuritySettings status={status} close={() => { setSettings(false); setPhrase(''); setConfirm(''); }} refresh={refresh} />}
    {lockChoice && <div className="vault-modal-backdrop"><section role="dialog" aria-modal="true" aria-labelledby="lock-title" className="vault-card"><h2 id="lock-title">Lock with work in progress?</h2><p>Terminal sessions will stay connected. Selai can finish the current AI command or cancel it immediately. New commands are blocked until you unlock.</p><p className="muted">A cancelled AI command may keep running on the server. Selai will not retry it automatically.</p>{feedback}<div className="vault-actions"><button disabled={busy} onClick={() => void lock(false)}>Finish current command, then lock</button><button disabled={busy} onClick={() => void lock(true)}>Lock now</button><button disabled={busy} onClick={() => setLockChoice(false)}>Keep working</button></div></section></div>}
  </>;
}

function SecuritySettings({ status, close, refresh }: {status: VaultStatus; close: () => void; refresh: () => Promise<void>}) {
  const [current, setCurrent] = useState('');
  const [next, setNext] = useState('');
  const [confirm, setConfirm] = useState('');
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [code, setCode] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const [busy, setBusy] = useState(false);
  const [storage, setStorage] = useState<{ databaseBytes: number; reclaimableBytes: number } | null>(null);
  useEffect(() => {
    let live = true;
    void vaultAction('storageStatus').then((result) => { if (live && result) setStorage(JSON.parse(result)); }).catch(() => undefined);
    return () => { live = false; };
  }, []);
  async function act(action: string, minutes?: number) {
    setError(null); setMessage(null); setBusy(true);
    try {
      if (action === 'change' && next !== confirm) throw new Error('New passphrases do not match.');
      const result = await vaultAction(action, current, next, minutes);
      if (action === 'compact' && result) { setStorage(JSON.parse(result)); setMessage('Vault compacted. Chat, history, and credentials were retained.'); }
      if (action === 'rotateRecovery') { setCode(result); setSaved(false); }
      else if (action === 'compact') { /* Storage result was rendered above. */ }
      else setMessage(result || (action === 'change' ? 'Passphrase changed. Existing backups still use their original passphrase.' : action === 'timeout' ? 'Auto-lock updated.' : 'Done.'));
      setCurrent(''); setNext(''); setConfirm(''); await refresh();
    } catch (e) { setError(describeError(e)); }
    finally { setBusy(false); }
  }
  const [archives, setArchives] = useState<string[]>([]);
  const [archive, setArchive] = useState('');
  useEffect(() => { void vaultAction('archives').then((result) => { const names = JSON.parse(result || '[]') as string[]; setArchives(names); setArchive(names[0] || ''); }).catch(() => undefined); }, []);
  async function exportArchive() { setBusy(true); setError(null); try { setMessage(await vaultAction('exportArchive', '', archive)); } catch (e) { setError(describeError(e)); } finally { setBusy(false); } }
  return <div className="vault-modal-backdrop"><section role="dialog" aria-modal="true" aria-labelledby="security-title" className="vault-card vault-security"><h2 id="security-title">Vault security</h2>
    <p>Your database, credentials, private keys, chat, and history are encrypted. Locking releases the vault key and hides the workspace. Existing terminal sessions stay connected, with input blocked until you unlock. Close their tabs or quit Selai to disconnect.</p>
    <label>Auto-lock after inactivity<ThemedSelect label="Auto-lock timeout" disabled={busy} value={String(status.timeoutMinutes)} options={[5,15,30,60].map((n) => ({ value: String(n), label: `${n} minutes` }))} onChange={(value) => void act('timeout', Number(value))} /></label>
    <p className="muted">Only your interaction with Selai resets this timer. Terminal output and AI replies do not.</p>
    {storage && <details><summary>Storage</summary>
      <p>Vault: {(storage.databaseBytes / 1048576).toFixed(1)} MiB · Reusable space: {(storage.reclaimableBytes / 1048576).toFixed(1)} MiB</p>
      <p className="muted">Compaction reclaims unused space without removing history. Close active tasks, terminals, transfers, and tunnels first. Available for vaults up to 128 MiB.</p>
      <button disabled={busy || status.activeTasks > 0 || status.terminals > 0 || storage.reclaimableBytes === 0 || storage.databaseBytes > 128 * 1048576} onClick={() => void act('compact')}>Compact vault</button>
    </details>}
    {status.cleanupPending && <div role="alert" className="vault-warning">Some old files or Credential Manager entries still need removal. Close older Selai instances and retry.<button disabled={busy} onClick={() => void act('cleanup')}>Retry legacy cleanup</button></div>}
    {code ? <><h3>New recovery key</h3><p>Your previous recovery key no longer opens this vault. Existing backups retain their earlier recovery key.</p><textarea aria-label="New recovery key" rows={3} readOnly value={code} /><label className="vault-check"><input type="checkbox" checked={saved} onChange={(e) => setSaved(e.target.checked)} />I saved this recovery key separately.</label><button disabled={!saved} onClick={() => { setCode(null); setSaved(false); }}>Continue</button></> : <>
      <label>Current passphrase<input aria-label="Current passphrase" type="password" autoComplete="current-password" value={current} onChange={(e) => setCurrent(e.target.value)} maxLength={1024} /></label>
      <div className="vault-actions"><button disabled={busy || !current} onClick={() => void act('backup')}>Save encrypted backup</button><button disabled={busy || !current} onClick={() => void act('rotateRecovery')}>Replace recovery key</button></div>
      <details><summary>Change passphrase</summary><label>New passphrase<input aria-label="New passphrase" type="password" autoComplete="new-password" value={next} onChange={(e) => setNext(e.target.value)} minLength={12} maxLength={1024} /></label><label>Confirm new passphrase<input aria-label="Confirm new passphrase" type="password" autoComplete="new-password" value={confirm} onChange={(e) => setConfirm(e.target.value)} /></label><button disabled={busy || !current || next.length < 12} onClick={() => void act('change')}>Change passphrase</button></details>
      <details><summary>Decrypt a history export</summary><p>This explicitly saves an unencrypted JSON file. Anyone with access to that file can read it. Enter the passphrase used when the export was created.</p><button disabled={busy || !current} onClick={() => void act('decryptHistory')}>Choose export and save plaintext JSON</button></details>
      {archives.length > 0 && <details><summary>Migrated history exports ({archives.length})</summary><p>These old exports were moved into your vault. Save an encrypted copy, then use the decrypt option above if you need JSON.</p><ThemedSelect label="Migrated export" value={archive} options={archives.map((name) => ({ value: name, label: name }))} onChange={setArchive} /><button disabled={busy || !archive} onClick={() => void exportArchive()}>Save encrypted copy</button></details>}
      <p className="muted">Restore a backup from the setup screen on a fresh Selai installation. Keep the backup and recovery key in separate locations.</p>
    </>}
    {error && <p role="alert" className="vault-error">{error}</p>}{message && <p role="status" className="vault-notice">{message}</p>}
    <button disabled={busy || code !== null} onClick={close}>Close</button>
  </section></div>;
}
