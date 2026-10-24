import { useEffect, useState } from 'react';
import { describeError } from '../ipc';
import { proxyList, proxySave, proxyDelete, type ProxyProfile, type ProxyInput } from '../network';
import { Field } from '../ui/Field';
import { ThemedSelect } from '../ui/ThemedSelect';
import { PasswordInput } from '../ui/PasswordInput';
import { Drawer, Dialog } from '../ui/Overlay';
import { PlusIcon, EditIcon, TrashIcon, ProxyIcon, SearchIcon } from '../ui/icons';

export function ProxiesPage({
  visible,
  onReturnToProfile,
}: {
  visible: boolean;
  onReturnToProfile?: () => void;
}) {
  const [items, setItems] = useState<ProxyProfile[]>([]);
  const [editing, setEditing] = useState<ProxyProfile | 'new' | null>(null);
  const [deleting, setDeleting] = useState<ProxyProfile | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState('');
  useEffect(() => {
    if (!visible) return;
    let live = true;
    proxyList()
      .then((rows) => {
        if (live) setItems(rows);
      })
      .catch((e) => {
        if (live) setError(describeError(e));
      });
    return () => {
      live = false;
    };
  }, [visible]);
  useEffect(() => {
    if (!visible) {
      setEditing(null);
      setDeleting(null);
    }
  }, [visible]);
  async function remove() {
    if (!deleting) return;
    setBusy(true);
    setError(null);
    try {
      await proxyDelete(deleting.id);
      setItems((rows) => rows.filter((p) => p.id !== deleting.id));
      setDeleting(null);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="network-page">
      {onReturnToProfile && (
        <button className="btn btn-sm" onClick={onReturnToProfile}>
          Back to SSH profile
        </button>
      )}
      <header className="page-head">
        <div>
          <p className="eyebrow">NETWORK</p>
          <h1>
            Proxies <span className="count">{items.length}</span>
          </h1>
          <p className="muted">Reusable routes for your SSH connections.</p>
        </div>
        <button className="btn btn-primary" onClick={() => setEditing('new')}>
          <PlusIcon /> New proxy
        </button>
      </header>
      <div className="network-toolbar">
        <label className="search">
          <SearchIcon />
          <input
            aria-label="Search proxies"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search proxies"
          />
        </label>
      </div>
      {error && !deleting && (
        <p className="banner" data-tone="danger" role="alert">
          {error}
        </p>
      )}
      <div className="network-cards">
        {items
          .filter((p) => `${p.name} ${p.hostname}`.toLowerCase().includes(query.toLowerCase()))
          .map((proxy) => (
            <article className="network-card" key={proxy.id}>
              <div className="network-card-head">
                <span className="network-symbol">
                  <ProxyIcon size={22} />
                </span>
                <div className="grow">
                  <h2>{proxy.name}</h2>
                  <p className="muted mono">
                    {proxy.hostname}:{proxy.port}
                  </p>
                </div>
                <span className="badge">{proxy.kind === 'http' ? 'HTTP CONNECT' : 'SOCKS5'}</span>
                <button
                  className="icon-btn"
                  aria-label={`Edit ${proxy.name}`}
                  onClick={() => setEditing(proxy)}
                >
                  <EditIcon />
                </button>
                <button
                  className="icon-btn"
                  aria-label={`Delete ${proxy.name}`}
                  onClick={() => {
                    setError(null);
                    setDeleting(proxy);
                  }}
                >
                  <TrashIcon />
                </button>
              </div>
              <div className="network-card-foot">
                <span>{proxy.username ? `Authentication · ${proxy.username}` : 'No authentication'}</span>
                <span>Available in SSH profiles</span>
              </div>
            </article>
          ))}
      </div>
      {!items.length && (
        <div className="network-empty">
          <ProxyIcon size={34} />
          <h2>Choose a route for SSH</h2>
          <p>
            Add an HTTP CONNECT or SOCKS5 proxy, then enable it in an SSH profile.
            <br />
            Terminal, AI Assistant, SFTP, and port forwarding use the same route.
          </p>
          <button className="btn" onClick={() => setEditing('new')}>
            <PlusIcon /> Add your first proxy
          </button>
        </div>
      )}
      {editing && (
        <ProxyEditor
          profile={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)}
          onSaved={(item) => {
            setItems((rows) => [item, ...rows.filter((p) => p.id !== item.id)]);
            setEditing(null);
          }}
        />
      )}
      {deleting && (
        <Dialog
          title={`Delete ${deleting.name}?`}
          tone="danger" icon={<TrashIcon size={20} />} closeDisabled={busy}
          onClose={() => !busy && setDeleting(null)}
          footer={
            <>
              <button className="btn" data-autofocus disabled={busy} onClick={() => setDeleting(null)}>
                Cancel
              </button>
              <button className="btn btn-danger" disabled={busy} onClick={() => void remove()}>
                {busy ? 'Deleting…' : 'Delete proxy'}
              </button>
            </>
          }
        >
          <p>Proxies assigned to SSH profiles must be removed from those profiles first.</p>
          {error && (
            <p role="alert" className="banner" data-tone="danger">
              {error}
            </p>
          )}
        </Dialog>
      )}
    </section>
  );
}
function ProxyEditor({
  profile,
  onClose,
  onSaved,
}: {
  profile: ProxyProfile | null;
  onClose: () => void;
  onSaved: (p: ProxyProfile) => void;
}) {
  const [form, setForm] = useState<ProxyInput>({
    id: profile?.id,
    name: profile?.name ?? '',
    kind: profile?.kind ?? 'socks5',
    hostname: profile?.hostname ?? '',
    port: profile?.port ?? 1080,
    username: profile?.username ?? '',
    password: '',
  });
  const [auth, setAuth] = useState(Boolean(profile?.username));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function save() {
    setBusy(true);
    setError(null);
    try {
      const p = await proxySave({
        ...form,
        username: auth ? form.username : null,
        password: auth ? form.password : undefined,
        clearPassword: !auth || form.clearPassword,
      });
      setForm((f) => ({ ...f, password: '' }));
      onSaved(p);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Drawer
      title={profile ? 'Edit proxy' : 'New proxy'}
      subtitle="A shared route for SSH traffic"
      onClose={() => !busy && onClose()}
      footer={
        <>
          <button className="btn btn-ghost" disabled={busy} onClick={onClose}>
            Cancel
          </button>
          <span className="spacer" />
          <button type="submit" form="proxy-editor" className="btn btn-primary" disabled={busy}>
            {busy ? 'Saving…' : 'Save proxy'}
          </button>
        </>
      }
    >
      <form
        id="proxy-editor"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        {error && (
          <p role="alert" className="banner" data-tone="danger">
            {error}
          </p>
        )}
        <Field label="Name">
          <input
            autoFocus
            required
            maxLength={128}
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
            placeholder="Office gateway"
          />
        </Field>
        <Field label="Proxy protocol">
          <ThemedSelect label="Proxy protocol" value={form.kind} options={[{ value: 'socks5', label: 'SOCKS5 · remote DNS' }, { value: 'http', label: 'HTTP CONNECT' }]}
            onChange={(value) => setForm({ ...form, kind: value as ProxyInput['kind'], port: value === 'http' ? 8080 : 1080 })} />
        </Field>
        <div className="field-row">
          <Field label="Proxy host">
            <input
              required
              value={form.hostname}
              placeholder="proxy.example.com"
              onChange={(e) => setForm({ ...form, hostname: e.target.value })}
            />
          </Field>
          <Field label="Port">
            <input
              required
              type="number"
              min={1}
              max={65535}
              value={form.port}
              onChange={(e) => setForm({ ...form, port: Number(e.target.value) })}
            />
          </Field>
        </div>
        <label className="check">
          <input type="checkbox" checked={auth} onChange={(e) => setAuth(e.target.checked)} />
          Use proxy authentication
        </label>
        {auth && (
          <div className="form-section">
            <Field label="Username">
              <input
                required
                value={form.username ?? ''}
                onChange={(e) => setForm({ ...form, username: e.target.value })}
                autoComplete="off"
              />
            </Field>
            <Field
              label="Password"
              hint={
                profile?.passwordConfigured
                  ? 'A password is saved. Leave blank to keep it.'
                  : 'Stored in your encrypted vault.'
              }
            >
              <PasswordInput
                value={form.password ?? ''}
                onChange={(value) => setForm({ ...form, password: value })}
              />
            </Field>
            {profile?.passwordConfigured && (
              <label className="check">
                <input
                  type="checkbox"
                  checked={form.clearPassword ?? false}
                  onChange={(e) => setForm({ ...form, clearPassword: e.target.checked })}
                />
                Remove saved password
              </label>
            )}
          </div>
        )}
        <p className="hint">
          HTTP uses CONNECT tunneling with optional Basic authentication. SOCKS5 supports optional
          username/password. SSH remains encrypted end to end; proxy authentication uses the proxy's
          transport.
        </p>
      </form>
    </Drawer>
  );
}
