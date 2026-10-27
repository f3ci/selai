import { Fragment, memo, useCallback, useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import {
  agentCancel,
  agentStart,
  approvalDecide,
  auditVerify,
  chatSend,
  describeError,
  HOSTS_CHANGED,
  hostKeyProblem,
  sudoOptions,
  hostList,
  preferencesGet,
  preferencesSet,
  taskTimeline,
  providerModels,
  providerList,
  type HostKeyProblem,
  type SudoAnswer,
  type SudoOptions,
  type HostView,
  type ProviderView,
  type TaskEvent,
  type ChatMessage,
} from '../ipc';
import { HostAvatar } from '../ui/HostAvatar';
import { LocalEnvironmentAvatar } from '../ui/LocalEnvironmentIcon';
import { collectPages } from '../ui/collectPages';
import { HostKeyDialog } from '../ui/HostKeyDialog';
import { Markdown } from '../ui/Markdown';
import { TranscriptWindow } from '../ui/TranscriptWindow';
import { ComposerPicker } from '../ui/ComposerPicker';
import {
  AlertIcon,
  AtIcon,
  CheckIcon,
  CloseIcon,
  ClockIcon,
  PinIcon,
  PlusIcon,
  SendIcon,
  ShieldIcon,
  StopIcon,
  TerminalIcon,
} from '../ui/icons';
import { SelaiLogo } from '../ui/SelaiLogo';
import { applyEvent, isTerminal, setApprovalState, setExecutionOutput, type TranscriptItem } from './agentTranscript';
import { PROVIDERS_CHANGED } from './ProvidersPanel';
import { useChatSessions, type ChatTurn as Turn } from './useChatSessions';
import { loadLocalTargets } from '../ui/localTargets';

/** Load targets once; the @ picker filters this list without an IPC call per key. */
async function loadTargets(): Promise<HostView[]> {
  const [local, hosts] = await Promise.all([
    loadLocalTargets().catch(() => []),
    collectPages((cursor) => hostList(undefined, false, cursor)),
  ]);
  return [...local, ...hosts.filter((h) => !h.archived)];
}

/** The trailing "@query" being typed at the end of the request. */
function activeMention(value: string): { at: number; query: string } | null {
  const at = value.lastIndexOf('@');
  if (at < 0 || (at > 0 && !/\s/.test(value[at - 1]!))) return null;
  const query = value.slice(at + 1);
  return query.length <= 64 && !query.includes('\n') ? { at, query } : null;
}

interface PendingKey {
  turn: Turn;
  host: HostView;
  problem: HostKeyProblem;
  executionProviderId: string;
  modelId: string;
  history: ChatMessage[];
}

const SUGGESTIONS = [
  { title: 'Check disk usage', text: 'check disk usage and the largest directories' },
  { title: 'Is nginx healthy?', text: 'is nginx healthy? check the service and recent errors' },
  { title: 'Explain a config', text: 'Explain what this nginx config does:\n' },
  { title: 'Plan a change', text: 'Help me plan a zero-downtime PostgreSQL major upgrade.' },
];

interface Props {
  onOpenProviders?: () => void;
  onOpenHosts?: () => void;
  onClose?: () => void;
  /** The host of the terminal in focus. An unpinned chat follows it. */
  activeHost?: HostView | null;
  /** Another view asked to use this host in the chat (pins it). */
  contextRequest?: { host: HostView; n: number } | null;
}

export function AgentPanel({ onOpenProviders, onOpenHosts, onClose, activeHost = null, contextRequest = null }: Props = {}) {
  const [providers, setProviders] = useState<ProviderView[]>([]);
  const [providerId, setProviderId] = useState('');
  const [modelId, setModelId] = useState('');
  const [models, setModels] = useState<string[]>([]);
  const [modelsLoading, setModelsLoading] = useState(false);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const modelChoices = useRef(new Map<string, string>());
  /** A server mentioned with `@` in the message being written: this message only. */
  const [hosts, setHosts] = useState<HostView[]>([]);
  /**
   * The chat's server: messages without a mention go there, so the operator does not
   * mention it every time. It follows the terminal in focus unless pinned. The model
   * decides per message whether it needs the server at all; a question it can answer
   * directly runs nothing.
   */
  const [contextId, setContextId] = useState<string | null>(null);
  const [pinned, setPinned] = useState(false);
  const [contextMenu, setContextMenu] = useState(false);
  const [targets, setTargets] = useState<HostView[]>([]);
  const [draftMentions, setDraftMentions] = useState<string[]>([]);
  const [prompt, setPrompt] = useState('');
  const [mention, setMention] = useState<string | null>(null);
  const [active, setActive] = useState(0);
  const sessions = useChatSessions();
  const { turns, setTurns, updateAllTurns } = sessions;
  const promptNavigation = useRef<{
    entries: Turn[];
    index: number;
    draft: { text: string; hosts: HostView[]; mentions: string[] };
  } | null>(null);
  const [sessionHistoryOpen, setSessionHistoryOpen] = useState(false);
  const [sessionQuery, setSessionQuery] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [audit, setAudit] = useState<{ ok: boolean; text: string } | null>(null);
  const [keyPrompt, setKeyPrompt] = useState<PendingKey | null>(null);
  const keyPromptRef = useRef<PendingKey | null>(null);
  const keyPromptQueue = useRef<PendingKey[]>([]);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const browsingHistory = useRef(false);
  const pendingText = useRef(new Map<string, string>());
  const textFlushTimer = useRef<number | null>(null);

  const running = turns.some((t) => t.running);
  const runningTasks = turns.filter((t) => t.running).flatMap((t) => t.taskId ? [t.taskId] : []);

  useEffect(() => {
    let active = true;
    // Loads the provider list and picks one when the current choice is missing or no
    // longer verified: the remembered provider if still verified, else the first
    // verified one. A missing preference is not an error.
    const load = () =>
      Promise.all([providerList(), Promise.resolve().then(preferencesGet).catch(() => null)])
        .then(([page, prefs]) => {
          if (!active) return;
          setProviders(page.items);
          const isVerified = (id: string | null | undefined) =>
            page.items.some((p) => p.id === id && p.toolCalling.status === 'verified');
          const exists = (id: string | null | undefined) => page.items.some((p) => p.id === id);
          // Prefer a provider verified for tool calling (needed for host tasks);
          // any provider can still be used for plain discussion.
          setProviderId((current) => {
            if (isVerified(current)) return current;
            if (isVerified(prefs?.lastProviderId)) return prefs?.lastProviderId ?? '';
            const firstVerified = page.items.find((p) => p.toolCalling.status === 'verified')?.id;
            if (firstVerified) return firstVerified;
            if (exists(current)) return current;
            return page.items[0]?.id ?? '';
          });
        })
        .catch((err: unknown) => active && setError(describeError(err)));
    window.addEventListener(PROVIDERS_CHANGED, load);
    void load();
    return () => {
      active = false;
      window.removeEventListener(PROVIDERS_CHANGED, load);
    };
  }, []);

  const provider = providers.find((p) => p.id === providerId);
  useEffect(() => {
    let active = true;
    if (!provider) {
      setModels([]);
      setModelId('');
      setModelsError(null);
      return;
    }
    setModels([]);
    setModelsLoading(true);
    setModelsError(null);
    void providerModels({
      id: provider.id,
      protocol: provider.protocol,
      baseUrl: provider.baseUrl,
      allowInsecureHttp: provider.allowInsecureHttp,
    }).then((result) => {
      if (!active) return;
      const available = [...new Set(result.models)];
      if (available.length === 0 || (provider.modelId !== 'default' && !available.includes(provider.modelId))) {
        available.push(provider.modelId);
      }
      setModels(available);
      const remembered = modelChoices.current.get(provider.id);
      const next = (remembered && available.includes(remembered) ? remembered : null)
        ?? (available.includes(provider.modelId) ? provider.modelId : null)
        ?? available[0]
        ?? provider.modelId;
      setModelId(next);
      modelChoices.current.set(provider.id, next);
    }).catch((err: unknown) => {
      if (!active) return;
      setModels([provider.modelId]);
      setModelId(provider.modelId);
      setModelsError(describeError(err));
    }).finally(() => {
      if (active) setModelsLoading(false);
    });
    return () => { active = false; };
  }, [provider?.id, provider?.protocol, provider?.baseUrl, provider?.allowInsecureHttp, provider?.modelId]);

  function chooseModel(id: string) {
    setModelId(id);
    if (provider) modelChoices.current.set(provider.id, id);
  }

  useEffect(() => {
    let active = true;
    const refreshTargets = () =>
      loadTargets()
        .then((list) => active && setTargets(list))
        .catch(() => undefined);
    refreshTargets();
    window.addEventListener(HOSTS_CHANGED, refreshTargets);
    preferencesGet()
      .then((prefs) => {
        if (!active || !prefs.pinnedHostId) return;
        setContextId(prefs.pinnedHostId);
        setPinned(true);
      })
      .catch(() => undefined);
    return () => {
      active = false;
      window.removeEventListener(HOSTS_CHANGED, refreshTargets);
    };
  }, []);

  const mentionQuery = mention?.trim().toLowerCase();
  const matches = mentionQuery === undefined ? [] : targets.filter((target) =>
    target.label.toLowerCase().includes(mentionQuery) ||
    (target.kind === 'local' && 'local'.startsWith(mentionQuery)));

  useEffect(() => {
    if (!contextMenu) return;
    const close = (e: MouseEvent) => {
      if (!(e.target instanceof Element && e.target.closest('.context-row'))) setContextMenu(false);
    };
    const esc = (e: globalThis.KeyboardEvent) => e.key === 'Escape' && setContextMenu(false);
    document.addEventListener('mousedown', close);
    document.addEventListener('keydown', esc);
    return () => {
      document.removeEventListener('mousedown', close);
      document.removeEventListener('keydown', esc);
    };
  }, [contextMenu]);

  // Follow the terminal in focus, unless the operator pinned a server. Adjusted while
  // rendering, so the chip never shows a stale server for a frame.
  const activeId = activeHost?.id ?? null;
  const [seenActive, setSeenActive] = useState<string | null>(null);
  if (activeId !== seenActive) {
    setSeenActive(activeId);
    if (!pinned && activeId) setContextId(activeId);
  }
  const [seenRequest, setSeenRequest] = useState(0);
  if (contextRequest && contextRequest.n !== seenRequest) {
    setSeenRequest(contextRequest.n);
    setContextId(contextRequest.host.id);
    setPinned(true);
  }
  useEffect(() => {
    // Remember a server pinned from another view.
    if (contextRequest) preferencesSet({ pinnedHostId: contextRequest.host.id }).catch(() => undefined);
  }, [contextRequest]);

  useEffect(() => {
    browsingHistory.current = false;
  }, [sessions.current.id]);

  useEffect(() => {
    const el = scrollRef.current;
    if (el && !browsingHistory.current) el.scrollTop = el.scrollHeight;
  }, [turns]);

  // A query that already holds a space and matches nothing is plain text, not a mention.
  const pickerOpen = mention !== null && !(matches.length === 0 && mention.includes(' '));

  function onPromptChange(value: string) {
    if (promptNavigation.current) setError(null);
    promptNavigation.current = null;
    setPrompt(value);
    const remaining = draftMentions.filter((m) => value.includes(m));
    if (remaining.length !== draftMentions.length) {
      setDraftMentions(remaining);
      setHosts((current) => current.filter((h) => remaining.includes(`@${h.label}`)));
    }
    const m = activeMention(value);
    if (!m) {
      setMention(null);
      return;
    }
    setMention(m.query);
    setActive(0);
  }

  function pickHost(h: HostView) {
    promptNavigation.current = null;
    const text = `@${h.label}`;
    if (!hosts.some((item) => item.id === h.id)) {
      setHosts((current) => [...current, h]);
      setDraftMentions((current) => [...current, text]);
    }
    setPrompt((p) => {
      const active = activeMention(p);
      return active ? `${p.slice(0, active.at)}${text} ` : `${p}${text} `;
    });
    setMention(null);
    textareaRef.current?.focus();
  }

  function insertAt() {
    onPromptChange(prompt.length === 0 || /\s$/.test(prompt) ? `${prompt}@` : `${prompt} @`);
    textareaRef.current?.focus();
  }

  function clearTarget(hostId: string) {
    promptNavigation.current = null;
    const selected = hosts.find((h) => h.id === hostId);
    if (selected) {
      const text = `@${selected.label}`;
      setPrompt((p) => p.replace(text, '').replace(/\s{2,}/g, ' ').trimStart());
      setDraftMentions((current) => current.filter((m) => m !== text));
      setHosts((current) => current.filter((h) => h.id !== hostId));
    }
  }

  function onPromptKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.nativeEvent.isComposing) return;
    if (pickerOpen) {
      if (e.key === 'Escape') {
        e.preventDefault();
        setMention(null);
        return;
      }
      if (matches.length > 0) {
        if (e.key === 'ArrowDown') {
          e.preventDefault();
          setActive((i) => (i + 1) % matches.length);
          return;
        }
        if (e.key === 'ArrowUp') {
          e.preventDefault();
          setActive((i) => (i - 1 + matches.length) % matches.length);
          return;
        }
        if ((e.key === 'Enter' && !e.shiftKey) || e.key === 'Tab') {
          e.preventDefault();
          const target = matches[active] ?? matches[0];
          if (target) pickHost(target);
          return;
        }
      }
    }
    if (!pickerOpen && !e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey
      && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
      const { selectionStart, selectionEnd } = e.currentTarget;
      const up = e.key === 'ArrowUp';
      const atBoundary = up ? !prompt.slice(0, selectionStart).includes('\n') : !prompt.slice(selectionEnd).includes('\n');
      if (selectionStart === selectionEnd && atBoundary) {
        let navigation = promptNavigation.current;
        if (!navigation && up) {
          const entries = turns.filter((turn) => turn.showPrompt);
          if (entries.length) {
            navigation = { entries, index: entries.length, draft: { text: prompt, hosts: [...hosts], mentions: [...draftMentions] } };
            promptNavigation.current = navigation;
          }
        }
        if (navigation) {
          e.preventDefault();
          navigation.index = Math.max(0, Math.min(navigation.entries.length, navigation.index + (up ? -1 : 1)));
          const entry = navigation.entries[navigation.index];
          const text = entry?.prompt ?? navigation.draft.text;
          const mentions = entry ? entry.mentionText ?? [] : navigation.draft.mentions;
          const recalledHosts = entry ? targets.filter((host) => mentions.includes(`@${host.label}`)) : navigation.draft.hosts;
          setPrompt(text);
          setHosts(recalledHosts);
          setDraftMentions(mentions);
          setMention(null);
          setError(mentions.some((mention) => !recalledHosts.some((host) => mention === `@${host.label}`))
            ? 'A server from this message is unavailable. Remove or reselect its @mention before sending.' : null);
          if (!entry) promptNavigation.current = null;
          requestAnimationFrame(() => {
            const caret = up ? 0 : text.length;
            textareaRef.current?.setSelectionRange(caret, caret);
          });
          return;
        }
      }
    }
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      void send();
    }
  }

  function chooseProvider(id: string) {
    setProviderId(id);
    // Remember the choice for the next start. A failed save is not shown; the
    // selection still applies to this session.
    preferencesSet({ lastProviderId: id }).catch(() => undefined);
  }

  const verified = provider?.toolCalling.status === 'verified';
  const modelVerified = verified && modelId === provider?.modelId;
  // An archived or deleted server drops out of the list and so out of the chat.
  const contextHost =
    targets.find((t) => t.id === contextId) ??
    (activeHost?.id === contextId ? activeHost : null) ??
    (contextRequest?.host.id === contextId ? contextRequest.host : null);
  // The server for this message: a mention, else the chat's server, else none
  // (plain discussion, which any provider can answer). A server needs tool calling.
  const messageHosts = hosts.length ? hosts : contextHost ? [contextHost] : [];
  const target = hosts[0] ?? contextHost;
  const mentionsAvailable = draftMentions.every((mention) => hosts.some((host) => mention === `@${host.label}`));
  const canSend = sessions.ready && !sessions.loading && !running && !modelsLoading && !!modelId && provider !== undefined && mentionsAvailable && (messageHosts.length === 0 || modelVerified) && prompt.trim().length > 0;

  /** Pinned servers are remembered across restarts; a failed save keeps the choice for now. */
  function rememberPin(id: string | null) {
    preferencesSet({ pinnedHostId: id ?? '' }).catch(() => undefined);
  }

  /** Choosing a server by hand pins it, so switching terminals does not change it. */
  function chooseContext(id: string | null) {
    setContextMenu(false);
    setContextId(id);
    setPinned(id !== null);
    rememberPin(id);
    textareaRef.current?.focus();
  }

  function togglePin() {
    if (pinned) {
      setPinned(false);
      rememberPin(null);
      // Back to following the terminal in focus.
      if (activeHost) setContextId(activeHost.id);
    } else if (contextId) {
      setPinned(true);
      rememberPin(contextId);
    }
  }

  const updateTurn = (id: string, change: (t: Turn) => Turn) =>
    updateAllTurns((prev) => {
      const index = prev.findIndex((t) => t.id === id);
      if (index < 0) return prev;
      const next = change(prev[index]!);
      if (next === prev[index]) return prev;
      const updated = prev.slice();
      updated[index] = next;
      return updated;
    });

  // Model providers can stream many tiny tokens per second. Render text at most
  // once per short interval, but flush it before approvals and terminal events.
  function flushText() {
    if (textFlushTimer.current !== null) window.clearTimeout(textFlushTimer.current);
    textFlushTimer.current = null;
    if (!pendingText.current.size) return;
    const pending = pendingText.current;
    pendingText.current = new Map();
    updateAllTurns((prev) => prev.map((turn) => {
      const text = pending.get(turn.id);
      return text ? { ...turn, items: applyEvent(turn.items, { type: 'textDelta', text }) } : turn;
    }));
  }

  function displayEvent(id: string, event: TaskEvent) {
    if (event.type === 'textDelta') {
      pendingText.current.set(id, (pendingText.current.get(id) ?? '') + event.text);
      if (pendingText.current.get(id)!.length >= 32_768) flushText();
      else if (textFlushTimer.current === null) textFlushTimer.current = window.setTimeout(flushText, 32);
      return;
    }
    flushText();
    updateTurn(id, (turn) => {
      const items = applyEvent(turn.items, event);
      const running = turn.running && !isTerminal(event);
      return items === turn.items && running === turn.running ? turn : { ...turn, items, running };
    });
    if (event.type === 'toolFinished' && 'taskId' in event && typeof event.taskId === 'string') {
      void taskTimeline(event.taskId).then((executions) => {
        const execution = executions.find((item) => item.id === event.executionId);
        if (execution) updateTurn(id, (turn) => ({ ...turn, items: setExecutionOutput(turn.items, execution) }));
      }).catch(() => undefined);
    }
  }

  useEffect(() => () => {
    if (textFlushTimer.current !== null) window.clearTimeout(textFlushTimer.current);
    pendingText.current.clear();
  }, []);

  async function startTurn(turn: Turn, target: HostView, executionProviderId = providerId, executionModelId = modelId, history = conversation()) {
    try {
      // The new message has no reply yet, so it is not part of its own history.
      const id = await agentStart(target.id, executionProviderId, turn.taskPrompt, history, (event) => {
        displayEvent(turn.id, event);
      }, executionModelId);
      updateTurn(turn.id, (t) => ({ ...t, taskId: id }));
    } catch (err) {
      const problem = hostKeyProblem(err);
      if (problem) {
        updateTurn(turn.id, (t) => ({ ...t, running: false, notice: 'Waiting for you to approve the server key…' }));
        const promptForKey = { turn, host: target, problem, executionProviderId, modelId: executionModelId, history };
        keyPromptQueue.current.push(promptForKey);
        if (!keyPromptRef.current) {
          keyPromptRef.current = keyPromptQueue.current.shift() ?? null;
          setKeyPrompt(keyPromptRef.current);
        }
        return;
      }
      updateTurn(turn.id, (t) => ({ ...t, running: false, error: describeError(err) }));
    }
  }

  /** Earlier messages as context for plain discussion. Turns with no reply are skipped. */
  function conversation(): { role: 'user' | 'assistant'; content: string }[] {
    const out: { role: 'user' | 'assistant'; content: string }[] = [];
    for (const t of turns) {
      const reply = t.items
        .filter((i): i is Extract<TranscriptItem, { kind: 'text' }> => i.kind === 'text')
        .map((i) => i.text)
        .join('\n')
        .trim();
      if (!reply) continue;
      out.push({ role: 'user', content: t.prompt }, { role: 'assistant', content: reply });
    }
    return out;
  }

  async function startChat(turn: Turn, selectedModelId: string, history: { role: 'user' | 'assistant'; content: string }[]) {
    try {
      const id = await chatSend(providerId, [...history, { role: 'user', content: turn.prompt }], (event) => {
        const asTask = event.type === 'completed' ? ({ type: 'completed', text: '' } as const) : event;
        displayEvent(turn.id, asTask);
      }, selectedModelId);
      updateTurn(turn.id, (t) => ({ ...t, taskId: id }));
    } catch (err) {
      updateTurn(turn.id, (t) => ({ ...t, running: false, error: describeError(err) }));
    }
  }

  async function send() {
    if (!canSend) return;
    const selectedModelId = modelId;
    const selectedTargets = hosts.length ? hosts : contextHost ? [contextHost] : [];
    const originalPrompt = prompt.trim();
    const taskPrompt = draftMentions.reduce((text, mention) => text.replaceAll(mention, ''), originalPrompt).replace(/\s{2,}/g, ' ').trim();
    const newTurns: Turn[] = (selectedTargets.length ? selectedTargets : [null]).map((server, index) => ({
      id: crypto.randomUUID(),
      hostId: server?.id ?? null,
      hostLabel: server?.label ?? null,
      showPrompt: index === 0,
      taskPrompt,
      mentionText: draftMentions.length ? [...draftMentions] : null,
      prompt: originalPrompt,
      items: [],
      taskId: null,
      running: true,
      notice: null,
      error: null,
    }));
    const history = conversation();
    promptNavigation.current = null;
    setTurns((prev) => [...prev, ...newTurns]);
    setPrompt('');
    // Explicit mentions apply to this message only. The chat server stays.
    setDraftMentions([]);
    setHosts([]);
    setMention(null);
    setError(null);
    if (selectedTargets.length) await Promise.all(newTurns.map((turn, index) => startTurn(turn, selectedTargets[index]!, providerId, selectedModelId)));
    else await startChat(newTurns[0]!, selectedModelId, history);
  }

  async function stop() {
    if (!runningTasks.length) return;
    try {
      await Promise.all(runningTasks.map((taskId) => agentCancel(taskId)));
    } catch (err) {
      setError(describeError(err));
    }
  }

  const decide = useCallback(async (approvalId: string, approve: boolean, sudo?: SudoAnswer, approveCommandsForTask = false) => {
    const update = (state: 'sending' | 'approved' | 'denied' | 'pending', why: string | null = null) =>
      updateAllTurns((prev) => prev.map((t) => t.items.some((item) => item.kind === 'approval' && item.approvalId === approvalId)
        ? { ...t, items: setApprovalState(t.items, approvalId, state, why) } : t));
    update('sending');
    try {
      await approvalDecide(approvalId, approve, sudo, approveCommandsForTask);
      update(approve ? 'approved' : 'denied');
    } catch (err) {
      // The database refused it (expired or already decided). Show why.
      update('pending', describeError(err));
    }
  }, [updateAllTurns]);

  async function verifyAudit() {
    try {
      const result = await auditVerify();
      setAudit(
        result.ok
          ? { ok: true, text: `Audit log intact: ${result.eventsChecked} event(s) checked.` }
          : { ok: false, text: `Audit log broken at sequence ${result.brokenAtSeq ?? '?'}.` },
      );
    } catch (err) {
      setAudit({ ok: false, text: describeError(err) });
    }
  }

  function resetComposer() {
    promptNavigation.current = null;
    setPrompt('');
    setHosts([]);
    setDraftMentions([]);
    setMention(null);
    setError(null);
    setAudit(null);
    setSessionHistoryOpen(false);
    setSessionQuery('');
    textareaRef.current?.focus();
  }

  function newSession() {
    flushText();
    sessions.newSession();
    resetComposer();
  }

  async function openSession(id: string) {
    flushText();
    await sessions.selectSession(id);
    resetComposer();
  }

  return (
    <section aria-label="AI Agent" className="chat">
      <header className="panel-head">
        <SelaiLogo size={22} />
        <h2>Assistant</h2>
        <span className="grow" />
        <button
          type="button"
          className="icon-btn"
          aria-label="New session"
          title="New session"
          onClick={newSession}
          disabled={!sessions.ready || sessions.loading}
        >
          <PlusIcon size={15} />
        </button>
        <button type="button" className="icon-btn" aria-label="Verify audit log" title="Verify audit log" onClick={verifyAudit}>
          <ShieldIcon size={14} />
        </button>
        {onClose && (
          <button type="button" className="icon-btn" aria-label="Close chat" title="Close chat (Ctrl+Shift+L to open)" onClick={onClose}>
            <CloseIcon size={14} />
          </button>
        )}
      </header>

      <div className="chat-session-bar">
        <label className="chat-session-title">
          <span className="context-label">Session</span>
          <input aria-label="Session title" value={sessions.current.title} maxLength={120}
            placeholder="Name this session" disabled={!sessions.ready || sessions.loading}
            onChange={(event) => sessions.rename(event.target.value)} />
        </label>
        <button type="button" className="icon-btn" aria-label="Session history" title="Session history"
          aria-expanded={sessionHistoryOpen} onClick={() => setSessionHistoryOpen((open) => !open)}>
          <ClockIcon size={15} />
        </button>
      </div>
      {sessionHistoryOpen && (
        <section className="chat-session-history" aria-label="Chat sessions">
          <input aria-label="Search sessions" placeholder="Search sessions…" value={sessionQuery}
            onChange={(event) => setSessionQuery(event.target.value)} />
          <ul>
            {sessions.sessions.filter((session) => session.title.toLowerCase().includes(sessionQuery.toLowerCase())).map((session) => (
              <li key={session.id}>
                <button type="button" aria-current={session.id === sessions.current.id ? 'true' : undefined}
                  disabled={sessions.loading} onClick={() => void openSession(session.id)}>
                  <span>{session.title || 'New session'}</span>
                  <small>{session.running ? 'Running' : new Date(session.updatedAtMs).toLocaleString()}</small>
                </button>
              </li>
            ))}
          </ul>
          {sessions.loading && <p role="status">Opening session…</p>}
        </section>
      )}

      <div className="chat-scroll" ref={scrollRef}>
        {audit && (
          <div className="thread" style={{ paddingBottom: 0 }}>
            <p role="status" className="banner" data-tone={audit.ok ? 'success' : 'danger'}>
              {audit.ok ? <CheckIcon /> : <AlertIcon />}
              <span className="grow">{audit.text}</span>
            </p>
          </div>
        )}
        {turns.length === 0 ? (
          <div className="chat-empty">
            <span className="empty-icon">
              <SelaiLogo size={44} />
            </span>
            <h2>What are you working on?</h2>
            <p>
              {contextHost
                ? `Ask about ${contextHost.label}, or just discuss. Commands run only when an answer needs them; anything beyond a read-only check waits for your approval.`
                : 'Think through a problem, review a config, or investigate a server. You choose where we work.'}
            </p>
            <div className="suggestions">
              {SUGGESTIONS.map((s) => (
                <button
                  key={s.title}
                  type="button"
                  className="suggestion"
                  onClick={() => {
                    setPrompt(draftMentions.length ? `${draftMentions.join(' ')} ${s.text}` : s.text);
                    textareaRef.current?.focus();
                  }}
                >
                  {s.title}
                  <span aria-hidden="true">↗</span>
                </button>
              ))}
            </div>
          </div>
        ) : (
          <ol className="thread" aria-label="Transcript">
            <TranscriptWindow key={sessions.current.id} items={turns} onBrowse={(older) => {
              browsingHistory.current = older;
              if (!older) requestAnimationFrame(() => { const el = scrollRef.current; if (el) el.scrollTop = el.scrollHeight; });
            }} render={(turn) => <TurnEntry turn={turn} onDecide={decide} />} />
          </ol>
        )}
      </div>

      <div className="composer-wrap">
        {sessions.saveError && (
          <p role="alert" className="banner" data-tone="danger">
            <span className="grow">{sessions.saveError}</span>
            <button type="button" className="btn btn-sm" onClick={sessions.retrySave}>Retry</button>
          </p>
        )}
        {error && (
          <p role="alert" className="banner" data-tone="danger" style={{ maxWidth: 820, margin: '0 auto 0.6rem' }}>
            {error}
          </p>
        )}
        {(providers.length === 0 || (messageHosts.length > 0 && !modelVerified)) && (
          <div className="banner assistant-warning" data-tone="warning">
            <AlertIcon />
            <span className="grow">
              {providers.length === 0
                ? 'Add an AI provider first.'
                : provider && modelId !== provider.modelId
                  ? `Server tasks need the model tested for tool calling (${provider?.modelId}).`
                  : 'Working on a server needs a provider whose tool calling test passed.'}
            </span>
            <span className="assistant-warning-actions">
              {providers.length > 0 && hosts.length === 0 && contextHost && (
                <button type="button" className="btn btn-sm" onClick={() => chooseContext(null)}>
                  Chat without server
                </button>
              )}
              {onOpenProviders && (
                <button type="button" className="btn btn-sm" onClick={onOpenProviders}>
                  Open Providers
                </button>
              )}
            </span>
          </div>
        )}
        <div className="composer">
          {pickerOpen && (
            <ul className="mention-pop" aria-label="Matching hosts">
              <li className="head">Hosts</li>
              {matches.length === 0 && (
                <li className="none">
                  No matching host.{' '}
                  {onOpenHosts && (
                    <button type="button" className="btn btn-ghost btn-sm" onClick={onOpenHosts}>
                      Add a host
                    </button>
                  )}
                </li>
              )}
              {matches.map((h, i) => (
                <li key={h.id} className={i === active ? 'active' : undefined}>
                  <button
                    type="button"
                    tabIndex={-1}
                    aria-current={i === active ? 'true' : undefined}
                    onMouseDown={(e) => e.preventDefault()}
                    onClick={() => pickHost(h)}
                  >
                    <TargetAvatar host={h} />
                    <span>
                      {h.label}
                      <small>{h.kind === 'local' ? 'This machine · local shell' : `${h.username}@${h.hostname}`}</small>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
          <div className="context-row">
            <span className="context-label">{hosts.length ? `This message · ${hosts.length} server${hosts.length === 1 ? '' : 's'}` : contextHost ? pinned ? 'Pinned server' : 'Following terminal' : 'Context'}</span>
            {hosts.map((h) => (
              <span key={h.id} className="target-chip" data-kind="mention" title={`${h.username}@${h.hostname}`}>
                <AtIcon size={12} />
                {h.label}
                <button type="button" aria-label={`Remove ${h.label}`} onClick={() => clearTarget(h.id)}>
                  <CloseIcon size={11} />
                </button>
              </span>
            ))}
            {contextHost ? (
              <span
                className="target-chip"
                data-muted={hosts.length ? 'true' : undefined}
                title={hosts.length ? `This message goes to ${hosts.map((h) => h.label).join(', ')}` : `${contextHost.username}@${contextHost.hostname}`}
              >
                <button
                  type="button"
                  className="chip-main"
                  aria-label={`Chat server: ${contextHost.label}. Change`}
                  aria-haspopup="menu"
                  aria-expanded={contextMenu}
                  onClick={() => setContextMenu((o) => !o)}
                >
                  <TerminalIcon size={12} />
                  {contextHost.label}
                </button>
                <button
                  type="button"
                  className="chip-pin"
                  aria-pressed={pinned}
                  aria-label={pinned ? `Unpin ${contextHost.label}` : `Pin ${contextHost.label}`}
                  title={pinned ? 'Pinned: stays when you switch terminals. Click to follow the terminal again.' : 'Follows the terminal in focus. Click to keep this server.'}
                  onClick={togglePin}
                >
                  <PinIcon size={12} />
                </button>
                <button type="button" aria-label={`Chat without ${contextHost.label}`} title="Chat without a server" onClick={() => chooseContext(null)}>
                  <CloseIcon size={11} />
                </button>
              </span>
            ) : (
              hosts.length === 0 && (
                <button
                  type="button"
                  className="target-chip"
                  data-empty="true"
                  aria-haspopup="menu"
                  aria-expanded={contextMenu}
                  onClick={() => setContextMenu((o) => !o)}
                >
                  <PlusIcon size={12} /> Server
                </button>
              )
            )}
            {contextMenu && (
              <ul className="mention-pop context-pop" role="menu" aria-label="Chat server">
                <li className="head">Chat server</li>
                <li>
                  <button type="button" role="menuitemradio" aria-checked={contextHost === null} onClick={() => chooseContext(null)}>
                    <SelaiLogo size={18} />
                    <span>
                      No server
                      <small>plain chat, nothing runs</small>
                    </span>
                  </button>
                </li>
                {targets.map((t) => (
                  <li key={t.id}>
                    <button type="button" role="menuitemradio" aria-checked={contextHost?.id === t.id} onClick={() => chooseContext(t.id)}>
                      <TargetAvatar host={t} />
                      <span>
                        {t.label}
                        <small>{t.kind === 'local' ? 'local shell' : `${t.username}@${t.hostname}`}</small>
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <textarea
            ref={textareaRef}
            aria-label="Request"
            title="↑ / ↓ at the first / last line: browse sent messages"
            value={prompt}
            onChange={(e) => onPromptChange(e.target.value)}
            onKeyDown={onPromptKeyDown}
            rows={Math.min(8, Math.max(2, prompt.split('\n').length))}
            placeholder={hosts.length > 1 ? `Ask about ${hosts.length} servers, or anything…` : target ? `Ask about ${target.label}, or anything…` : 'Ask anything…'}
          />
          <div className="composer-bar">
            <button type="button" className="icon-btn" aria-label="Mention a host" title="Ask about another host for this message (@)" onClick={insertAt}>
              <AtIcon size={14} />
            </button>
            <ComposerPicker
              label="Provider"
              className="provider-choice"
              value={providerId}
              disabled={running}
              options={providers.map((p) => ({
                value: p.id,
                label: p.name,
                detail: p.toolCalling.status === 'verified' ? 'Server tools ready' : 'Chat only',
              }))}
              onChange={chooseProvider}
            />
            <ComposerPicker
              label="Model"
              title={modelsError ? `Could not refresh models: ${modelsError}` : undefined}
              className="model-choice"
              value={modelId}
              disabled={!provider || modelsLoading || running || models.length === 0}
              options={models.map((model) => ({
                value: model,
                label: model,
                detail: model === provider?.modelId && provider.toolCalling.status === 'verified' ? 'Server tools ready' : undefined,
              }))}
              onChange={chooseModel}
            />
            <span className="grow" />
            {running ? (
              <button type="button" className="btn btn-danger send-btn" aria-label="Stop" title="Stop all server tasks" onClick={stop} disabled={!runningTasks.length}>
                <StopIcon size={14} />
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-primary send-btn"
                aria-label="Send"
                title="Send (Enter)"
                onClick={() => void send()}
                disabled={!canSend}
              >
                <SendIcon />
              </button>
            )}
          </div>
        </div>
        <p className="composer-note">
          {hosts.length > 0
            ? hosts.length === 1
              ? `This message goes to ${hosts[0]!.label}. Changes wait for your approval.`
              : `This message runs on ${hosts.map((h) => h.label).join(', ')} in parallel. Changes wait for approval on each server.`
            : contextHost
              ? `Working on ${contextHost.label}: commands run only when needed, and changes wait for your approval.`
              : 'Plain chat: nothing runs on any server.'}
        </p>
      </div>

      {keyPrompt && (
        <HostKeyDialog
          hostId={keyPrompt.host.id}
          hostLabel={keyPrompt.host.label}
          address={`${keyPrompt.host.hostname}:${keyPrompt.host.port}`}
          problem={keyPrompt.problem}
          onTrusted={() => {
            const { turn, host: target, executionProviderId, modelId: executionModelId, history } = keyPrompt;
            keyPromptRef.current = null;
            setKeyPrompt(null);
            updateTurn(turn.id, (t) => ({ ...t, running: true, notice: null }));
            void startTurn(turn, target, executionProviderId, executionModelId, history);
            keyPromptRef.current = keyPromptQueue.current.shift() ?? null;
            setKeyPrompt(keyPromptRef.current);
          }}
          onCancel={() => {
            updateTurn(keyPrompt.turn.id, (t) => ({
              ...t,
              running: false,
              notice: null,
              error: 'Not started: the server key was not approved, so nothing was sent to the host.',
            }));
            keyPromptRef.current = keyPromptQueue.current.shift() ?? null;
            setKeyPrompt(keyPromptRef.current);
          }}
        />
      )}
    </section>
  );
}

/** Completed turns keep their identity, so typing or streaming a later turn does
 *  not parse and render all earlier Markdown again. */
const TurnEntry = memo(function TurnEntry({ turn, onDecide }: {
  turn: Turn;
  onDecide: (approvalId: string, approve: boolean, sudo?: SudoAnswer, approveCommandsForTask?: boolean) => void;
}) {
  const awaitingApproval = turn.items.some((i) => i.kind === 'approval' && i.state === 'pending');
  const commandRunning = turn.items.some((i) => i.kind === 'tool' && (i.status === 'proposed' || i.status === 'running'));
  const hasReplyText = turn.items.some((i) => i.kind === 'text');
  return (
    <Fragment>
      {turn.showPrompt && <li className="turn-user">
        <div className="bubble">{highlight(turn.prompt, turn.mentionText)}</div>
      </li>}
      <li className="turn-ai">
        <span className="ai-avatar" role="img" aria-label="Selai assistant">
          <SelaiLogo size={26} />
        </span>
        <div className="ai-body">
          {turn.hostLabel && (
            <div className="ai-meta">
              <span className="host-tag" title="Commands in this reply ran on this host">
                <TerminalIcon size={11} /> {turn.hostLabel}
              </span>
            </div>
          )}
          {turn.items.map((item, i) => (
            <TranscriptRow key={i} item={item} items={turn.items} onDecide={onDecide} />
          ))}
          {turn.running && !awaitingApproval && !commandRunning && (
            hasReplyText ? (
              <div className="turn-progress" role="status" aria-label="Working">
                <span className="spinner" aria-hidden="true" />
                <span>Working…</span>
              </div>
            ) : (
              <span className="typing" aria-label="Working">
                <span />
                <span />
                <span />
              </span>
            )
          )}
          {turn.notice && <p className="status-line">{turn.notice}</p>}
          {turn.error && (
            <p role="alert" className="banner" data-tone="danger">
              <AlertIcon />
              <span className="grow">{turn.error}</span>
            </p>
          )}
        </div>
      </li>
    </Fragment>
  );
});

function TargetAvatar({ host }: { host: HostView }) {
  if (host.kind !== 'local') return <HostAvatar label={host.label} size={13} />;
  return <LocalEnvironmentAvatar host={host} size={15} />;
}

/** Shows the bound "@Label" as a pill inside the sent message. */
function highlight(text: string, mentionTexts: string[] | null): ReactNode {
  if (!mentionTexts?.length) return text;
  const tokens = [...mentionTexts].sort((a, b) => b.length - a.length);
  const parts: ReactNode[] = [];
  let remainder = text;
  while (remainder) {
    let bestAt = -1;
    let bestToken = '';
    for (const token of tokens) {
      const at = remainder.indexOf(token);
      if (at >= 0 && (bestAt < 0 || at < bestAt)) {
        bestAt = at;
        bestToken = token;
      }
    }
    if (bestAt < 0) {
      parts.push(remainder);
      break;
    }
    if (bestAt > 0) parts.push(remainder.slice(0, bestAt));
    parts.push(<span className="mention" key={`${bestToken}-${parts.length}`}>{bestToken}</span>);
    remainder = remainder.slice(bestAt + bestToken.length);
  }
  return <>{parts}</>;
}

function TranscriptRow({
  item,
  items,
  onDecide,
}: {
  item: TranscriptItem;
  items: TranscriptItem[];
  onDecide: (approvalId: string, approve: boolean, sudo?: SudoAnswer, approveCommandsForTask?: boolean) => void;
}) {
  switch (item.kind) {
    case 'text':
      return <Markdown text={item.text} />;
    case 'status':
      return <p className="status-line">{item.text}</p>;
    case 'failed':
      return (
        <p role="alert" className="banner" data-tone="danger">
          <AlertIcon />
          <span className="grow">
            {item.message} <small>({item.code})</small>
          </span>
        </p>
      );
    case 'tool': {
      // A command that needs approval is one card: the command, then the decision.
      const approval = items.find(
        (i): i is Extract<TranscriptItem, { kind: 'approval' }> =>
          i.kind === 'approval' && i.executionId === item.executionId,
      );
      if (approval) return <ApprovalCard item={approval} hostLabel={item.hostLabel} status={item} onDecide={onDecide} />;
      return <Activity item={item} />;
    }
    case 'approval': {
      // Shown inside its command card when that card exists.
      if (items.some((i) => i.kind === 'tool' && i.executionId === item.executionId)) return null;
      return <ApprovalCard item={item} hostLabel={null} status={null} onDecide={onDecide} />;
    }
  }
}

type ToolItem = Extract<TranscriptItem, { kind: 'tool' }>;
type ApprovalItem = Extract<TranscriptItem, { kind: 'approval' }>;

function StatusBadge({ item }: { item: ToolItem }) {
  const done = item.status === 'completed';
  const bad = ['failed', 'denied', 'timed_out', 'cancelled', 'interrupted'].includes(item.status);
  const running = item.status === 'proposed' || item.status === 'running';
  return (
    <span className="badge" data-tone={done ? 'success' : bad ? 'danger' : 'accent'}>
      {running && <span className="spinner" role="status" aria-label="Running command" style={{ width: 11, height: 11, borderWidth: 1.5 }} />}
      {running ? 'running' : item.status.replace(/_/g, ' ')}
      {item.exitCode !== null ? ` · exit ${item.exitCode}` : ''}
    </span>
  );
}

/** Keep the result attached to the command it belongs to. */
function Activity({ item }: { item: ToolItem }) {
  const done = item.status === 'completed';
  const bad = ['failed', 'denied', 'timed_out', 'cancelled', 'interrupted'].includes(item.status);
  const pending = item.status === 'proposed';
  return (
    <div className="command-activity" aria-label="Command">
    <div className="activity" data-tone={done ? 'success' : bad ? 'danger' : undefined}>
      <span className="mark">
        {pending ? <span className="spinner" role="status" aria-label="Running command" style={{ width: 12, height: 12 }} /> : done ? <CheckIcon size={11} /> : bad ? <CloseIcon size={11} /> : <TerminalIcon size={11} />}
      </span>
      <span className="when">{pending ? 'Running' : item.status === 'detached' ? 'Check History' : 'Ran'}</span>
      <code title={item.command}>{item.command}</code>
      {!pending && (
        <span className="when">
          {done ? '' : item.status.replace(/_/g, ' ')}
          {item.exitCode !== null ? `${done ? '' : ' · '}exit ${item.exitCode}` : ''}
        </span>
      )}
    </div>
    <CommandOutput item={item} />
    </div>
  );
}

function CommandOutput({ item }: { item: ToolItem | null }) {
  if (!item || (item.stdoutPreview === undefined && item.stderrPreview === undefined)) return null;
  return <details className="activity-output">
    <summary>Output</summary>
    {item.outputTruncated && <p className="muted">Output was truncated.</p>}
    {item.stdoutPreview && <pre>{item.stdoutPreview}</pre>}
    {item.stderrPreview && <pre data-tone="danger">{item.stderrPreview}</pre>}
    {!item.stdoutPreview && !item.stderrPreview && <p className="muted">No output captured.</p>}
  </details>;
}

function ApprovalCard({
  item,
  hostLabel,
  status,
  onDecide,
}: {
  item: ApprovalItem;
  hostLabel: string | null;
  status: ToolItem | null;
  onDecide: (approvalId: string, approve: boolean, sudo?: SudoAnswer, approveCommandsForTask?: boolean) => void;
}) {
  const [sudo, setSudo] = useState<SudoAnswer | null>(null);
  const [approveCommandsForTask, setApproveRootForTask] = useState(false);
  const expired = Date.now() > item.expiresAtMs;
  const open = item.state === 'pending' && !expired;
  const asRoot = item.sudo !== null;
  const needsPassword = item.sudo === 'password_required';
  const ready = !needsPassword || sudo !== null;
  return (
    <div
      className={open || item.state === 'sending' ? 'tool-card approval' : 'tool-card'}
      role="group"
      aria-label="Approval required"
    >
      <div className="tool-head">
        <ShieldIcon size={13} />
        <span className="tool-title">
          Run this command{asRoot ? ' as root' : ''} on {hostLabel ?? 'the host'}?
        </span>
        {asRoot && (
          <span className="badge" data-tone="warning">
            root
          </span>
        )}
        {status && item.state === 'approved' && <StatusBadge item={status} />}
      </div>
      <pre className="tool-cmd">
        <code>{item.command}</code>
      </pre>
      <CommandOutput item={status} />
      {needsPassword && open && <SudoPassword approvalId={item.approvalId} onChange={setSudo} />}
      {asRoot && !needsPassword && open && (
        <p className="sudo-note">This host's sudo does not ask for a password.</p>
      )}
      {open && (
        <label className="check approval-task-root">
          <input
            type="checkbox"
            checked={approveCommandsForTask}
            onChange={(e) => setApproveRootForTask(e.target.checked)}
          />
          <span>Allow further commands, including root commands, in this request on this host</span>
        </label>
      )}
      <div className="approval-foot">
        <span className="grow">
          {item.state === 'sending' && 'Sending your decision…'}
          {item.state === 'approved' &&
            (approveCommandsForTask
              ? 'Commands are approved until this request ends.'
              : status && status.status !== 'proposed' ? 'Approved by you.' : 'Approved. Waiting for the result.')}
          {item.state === 'denied' && 'Denied. The command will not run.'}
          {item.state === 'pending' &&
            (expired ? 'Expired. The command will not run.' : `Expires at ${new Date(item.expiresAtMs).toLocaleTimeString()}`)}
        </span>
        {item.state === 'pending' && (
          <>
            <button type="button" className="btn btn-sm" disabled={!open} onClick={() => onDecide(item.approvalId, false)}>
              Deny
            </button>
            <button
              type="button"
              className="btn btn-primary btn-sm"
              disabled={!open || !ready}
              onClick={() => onDecide(item.approvalId, true, needsPassword && sudo ? sudo : undefined, approveCommandsForTask)}
            >
              {asRoot ? 'Approve as root' : 'Approve'}
            </button>
          </>
        )}
      </div>
      {item.error && (
        <p role="alert" className="banner" data-tone="danger" style={{ margin: '0 0.75rem 0.75rem' }}>
          {item.error}
        </p>
      )}
    </div>
  );
}

/**
 * Asks how to answer sudo's password question. The typed password stays in this
 * component until the approval is sent; the model never sees it, and saved
 * passwords never come back to the webview.
 */
function SudoPassword({ approvalId, onChange }: { approvalId: string; onChange: (answer: SudoAnswer | null) => void }) {
  const [options, setOptions] = useState<SudoOptions | null>(null);
  const [source, setSource] = useState<'typed' | 'saved' | 'login'>('typed');
  const [password, setPassword] = useState('');
  const [remember, setRemember] = useState(false);

  // The answer is handed to the card in the same update as each change, not from
  // an effect, so Approve always sees the current choice.
  function answer(next: 'typed' | 'saved' | 'login', typed = password, keep = remember) {
    setSource(next);
    onChange(next === 'typed' ? (typed ? { source: 'typed', password: typed, remember: keep } : null) : { source: next });
  }

  useEffect(() => {
    let active = true;
    sudoOptions(approvalId)
      .then((o) => {
        if (!active) return;
        setOptions(o);
        const first = o.savedPassword ? 'saved' : o.loginPassword ? 'login' : 'typed';
        setSource(first);
        onChange(first === 'typed' ? null : { source: first });
      })
      .catch(() => active && setOptions({ hostLabel: '', savedPassword: false, loginPassword: false }));
    return () => {
      active = false;
    };
    // The choice is made once per approval; onChange is stable enough for that.
  }, [approvalId]);

  return (
    <fieldset className="sudo-box">
      <legend>sudo asks for a password on this host</legend>
      {options?.savedPassword && (
        <label className="check">
          <input type="radio" name={`sudo-${approvalId}`} checked={source === 'saved'} onChange={() => answer('saved')} />
          <span>Use the saved sudo password</span>
        </label>
      )}
      {options?.loginPassword && (
        <label className="check">
          <input type="radio" name={`sudo-${approvalId}`} checked={source === 'login'} onChange={() => answer('login')} />
          <span>Use my SSH login password</span>
        </label>
      )}
      {(options?.savedPassword || options?.loginPassword) && (
        <label className="check">
          <input type="radio" name={`sudo-${approvalId}`} checked={source === 'typed'} onChange={() => answer('typed')} />
          <span>Type a password</span>
        </label>
      )}
      {source === 'typed' && (
        <>
          <input
            type="password"
            autoComplete="off"
            aria-label="Sudo password"
            placeholder="Sudo password"
            value={password}
            onChange={(e) => {
              setPassword(e.target.value);
              answer('typed', e.target.value, remember);
            }}
          />
          <label className="check">
            <input
              type="checkbox"
              checked={remember}
              onChange={(e) => {
                setRemember(e.target.checked);
                answer('typed', password, e.target.checked);
              }}
            />
            <span>Remember for this host (saved in the encrypted vault)</span>
          </label>
        </>
      )}
      <p className="hint">Sent only to sudo on the host. The AI never sees it.</p>
    </fieldset>
  );
}
