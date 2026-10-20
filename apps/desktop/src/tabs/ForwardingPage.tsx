import { useEffect, useState } from 'react';
import { describeError, hostKeyProblem, type HostKeyProblem, type HostView } from '../ipc';
import {
  bytes,
  forwardWatch,
  forwardSave,
  forwardStart,
  forwardStop,
  forwardDelete,
  type ForwardProfile,
  type ForwardView,
} from '../network';
import { useSshHosts } from './useSshHosts';
import { Drawer, Dialog } from '../ui/Overlay';
import { HostKeyDialog } from '../ui/HostKeyDialog';
import { Field } from '../ui/Field';
import { ThemedSelect } from '../ui/ThemedSelect';
import { PlusIcon, EditIcon, TrashIcon, ForwardIcon, PlayIcon, StopIcon, CopyIcon } from '../ui/icons';

const MODE_INFO = {
  local: ['Local', 'Open a local port that reaches a service through SSH.'],
  remote: ['Remote', 'Open a port on the server that reaches this computer.'],
  dynamic: ['Dynamic', 'Use the SSH server as a SOCKS5 TCP proxy.'],
} as const;
function endpoint(host: string, port: number | string) {
  return `${host.includes(':') ? `[${host}]` : host}:${port}`;
}
export function ForwardingPage({ visible }: { visible: boolean }) {
  const { hosts, error: hostError } = useSshHosts(visible);
  const [items, setItems] = useState<ForwardView[]>([]);
  const [editor, setEditor] = useState<ForwardProfile | 'new' | null>(null);
  const [deleting, setDeleting] = useState<ForwardView | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [trust, setTrust] = useState<{ view: ForwardView; problem: HostKeyProblem } | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const upsert = (view: ForwardView) =>
    setItems((rows) =>
      rows.some((v) => v.profile.id === view.profile.id)
        ? rows.map((v) => (v.profile.id === view.profile.id ? view : v))
        : [view, ...rows],
    );
  useEffect(() => {
    if (!visible) return;
    let live = true;
    forwardWatch((view) => {
      if (live)
        setItems((rows) =>
          rows.some((v) => v.profile.id === view.profile.id)
            ? rows.map((v) => (v.profile.id === view.profile.id ? view : v))
            : [view, ...rows],
        );
    })
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
    if (!visible) setEditor(null);
  }, [visible]);
  useEffect(() => {
    if (!copied) return;
    const id = window.setTimeout(() => setCopied(null), 2000);
    return () => window.clearTimeout(id);
  }, [copied]);
  async function start(view: ForwardView) {
    setPending(view.profile.id);
    setError(null);
    try {
      // The event channel owns status ordering, including a disconnect that
      // arrives before the invoke response reaches the webview.
      await forwardStart(view.profile.id, upsert);
    } catch (e) {
      const problem = hostKeyProblem(e);
      if (problem) setTrust({ view, problem });
      else setError(describeError(e));
    } finally {
      setPending(null);
    }
  }
  async function stop(view: ForwardView) {
    try {
      await forwardStop(view.profile.id);
    } catch (e) {
      setError(describeError(e));
    }
  }
  async function remove() {
    if (!deleting) return;
    try {
      await forwardDelete(deleting.profile.id);
      setItems((rows) => rows.filter((v) => v.profile.id !== deleting.profile.id));
      setDeleting(null);
    } catch (e) {
      setError(describeError(e));
    }
  }
  return (
    <section className="network-page">
      <header className="page-head">
        <div>
          <p className="eyebrow">NETWORK</p>
          <h1>
            Port forwarding{' '}
            <span className="count">{items.filter((v) => v.status === 'running').length} active</span>
          </h1>
          <p className="muted">Reach services through encrypted SSH tunnels.</p>
        </div>
        <button className="btn btn-primary" disabled={!hosts.length} onClick={() => setEditor('new')}>
          <PlusIcon /> New tunnel
        </button>
      </header>
      {(error || hostError) && (
        <p className="banner" data-tone="danger" role="alert">
          {error || hostError}
        </p>
      )}
      <div className="tunnel-list">
        {items.map((view) => {
          const p = view.profile;
          const host = hosts.find((h) => h.id === p.hostId);
          const running = view.status === 'running';
          const active = running || view.status === 'connecting';
          return (
            <article className="network-card tunnel-card" key={p.id} aria-label={p.name}>
              <div className="tunnel-card-head">
                <h2 title={p.name}>{p.name}</h2>
                <span className="tunnel-mode">{MODE_INFO[p.mode][0]}</span>
                <span className="network-status" data-state={view.status}>
                  <i />
                  {view.status === 'connecting' ? 'Connecting…' : view.status}
                </span>
                <span className="tunnel-via" title={`SSH profile: ${host?.label ?? 'Unavailable'}`}>
                  via {host?.label ?? 'Unavailable SSH profile'}
                </span>
              </div>
              <div className="tunnel-route">
                <span className="tunnel-endpoint">
                  <span>{p.mode === 'remote' ? 'Server' : 'Local'}</span>
                  <code title={`Listening on ${p.mode === 'remote' ? (host?.label ?? 'remote server') : 'this computer'}`}>
                    {endpoint(p.bindAddress, running ? view.actualPort : p.bindPort || 'auto')}
                  </code>
                </span>
                <span className="tunnel-arrow" aria-hidden="true">→</span>
                <span className="tunnel-endpoint">
                  <span>{p.mode === 'dynamic' ? 'SOCKS5' : p.mode === 'local' ? 'Remote' : 'Local'}</span>
                  <code title={p.mode === 'dynamic' ? 'Remote DNS · TCP CONNECT' : `Destination reached from ${p.mode === 'local' ? 'the SSH server' : 'this computer'}`}>
                    {p.mode === 'dynamic' ? 'Any TCP destination' : endpoint(p.targetHost, p.targetPort)}
                  </code>
                </span>
              </div>
              {view.error && (
                <p className="tunnel-error" role="alert">
                  {view.error}
                </p>
              )}
              <div className="tunnel-metrics">
                <span>
                  <strong>{view.connections}</strong> connections
                </span>
                <span title="Sent">↑ {bytes(view.sent)}</span>
                <span title="Received">↓ {bytes(view.received)}</span>
                {view.connectionErrors > 0 && (
                  <span data-tone="warning">{view.connectionErrors} connection errors</span>
                )}
              </div>
              <div className="tunnel-actions">
                {running && (
                  <button
                    className="icon-btn"
                    aria-label={`Copy endpoint for ${p.name}`}
                    title={copied === p.id ? 'Copied' : 'Copy endpoint'}
                    onClick={() => {
                      void navigator.clipboard
                        .writeText(`${p.mode === 'dynamic' ? 'socks5h://' : ''}${endpoint(p.bindAddress, view.actualPort)}`)
                        .then(() => setCopied(p.id))
                        .catch((e) => setError(describeError(e)));
                    }}
                  >
                    <CopyIcon />
                  </button>
                )}
                <button
                  className="icon-btn"
                  aria-label={`Edit ${p.name}`}
                  title="Edit tunnel"
                  disabled={active}
                  onClick={() => setEditor(p)}
                >
                  <EditIcon />
                </button>
                <button
                  className="icon-btn"
                  aria-label={`Delete ${p.name}`}
                  title="Delete tunnel"
                  disabled={active}
                  onClick={() => setDeleting(view)}
                >
                  <TrashIcon />
                </button>
                {active ? (
                  <button className="btn" onClick={() => void stop(view)}>
                    <StopIcon /> Stop
                  </button>
                ) : (
                  <button
                    className="btn btn-primary"
                    disabled={pending === p.id || !host}
                    onClick={() => void start(view)}
                  >
                    <PlayIcon /> Start
                  </button>
                )}
              </div>
            </article>
          );
        })}
      </div>
      {!items.length && (
        <div className="network-empty">
          <ForwardIcon size={34} />
          <h2>A tunnel for every service</h2>
          <p>
            Connect to a remote database, expose a local development service,
            <br />
            or route an application through a SOCKS5 tunnel.
          </p>
          {!hosts.length && <p>Add an SSH profile in Hosts to get started.</p>}
          <button className="btn" disabled={!hosts.length} onClick={() => setEditor('new')}>
            <PlusIcon /> Create a tunnel
          </button>
        </div>
      )}
      {editor && (
        <ForwardEditor
          profile={editor === 'new' ? null : editor}
          hosts={hosts}
          onClose={() => setEditor(null)}
          onSaved={(view, run) => {
            upsert(view);
            setEditor(null);
            if (run) void start(view);
          }}
        />
      )}
      {deleting && (
        <Dialog
          title={`Delete ${deleting.profile.name}?`}
          onClose={() => setDeleting(null)}
          footer={
            <>
              <button className="btn" onClick={() => setDeleting(null)}>
                Cancel
              </button>
              <button className="btn btn-danger" onClick={() => void remove()}>
                Delete tunnel
              </button>
            </>
          }
        >
          <p>The saved forwarding configuration will be removed.</p>
          {error && <p role="alert">{error}</p>}
        </Dialog>
      )}
      {trust && (
        <HostKeyDialog
          hostId={trust.view.profile.hostId}
          hostLabel={hosts.find((h) => h.id === trust.view.profile.hostId)?.label ?? 'SSH server'}
          address={hosts.find((h) => h.id === trust.view.profile.hostId)?.hostname ?? ''}
          problem={trust.problem}
          onCancel={() => setTrust(null)}
          onTrusted={() => {
            const v = trust.view;
            setTrust(null);
            void start(v);
          }}
        />
      )}
    </section>
  );
}
function ForwardEditor({
  profile,
  hosts,
  onClose,
  onSaved,
}: {
  profile: ForwardProfile | null;
  hosts: HostView[];
  onClose: () => void;
  onSaved: (v: ForwardView, start: boolean) => void;
}) {
  const [form, setForm] = useState<ForwardProfile>(
    profile ?? {
      id: '',
      name: '',
      hostId: hosts[0]?.id ?? '',
      mode: 'local',
      bindAddress: '127.0.0.1',
      bindPort: 8080,
      targetHost: '127.0.0.1',
      targetPort: 80,
    },
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function save(start: boolean) {
    setBusy(true);
    setError(null);
    try {
      onSaved(await forwardSave(form), start);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Drawer
      title={profile ? 'Edit tunnel' : 'New tunnel'}
      subtitle="Configure once, start when you need it"
      onClose={() => !busy && onClose()}
      footer={
        <>
          <button className="btn btn-ghost" disabled={busy} onClick={onClose}>
            Cancel
          </button>
          <span className="spacer" />
          <button form="forward-editor" type="submit" className="btn" disabled={busy}>
            {busy ? 'Saving…' : 'Save'}
          </button>
          <button
            className="btn btn-primary"
            disabled={busy}
            onClick={() => {
              const el = document.getElementById('forward-editor') as HTMLFormElement;
              if (el.reportValidity()) void save(true);
            }}
          >
            Save & start
          </button>
        </>
      }
    >
      <form
        id="forward-editor"
        onSubmit={(e) => {
          e.preventDefault();
          void save(false);
        }}
      >
        {error && (
          <p className="banner" data-tone="danger" role="alert">
            {error}
          </p>
        )}
        <Field label="Name">
          <input
            required
            autoFocus
            maxLength={128}
            placeholder="Production database"
            value={form.name}
            onChange={(e) => setForm({ ...form, name: e.target.value })}
          />
        </Field>
        <Field label="SSH server">
          <ThemedSelect label="SSH server" required value={form.hostId} options={hosts.map((h) => ({ value: h.id, label: `${h.label} · ${h.username}@${h.hostname}` }))}
            onChange={(value) => setForm({ ...form, hostId: value })} />
        </Field>
        <fieldset className="mode-picker">
          <legend>Forwarding mode</legend>
          {Object.entries(MODE_INFO).map(([key, [label, hint]]) => (
            <label key={key} data-selected={form.mode === key}>
              <input
                type="radio"
                name="forward-mode"
                value={key}
                checked={form.mode === key}
                onChange={() => setForm({ ...form, mode: key as ForwardProfile['mode'] })}
              />
              <strong>{label}</strong>
              <span>{hint}</span>
            </label>
          ))}
        </fieldset>
        <div className="form-section">
          <h3>{form.mode === 'remote' ? 'Listen on the SSH server' : 'Listen on this computer'}</h3>
          <div className="field-row">
            <Field label="Bind address">
              <input
                required
                value={form.bindAddress}
                onChange={(e) => setForm({ ...form, bindAddress: e.target.value })}
              />
            </Field>
            <Field label="Listen port" hint="0 selects a free port.">
              <input
                required
                type="number"
                min={0}
                max={65535}
                value={form.bindPort}
                onChange={(e) => setForm({ ...form, bindPort: Number(e.target.value) })}
              />
            </Field>
          </div>
          {!['127.0.0.1', 'localhost', '::1'].includes(form.bindAddress) && (
            <p className="hint" data-tone="warning">
              This bind address may make the listening port accessible to other devices.
              {form.mode === 'dynamic' ? ' The SOCKS5 listener has no authentication.' : ''}
            </p>
          )}
          {form.mode === 'remote' && (
            <p className="hint">
              The SSH server must allow TCP forwarding. External bind addresses also require GatewayPorts on
              the server.
            </p>
          )}
        </div>
        {form.mode !== 'dynamic' && (
          <div className="form-section">
            <h3>
              {form.mode === 'local'
                ? 'Destination reached from the SSH server'
                : 'Destination reached from this computer'}
            </h3>
            <div className="field-row">
              <Field label="Destination host">
                <input
                  required
                  placeholder="127.0.0.1"
                  value={form.targetHost}
                  onChange={(e) => setForm({ ...form, targetHost: e.target.value })}
                />
              </Field>
              <Field label="Destination port">
                <input
                  required
                  type="number"
                  min={1}
                  max={65535}
                  value={form.targetPort}
                  onChange={(e) => setForm({ ...form, targetPort: Number(e.target.value) })}
                />
              </Field>
            </div>
          </div>
        )}
        <div className="network-explainer">
          <ForwardIcon size={20} />
          <p>
            {MODE_INFO[form.mode][1]}{' '}
            {form.mode === 'dynamic'
              ? 'Configure your application to use socks5h://127.0.0.1:port for remote DNS.'
              : 'Connections use the credentials and proxy saved in the SSH profile.'}
          </p>
        </div>
      </form>
    </Drawer>
  );
}
