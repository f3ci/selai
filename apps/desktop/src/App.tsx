import { lazy, Suspense, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import { isTauri } from '@tauri-apps/api/core';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { AgentPanel } from './tabs/AgentPanel';
import { HistoryTab } from './tabs/HistoryTab';
import { HostsSidebar } from './tabs/HostsSidebar';
import { ProvidersPanel } from './tabs/ProvidersPanel';
import { appInfo, describeError, localHost, storageStatus, type AppInfo, type HostView } from './ipc';
import type { SessionStatus, TerminalSessionTab } from './tabs/TerminalTab';
import { ClockIcon, CloseIcon, KeyIcon, MaximizeIcon, MinimizeIcon, PlugIcon, PlusIcon, ServerIcon, TerminalIcon, FilesIcon, ForwardIcon, ProxyIcon } from './ui/icons';
import { SelaiLogo } from './ui/SelaiLogo';
import { VaultGate } from './VaultGate';
import { VaultWorkspacePhase } from './vaultWorkspace';
import { applyTheme, readThemePreference, THEME_KEY, THEME_OPTIONS, useResolvedTheme, type Theme } from './theme';
import { ThemedSelect } from './ui/ThemedSelect';

// xterm.js is large; load it only when a terminal is first needed.
const TerminalStack = lazy(() => import('./tabs/TerminalTab').then((m) => ({ default: m.TerminalStack })));
const SshKeysPage = lazy(() => import('./tabs/SshKeysPage').then((m) => ({ default: m.SshKeysPage })));

const SftpPage = lazy(() => import('./tabs/SftpPage').then((m) => ({ default: m.SftpPage })));
const ForwardingPage = lazy(() => import('./tabs/ForwardingPage').then((m) => ({ default: m.ForwardingPage })));
const ProxiesPage = lazy(() => import('./tabs/ProxiesPage').then((m) => ({ default: m.ProxiesPage })));

/** A tab in the centre area: a terminal, or one of the pages that open as a tab. */
type PageId = 'history' | 'providers' | 'keys' | 'sftp' | 'forwarding' | 'proxies';
type EditorTab = ({ kind: 'terminal' } & TerminalSessionTab) | { kind: PageId; key: PageId };

const PAGE_TITLES: Record<PageId, string> = { history: 'History', providers: 'Providers', keys: 'SSH Keys', sftp: 'SFTP', forwarding: 'Port forwarding', proxies: 'Proxies' };

let nextTabNumber = 0;

const TERMINAL_FONT_KEY = 'selai.terminal.fontSize';
const MIN_TERMINAL_FONT = 9;
const MAX_TERMINAL_FONT = 24;

/** Display preferences are per device, so browser storage is enough. */
function stored(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key: string, value: string) {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // Not saved; the choice still applies until the app closes.
  }
}

function storedTerminalFontSize(): number {
  const saved = Number(stored(TERMINAL_FONT_KEY));
  return Number.isFinite(saved) && saved >= MIN_TERMINAL_FONT && saved <= MAX_TERMINAL_FONT ? saved : 13.5;
}

/** A panel width the operator drags, remembered across starts. */
function usePanelWidth(key: string, initial: number, min: number, max: number) {
  const [width, setWidth] = useState(() => {
    const saved = Number(stored(key));
    return Number.isFinite(saved) && saved >= min && saved <= max ? saved : initial;
  });
  const clamp = useCallback((w: number) => Math.round(Math.min(max, Math.max(min, w))), [min, max]);
  const commit = useCallback((w: number) => store(key, String(w)), [key]);
  const set = (w: number) => {
    const next = clamp(w);
    setWidth(next);
    return next;
  };
  return { width, setWidth: set, commit };
}

function Resizer({
  label,
  width,
  setWidth,
  commit,
  direction,
}: {
  label: string;
  width: number;
  setWidth: (w: number) => number;
  commit: (w: number) => void;
  /** 1 when dragging right widens the panel (left sidebar), -1 for the right panel. */
  direction: 1 | -1;
}) {
  const drag = useRef<{ x: number; start: number; now: number } | null>(null);
  const frame = useRef<number | null>(null);
  useEffect(() => () => {
    if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    document.body.classList.remove('resizing');
  }, []);
  const finishDrag = () => {
    if (frame.current !== null) window.cancelAnimationFrame(frame.current);
    frame.current = null;
    if (drag.current) commit(setWidth(drag.current.now));
    drag.current = null;
    document.body.classList.remove('resizing');
  };
  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={width}
      tabIndex={0}
      className="resizer"
      onPointerDown={(e) => {
        e.currentTarget.setPointerCapture(e.pointerId);
        drag.current = { x: e.clientX, start: width, now: width };
        document.body.classList.add('resizing');
      }}
      onPointerMove={(e) => {
        const d = drag.current;
        if (!d) return;
        d.now = d.start + direction * (e.clientX - d.x);
        if (frame.current === null) frame.current = window.requestAnimationFrame(() => {
          frame.current = null;
          if (drag.current) drag.current.now = setWidth(drag.current.now);
        });
      }}
      onPointerUp={finishDrag}
      onPointerCancel={finishDrag}
      onKeyDown={(e) => {
        const step = e.key === 'ArrowRight' ? 16 : e.key === 'ArrowLeft' ? -16 : 0;
        if (!step) return;
        e.preventDefault();
        commit(setWidth(width + direction * step));
      }}
    />
  );
}

export function App() { return <VaultGate><Workbench /></VaultGate>; }

export function Workbench() {
  const vaultPhase = useContext(VaultWorkspacePhase);
  const workspaceAvailable = vaultPhase !== 'locked';
  const [info, setInfo] = useState<AppInfo | null>(null);
  const [storage, setStorage] = useState<{ ok: boolean; text: string }>({ ok: true, text: 'Checking database…' });
  const [tabs, setTabs] = useState<EditorTab[]>([]);
  const tabsRef = useRef(tabs);
  tabsRef.current = tabs;
  const [activeKey, setActiveKey] = useState<string | null>(null);
  /** The terminal focused last; the chat follows its host. */
  const [lastTerminal, setLastTerminal] = useState<string | null>(null);
  const [statuses, setStatuses] = useState<Record<string, SessionStatus>>({});
  const statusRef = useRef<Record<string, SessionStatus>>({});
  const [editRequest, setEditRequest] = useState<{ hostId: string; n: number } | null>(null);
  const [profilePaused, setProfilePaused] = useState(false);
  const [theme, setTheme] = useState<Theme>(readThemePreference);
  const terminalTheme = useResolvedTheme(theme);
  const [terminalFontSize, setTerminalFontSize] = useState(storedTerminalFontSize);
  const [sidebarOpen, setSidebarOpen] = useState(() => stored('selai.sidebar') !== 'closed');
  const [chatOpen, setChatOpen] = useState(() => {
    const saved = stored('selai.chat');
    return saved === null ? window.innerWidth > 1100 : saved !== 'closed';
  });
  const side = usePanelWidth('selai.sidebar.width', 264, 220, 420);
  const chat = usePanelWidth('selai.chat.width', 380, 300, 1200);
  const [viewportWidth, setViewportWidth] = useState(() => window.innerWidth);
  useEffect(() => {
    const update = () => setViewportWidth(window.innerWidth);
    window.addEventListener('resize', update);
    return () => window.removeEventListener('resize', update);
  }, []);
  const maxChatWidth = Math.max(300, viewportWidth - 68 - (viewportWidth > 1100 && sidebarOpen ? side.width : 0) - (viewportWidth > 1100 ? 260 : 0));
  const chatWidth = Math.min(chat.width, maxChatWidth);

  useEffect(() => {
    applyTheme(theme);
    store(THEME_KEY, theme);
  }, [theme]);

  useEffect(() => store('selai.sidebar', sidebarOpen ? 'open' : 'closed'), [sidebarOpen]);
  useEffect(() => store('selai.chat', chatOpen ? 'open' : 'closed'), [chatOpen]);
  useEffect(() => store(TERMINAL_FONT_KEY, String(terminalFontSize)), [terminalFontSize]);
  const changeTerminalFontSize = useCallback((step: number) => {
    setTerminalFontSize((size) => Math.min(MAX_TERMINAL_FONT, Math.max(MIN_TERMINAL_FONT, size + step)));
  }, []);

  // Panel shortcuts, caught before the terminal sees them. Plain Ctrl+B and Ctrl+L
  // belong to the shell, so these use Shift.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (vaultPhase !== 'unlocked') return;
      if (!(e.ctrlKey || e.metaKey) || !e.shiftKey || e.altKey) return;
      const key = e.key.toLowerCase();
      if (key === 'e') {
        e.preventDefault();
        setSidebarOpen((o) => !o);
      } else if (key === 'l') {
        e.preventDefault();
        setChatOpen(true);
        window.setTimeout(() => document.querySelector<HTMLTextAreaElement>('textarea[aria-label="Request"]')?.focus(), 0);
      }
    }
    window.addEventListener('keydown', onKey, true);
    return () => window.removeEventListener('keydown', onKey, true);
  }, [vaultPhase]);

  useEffect(() => {
    let active = true;
    appInfo()
      .then((value) => active && setInfo(value))
      .catch(() => active && setInfo(null));
    storageStatus()
      .then((status) => {
        if (!active) return;
        const interrupted =
          status.interruptedTasksAtStartup > 0
            ? `, ${status.interruptedTasksAtStartup} task(s) marked interrupted at startup`
            : '';
        setStorage({ ok: true, text: `Database schema v${status.schemaVersion}, ${status.journalMode} mode${interrupted}` });
      })
      .catch((err: unknown) => active && setStorage({ ok: false, text: `Database unavailable: ${describeError(err)}` }));
    return () => {
      active = false;
    };
  }, []);

  const terminals = tabs.filter((t): t is Extract<EditorTab, { kind: 'terminal' }> => t.kind === 'terminal');

  function activate(key: string) {
    if (key !== 'keys') setProfilePaused(false);
    setActiveKey(key);
    if (tabs.some((t) => t.key === key && t.kind === 'terminal')) setLastTerminal(key);
  }

  function openTerminal(host: HostView) {
    nextTabNumber += 1;
    const key = `term-${nextTabNumber}`;
    // A tab for this host that is not connected (failed login, refused key, ended
    // session) is replaced in place instead of piling up a second tab.
    // Statuses are read from the ref: a click can land before the re-render that
    // follows a status change.
    const tab: EditorTab = { kind: 'terminal', key, host };
    setTabs((prev) => {
      const stale = prev.find(
        (t) =>
          t.kind === 'terminal' &&
          t.host.id === host.id &&
          ['failed', 'closed', 'hostkey'].includes(statusRef.current[t.key] ?? ''),
      );
      return stale ? prev.map((t) => (t.key === stale.key ? tab : t)) : [...prev, tab];
    });
    setActiveKey(key);
    setLastTerminal(key);
  }

  function openPage(page: PageId) {
    if (page !== 'keys') setProfilePaused(false);
    setTabs((prev) => (prev.some((t) => t.key === page) ? prev : [...prev, { kind: page, key: page }]));
    setActiveKey(page);
  }

  function closeTab(key: string) {
    if (key === 'keys') setProfilePaused(false);
    const currentTabs = tabsRef.current;
    const index = currentTabs.findIndex((t) => t.key === key);
    const next = currentTabs.filter((t) => t.key !== key);
    tabsRef.current = next;
    setTabs(next);
    delete statusRef.current[key];
    setStatuses((prev) => Object.fromEntries(Object.entries(prev).filter(([k]) => k !== key)));
    const neighbour = next[Math.min(index, next.length - 1)] ?? null;
    setActiveKey((current) => current === key ? neighbour?.key ?? null : current);
    setLastTerminal((current) => current === key
      ? (neighbour?.kind === 'terminal' ? neighbour.key : next.find((t) => t.kind === 'terminal')?.key ?? null)
      : current);
  }

  const onStatus = useCallback((key: string, status: SessionStatus) => {
    statusRef.current = { ...statusRef.current, [key]: status };
    setStatuses((prev) => (prev[key] === status ? prev : { ...prev, [key]: status }));
  }, []);

  function editHost(hostId: string) {
    setEditRequest((prev) => ({ hostId, n: (prev?.n ?? 0) + 1 }));
    setSidebarOpen(true);
  }

  function openLocal() {
    localHost()
      .then(openTerminal)
      .catch(() => setSidebarOpen(true));
  }

  const active = tabs.find((t) => t.key === activeKey) ?? null;
  const focusedTerminal = terminals.find((t) => t.key === lastTerminal) ?? null;
  const connectedSessions = Object.values(statuses).filter((status) => status === 'connected').length;
  const locationTitle = active?.kind === 'terminal' ? active.host.label : active ? PAGE_TITLES[active.kind] : 'Overview';
  // Status per host for the explorer: the best state among its open terminals.
  const sessions: Record<string, SessionStatus> = {};
  for (const t of terminals) {
    const s = statuses[t.key] ?? 'connecting';
    if (sessions[t.host.id] !== 'connected') sessions[t.host.id] = s;
  }

  const activity = (
    label: string,
    icon: ReactNode,
    pressed: boolean,
    onClick: () => void,
    shortcut?: string,
  ) => (
    <button
      type="button"
      className="activity-btn"
      aria-label={label}
      aria-pressed={pressed}
      title={shortcut ? `${label} (${shortcut})` : label}
      onClick={onClick}
    >
      <span className="activity-icon">{icon}</span>
      <span className="activity-label">{label}</span>
    </button>
  );

  return (
    <div className="workbench">
      <header className="workspace-head" data-tauri-drag-region>
        <div className="workspace-brand" role="img" aria-label="Selai" data-tauri-drag-region>
          <SelaiLogo size={28} />
        </div>
        <span className="workspace-divider" aria-hidden="true" data-tauri-drag-region />
        <span className="workspace-title" data-tauri-drag-region>Workspace</span>
        <span className="workspace-location" data-tauri-drag-region>/ {locationTitle}</span>
        <span className="grow" data-tauri-drag-region />
        <span className="workspace-session-count">
          <span className="dot" data-status={connectedSessions > 0 ? 'ok' : undefined} />
          {connectedSessions} connected
        </span>
        <button type="button" className="btn btn-sm" title="Vault security settings" onClick={() => window.dispatchEvent(new Event('selai:security'))}>Security</button>
        <button type="button" className="btn btn-sm" title="Lock vault" onClick={() => window.dispatchEvent(new Event('selai:lock'))}>Lock</button>
        <button
          type="button"
          className="assistant-toggle"
          aria-label="AI Assistant"
          aria-pressed={chatOpen}
          aria-expanded={chatOpen}
          aria-controls="assistant-panel"
          title="Toggle AI Assistant (Ctrl+Shift+L)"
          onClick={() => setChatOpen((open) => !open)}
        >
          <SelaiLogo size={19} />
          <span>AI Assistant</span>
        </button>
        <WindowControls />
      </header>
      <div className="workbench-main">
        <nav className="activitybar" aria-label="Main">
          {activity('Hosts', <ServerIcon size={20} />, sidebarOpen, () => {
            if (profilePaused) { setProfilePaused(false); setSidebarOpen(true); }
            else setSidebarOpen((o) => !o);
          }, 'Ctrl+Shift+E')}
          {activity('SSH Keys', <KeyIcon size={20} />, active?.kind === 'keys', () => openPage('keys'))}
          {activity('SFTP', <FilesIcon size={20} />, active?.kind === 'sftp', () => openPage('sftp'))}
          {activity('Port forwarding', <ForwardIcon size={20} />, active?.kind === 'forwarding', () => openPage('forwarding'))}
          {activity('Proxies', <ProxyIcon size={20} />, active?.kind === 'proxies', () => openPage('proxies'))}
          {activity('History', <ClockIcon size={20} />, active?.kind === 'history', () => openPage('history'))}
          {activity('Providers', <PlugIcon size={20} />, active?.kind === 'providers', () => openPage('providers'))}
        </nav>

        <aside className="sidebar" hidden={!sidebarOpen} style={{ width: side.width }}>
          {workspaceAvailable && <HostsSidebar onConnect={openTerminal} sessions={sessions} editRequest={editRequest}
            onManageProxies={() => { setProfilePaused(true); openPage('proxies'); if (window.innerWidth <= 1100) setChatOpen(false); }}
            profilePaused={profilePaused} onManageKeys={() => {
              setProfilePaused(true); openPage('keys');
              if (window.innerWidth <= 1100) setChatOpen(false);
            }} />}
        </aside>
        {sidebarOpen && <Resizer label="Resize sidebar" direction={1} {...side} />}

        <main className="editor">
          {tabs.length > 0 && (
            <div role="tablist" aria-label="Open tabs" className="editor-tabs">
              {tabs.map((tab) => {
                const title = tab.kind === 'terminal' ? tab.host.label : PAGE_TITLES[tab.kind];
                return (
                  <span key={tab.key} className="editor-tab" data-active={tab.key === activeKey}>
                    <button
                      type="button"
                      role="tab"
                      aria-selected={tab.key === activeKey}
                      onClick={() => activate(tab.key)}
                      onAuxClick={(e) => e.button === 1 && closeTab(tab.key)}
                      title={tab.kind === 'terminal' ? `${tab.host.username}@${tab.host.hostname}` : title}
                    >
                      {tab.kind === 'terminal' ? (
                        <span className="dot" data-status={statuses[tab.key] ?? 'connecting'} />
                      ) : tab.kind === 'keys' ? (
                        <KeyIcon size={13} />
                      ) : tab.kind === 'sftp' ? (<FilesIcon size={13} />) : tab.kind === 'forwarding' ? (<ForwardIcon size={13} />) : tab.kind === 'proxies' ? (<ProxyIcon size={13} />) : tab.kind === 'history' ? (
                        <ClockIcon size={13} />
                      ) : (
                        <PlugIcon size={13} />
                      )}
                      <span>{title}</span>
                    </button>
                    <button type="button" className="icon-btn" aria-label={`Close ${title}`} onClick={() => closeTab(tab.key)}>
                      <CloseIcon size={12} />
                    </button>
                  </span>
                );
              })}
              <button type="button" className="icon-btn add" aria-label="New local terminal" title="New local terminal" onClick={openLocal}>
                <PlusIcon size={14} />
              </button>
            </div>
          )}
          <div className="editor-body">
            {tabs.length === 0 && (
              <Welcome
                onLocal={openLocal}
                onHosts={() => setSidebarOpen(true)}
                onChat={() => setChatOpen(true)}
                onProviders={() => openPage('providers')}
              />
            )}
            {terminals.length > 0 && (
              <Suspense fallback={<div className="welcome"><span className="spinner spinner-lg" /></div>}>
                <TerminalStack
                  theme={terminalTheme}
                  tabs={terminals}
                  activeKey={activeKey}
                  visible={vaultPhase === 'unlocked' && active?.kind === 'terminal'}
                  onClose={closeTab}
                  onStatus={onStatus}
                  onEditHost={editHost}
                  fontSize={terminalFontSize}
                  onFontSizeChange={changeTerminalFontSize}
                />
              </Suspense>
            )}
            {workspaceAvailable && tabs.some((t) => t.kind === 'history') && (
              <div className="editor-page" hidden={active?.kind !== 'history'}>
                <HistoryTab visible={active?.kind === 'history'} />
              </div>
            )}
            {workspaceAvailable && tabs.some((t) => t.kind === 'providers') && (
              <div className="editor-page" hidden={active?.kind !== 'providers'}>
                <ProvidersPanel />
              </div>
            )}
            {workspaceAvailable && tabs.some((t) => t.kind === 'keys') && (
              <div className="editor-page" hidden={active?.kind !== 'keys'}>
                <Suspense fallback={<div className="welcome"><span className="spinner spinner-lg" /></div>}>
                  <SshKeysPage visible={active?.kind === 'keys'} onReturnToProfile={profilePaused ? () => {
                    setProfilePaused(false); setSidebarOpen(true);
                  } : undefined} />
                </Suspense>
              </div>
            )}
            {workspaceAvailable && (['sftp', 'forwarding', 'proxies'] as const).filter((page) => tabs.some((t) => t.kind === page)).map((page) => <div className="editor-page" key={page} hidden={active?.kind !== page}><Suspense fallback={<div className="welcome"><span className="spinner spinner-lg" /></div>}>
              {page === 'sftp' ? <SftpPage visible={active?.kind === page} /> : page === 'forwarding' ? <ForwardingPage visible={active?.kind === page} /> : <ProxiesPage visible={active?.kind === page} onReturnToProfile={profilePaused ? () => { setProfilePaused(false); setSidebarOpen(true); } : undefined} />}
            </Suspense></div>)}
          </div>
        </main>

        {chatOpen && <Resizer label="Resize chat" direction={-1} width={chatWidth} setWidth={(value) => chat.setWidth(Math.min(value, maxChatWidth))} commit={chat.commit} />}
        <aside id="assistant-panel" className="chatbar" hidden={!chatOpen} style={{ width: chatWidth }}>
          {workspaceAvailable && <AgentPanel
            activeHost={focusedTerminal?.host ?? null}
            onOpenProviders={() => {
              openPage('providers');
              if (window.innerWidth <= 1100) setChatOpen(false);
            }}
            onOpenHosts={() => {
              setSidebarOpen(true);
              if (window.innerWidth <= 1100) setChatOpen(false);
            }}
            onClose={() => setChatOpen(false)}
          />}
        </aside>
      </div>

      <footer className="statusbar">
        <span className="status-item" title={storage.text} data-tone={storage.ok ? undefined : 'danger'}>
          <span className="dot" data-status={storage.ok ? 'ok' : 'failed'} />
          {storage.ok ? 'Vault ready' : storage.text}
        </span>
        {focusedTerminal && (
          <span className="status-item">
            <TerminalIcon size={12} />
            {focusedTerminal.host.label} · {statuses[focusedTerminal.key] ?? 'connecting'}
          </span>
        )}
        <span className="grow" />
        <span className="status-item">Theme</span>
        <ThemedSelect label="Theme" className="theme-choice" value={theme} options={THEME_OPTIONS} onChange={(value) => setTheme(value as Theme)} />
        <span className="status-item">{info ? `Selai ${info.version}` : ''}</span>
      </footer>
    </div>
  );
}

export function WindowControls() {
  if (!isTauri()) return null;
  const appWindow = getCurrentWindow();
  return (
    <div className="window-controls" aria-label="Window controls">
      <button type="button" aria-label="Minimize window" title="Minimize" onClick={() => void appWindow.minimize()}><MinimizeIcon size={15} /></button>
      <button type="button" aria-label="Maximize or restore window" title="Maximize or restore" onClick={() => void appWindow.toggleMaximize()}><MaximizeIcon size={14} /></button>
      <button type="button" aria-label="Close window" title="Close" onClick={() => void appWindow.close()}><CloseIcon size={15} /></button>
    </div>
  );
}

function Welcome({ onLocal, onHosts, onChat, onProviders }: {
  onLocal: () => void;
  onHosts: () => void;
  onChat: () => void;
  onProviders: () => void;
}) {
  return (
    <div className="welcome">
      <div className="welcome-inner">
        <span className="eyebrow">YOUR WORKSPACE</span>
        <h1>Ready when you are.</h1>
        <p className="welcome-intro">Open a terminal, connect to a server, or work through a problem with the assistant.</p>
        <h2>Get started</h2>
        <ul className="welcome-actions">
          <li>
            <button type="button" onClick={onLocal}>
              <span className="welcome-action-icon"><TerminalIcon size={19} /></span>
              <span className="welcome-action-copy">
                <strong>Open a terminal on this computer</strong>
                <small>Start a local shell, right here.</small>
              </span>
              <span className="welcome-arrow" aria-hidden="true">↗</span>
            </button>
          </li>
          <li>
            <button type="button" onClick={onHosts}>
              <span className="welcome-action-icon"><ServerIcon size={19} /></span>
              <span className="welcome-action-copy">
                <strong>Connect to an SSH host</strong>
                <small>Choose a saved host or add a new one.</small>
              </span>
              <kbd>Ctrl+Shift+E</kbd>
            </button>
          </li>
          <li>
            <button type="button" onClick={onChat}>
              <span className="welcome-action-icon"><SelaiLogo size={24} /></span>
              <span className="welcome-action-copy">
                <strong>Ask the AI</strong>
                <small>Investigate an issue or think through a change.</small>
              </span>
              <kbd>Ctrl+Shift+L</kbd>
            </button>
          </li>
        </ul>
        <div className="welcome-foot">
          <span>Set up the assistant to work your way.</span>
          <button type="button" onClick={onProviders}>Configure AI providers <span aria-hidden="true">→</span></button>
        </div>
      </div>
    </div>
  );
}
