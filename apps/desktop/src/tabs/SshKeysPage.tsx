import { useEffect, useState, type FormEvent } from 'react';
import { describeError, sshKeyDelete, sshKeyGenerate, sshKeyImport, sshKeyImportFromFile, sshKeyList, sshKeyScanLocal, type SshKeyView } from '../ipc';
import { Field } from '../ui/Field';
import { ThemedSelect } from '../ui/ThemedSelect';
import { Dialog, Drawer } from '../ui/Overlay';
import { FolderIcon, ImportIcon, KeyIcon, PlusIcon, RefreshIcon, SearchIcon, TrashIcon } from '../ui/icons';

interface Props {
  visible: boolean;
  onReturnToProfile?: () => void;
}

type Mode = 'generate' | 'import';
const ALGORITHMS = [
  ['ed25519', 'Ed25519'],
  ['rsa-2048', 'RSA 2048-bit'], ['rsa-3072', 'RSA 3072-bit'],
  ['rsa-4096', 'RSA 4096-bit'], ['rsa-8192', 'RSA 8192-bit'],
  ['ecdsa-p256', 'ECDSA P-256'],
  ['ecdsa-p384', 'ECDSA P-384'], ['ecdsa-p521', 'ECDSA P-521'],
];

export function SshKeysPage({ visible, onReturnToProfile }: Props) {
  const [keys, setKeys] = useState<SshKeyView[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  const [query, setQuery] = useState('');
  const [editor, setEditor] = useState<Mode | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<SshKeyView | null>(null);
  const [deletingBusy, setDeletingBusy] = useState(false);
  const [deleteError, setDeleteError] = useState<string | null>(null);
  const [scanning, setScanning] = useState(false);
  const [scanNote, setScanNote] = useState<string | null>(null);

  useEffect(() => {
    if (!visible) return;
    let active = true;
    setLoading(true); setError(null);
    sshKeyList().then((items) => { if (active) setKeys(items); })
      .catch((err) => { if (active) setError(describeError(err)); })
      .finally(() => { if (active) setLoading(false); });
    return () => { active = false; };
  }, [visible, revision]);

  useEffect(() => {
    if (!visible) { setEditor(null); setDeleting(null); setCopied(null); }
  }, [visible]);

  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(null), 2500);
    return () => window.clearTimeout(timer);
  }, [copied]);

  async function copy(key: SshKeyView) {
    try {
      await navigator.clipboard.writeText(key.publicKey);
      setCopied(key.id); setError(null);
    } catch { setError('Could not copy the public key. Expand Public key to copy it manually.'); }
  }

  async function remove() {
    if (!deleting) return;
    setDeletingBusy(true); setDeleteError(null);
    try {
      await sshKeyDelete(deleting.id);
      setKeys((items) => items.filter((key) => key.id !== deleting.id));
      setDeleting(null);
    } catch (err) { setDeleteError(describeError(err)); }
    finally { setDeletingBusy(false); }
  }

  async function scanLocalKeys() {
    setScanning(true); setError(null); setScanNote(null);
    try {
      const result = await sshKeyScanLocal();
      if (result.imported.length) setKeys((items) => [...result.imported, ...items]);
      const parts = [`Imported ${result.imported.length} key${result.imported.length === 1 ? '' : 's'}.`];
      if (result.alreadySaved) parts.push(`${result.alreadySaved} already saved.`);
      if (result.skipped) parts.push(`${result.skipped} unsupported or encrypted file${result.skipped === 1 ? '' : 's'} skipped; import encrypted keys individually with their passphrase.`);
      setScanNote(parts.join(' '));
    } catch (err) { setError(describeError(err)); }
    finally { setScanning(false); }
  }

  const q = query.trim().toLowerCase();
  const filtered = keys.filter((key) => !q || [key.name, key.algorithm, key.fingerprintSha256].some((value) => value.toLowerCase().includes(q)));

  return (
    <section className="page ssh-keys-page" aria-label="SSH key management">
      <header className="page-head">
        <div><h1>SSH Keys</h1><p>Generate or import a key once, then select it in any SSH profile.</p></div>
        <span className="grow" />
        <div className="ssh-key-actions">
          <button className="btn" type="button" disabled={scanning} onClick={() => void scanLocalKeys()}>
            {scanning ? <span className="spinner" /> : <FolderIcon size={15} />}{scanning ? 'Scanning…' : 'Scan ~/.ssh'}
          </button>
          <button className="btn" type="button" onClick={() => setEditor('import')}><ImportIcon size={15} /> Import key</button>
          <button className="btn btn-primary" type="button" onClick={() => setEditor('generate')}><PlusIcon size={15} /> Generate key</button>
        </div>
      </header>
      {onReturnToProfile && <div className="ssh-key-return">
        <span>Your SSH profile draft is ready to continue.</span>
        <button className="btn btn-sm" type="button" onClick={onReturnToProfile}>Back to SSH profile</button>
      </div>}
      {error && <p className="banner" data-tone="danger" role="alert">{error}</p>}
      {scanNote && <p className="banner" data-tone="success" role="status">{scanNote}</p>}
      <div className="ssh-key-toolbar">
        <div className="ssh-key-search"><SearchIcon size={15} />
          <input type="search" aria-label="Filter SSH keys" placeholder="Search keys…" value={query} onChange={(e) => setQuery(e.target.value)} />
        </div>
        <span className="hint">{keys.length} {keys.length === 1 ? 'key' : 'keys'}</span>
        <button className="icon-btn" type="button" aria-label="Refresh SSH keys" disabled={loading}
          onClick={() => setRevision((value) => value + 1)}><RefreshIcon size={15} /></button>
      </div>
      {loading ? <p className="ssh-key-empty" role="status"><span className="spinner" /> Loading SSH keys…</p> : error && keys.length === 0 ? null :
        !error && keys.length === 0 ? <div className="ssh-key-empty">
          <KeyIcon size={30} /><h2>No SSH keys yet</h2><p>Generate a new key or import an existing private key to get started.</p>
          <span className="hint">Private keys are stored in your encrypted vault.</span>
        </div> : filtered.length === 0 ? <p className="ssh-key-empty">No keys match your search.</p> :
          <ul className="ssh-key-list" aria-label="Saved SSH keys">
            {filtered.map((key) => <li className="ssh-key-card" key={key.id}>
              <div className="ssh-key-card-head">
                <span className="ssh-key-symbol"><KeyIcon size={18} /></span>
                <div className="ssh-key-title"><h2>{key.name}</h2><span className="hint">{key.algorithm}</span></div>
                <div className="ssh-key-actions">
                  <button type="button" className="btn btn-sm" onClick={() => void copy(key)} aria-label={`Copy public key for ${key.name}`}>Copy public key</button>
                  <button type="button" className="icon-btn" aria-label={`Delete ${key.name}`} title="Delete key"
                    onClick={() => { setDeleteError(null); setDeleting(key); }}><TrashIcon size={15} /></button>
                </div>
              </div>
              <div className="ssh-key-fingerprint"><span>Fingerprint</span><code>{key.fingerprintSha256}</code></div>
              <details className="ssh-key-public"><summary>Public key</summary><pre>{key.publicKey}</pre></details>
              {copied === key.id && <span className="hint" role="status">Public key copied.</span>}
            </li>)}
          </ul>}
      <p className="ssh-key-vault-note"><KeyIcon size={13} /> Private keys stay in the vault. Only public keys can be copied.</p>
      {visible && editor && <KeyEditor key={editor} mode={editor} onClose={() => setEditor(null)} onCreated={(key) => {
        setKeys((items) => [key, ...items]); setQuery(''); setError(null); setEditor(null);
      }} />}
      {visible && deleting && <Dialog title="Delete SSH key?" tone="danger" icon={<TrashIcon />} onClose={() => { if (!deletingBusy) setDeleting(null); }}
        footer={<><button className="btn" type="button" disabled={deletingBusy} onClick={() => setDeleting(null)}>Cancel</button>
          <button className="btn btn-danger" type="button" disabled={deletingBusy} onClick={() => void remove()}>{deletingBusy ? 'Deleting…' : 'Delete key'}</button></>}>
        <p>Delete “{deleting.name}” from the vault? Keys assigned to a profile must be unassigned first.</p>
        {deleteError && <p role="alert" className="banner" data-tone="danger">{deleteError}</p>}
      </Dialog>}
    </section>
  );
}

function KeyEditor({ mode, onClose, onCreated }: { mode: Mode; onClose: () => void; onCreated: (key: SshKeyView) => void }) {
  const [name, setName] = useState('');
  const [algorithm, setAlgorithm] = useState('ed25519');
  const [file, setFile] = useState<File | null>(null);
  const [passphrase, setPassphrase] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const formId = 'ssh-key-editor';

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (busy) return;
    if (!name.trim()) { setError('Enter a name for the key.'); return; }
    if (mode === 'import' && !file) { setError('Choose a private key file.'); return; }
    if (file && file.size > 1024 * 1024) { setError('Private key must be 1 MB or smaller.'); return; }
    setBusy(true); setError(null);
    let bytes: Uint8Array | undefined;
    try {
      let key: SshKeyView;
      if (mode === 'generate') key = await sshKeyGenerate(name.trim(), algorithm);
      else {
        bytes = new Uint8Array(await file!.arrayBuffer());
        let binary = '';
        for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
        key = await sshKeyImport(name.trim(), btoa(binary), passphrase || undefined);
      }
      setPassphrase(''); setFile(null); onCreated(key);
    } catch (err) { setError(describeError(err)); }
    finally { bytes?.fill(0); setBusy(false); }
  }

  async function chooseFromSshFolder() {
    if (!name.trim()) { setError('Enter a name for the key first.'); return; }
    setBusy(true); setError(null);
    try {
      const key = await sshKeyImportFromFile(name.trim(), passphrase || undefined);
      if (key) { setPassphrase(''); onCreated(key); }
    } catch (err) { setError(describeError(err)); }
    finally { setBusy(false); }
  }

  return <Drawer title={mode === 'generate' ? 'Generate SSH key' : 'Import SSH key'}
    subtitle="Stored securely in your encrypted vault." onClose={() => { if (!busy) onClose(); }}
    footer={<><button className="btn btn-ghost" type="button" disabled={busy} onClick={onClose}>Cancel</button><span className="grow" />
      <button className="btn btn-primary" type="submit" form={formId} disabled={busy}>
        {busy && <span className="spinner" />}{busy ? (mode === 'generate' ? 'Generating key…' : 'Importing key…') : mode === 'generate' ? 'Generate key' : 'Import key'}
      </button></>}>
    <form id={formId} onSubmit={(e) => void submit(e)} className="ssh-key-form">
      {error && <p role="alert" className="banner" data-tone="danger">{error}</p>}
      <Field label="Key name" hint="A name to recognize this key when adding a profile.">
        <input autoFocus value={name} onChange={(e) => setName(e.target.value)} placeholder="Work laptop" maxLength={80} disabled={busy} />
      </Field>
      {mode === 'generate' ? <>
        <Field label="Algorithm"><ThemedSelect label="Algorithm" value={algorithm} onChange={setAlgorithm} disabled={busy}
          options={ALGORITHMS.map(([value, label]) => ({ value: value!, label: label! }))} /></Field>
        <p className="hint">Ed25519 is the default. Generated private keys stay in the backend and are encrypted by your vault passphrase.</p>
        {algorithm.startsWith('rsa-') && <p className="hint">Generating RSA keys can take longer, especially at 8192 bits. RSA authentication requires SHA-2 support on the server.</p>}
      </> : <>
        <Field label="Private key file" hint="OpenSSH or PEM format, up to 1 MB.">
          <input type="file" disabled={busy} onChange={(e) => setFile(e.target.files?.[0] ?? null)} />
        </Field>
        <button className="btn btn-sm" type="button" disabled={busy} onClick={() => void chooseFromSshFolder()}>
          {busy ? 'Reading key…' : 'Choose from ~/.ssh'}
        </button>
        <Field label="Key passphrase" hint="Required only if the imported key is encrypted.">
          <input type="password" autoComplete="off" value={passphrase} onChange={(e) => setPassphrase(e.target.value)} disabled={busy} />
        </Field>
        <p className="hint">Supports RSA (2048–8192 bits), Ed25519, and ECDSA P-256/P-384/P-521. RSA requires SHA-2 support on the server.</p>
      </>}
    </form>
  </Drawer>;
}
