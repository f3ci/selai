import { useEffect, useState, type FormEvent } from 'react';
import {
  describeError,
  groupList,
  hostForgetSudoPassword,
  hostRevealPassword,
  hostSave,
  sshKeyList,
  type AuthMethod,
  type HostInput,
  type HostView,
  type GroupView,
  type SshKeyView,
} from '../ipc';
import { Field } from '../ui/Field';
import { Drawer } from '../ui/Overlay';
import { PasswordInput } from '../ui/PasswordInput';
import { ChevronIcon, KeyIcon } from '../ui/icons';
import { ThemedSelect } from '../ui/ThemedSelect';
import { proxyList, type ProxyProfile } from '../network';

interface Props {
  host: HostView | null;
  onCancel: () => void;
  /** `connect` is true when the operator chose "Save & connect". */
  onSaved: (host: HostView, connect: boolean) => void;
  onManageKeys?: () => void;
  onManageProxies?: () => void;
  suspended?: boolean;
}

interface FormState {
  label: string;
  groupId: string;
  proxyId: string;
  hostname: string;
  port: string;
  username: string;
  authMethod: AuthMethod;
  privateKeyPath: string;
  connectTimeoutSeconds: string;
  keepAliveSeconds: string;
  terminalType: string;
  notes: string;
}

function initial(host: HostView | null): FormState {
  return {
    label: host?.label ?? '',
    groupId: host?.groupId ?? '',
    proxyId: host?.proxyId ?? '',
    hostname: host?.hostname ?? '',
    port: String(host?.port ?? 22),
    username: host?.username ?? '',
    authMethod: host?.authMethod ?? 'password',
    privateKeyPath: host?.privateKeyPath ?? '',
    connectTimeoutSeconds: String(host?.connectTimeoutSeconds ?? 15),
    keepAliveSeconds: String(host?.keepAliveSeconds ?? 30),
    terminalType: host?.terminalType ?? 'xterm-256color',
    notes: host?.notes ?? '',
  };
}

/** Client-side checks for quick feedback. The Rust core validates again. */
function validate(form: FormState): string | null {
  if (!form.label.trim()) return 'Label is required.';
  if (!form.hostname.trim()) return 'Host name or IP is required.';
  if (!form.username.trim()) return 'Username is required.';
  const port = Number(form.port);
  if (!Number.isInteger(port) || port < 1 || port > 65535) return 'Port must be 1 to 65535.';
  if ((form.authMethod === 'private_key' || form.authMethod === 'certificate') && !form.privateKeyPath.trim()) {
    return 'Choose an SSH key for key authentication.';
  }
  return null;
}

export function HostForm({ host, onCancel, onSaved, onManageKeys, onManageProxies, suspended = false }: Props) {
  const [form, setForm] = useState<FormState>(() => initial(host));
  // Secrets live only in this component's local state until submit. They are
  // cleared right after a save. Saved values are revealed only on demand.
  const [password, setPassword] = useState('');
  const [passphrase, setPassphrase] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [sudoNote, setSudoNote] = useState<string | null>(null);
  const [keys, setKeys] = useState<SshKeyView[]>([]);
  const [keyError, setKeyError] = useState<string | null>(null);
  const [keysLoading, setKeysLoading] = useState(false);
  const [groups, setGroups] = useState<GroupView[]>([]);
  const [proxyEnabled, setProxyEnabled] = useState(Boolean(host?.proxyId));
  const [proxies, setProxies] = useState<ProxyProfile[]>([]);
  const [proxyError, setProxyError] = useState<string | null>(null);
  useEffect(() => { if (suspended || !proxyEnabled) return; let live = true; proxyList().then((rows) => { if (live) { setProxies(rows); setProxyError(null); } }).catch((e) => { if (live) setProxyError(describeError(e)); }); return () => { live = false; }; }, [suspended, proxyEnabled]);


  useEffect(() => {
    if (suspended || form.authMethod !== 'private_key') return;
    let active = true;
    setKeysLoading(true);
    setKeyError(null);
    sshKeyList().then((items) => { if (active) setKeys(items); })
      .catch((err) => { if (active) setKeyError(describeError(err)); })
      .finally(() => { if (active) setKeysLoading(false); });
    return () => { active = false; };
  }, [suspended, form.authMethod]);
  useEffect(() => {
    let active = true;
    groupList().then((items) => { if (active) setGroups(items); })
      .catch((err) => { if (active) setError(describeError(err)); });
    return () => { active = false; };
  }, []);

  async function forgetSudo() {
    if (!host) return;
    try {
      await hostForgetSudoPassword(host.id);
      setSudoNote('No sudo password is saved for this host now.');
    } catch (err) {
      setSudoNote(describeError(err));
    }
  }

  const set = (key: keyof FormState) => (value: string) =>
    setForm((prev) => ({ ...prev, [key]: value }));

  async function save(connect: boolean) {
    if (proxyEnabled && !form.proxyId) { setError('Choose a proxy profile or disable the proxy.'); return; }
    const problem = validate(form);
    if (problem) {
      setError(problem);
      return;
    }
    const input: HostInput = {
      proxyId: proxyEnabled ? form.proxyId : null,
      id: host?.id,
      expectedUpdatedAtMs: host?.updatedAtMs,
      label: form.label.trim(),
      groupId: form.groupId || null,
      hostname: form.hostname.trim(),
      port: Number(form.port),
      username: form.username.trim(),
      authMethod: form.authMethod,
      privateKeyPath: form.authMethod !== 'password' && form.privateKeyPath.trim() ? form.privateKeyPath.trim() : null,
      connectTimeoutSeconds: Number(form.connectTimeoutSeconds),
      keepAliveSeconds: Number(form.keepAliveSeconds),
      terminalType: form.terminalType.trim(),
      notes: form.notes.trim() ? form.notes : null,
      password: form.authMethod === 'password' && password ? password : undefined,
      keyPassphrase: form.authMethod === 'private_key' && passphrase ? passphrase : undefined,
    };
    setBusy(true);
    try {
      const saved = await hostSave(input);
      setPassword('');
      setPassphrase('');
      onSaved(saved, connect);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    void save(false);
  }

  const hasSavedPassword = host?.authMethod === 'password' && host.credentialConfigured;
  const savedPlaceholder = hasSavedPassword ? '•••• saved (leave blank to keep)' : '';
  const formId = 'host-form';

  // Keep the draft in component state while the operator visits SSH Keys.
  if (suspended) return null;

  return (
    <Drawer
      title={host ? `Edit ${host.label}` : 'New host'}
      subtitle={host ? `${host.username}@${host.hostname}` : 'The server key is confirmed on first connect.'}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn btn-ghost" onClick={onCancel}>
            Cancel
          </button>
          <span className="spacer" />
          <button type="submit" form={formId} className="btn" disabled={busy}>
            {busy ? 'Saving…' : 'Save'}
          </button>
          <button type="button" className="btn btn-primary" disabled={busy} onClick={() => void save(true)}>
            Save & connect
          </button>
        </>
      }
    >
      <form id={formId} aria-label={host ? 'Edit host' : 'New host'} onSubmit={submit}>
        {error && (
          <p role="alert" className="banner" data-tone="danger" style={{ marginBottom: '1rem' }}>
            {error}
          </p>
        )}
        <div className="form-section">
          <h3>Server</h3>
          <Field label="Label" hint="Spaces are fine. Mention it as @Label in the AI Agent.">
            <input
              value={form.label}
              placeholder="Production Web"
              onChange={(e) => set('label')(e.target.value)}
              autoFocus
            />
          </Field>
          <Field label="Group" hint="Create groups with + beside SSH hosts in the sidebar. You can also drag profiles into groups.">
            <ThemedSelect label="Group" value={form.groupId} options={[{ value: '', label: 'Ungrouped' }, ...groups.map((group) => ({ value: group.id, label: group.name }))]} onChange={(value) => set('groupId')(value)} />
          </Field>
          <div className="field-row">
            <label className="field">
              Host name or IP
              <input value={form.hostname} placeholder="10.0.0.5" onChange={(e) => set('hostname')(e.target.value)} />
            </label>
            <label className="field">
              Port
              <input inputMode="numeric" value={form.port} onChange={(e) => set('port')(e.target.value)} />
            </label>
          </div>
        </div>

        <div className="form-section">
          <h3>Connection route</h3>
          <label className="check"><input type="checkbox" checked={proxyEnabled} onChange={(e) => setProxyEnabled(e.target.checked)} />Connect through a proxy</label>{proxyEnabled && <>
            <Field label="Proxy profile" hint="Used by terminal, AI Assistant, SFTP, and port forwarding."><ThemedSelect label="Proxy profile" value={form.proxyId} options={[{ value: '', label: 'Choose proxy' }, ...proxies.map((proxy) => ({ value: proxy.id, label: `${proxy.name} / ${proxy.kind === 'http' ? 'HTTP CONNECT' : 'SOCKS5'}` }))]} onChange={(value) => set('proxyId')(value)} /></Field>
            {proxyError && <p role="alert" className="banner" data-tone="danger">{proxyError}</p>}
            {onManageProxies && <button type="button" className="btn btn-sm" onClick={onManageProxies}>{proxies.length ? 'Manage proxies' : 'Add a proxy'}<ChevronIcon size={14} /></button>}
          </>}
        </div><div className="form-section"><h3>Credentials</h3>
          <label className="field">
            Username
            <input value={form.username} placeholder="root" onChange={(e) => set('username')(e.target.value)} />
          </label>
          <div className="field">
            <span id="auth-method">Authentication</span>
            <div className="segmented" role="group" aria-labelledby="auth-method">
              <button
                type="button"
                aria-pressed={form.authMethod === 'password'}
                onClick={() => set('authMethod')('password')}
              >
                Password
              </button>
              <button
                type="button"
                aria-pressed={form.authMethod === 'private_key'}
                onClick={() => set('authMethod')('private_key')}
              >
                Private key
              </button>
            </div>
          </div>
          <Field label="Additional authentication methods">
            <ThemedSelect label="Additional authentication methods" value={['password', 'private_key'].includes(form.authMethod) ? '' : form.authMethod}
              onChange={(value) => { if (value) { setForm((previous) => ({ ...previous, authMethod: value as AuthMethod, privateKeyPath: '' })); } }}
              options={[{ value: '', label: 'Password or private key above' },
                { value: 'auto', label: 'Automatic / multi-factor (OpenSSH)' },
                { value: 'keyboard_interactive', label: 'Keyboard-interactive / OTP' },
                { value: 'agent', label: 'SSH agent' },
                { value: 'certificate', label: 'OpenSSH user certificate' },
                { value: 'security_key', label: 'FIDO2 / security key' },
                { value: 'smart_card', label: 'Smart card / PKCS#11' },
                { value: 'gssapi', label: 'Kerberos / GSSAPI' },
                { value: 'host_based', label: 'Host-based authentication' }]} />
          </Field>
          {form.authMethod !== 'password' && form.authMethod !== 'private_key' ? (
            <>
              <p className="hint">Uses your system OpenSSH client and SSH configuration. Passwords, OTPs and PINs are requested when connecting. Server-required MFA steps are supported.</p>
              {form.authMethod === 'agent' && <p className="hint">Start your SSH agent and load the required key. Agent-backed certificates and hardware keys can also be used.</p>}
              {form.authMethod === 'gssapi' && <p className="hint">Requires a GSSAPI-capable OpenSSH build and a valid Kerberos ticket. Use the server DNS name that matches its service principal.</p>}
              {form.authMethod === 'host_based' && <p className="hint">Requires ssh-keysign, local host keys and server-side host-based trust.</p>}
              {form.authMethod === 'security_key' && <p className="hint">Requires a FIDO-capable OpenSSH build and the token. Load its key into your agent or select the local security-key identity file below.</p>}
              {form.authMethod === 'certificate' && <p className="hint">Enter the local private-key path. OpenSSH reads the matching certificate from &lt;key-path&gt;-cert.pub, or CertificateFile in your SSH config.</p>}
              {['auto', 'certificate', 'security_key', 'smart_card'].includes(form.authMethod) && <Field label={form.authMethod === 'smart_card' ? 'PKCS#11 provider path (optional)' : 'Local identity file (optional except for certificates)'}>
                <input value={form.privateKeyPath} onChange={(e) => set('privateKeyPath')(e.target.value)} placeholder={form.authMethod === 'smart_card' ? 'Provider DLL / shared library, or configure PKCS11Provider' : 'Local file path, or use IdentityFile in SSH config'} />
              </Field>}
            </>
          ) : form.authMethod === 'password' ? (
            <Field label="Password" hint={hasSavedPassword ? 'Saved in your encrypted vault. Click the eye to reveal it; leave blank to keep it.' : 'Stored inside your encrypted vault.'}>
              <PasswordInput
                value={password}
                placeholder={savedPlaceholder}
                disabled={busy}
                onChange={setPassword}
                loadSaved={hasSavedPassword && host ? () => hostRevealPassword(host.id) : undefined}
              />
            </Field>
          ) : (
            <>
              <Field label="SSH key" hint="Select a key from your encrypted vault.">
                <ThemedSelect label="SSH key" value={form.privateKeyPath} disabled={keysLoading}
                  options={[{ value: '', label: keysLoading ? 'Loading keys…' : 'Choose an SSH key…' },
                    ...(form.privateKeyPath && !keys.some((key) => form.privateKeyPath === `vault://key:${key.id}`) ? [{ value: form.privateKeyPath, label: form.privateKeyPath.startsWith('vault://key:') ? 'Previously selected key (unavailable)' : form.privateKeyPath.startsWith('vault://') ? 'Existing key saved for this host' : 'Existing key file' }] : []),
                    ...keys.map((key) => ({ value: `vault://key:${key.id}`, label: `${key.name} · ${key.algorithm}` }))]}
                  onChange={(value) => set('privateKeyPath')(value)} />
              </Field>
              {keyError && <p role="alert" className="banner" data-tone="danger">{keyError}</p>}
              {!keysLoading && !keyError && keys.length === 0 && <p className="hint">No managed SSH keys yet. Generate or import one in SSH Keys, then return to this profile.</p>}
              {form.privateKeyPath && !form.privateKeyPath.startsWith('vault://') && <p className="hint">This profile uses an existing key file. Import it in SSH Keys to reuse it across profiles.</p>}
              {onManageKeys && <button className="btn btn-sm" type="button" onClick={onManageKeys}>
                <KeyIcon size={14} /> {keys.length === 0 ? 'Add SSH key' : 'Manage SSH keys'}
              </button>}

            </>
          )}
        </div>

        <div className="form-section">
          <details className="advanced">
            <summary>
              <ChevronIcon size={14} /> Advanced
            </summary>
            <div className="form-section">
              <div className="field-grid">
                <label className="field">
                  Connect timeout (seconds)
                  <input
                    value={form.connectTimeoutSeconds}
                    onChange={(e) => set('connectTimeoutSeconds')(e.target.value)}
                  />
                </label>
                <label className="field">
                  Keepalive (seconds, 0 disables)
                  <input value={form.keepAliveSeconds} onChange={(e) => set('keepAliveSeconds')(e.target.value)} />
                </label>
              </div>
              <label className="field">
                Terminal type
                <input value={form.terminalType} onChange={(e) => set('terminalType')(e.target.value)} />
              </label>
              <label className="field">
                Notes
                <textarea rows={3} value={form.notes} onChange={(e) => set('notes')(e.target.value)} />
              </label>
              {host && (
                <div className="field">
                  <span>Sudo password</span>
                  <span className="hint">
                    Saved only when you tick "Remember for this host" while approving a root command.
                  </span>
                  <div>
                    <button type="button" className="btn btn-sm" onClick={() => void forgetSudo()}>
                      Forget saved sudo password
                    </button>
                  </div>
                  {sudoNote && (
                    <span role="status" className="hint">
                      {sudoNote}
                    </span>
                  )}
                </div>
              )}
            </div>
          </details>
        </div>
      </form>
    </Drawer>
  );
}
