import { useEffect, useMemo, useRef, useState } from 'react';
import {
  chatSessionGet,
  chatSessionList,
  describeError,
  historyExport,
  historyPurgeOutput,
  historyQuery,
  historySummary,
  historyTasks,
  hostList,
  preferencesGet,
  preferencesSet,
  taskTimeline,
  type ChatSessionSummary,
  type ExecutionView,
  type ExportResult,
  type HistoryFilter,
  type HostUsageView,
  type HostView,
  type TaskView,
} from '../ipc';
import { formatCompact, formatCount, formatDuration, formatWhen, sumTokens } from '../ui/format';
import { HostAvatar } from '../ui/HostAvatar';
import { LocalEnvironmentAvatar } from '../ui/LocalEnvironmentIcon';
import { collectPages } from '../ui/collectPages';
import { loadLocalTargets } from '../ui/localTargets';
import { Drawer } from '../ui/Overlay';
import { ConfirmDialog } from '../ui/ConfirmDialog';
import { ChevronIcon, ClockIcon, RefreshIcon, SearchIcon, TerminalIcon } from '../ui/icons';
import { SelaiLogo } from '../ui/SelaiLogo';
import { ThemedSelect } from '../ui/ThemedSelect';
import { restoreTranscript, type ChatTurn } from './useChatSessions';

const STATES = ['completed', 'failed', 'denied', 'cancelled', 'interrupted', 'timed_out'] as const;

type View = 'tasks' | 'commands';

/** Local calendar date (yyyy-mm-dd) to epoch milliseconds, start or end of day. */
function dayBoundary(date: string, end: boolean): number | null {
  if (!date) return null;
  const [y, m, d] = date.split('-').map(Number);
  if (!y || !m || !d) return null;
  const at = end
    ? new Date(y, m - 1, d, 23, 59, 59, 999)
    : new Date(y, m - 1, d, 0, 0, 0, 0);
  return at.getTime();
}

function stateTone(state: string): string | undefined {
  if (state === 'completed') return 'success';
  if (['failed', 'denied', 'timed_out', 'interrupted'].includes(state)) return 'danger';
  if (state === 'cancelled' || state === 'awaiting_approval') return 'warning';
  return 'accent';
}

function stateText(state: string): string {
  return state.replace(/_/g, ' ');
}

/** Load metadata first; transcripts and audit records are read only on opening a session. */
export function HistoryTab({ visible }: { visible: boolean }) {
  const [sessions, setSessions] = useState<ChatSessionSummary[]>([]);
  const [selected, setSelected] = useState<ChatSessionSummary | null>(null);
  const [byHost, setByHost] = useState(false);
  const [query, setQuery] = useState('');
  const [sort, setSort] = useState<'latest' | 'oldest'>('latest');
  const [limit, setLimit] = useState(30);
  const [refresh, setRefresh] = useState(0);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!visible || byHost) return;
    let active = true;
    setLoading(true);
    setError(null);
    chatSessionList()
      .then((items) => active && setSessions(items))
      .catch((err: unknown) => active && setError(describeError(err)))
      .finally(() => active && setLoading(false));
    return () => { active = false; };
  }, [visible, byHost, refresh]);

  if (byHost) return (
    <div className="history-host-view">
      <button type="button" className="btn btn-ghost btn-sm history-back" onClick={() => setByHost(false)}>
        <ChevronIcon className="history-back-icon" size={14} /> All sessions
      </button>
      <HostHistoryTab visible={visible} />
    </div>
  );

  if (selected) return (
    <SessionHistory key={selected.id} session={selected} visible={visible} onBack={() => {
      setSelected(null);
      setRefresh((value) => value + 1);
    }} />
  );

  const matching = sessions
    .filter((session) => session.title.toLowerCase().includes(query.trim().toLowerCase()))
    .sort((a, b) => (sort === 'latest' ? -1 : 1)
      * (a.updatedAtMs - b.updatedAtMs || a.id.localeCompare(b.id)));
  return (
    <section aria-label="Session history" className="page history-page">
      <header className="page-head">
        <div className="grow">
          <h1>Session history</h1>
          <p>Open an AI session to review its requests and the commands executed on each server.</p>
        </div>
        <button type="button" className="btn btn-sm" onClick={() => setByHost(true)}>Browse by host</button>
      </header>
      <div className="view-bar session-history-toolbar">
        <label className="session-history-search">
          <SearchIcon size={15} />
          <input type="search" aria-label="Search sessions" placeholder="Search sessions by title" value={query}
            onChange={(event) => { setQuery(event.target.value); setLimit(30); }} />
        </label>
        <span className="grow" />
        <ThemedSelect label="Sort sessions by" value={sort} options={[
          { value: 'latest', label: 'Latest activity' },
          { value: 'oldest', label: 'Oldest activity' },
        ]} onChange={(value) => { setSort(value as 'latest' | 'oldest'); setLimit(30); }} />
        <span className="muted">{matching.length} session{matching.length === 1 ? '' : 's'}</span>
        <button type="button" className="icon-btn" aria-label="Refresh sessions" title="Refresh sessions"
          disabled={loading} onClick={() => setRefresh((value) => value + 1)}><RefreshIcon size={15} /></button>
      </div>
      {error && <p role="alert" className="banner" data-tone="danger">{error}</p>}
      {loading && <p role="status" className="history-loading"><span className="spinner" /> Loading sessions…</p>}
      {!loading && !error && matching.length === 0 && <Empty text={query.trim()
        ? 'No sessions match this title.' : 'Saved AI sessions appear here. Older command records are available in Browse by host.'} />}
      <ul className="session-history-list" aria-label="Sessions" aria-busy={loading}>
        {matching.slice(0, limit).map((session) => (
          <li key={session.id}>
            <button type="button" className="session-history-row" onClick={() => setSelected(session)}>
              <span className="session-history-mark"><SelaiLogo size={23} /></span>
              <span className="session-history-title"><strong>{session.title}</strong>
                <span className="meta">Created {formatWhen(session.createdAtMs)}</span></span>
              <span className="session-history-updated"><span>Last activity</span><time dateTime={new Date(session.updatedAtMs).toISOString()}>
                {formatWhen(session.updatedAtMs)}</time></span>
              <ChevronIcon className="history-forward-icon" size={16} />
            </button>
          </li>
        ))}
      </ul>
      {matching.length > limit && <div className="history-pagination">
        <button type="button" className="btn" onClick={() => setLimit((value) => value + 30)}>Load more sessions</button>
        <button type="button" className="btn btn-ghost" onClick={() => setLimit(matching.length)}>Load all sessions</button>
      </div>}
    </section>
  );
}

type CommandResult = { rows: ExecutionView[]; error?: undefined } | { rows?: undefined; error: string };
const REQUESTS_PER_PAGE = 12;

function SessionHistory({ session, visible, onBack }: { session: ChatSessionSummary; visible: boolean; onBack: () => void }) {
  const [record, setRecord] = useState(session);
  const [turns, setTurns] = useState<ChatTurn[] | null>(null);
  const [limit, setLimit] = useState(REQUESTS_PER_PAGE);
  const [sort, setSort] = useState<'latest' | 'oldest'>('latest');
  const orderedTurns = useMemo(() => turns && (sort === 'latest' ? [...turns].reverse() : turns), [turns, sort]);
  const [results, setResults] = useState<Record<string, CommandResult>>({});
  const cache = useRef(new Map<string, CommandResult>());
  const transcriptReady = useRef(false);
  const [error, setError] = useState<string | null>(null);
  const [refresh, setRefresh] = useState(0);
  const [retry, setRetry] = useState(0);
  const [loading, setLoading] = useState(true);
  const [selected, setSelected] = useState<ExecutionView | null>(null);

  useEffect(() => {
    if (!visible) return;
    let active = true;
    transcriptReady.current = false;
    setTurns(null);
    setResults({});
    cache.current.clear();
    setLimit(REQUESTS_PER_PAGE);
    setSelected(null);
    setLoading(true);
    setError(null);
    chatSessionGet(session.id).then((saved) => {
      if (!active) return;
      const restored = restoreTranscript(saved.transcript);
      const seen = new Set<string>();
      transcriptReady.current = true;
      // Multiple UI entries can reference the same task; show its commands once.
      setTurns(restored.filter((turn) => {
        if (!turn.taskId) return true;
        if (seen.has(turn.taskId)) return false;
        seen.add(turn.taskId);
        return true;
      }));
      setRecord(saved);
    }).catch((err: unknown) => active && setError(describeError(err)))
      .finally(() => active && setLoading(false));
    return () => { active = false; transcriptReady.current = false; };
  }, [visible, session.id, refresh]);

  useEffect(() => {
    if (!visible || !orderedTurns || !transcriptReady.current) return;
    let active = true;
    const ids = [...new Set(orderedTurns.slice(0, limit).flatMap((turn) => turn.taskId ? [turn.taskId] : []))]
      .filter((id) => !cache.current.has(id));
    // Keep IPC concurrency bounded, including when loading every request.
    void (async () => {
      for (let offset = 0; offset < ids.length && active; offset += 4) {
        const batch = await Promise.all(ids.slice(offset, offset + 4).map(async (id): Promise<[string, CommandResult]> => {
          try { return [id, { rows: await taskTimeline(id) }]; }
          catch (err) { return [id, { error: describeError(err) }]; }
        }));
        if (!active) return;
        batch.forEach(([id, result]) => cache.current.set(id, result));
        setResults(Object.fromEntries(cache.current));
      }
    })();
    return () => { active = false; };
  }, [visible, orderedTurns, limit, retry]);

  const shown = orderedTurns?.slice(0, limit) ?? [];
  const commands = Object.values(results).flatMap((result) => result.rows ?? []);
  const pending = shown.some((turn) => turn.taskId && !results[turn.taskId]);
  const hosts = new Set(turns?.flatMap((turn) => turn.hostId ? [turn.hostId] : []) ?? []);

  return (
    <section aria-label="Session commands" className="page history-page">
      <button type="button" className="btn btn-ghost btn-sm history-back" onClick={onBack}>
        <ChevronIcon className="history-back-icon" size={14} /> All sessions
      </button>
      <header className="page-head">
        <span className="session-history-mark"><SelaiLogo size={26} /></span>
        <div className="grow session-history-heading"><h1>{record.title}</h1>
          <p>Created {formatWhen(record.createdAtMs)} · Last activity {formatWhen(record.updatedAtMs)}</p></div>
        <button type="button" className="icon-btn" aria-label="Refresh session commands" title="Refresh session commands"
          disabled={loading || pending} onClick={() => setRefresh((value) => value + 1)}><RefreshIcon size={15} /></button>
      </header>
      {error && <p role="alert" className="banner" data-tone="danger">{error}</p>}
      {loading && <p role="status" className="history-loading"><span className="spinner" /> Loading session…</p>}
      {turns && <>
        <div className="session-history-summary">
          <span>{turns.length} request{turns.length === 1 ? '' : 's'}</span><span>{hosts.size} server{hosts.size === 1 ? '' : 's'}</span>
          <span>{commands.length} command{commands.length === 1 ? '' : 's'} loaded</span>
        </div>
        <div className="session-commands-toolbar">
          <h2 className="session-commands-title">Command history</h2>
          <ThemedSelect label="Sort by" value={sort} disabled={pending} options={[
            { value: 'latest', label: 'Latest first' },
            { value: 'oldest', label: 'Oldest first' },
          ]} onChange={(value) => { setSort(value as 'latest' | 'oldest'); }} />
        </div>
        {turns.length === 0 && <Empty text="This session has no requests or commands yet." />}
        <ol className="session-request-list" aria-label="Session requests" aria-busy={pending}>
          {shown.map((turn, index) => {
            const result = turn.taskId ? results[turn.taskId] : undefined;
            return (
              <li key={turn.id} className="card session-request">
                <header className="session-request-head"><span className="session-request-number">{index + 1}</span>
                  <h3>{turn.taskPrompt || turn.prompt || 'AI request'}</h3>
                  <span className="badge">{turn.hostLabel || 'Chat'}</span></header>
                {turn.taskId && !result && <p role="status" className="history-loading"><span className="spinner" /> Loading command records…</p>}
                {result?.error !== undefined && <div className="session-request-error">
                  <p role="alert">Could not load commands: {result.error}</p>
                  <button type="button" className="btn btn-sm" disabled={pending} onClick={() => {
                    cache.current.delete(turn.taskId!);
                    setResults((previous) => { const next = { ...previous }; delete next[turn.taskId!]; return next; });
                    setRetry((value) => value + 1);
                  }}>Retry commands</button>
                </div>}
                {(!turn.taskId || result?.rows?.length === 0) && <p className="session-request-empty">No command execution recorded for this request.</p>}
                {result?.rows && result.rows.length > 0 && <ul className="session-command-list" aria-label={`Commands for request ${index + 1}`}>
                  {[...result.rows].sort((a, b) => (sort === 'latest' ? -1 : 1)
                    * (a.createdAtMs - b.createdAtMs || a.id.localeCompare(b.id))).map((row) => <li key={row.id}>
                    <button type="button" className="session-command" onClick={() => setSelected(row)}>
                      <TerminalIcon size={15} /><span className="session-command-main"><code>{row.commandRedacted}</code>
                        <span className="meta">{formatWhen(row.createdAtMs)} · {row.modelId}{row.exitCode !== null ? ` · exit ${row.exitCode}` : ''}</span></span>
                      <span className="badge" data-tone={stateTone(row.state)}>{stateText(row.state)}</span>
                      <ChevronIcon className="history-forward-icon" size={14} />
                    </button>
                  </li>)}
                </ul>}
              </li>
            );
          })}
        </ol>
        {turns.length > limit && <div className="history-pagination">
          <button type="button" className="btn" disabled={pending}
            onClick={() => setLimit((value) => value + REQUESTS_PER_PAGE)}>Load more requests</button>
          <button type="button" className="btn btn-ghost" disabled={pending}
            onClick={() => setLimit(turns.length)}>Load all requests</button>
        </div>}
      </>}
      {selected && <Drawer title="Execution" subtitle={formatWhen(selected.createdAtMs)} onClose={() => setSelected(null)}>
        <section aria-label="Execution details"><ExecutionDetails execution={selected} /></section>
      </Drawer>}
    </section>
  );
}

/** Host audit remains available for older tasks, exports and output retention. */
export function HostHistoryTab({ visible }: { visible: boolean }) {
  const [hosts, setHosts] = useState<HostView[]>([]);
  const [hostId, setHostId] = useState('');
  const [view, setView] = useState<View>('tasks');
  const [summary, setSummary] = useState<HostUsageView | null>(null);
  const [tasks, setTasks] = useState<TaskView[]>([]);
  const [tasksCursor, setTasksCursor] = useState<string | null>(null);
  const [openTask, setOpenTask] = useState<TaskView | null>(null);
  const [states, setStates] = useState<string[]>([]);
  const [from, setFrom] = useState('');
  const [to, setTo] = useState('');
  const [rows, setRows] = useState<ExecutionView[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [selected, setSelected] = useState<ExecutionView | null>(null);
  const [timeline, setTimeline] = useState<ExecutionView[] | null>(null);
  const [retention, setRetention] = useState(30);
  const [notice, setNotice] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [loadingAll, setLoadingAll] = useState(false);
  const [commandSort, setCommandSort] = useState<'latest' | 'oldest'>('latest');
  const [purging, setPurging] = useState<{ id: string; label: string } | null>(null);

  useEffect(() => {
    let active = true;
    Promise.all([loadLocalTargets().catch(() => []), collectPages((cursor) => hostList(undefined, true, cursor))])
      .then(([local, items]) => {
        if (!active) return;
        const all = [...items, ...local];
        setHosts(all);
        setHostId((current) => current || all[0]?.id || '');
      })
      .catch((err: unknown) => active && setError(describeError(err)));
    preferencesGet()
      .then((prefs) => {
        if (!active) return;
        setRetention(prefs.outputRetentionDays);
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, []);

  function currentFilter(): HistoryFilter {
    return { states, sinceMs: dayBoundary(from, false), untilMs: dayBoundary(to, true) };
  }

  async function search(append = false) {
    if (!hostId) return;
    setError(null);
    try {
      const page = await historyQuery(hostId, currentFilter(), append ? nextCursor ?? undefined : undefined);
      setRows((prev) => (append ? [...prev, ...page.items] : page.items));
      setNextCursor(page.nextCursor);
      if (!append) {
        setSelected(null);
        setTimeline(null);
      }
    } catch (err) {
      setError(describeError(err));
    }
  }

  async function loadTasks(append = false) {
    if (!hostId) return;
    try {
      const page = await historyTasks(hostId, append ? tasksCursor ?? undefined : undefined);
      setTasks((prev) => (append ? [...prev, ...page.items] : page.items));
      setTasksCursor(page.nextCursor);
    } catch (err) {
      setError(describeError(err));
    }
  }

  async function loadAllTasks() {
    if (!hostId || !tasksCursor || loadingAll) return;
    setLoadingAll(true); setError(null);
    try {
      let cursor: string | undefined = tasksCursor;
      const rest: TaskView[] = [];
      while (cursor) {
        const page = await historyTasks(hostId, cursor);
        rest.push(...page.items);
        cursor = page.nextCursor ?? undefined;
      }
      setTasks((current) => [...current, ...rest]);
      setTasksCursor(null);
    } catch (err) { setError(describeError(err)); }
    finally { setLoadingAll(false); }
  }

  async function loadAllCommands() {
    if (!hostId || !nextCursor || loadingAll) return;
    setLoadingAll(true); setError(null);
    try {
      let cursor: string | undefined = nextCursor;
      const rest: ExecutionView[] = [];
      while (cursor) {
        const page = await historyQuery(hostId, currentFilter(), cursor);
        rest.push(...page.items);
        cursor = page.nextCursor ?? undefined;
      }
      setRows((current) => [...current, ...rest]);
      setNextCursor(null);
    } catch (err) { setError(describeError(err)); }
    finally { setLoadingAll(false); }
  }

  const sortedRows = commandSort === 'latest' ? rows : [...rows].reverse();

  async function loadSummary() {
    if (!hostId) return;
    try {
      setSummary(await historySummary(hostId));
    } catch (err) {
      setError(describeError(err));
    }
  }

  // Load when the tab is shown or the host changes. The command filter is
  // applied by the Apply button, not on every change.
  useEffect(() => {
    if (!visible || !hostId) return;
    setNotice(null);
    void Promise.all([loadSummary(), loadTasks(false), search(false)]).finally(() => setLoaded(true));
  }, [visible, hostId]);

  function toggleState(state: string) {
    setStates((prev) => (prev.includes(state) ? prev.filter((s) => s !== state) : [...prev, state]));
  }

  async function openExecution(row: ExecutionView) {
    setSelected(row);
    try {
      setTimeline(await taskTimeline(row.taskId));
    } catch (err) {
      setError(describeError(err));
    }
  }

  async function exportHistory() {
    setError(null);
    try {
      const result: ExportResult = await historyExport(hostId, currentFilter());
      const parts = [`Exported ${result.executions} row(s) to ${result.path}.`];
      if (result.truncatedRows) parts.push('The export was cut at the row limit.');
      if (result.extraRedactions > 0) parts.push(`${result.extraRedactions} secret-like value(s) were redacted again.`);
      setNotice(parts.join(' '));
    } catch (err) {
      setError(describeError(err));
    }
  }

  async function purge(id: string) {
    const result = await historyPurgeOutput(id);
    setNotice(`Output cleared for ${result.executions} command(s). Command records are kept.`);
    await search(false);
  }

  async function saveRetention() {
    try {
      const prefs = await preferencesSet({ outputRetentionDays: retention });
      setRetention(prefs.outputRetentionDays);
      setNotice(`Output is kept for ${prefs.outputRetentionDays} day(s).`);
    } catch (err) {
      setError(describeError(err));
    }
  }

  const host = hosts.find((h) => h.id === hostId);
  const maxTotal = Math.max(1, ...tasks.map((t) => sumTokens(t.inputTokens, t.outputTokens) ?? 0));

  return (
    <section aria-label="AI history" className="page history-page">
      <header className="page-head">
        {host && (host.kind === 'local' ? <LocalEnvironmentAvatar host={host} size={24} /> : <HostAvatar label={host.label} />)}
        <div className="grow">
          <h1>History</h1>
          <p>What the AI Agent did on a host: requests, commands and token usage. Archived hosts keep their history.</p>
        </div>
        <label className="field host-select">
          Host
          <ThemedSelect label="Host" value={hostId} options={hosts.length === 0 ? [{ value: '', label: 'No hosts' }] : hosts.map((h) => ({ value: h.id, label: `${h.label}${h.archived ? ' (archived)' : ''}` }))} onChange={setHostId} />
        </label>
      </header>

      {notice && (
        <p role="status" className="banner" data-tone="success">
          {notice}
        </p>
      )}
      {error && (
        <p role="alert" className="banner" data-tone="danger">
          {error}
        </p>
      )}

      <div className="stats" role="group" aria-label="Usage summary">
        <Stat label="AI tasks" value={formatCount(summary?.tasks ?? null)} />
        <Stat
          label="Commands"
          value={formatCount(summary?.executions ?? null)}
          sub={summary && summary.failedExecutions > 0 ? `${summary.failedExecutions} failed` : undefined}
          subTone="danger"
        />
        <Stat label="Input tokens" value={formatCount(summary?.inputTokens ?? null)} />
        <Stat label="Output tokens" value={formatCount(summary?.outputTokens ?? null)} />
        <Stat
          label="Last activity"
          value={summary?.lastActivityMs ? new Date(summary.lastActivityMs).toLocaleDateString() : '—'}
          sub={summary?.lastActivityMs ? new Date(summary.lastActivityMs).toLocaleTimeString() : undefined}
        />
      </div>

      <div className="view-bar">
        <div className="segmented" role="group" aria-label="History view">
          <button type="button" aria-pressed={view === 'tasks'} onClick={() => setView('tasks')}>
            Tasks
          </button>
          <button type="button" aria-pressed={view === 'commands'} onClick={() => setView('commands')}>
            Commands
          </button>
        </div>
        <span className="grow" />
        <button type="button" className="btn btn-sm" onClick={exportHistory} disabled={!hostId}>
          Export encrypted
        </button>
        <button type="button" className="btn btn-ghost btn-sm" onClick={() => setPurging({ id: hostId, label: hosts.find((host) => host.id === hostId)?.label ?? 'this host' })} disabled={!hostId}>
          Clear stored output
        </button>
      </div>

      {view === 'tasks' && (
        <>
          {loaded && tasks.length === 0 ? (
            <Empty text="Tasks you start from the AI Agent on this host appear here with their token usage." />
          ) : (
            <ul className="task-list" aria-label="Tasks">
              {tasks.map((t) => {
                const total = sumTokens(t.inputTokens, t.outputTokens);
                return (
                  <li key={t.id}>
                    <button type="button" className="task-row" onClick={() => setOpenTask(t)}>
                      <span className="task-icon" data-tone={stateTone(t.state)}>
                        <SelaiLogo size={20} />
                      </span>
                      <span className="task-main">
                        <strong>{t.promptPreview ?? 'Request not recorded (task from an older version)'}</strong>
                        <span className="meta">
                          {formatWhen(t.createdAtMs)} · {t.modelId} · {t.executions} command
                          {t.executions === 1 ? '' : 's'}
                          {t.finishedAtMs ? ` · ${formatDuration(t.finishedAtMs - t.createdAtMs)}` : ''}
                        </span>
                      </span>
                      <span
                        className="task-tokens"
                        title={
                          total === null
                            ? 'The provider did not report token usage'
                            : `${formatCount(t.inputTokens)} input · ${formatCount(t.outputTokens)} output tokens`
                        }
                      >
                        <span className="tok">
                          <b>{formatCompact(t.inputTokens)}</b> in
                        </span>
                        <span className="tok">
                          <b>{formatCompact(t.outputTokens)}</b> out
                        </span>
                        <span className="tok-bar" aria-hidden="true">
                          <span style={{ width: `${Math.round(((total ?? 0) / maxTotal) * 100)}%` }} />
                        </span>
                      </span>
                      <span className="badge" data-tone={stateTone(t.state)}>
                        {stateText(t.state)}
                      </span>
                    </button>
                  </li>
                );
              })}
            </ul>
          )}
          {tasksCursor && (
            <div className="toolbar-row">
              <button type="button" className="btn" disabled={loadingAll} onClick={() => loadTasks(true)}>
                Load more
              </button>
              <button type="button" className="btn btn-ghost" disabled={loadingAll} onClick={() => void loadAllTasks()}>
                {loadingAll ? 'Loading…' : 'Load all'}
              </button>
            </div>
          )}
        </>
      )}

      {view === 'commands' && (
        <>
          <div className="card filters-bar">
            <label className="field">
              From
              <input type="date" value={from} onChange={(e) => setFrom(e.target.value)} />
            </label>
            <label className="field">
              To
              <input type="date" value={to} onChange={(e) => setTo(e.target.value)} />
            </label>
            <fieldset className="state-filters">
              <legend>State</legend>
              {STATES.map((state) => (
                <label key={state} className="pill-check">
                  <input type="checkbox" checked={states.includes(state)} onChange={() => toggleState(state)} />
                  {stateText(state)}
                </label>
              ))}
            </fieldset>
            <label className="field history-sort">
              Sort by
              <ThemedSelect label="Sort by" value={commandSort} options={[{ value: 'latest', label: 'Latest command first' }, { value: 'oldest', label: 'Oldest command first' }]}
                onChange={(value) => { setCommandSort(value as 'latest' | 'oldest'); if (nextCursor) void loadAllCommands(); }} />
            </label>
            <span className="grow" />
            <button type="button" className="btn btn-primary" onClick={() => search(false)} disabled={!hostId}>
              Apply
            </button>
          </div>

          {rows.length === 0 ? (
            <Empty text="Commands the AI Agent runs on this host appear here with their output and exit code." />
          ) : (
            <div className="card table-card">
              <table className="history-table">
                <thead>
                  <tr>
                    <th scope="col">Time</th>
                    <th scope="col">Command</th>
                    <th scope="col">State</th>
                    <th scope="col">Exit</th>
                  </tr>
                </thead>
                <tbody>
                  {sortedRows.map((row) => (
                    <tr key={row.id} onClick={() => openExecution(row)} aria-selected={selected?.id === row.id}>
                      <td className="muted" style={{ whiteSpace: 'nowrap' }}>
                        {formatWhen(row.createdAtMs)}
                      </td>
                      <td>
                        <code>{row.commandRedacted}</code>
                      </td>
                      <td>
                        <span className="badge" data-tone={stateTone(row.state)}>
                          {stateText(row.state)}
                        </span>
                      </td>
                      <td className="mono">{row.exitCode ?? '—'}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          {nextCursor && (
            <div className="toolbar-row">
              <button type="button" className="btn" disabled={loadingAll} onClick={() => search(true)}>
                Load more
              </button>
              <button type="button" className="btn btn-ghost" disabled={loadingAll} onClick={() => void loadAllCommands()}>
                {loadingAll ? 'Loading…' : 'Load all'}
              </button>
            </div>
          )}
        </>
      )}

      <div className="card retention">
        <label className="field" style={{ width: 200 }}>
          Keep output for (days)
          <input
            type="number"
            min={1}
            max={3650}
            value={retention}
            onChange={(e) => setRetention(Number(e.target.value))}
          />
        </label>
        <button type="button" className="btn" onClick={saveRetention}>
          Save retention
        </button>
        <span className="muted">Older output is removed automatically. Command records and token counts are kept.</span>
      </div>

      {purging && <ConfirmDialog title="Clear stored output?" confirmLabel="Clear output" pendingLabel="Clearing…"
        onConfirm={() => purge(purging.id)} onClose={() => setPurging(null)}>
        <p>Delete stored command output for <strong>{purging.label}</strong>?</p>
        <p>Command history will be kept. Deleted output cannot be recovered.</p>
      </ConfirmDialog>}
      {openTask && <TaskDrawer task={openTask} onClose={() => setOpenTask(null)} />}

      {selected && (
        <Drawer
          title="Execution"
          subtitle={formatWhen(selected.createdAtMs)}
          onClose={() => {
            setSelected(null);
            setTimeline(null);
          }}
        >
          <section aria-label="Execution details">
            <ExecutionDetails execution={selected} />
            {timeline && (
              <>
                <h3>Task timeline</h3>
                <ol className="timeline">
                  {timeline.map((step) => (
                    <li key={step.id}>
                      <span>
                        <code>{step.commandRedacted}</code> — {stateText(step.state)}
                        {step.exitCode !== null ? `, exit ${step.exitCode}` : ''}
                      </span>
                    </li>
                  ))}
                </ol>
              </>
            )}
          </section>
        </Drawer>
      )}
    </section>
  );
}

function Stat({ label, value, sub, subTone }: { label: string; value: string; sub?: string; subTone?: string }) {
  return (
    <div className="stat card">
      <span className="stat-label">{label}</span>
      <span className="stat-value">{value}</span>
      {sub && (
        <span className="stat-sub" data-tone={subTone}>
          {sub}
        </span>
      )}
    </div>
  );
}

function Empty({ text }: { text: string }) {
  return (
    <div className="empty">
      <span className="empty-icon">
        <ClockIcon size={26} />
      </span>
      <h2>Nothing here yet</h2>
      <p>{text}</p>
    </div>
  );
}

function ExecutionDetails({ execution }: { execution: ExecutionView }) {
  return (
    <>
      <pre className="output" style={{ marginTop: 0 }}>
        <code>{execution.commandRedacted}</code>
      </pre>
      <dl className="details-list">
        <dt>State</dt>
        <dd>
          <span className="badge" data-tone={stateTone(execution.state)}>
            {stateText(execution.state)}
          </span>
        </dd>
        <dt>Exit code</dt>
        <dd>{execution.exitCode ?? '—'}</dd>
        <dt>Tool</dt>
        <dd>{execution.tool}</dd>
        <dt>Model</dt>
        <dd>{execution.modelId}</dd>
        <dt>Policy</dt>
        <dd>{execution.policyDecision ?? '—'}</dd>
        <dt>Command digest (SHA-256)</dt>
        <dd>
          <code>{execution.commandSha256}</code>
        </dd>
        <dt>Error</dt>
        <dd>{execution.errorCode ?? '—'}</dd>
      </dl>
      {execution.outputTruncated && (
        <p role="note" className="banner" data-tone="warning">
          The stored output was cut at its limit. It is not the full output.
        </p>
      )}
      {execution.stdoutPreview && (
        <>
          <h3>Output (stored preview)</h3>
          <pre className="output">{execution.stdoutPreview}</pre>
        </>
      )}
      {execution.stderrPreview && (
        <>
          <h3>Errors (stored preview)</h3>
          <pre className="output">{execution.stderrPreview}</pre>
        </>
      )}
    </>
  );
}

/** One task: the request, its token usage, and every command it ran. */
function TaskDrawer({ task, onClose }: { task: TaskView; onClose: () => void }) {
  const [steps, setSteps] = useState<ExecutionView[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let active = true;
    taskTimeline(task.id)
      .then((rows) => active && setSteps(rows))
      .catch((err: unknown) => active && setError(describeError(err)));
    return () => {
      active = false;
    };
  }, [task.id]);

  const total = sumTokens(task.inputTokens, task.outputTokens);

  return (
    <Drawer title="Task" subtitle={formatWhen(task.createdAtMs)} onClose={onClose}>
      <section aria-label="Task details">
        <p className="task-request">{task.promptPreview ?? 'The request was not recorded for this task.'}</p>
        <div className="token-grid" role="group" aria-label="Token usage">
          <div>
            <span>Input</span>
            <b>{formatCount(task.inputTokens)}</b>
          </div>
          <div>
            <span>Output</span>
            <b>{formatCount(task.outputTokens)}</b>
          </div>
          <div>
            <span>Total</span>
            <b>{formatCount(total)}</b>
          </div>
          <div>
            <span>Model turns</span>
            <b>{task.modelTurns}</b>
          </div>
        </div>
        {total === null && task.modelTurns > 0 && (
          <p className="muted" style={{ marginBottom: '1rem' }}>
            The provider did not report token usage for this task.
          </p>
        )}
        <dl className="details-list">
          <dt>State</dt>
          <dd>
            <span className="badge" data-tone={stateTone(task.state)}>
              {stateText(task.state)}
            </span>
          </dd>
          <dt>Model</dt>
          <dd>{task.modelId}</dd>
          <dt>Duration</dt>
          <dd>{task.finishedAtMs ? formatDuration(task.finishedAtMs - task.createdAtMs) : '—'}</dd>
          <dt>Error</dt>
          <dd>{task.errorCode ?? '—'}</dd>
        </dl>
        <h3>Commands</h3>
        {error && (
          <p role="alert" className="banner" data-tone="danger">
            {error}
          </p>
        )}
        {steps && steps.length === 0 && <p className="muted">This task ran no commands.</p>}
        <ol className="step-list">
          {steps?.map((step) => (
            <li key={step.id} className="step">
              <div className="step-head">
                <TerminalIcon size={13} />
                <code>{step.commandRedacted}</code>
                <span className="grow" />
                <span className="badge" data-tone={stateTone(step.state)}>
                  {stateText(step.state)}
                  {step.exitCode !== null ? ` · exit ${step.exitCode}` : ''}
                </span>
              </div>
              {(step.stdoutPreview || step.stderrPreview) && (
                <details>
                  <summary>Output</summary>
                  {step.stdoutPreview && <pre className="output">{step.stdoutPreview}</pre>}
                  {step.stderrPreview && <pre className="output">{step.stderrPreview}</pre>}
                </details>
              )}
            </li>
          ))}
        </ol>
      </section>
    </Drawer>
  );
}
