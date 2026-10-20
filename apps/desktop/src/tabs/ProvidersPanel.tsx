import { useEffect, useState, type FormEvent } from 'react';
import {
  describeError,
  providerDelete,
  providerList,
  providerModels,
  providerSave,
  providerTemplates,
  providerTest,
  type ProviderInput,
  type ProviderProtocolName,
  type ProviderTestView,
  type ProviderView,
  type TemplateView,
} from '../ipc';
import { Field } from '../ui/Field';
import { ThemedSelect } from '../ui/ThemedSelect';
import { Drawer } from '../ui/Overlay';
import { AlertIcon, BoltIcon, CheckIcon, CloseIcon, EditIcon, KeyIcon, PlugIcon, PlusIcon, TrashIcon } from '../ui/icons';

const PROTOCOLS: { value: ProviderProtocolName; label: string }[] = [
  { value: 'openai_chat', label: 'OpenAI-compatible Chat Completions' },
  { value: 'openai_responses', label: 'OpenAI Responses' },
  { value: 'anthropic_messages', label: 'Anthropic Messages' },
];

const STATUS_TEXT: Record<ProviderView['toolCalling']['status'], string> = {
  verified: 'Verified',
  failed: 'Test failed',
  not_verified: 'Not verified',
};

interface FormState {
  id?: string;
  expectedUpdatedAtMs?: number;
  name: string;
  protocol: ProviderProtocolName;
  baseUrl: string;
  modelId: string;
  timeoutSeconds: string;
  maxOutputTokens: string;
  keyConfigured: boolean;
  allowInsecureHttp: boolean;
  note: string;
}

const EMPTY: FormState = {
  name: '',
  protocol: 'openai_chat',
  baseUrl: '',
  modelId: '',
  timeoutSeconds: '120',
  maxOutputTokens: '4096',
  keyConfigured: false,
  allowInsecureHttp: false,
  note: '',
};

function fromProvider(p: ProviderView): FormState {
  return {
    id: p.id,
    expectedUpdatedAtMs: p.updatedAtMs,
    name: p.name,
    protocol: p.protocol,
    baseUrl: p.baseUrl,
    modelId: p.modelId,
    timeoutSeconds: String(p.timeoutSeconds),
    maxOutputTokens: String(p.maxOutputTokens),
    keyConfigured: p.keyConfigured,
    allowInsecureHttp: p.allowInsecureHttp,
    note: '',
  };
}

/** Plain http:// to anything but this machine. Rust makes the real decision. */
function isRemotePlainHttp(url: string): boolean {
  const m = /^http:\/\/(\[[^\]]+\]|[^/:?#]+)/i.exec(url.trim());
  if (!m) return false;
  const host = (m[1] ?? '').toLowerCase().replace(/^\[|\]$/g, '');
  return !(host === 'localhost' || host === '::1' || host.startsWith('127.'));
}

type Step = ProviderTestView | 'running' | 'skipped';

type KeyCheck =
  | { state: 'idle' }
  | { state: 'checking' }
  | { state: 'ok'; models: string[]; latencyMs: number }
  | { state: 'fail'; message: string };

interface TestRun {
  connectivity?: Step;
  toolCalling?: Step;
}

export const PROVIDERS_CHANGED = 'selai:providers-changed';

export function ProvidersPanel() {
  const [providers, setProviders] = useState<ProviderView[]>([]);
  const [templates, setTemplates] = useState<TemplateView[]>([]);
  const [form, setForm] = useState<FormState | null>(null);
  const [apiKey, setApiKey] = useState('');
  const [check, setCheck] = useState<KeyCheck>({ state: 'idle' });
  const [clearKey, setClearKey] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [runs, setRuns] = useState<Record<string, TestRun>>({});
  const [busy, setBusy] = useState(false);

  async function refresh() {
    try {
      setProviders((await providerList()).items);
      // Other panels (the AI composer) keep their own copy; let them reload.
      window.dispatchEvent(new Event(PROVIDERS_CHANGED));
    } catch (err) {
      setError(describeError(err));
    }
  }

  useEffect(() => {
    void refresh();
    providerTemplates()
      .then(setTemplates)
      .catch((err: unknown) => setError(describeError(err)));
  }, []);

  function openForm(state: FormState) {
    setForm(state);
    setApiKey('');
    setClearKey(false);
    setFormError(null);
    setCheck({ state: 'idle' });
  }

  /** Any change to where or how we connect makes an earlier check stale. */
  function changeEndpoint(next: FormState) {
    setForm(next);
    setCheck({ state: 'idle' });
  }

  async function checkKey() {
    if (!form) return;
    setCheck({ state: 'checking' });
    try {
      const result = await providerModels({
        id: form.id,
        protocol: form.protocol,
        baseUrl: form.baseUrl.trim(),
        allowInsecureHttp: isRemotePlainHttp(form.baseUrl) && form.allowInsecureHttp,
        apiKey: apiKey || undefined,
      });
      setCheck({ state: 'ok', models: result.models, latencyMs: result.latencyMs });
      // Keep the model used by the provider test if it is still available;
      // otherwise use the first model as the profile's tool-test model.
      setForm((prev) =>
        prev && result.models.length > 0 && !result.models.includes(prev.modelId)
          ? { ...prev, modelId: result.models[0] ?? prev.modelId }
          : prev,
      );
    } catch (err) {
      setCheck({ state: 'fail', message: describeError(err) });
    }
  }

  function applyTemplate(id: string) {
    const t = templates.find((x) => x.id === id);
    if (!t) return;
    setForm((prev) => ({
      ...(prev ?? EMPTY),
      protocol: t.protocol,
      baseUrl: t.defaultBaseUrl,
      note: t.note,
      name: prev?.name || t.displayName,
    }));
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!form) return;
    const remoteHttp = isRemotePlainHttp(form.baseUrl);
    const input: ProviderInput = {
      id: form.id,
      expectedUpdatedAtMs: form.expectedUpdatedAtMs,
      name: form.name.trim(),
      protocol: form.protocol,
      baseUrl: form.baseUrl.trim(),
      // The profile retains a model only as the target for its tool-calling
      // verification; ordinary chat model choice lives in the composer.
      modelId: form.modelId.trim() || 'default',
      timeoutSeconds: Number(form.timeoutSeconds),
      maxOutputTokens: Number(form.maxOutputTokens),
      allowInsecureHttp: remoteHttp && form.allowInsecureHttp,
      apiKey: apiKey || undefined,
      clearApiKey: clearKey || undefined,
    };
    setBusy(true);
    try {
      await providerSave(input);
      setApiKey('');
      setClearKey(false);
      setForm(null);
      setError(null);
      await refresh();
    } catch (err) {
      setFormError(describeError(err));
    } finally {
      setBusy(false);
    }
  }

  /** One button: connection first, then the tool-calling round trip that verifies the model. */
  async function test(provider: ProviderView) {
    const set = (run: TestRun) => setRuns((prev) => ({ ...prev, [provider.id]: run }));
    set({ connectivity: 'running' });
    setError(null);
    let connectivity: ProviderTestView;
    try {
      connectivity = await providerTest(provider.id, 'connectivity');
    } catch (err) {
      setError(describeError(err));
      set({});
      return;
    }
    if (!connectivity.ok) {
      set({ connectivity, toolCalling: 'skipped' });
      await refresh();
      return;
    }
    set({ connectivity, toolCalling: 'running' });
    try {
      const toolCalling = await providerTest(provider.id, 'tool_calling');
      set({ connectivity, toolCalling });
    } catch (err) {
      setError(describeError(err));
      set({ connectivity });
    }
    await refresh();
  }

  async function remove(provider: ProviderView) {
    if (!window.confirm(`Delete provider ${provider.name} and its saved key?`)) return;
    try {
      await providerDelete(provider.id);
      await refresh();
    } catch (err) {
      setError(describeError(err));
    }
  }

  const keyPlaceholder = form?.keyConfigured ? '•••• saved (leave blank to keep)' : '';
  const remoteHttp = form ? isRemotePlainHttp(form.baseUrl) : false;

  return (
    <section aria-label="AI providers" className="page">
      <header className="page-head">
        <div className="grow">
          <h1>AI Providers</h1>
          <p>Connect a provider and test it here. Choose its model from the chat composer.</p>
        </div>
        <button type="button" className="btn btn-primary" onClick={() => openForm({ ...EMPTY })}>
          <PlusIcon /> Add provider
        </button>
      </header>

      {error && (
        <p role="alert" className="banner" data-tone="danger">
          {error}
        </p>
      )}

      {providers.length === 0 && (
        <div className="empty">
          <span className="empty-icon">
            <PlugIcon size={26} />
          </span>
          <h2>No providers yet</h2>
          <p>Connect OpenAI, Anthropic, or a local server such as vLLM, Ollama or LiteLLM.</p>
          <button type="button" className="btn btn-primary" onClick={() => openForm({ ...EMPTY })}>
            <PlusIcon /> Add provider
          </button>
        </div>
      )}

      <ul className="provider-list">
        {providers.map((p) => {
          const run = runs[p.id];
          const testing = run?.connectivity === 'running' || run?.toolCalling === 'running';
          const insecure = p.baseUrl.startsWith('http://') && !p.isLoopback;
          return (
            <li key={p.id} className="card provider-card">
              <div className="provider-top">
                <span className="provider-icon">
                  <PlugIcon size={18} />
                </span>
                <div className="provider-main">
                  <strong>{p.name}</strong>
                  <div className="meta">
                    {p.baseUrl}
                  </div>
                  <div className="provider-tags">
                    <span className="badge" data-status={p.toolCalling.status}>
                      {p.toolCalling.status === 'verified' && <CheckIcon size={11} />}
                      {STATUS_TEXT[p.toolCalling.status]}
                    </span>
                    <span className="badge">{PROTOCOLS.find((x) => x.value === p.protocol)?.label}</span>
                    {insecure && (
                      <span className="badge" data-tone="warning">
                        Plain HTTP
                      </span>
                    )}
                    {!p.keyConfigured && !p.isLoopback && <span className="badge">No key saved</span>}
                  </div>
                </div>
                <div className="provider-actions">
                  <button type="button" className="btn btn-primary btn-sm" onClick={() => void test(p)} disabled={testing}>
                    <BoltIcon size={13} /> {testing ? 'Testing…' : 'Test'}
                  </button>
                  <button type="button" className="icon-btn" aria-label={`Edit ${p.name}`} title="Edit" onClick={() => openForm(fromProvider(p))}>
                    <EditIcon />
                  </button>
                  <button type="button" className="icon-btn" aria-label={`Delete ${p.name}`} title="Delete" onClick={() => void remove(p)}>
                    <TrashIcon />
                  </button>
                </div>
              </div>
              {run && (run.connectivity || run.toolCalling) && (
                <ul className="checks" aria-label={`Test results for ${p.name}`}>
                  <CheckRow name="Connection" step={run.connectivity} />
                  <CheckRow name="Tool calling" step={run.toolCalling} />
                </ul>
              )}
            </li>
          );
        })}
      </ul>

      {form && (
        <Drawer
          title={form.id ? `Edit ${form.name}` : 'Add provider'}
          subtitle="The API key is stored in your encrypted vault and never shown again."
          onClose={() => setForm(null)}
          footer={
            <>
              <button type="button" className="btn btn-ghost" onClick={() => setForm(null)}>
                Cancel
              </button>
              <button type="submit" form="provider-form" className="btn btn-primary" disabled={busy}>
                Save provider
              </button>
            </>
          }
        >
          <form id="provider-form" aria-label={form.id ? 'Edit provider' : 'New provider'} onSubmit={submit}>
            {formError && (
              <p role="alert" className="banner" data-tone="danger" style={{ marginBottom: '1rem' }}>
                {formError}
              </p>
            )}
            <div className="form-section">
              <Field label="Start from a template" hint={form.note || undefined}>
                <ThemedSelect label="Provider template" value="" placeholder="Choose…" options={templates.map((t) => ({ value: t.id, label: t.displayName }))} onChange={applyTemplate} />
              </Field>
              <label className="field">
                Name
                <input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
              </label>
              <label className="field">
                Protocol
                <ThemedSelect label="Protocol" value={form.protocol} options={PROTOCOLS.map((p) => ({ value: p.value, label: p.label }))}
                  onChange={(value) => changeEndpoint({ ...form, protocol: value as ProviderProtocolName })} />
              </label>
            </div>
            <div className="form-section">
              <h3>Endpoint</h3>
              <Field label="Base URL" hint="The API root, without the endpoint path.">
                <input
                  value={form.baseUrl}
                  placeholder="https://api.example.com/v1"
                  onChange={(e) => changeEndpoint({ ...form, baseUrl: e.target.value })}
                />
              </Field>
              {remoteHttp && (
                <div className="banner" data-tone="warning">
                  <AlertIcon />
                  <div className="grow">
                    <label className="check">
                      <input
                        type="checkbox"
                        checked={form.allowInsecureHttp}
                        onChange={(e) => changeEndpoint({ ...form, allowInsecureHttp: e.target.checked })}
                      />
                      <span>Allow plain HTTP</span>
                    </label>
                    <p className="muted" style={{ marginTop: 4 }}>
                      For a LiteLLM or similar gateway on a trusted network. The API key, your prompts and
                      command output travel unencrypted.
                    </p>
                  </div>
                </div>
              )}
              <Field label="API key" hint="Write-only. Leave blank for a local server without a key.">
                <input
                  type="password"
                  autoComplete="off"
                  value={apiKey}
                  placeholder={keyPlaceholder}
                  onChange={(e) => {
                    setApiKey(e.target.value);
                    setCheck({ state: 'idle' });
                  }}
                />
              </Field>
              {form.keyConfigured && (
                <label className="check">
                  <input type="checkbox" checked={clearKey} onChange={(e) => setClearKey(e.target.checked)} />
                  <span>Remove the saved key</span>
                </label>
              )}
              <div className="key-check">
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => void checkKey()}
                  disabled={check.state === 'checking' || !form.baseUrl.trim()}
                >
                  <KeyIcon size={13} /> {check.state === 'checking' ? 'Checking…' : 'Check API key'}
                </button>
                {check.state === 'ok' && (
                  <span role="status" className="key-check-result" data-tone="success">
                    <CheckIcon size={13} /> Key works · {check.models.length} model
                    {check.models.length === 1 ? '' : 's'} available · {check.latencyMs} ms
                  </span>
                )}
                {check.state === 'fail' && (
                  <span role="alert" className="key-check-result" data-tone="danger">
                    <AlertIcon size={13} /> {check.message}
                  </span>
                )}
                {check.state === 'idle' && (
                  <span className="hint">Checks the key and loads the model list.</span>
                )}
              </div>
              <p className="hint">Choose a provider and model in the chat composer. The first available model is used for the tool calling test.</p>
            </div>
            <div className="form-section">
              <h3>Limits</h3>
              <div className="field-grid">
                <label className="field">
                  Timeout (seconds)
                  <input value={form.timeoutSeconds} onChange={(e) => setForm({ ...form, timeoutSeconds: e.target.value })} />
                </label>
                <label className="field">
                  Max output tokens per turn
                  <input
                    value={form.maxOutputTokens}
                    onChange={(e) => setForm({ ...form, maxOutputTokens: e.target.value })}
                  />
                </label>
              </div>
            </div>
          </form>
        </Drawer>
      )}
    </section>
  );
}

function CheckRow({ name, step }: { name: string; step: Step | undefined }) {
  if (!step) return null;
  if (step === 'running') {
    return (
      <li data-result="wait">
        <span className="spinner" style={{ width: 18, height: 18 }} />
        <span>
          {name} <span className="detail">checking…</span>
        </span>
      </li>
    );
  }
  if (step === 'skipped') {
    return (
      <li data-result="skip">
        <span className="mark">–</span>
        <span>
          {name} <span className="detail">skipped because the connection failed</span>
        </span>
      </li>
    );
  }
  return (
    <li data-result={step.ok ? 'ok' : 'fail'} role="status">
      <span className="mark">{step.ok ? <CheckIcon size={11} /> : <CloseIcon size={11} />}</span>
      <span>
        {name}: {step.ok ? 'passed' : 'failed'}
        {step.code ? ` (${step.code})` : ''}{' '}
        <span className="detail">
          — {step.message} · {step.latencyMs} ms
          {step.replyPreview ? ` · “${step.replyPreview}”` : ''}
        </span>
      </span>
    </li>
  );
}
