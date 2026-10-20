import { useMemo, useState } from 'react';
import { describeError, hostList, hostSave, kdbxPreview, sshConfigRead, type HostView } from '../ipc';
import { Drawer } from '../ui/Overlay';
import { ThemedSelect } from '../ui/ThemedSelect';
import { collectPages } from '../ui/collectPages';
import {
  FIELDS, MAX_IMPORT_BYTES, MAX_KDBX_BYTES, TEMPLATE, guessMapping, parseProfileTable, parseSshConfig, prepareProfiles,
  type ColumnMapping, type ImportField, type ProfileTable,
} from './sshProfileImport';

interface Props {
  onClose: () => void;
  onImported: () => Promise<void>;
}

const LABELS: Record<ImportField, string> = {
  label: 'Label', hostname: 'Hostname', uri: 'SSH URI', port: 'Port', username: 'Username',
  authMethod: 'Authentication', password: 'Password', privateKeyPath: 'Private key path',
  keyPassphrase: 'Key passphrase', notes: 'Notes', type: 'Item type',
};

async function allHosts(): Promise<HostView[]> {
  return collectPages((cursor) => hostList(undefined, false, cursor));
}

async function fileToBase64(file: File): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = '';
  const chunkSize = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return window.btoa(binary);
}

export function HostImport({ onClose, onImported }: Props) {
  const [table, setTable] = useState<ProfileTable | null>(null);
  const [mapping, setMapping] = useState<ColumnMapping>(() => guessMapping([]));
  const [existing, setExisting] = useState<HostView[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [fileName, setFileName] = useState('');
  const [kdbxFile, setKdbxFile] = useState<File | null>(null);
  const [kdbxPassphrase, setKdbxPassphrase] = useState('');
  const [includeNotes, setIncludeNotes] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<{ saved: number; skipped: number; failures: string[] } | null>(null);
  const candidates = useMemo(() => table ? prepareProfiles(table, mapping, existing) : [], [table, mapping, existing]);
  const ready = candidates.filter((candidate, index) => selected.has(index) && candidate.input);

  async function chooseFile(file: File | undefined) {
    if (!file) return;
    setTable(null);
    setKdbxFile(null);
    setKdbxPassphrase('');
    setResult(null);
    setError(null);
    setFileName('');
    if (/\.kdbx$/i.test(file.name)) {
      if (file.size > MAX_KDBX_BYTES) {
        setError('KDBX files must be 10 MB or smaller.');
        return;
      }
      setKdbxFile(file);
      setFileName(file.name);
      return;
    }
    setBusy(true);
    try {
      if (file.size > MAX_IMPORT_BYTES) throw new Error('Import file is larger than 2 MB.');
      const parsed = parseProfileTable(await file.text(), file.name);
      const hosts = await allHosts();
      setTable(parsed);
      setMapping(guessMapping(parsed.headers));
      setExisting(hosts);
      setSelected(new Set(parsed.rows.map((_, index) => index)));
      setFileName(file.name);
    } catch (err) {
      setError(err instanceof Error ? err.message : describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function loadDefaultSshConfig() {
    setTable(null);
    setKdbxFile(null);
    setKdbxPassphrase('');
    setResult(null);
    setError(null);
    setBusy(true);
    try {
      const parsed = parseSshConfig(await sshConfigRead());
      const hosts = await allHosts();
      setTable(parsed);
      setMapping(guessMapping(parsed.headers));
      setExisting(hosts);
      setSelected(new Set(parsed.rows.map((_, index) => index)));
      setFileName('~/.ssh/config');
    } catch (err) {
      setError(err instanceof Error ? err.message : describeError(err));
    } finally {
      setBusy(false);
    }
  }

  async function unlockKdbx() {
    if (!kdbxFile || !kdbxPassphrase) return;
    setBusy(true);
    setError(null);
    try {
      const profiles = await kdbxPreview(await fileToBase64(kdbxFile), kdbxPassphrase);
      const rows = profiles.map(({ title, url, username, password, notes }) => ({ title, uri: url, username, password, notes }));
      const parsed = parseProfileTable(JSON.stringify(rows), 'kdbx-profiles.json');
      const hosts = await allHosts();
      setTable(parsed);
      setMapping(guessMapping(parsed.headers));
      setExisting(hosts);
      setSelected(new Set(parsed.rows.map((_, index) => index)));
      setKdbxFile(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : describeError(err));
    } finally {
      setKdbxPassphrase('');
      setBusy(false);
    }
  }

  async function importSelected() {
    if (!table || !ready.length) return;
    setBusy(true);
    setError(null);
    const failures: string[] = [];
    let saved = 0;
    try {
      // Recheck all pages just before writing. Each host still goes through the
      // existing Rust validation and encrypted vault path in host_save.
      const fresh = prepareProfiles(table, mapping, await allHosts());
      for (const [index, candidate] of fresh.entries()) {
        if (!selected.has(index) || !candidate.input) continue;
        try {
          await hostSave({ ...candidate.input, notes: includeNotes ? candidate.input.notes : null });
          saved += 1;
        } catch (err) {
          failures.push(`Row ${candidate.rowNumber} (${candidate.label}): ${describeError(err)}`);
        }
      }
      setResult({ saved, skipped: table.rows.length - saved - failures.length, failures });
    } catch (err) {
      setError(describeError(err));
    } finally {
      // Discard all parsed rows, including password strings, after one attempt.
      setTable(null);
      setSelected(new Set());
      setFileName('');
      setKdbxFile(null);
      setKdbxPassphrase('');
      try {
        if (saved) await onImported();
      } catch (err) {
        setError(describeError(err));
      } finally {
        setBusy(false);
      }
    }
  }

  return (
    <Drawer
      title="Import SSH profiles"
      subtitle="Review profiles before adding them to Selai."
      onClose={busy ? () => undefined : onClose}
      footer={
        <>
          <button type="button" className="btn btn-ghost" onClick={onClose} disabled={busy}>Close</button>
          <span className="spacer" />
          <button type="button" className="btn btn-primary" onClick={() => void importSelected()} disabled={busy || !ready.length}>
            {busy ? 'Working…' : `Import ${ready.length} profile${ready.length === 1 ? '' : 's'}`}
          </button>
        </>
      }
    >
      <div className="form-section">
        <p className="muted">Choose a CSV or JSON export, a password-protected KDBX file, or import your OpenSSH config. Bitwarden SSH logins, Passbolt exports, and host lists using common column names are recognized. Other exports can use the Selai CSV template.</p>
        <p className="muted">Only SSH/SFTP addresses are taken from password-manager URI fields. Passwords are hidden here and saved in your encrypted vault. The source export file is not retained by Selai.</p>
        <a className="btn btn-sm" download="selai-ssh-profiles.csv" href={`data:text/csv;charset=utf-8,${encodeURIComponent(TEMPLATE)}`}>Download CSV template</a>
        <button type="button" className="btn btn-sm" onClick={() => void loadDefaultSshConfig()} disabled={busy}>Import ~/.ssh/config</button>
        <label className="field">
          Profile export or SSH config file
          <input type="file" accept=".csv,.json,.kdbx,.config,.sshconfig,config,ssh_config,text/csv,application/json,text/plain,application/octet-stream" disabled={busy} onChange={(event) => {
            const file = event.currentTarget.files?.[0];
            event.currentTarget.value = '';
            void chooseFile(file);
          }} />
        </label>
        {kdbxFile && (
          <div className="kdbx-unlock">
            <label className="field">
              KDBX passphrase
              <input
                type="password"
                aria-label="KDBX passphrase"
                autoComplete="current-password"
                value={kdbxPassphrase}
                onChange={(event) => setKdbxPassphrase(event.target.value)}
                disabled={busy}
              />
            </label>
            <button type="button" className="btn btn-sm" onClick={() => void unlockKdbx()} disabled={busy || !kdbxPassphrase}>
              {busy ? 'Unlocking…' : 'Unlock KDBX'}
            </button>
            <p className="hint">The vault is decrypted locally. Only SSH/SFTP entries are shown for import.</p>
          </div>
        )}
        {error && <p role="alert" className="banner" data-tone="danger">{error}</p>}
        {result && <p role="status" className="banner" data-tone={result.failures.length ? 'warning' : 'success'}>
          Imported {result.saved}; skipped {result.skipped}; failed {result.failures.length}.
        </p>}
        {result?.failures.map((failure) => <p key={failure} className="hint">{failure}</p>)}
        <p className="hint">SSH config import uses concrete Host aliases and common options such as HostName, User, Port, IdentityFile, ConnectTimeout, and ServerAliveInterval. Include, Match, ProxyJump, and ProxyCommand are not applied. IdentityFile contents are read locally and stored in the encrypted vault when imported.</p>
      </div>

      {table && (
        <>
          <div className="form-section">
            <h3>{fileName} · {table.rows.length} rows</h3>
            <p className="hint">Check the detected columns. Change any mapping before importing.</p>
            <div className="import-mapping">
              {FIELDS.map((field) => (
                <label className="field" key={field}>
                  {LABELS[field]}
                  <ThemedSelect label={LABELS[field]} value={mapping[field]} options={[{ value: '', label: 'Not mapped' }, ...table.headers.map((header) => ({ value: header, label: header }))]} onChange={(value) => setMapping((current) => ({ ...current, [field]: value }))} />
                </label>
              ))}
            </div>
            <label className="import-note-option">
              <input type="checkbox" checked={includeNotes} onChange={(event) => setIncludeNotes(event.target.checked)} />
              Import notes (stored in the local database; leave off if notes contain secrets)
            </label>
          </div>
          <div className="form-section">
            <h3>Preview · {ready.length} selected</h3>
            <div className="import-actions">
              <button type="button" className="btn btn-sm" onClick={() => setSelected(new Set(candidates.flatMap((candidate, index) => candidate.input ? [index] : [])))}>Select valid</button>
              <button type="button" className="btn btn-sm" onClick={() => setSelected(new Set())}>Clear</button>
            </div>
            <div className="import-preview">
              {candidates.map((candidate, index) => (
                <label className="import-row" key={index} data-invalid={Boolean(candidate.issue)}>
                  <input type="checkbox" disabled={Boolean(candidate.issue)} checked={Boolean(candidate.input) && selected.has(index)} onChange={(event) => setSelected((current) => {
                    const next = new Set(current);
                    if (event.target.checked) next.add(index); else next.delete(index);
                    return next;
                  })} />
                  <span className="import-row-main">
                    <strong>{candidate.label || `Row ${candidate.rowNumber}`}</strong>
                    <small>{candidate.username && candidate.hostname ? `${candidate.username}@${candidate.hostname}` : 'No SSH target'}</small>
                  </span>
                  <span className="import-row-state">{candidate.issue ?? (candidate.authMethod === 'private_key' ? 'Local key path' : candidate.hasSecret ? 'Password ready' : 'No saved password')}</span>
                </label>
              ))}
            </div>
          </div>
        </>
      )}
    </Drawer>
  );
}
