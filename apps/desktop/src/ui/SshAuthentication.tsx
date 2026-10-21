import { useContext, useEffect, useRef, useState } from 'react';
import { VaultWorkspacePhase } from '../vaultWorkspace';
import { describeError, sshAuthWatch, sshAuthAnswer, type SshAuthChallenge as Challenge } from '../ipc';
import { Dialog } from './Overlay';
export function SshAuthentication() {
  const phase = useContext(VaultWorkspacePhase);
  const [queue, setQueue] = useState<Challenge[]>([]);
  useEffect(() => {
    if (phase !== 'unlocked') return;
    let live = true;
    const accept = (item: Challenge) => {
      if (!live) return;
      setQueue((previous) => item.kind === 'dismiss' ? previous.filter((p) => p.id !== item.id)
        : previous.some((p) => p.id === item.id) ? previous.map((p) => p.id === item.id ? item : p) : [...previous, item]);
    };
    void sshAuthWatch(accept).then((items) => items.forEach(accept)).catch(() => undefined);
    return () => { live = false; setQueue([]); };
  }, [phase]);
  const current = queue[0];
  if (phase !== 'unlocked' || !current) return null;
  return <Answer key={current.id} challenge={current} onDone={() => setQueue((items) => items.filter((item) => item.id !== current.id))} />;
}
function Answer({ challenge, onDone }: { challenge: Challenge; onDone: () => void }) {
  const [answer, setAnswer] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => { input.current?.focus(); }, []);
  async function send(value: string | null) {
    if (busy) return;
    setBusy(true);
    setAnswer('');
    try { await sshAuthAnswer(challenge.id, value); onDone(); }
    catch (err) { setError(describeError(err)); setBusy(false); }
  }
  return <Dialog title={`SSH authentication · ${challenge.hostLabel}`} onClose={() => { void send(null); }}>
    <form onSubmit={(event) => { event.preventDefault(); void send(challenge.kind === 'confirm' ? 'yes' : answer); }}>
      {challenge.instructions && <p className="muted" style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>{challenge.instructions}</p>}
      <p style={{ whiteSpace: 'pre-wrap', overflowWrap: 'anywhere' }}>{challenge.prompt}</p>
      {challenge.kind !== 'none' && challenge.kind !== 'confirm' && <label className="field">Response
        <input ref={input} type="password" value={answer} autoComplete="off" spellCheck={false} disabled={busy} onChange={(event) => setAnswer(event.target.value)} />
      </label>}
      <p className="hint">Used only for this authentication request. Responses are not saved.</p>
      {error && <p role="alert" className="banner" data-tone="danger">{error}</p>}
      <div className="dialog-foot">
        <button type="button" className="btn" disabled={busy} onClick={() => { void send(null); }}>Cancel</button>
        <button type="submit" className="btn btn-primary" disabled={busy}>Continue</button>
      </div>
    </form>
  </Dialog>;
}
