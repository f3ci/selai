import { useContext, useEffect, useRef, useState } from 'react';
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import {
  decodeBase64,
  hostKeyProblem,
  isCommandError,
  terminalConnect,
  terminalAck,
  terminalDisconnect,
  terminalInput,
  terminalResize,
  type CommandError,
  type HostKeyProblem,
  type HostView,
  type TerminalEvent,
} from '../ipc';
import { HostKeyDialog } from '../ui/HostKeyDialog';
import { AlertIcon, EditIcon, RefreshIcon } from '../ui/icons';
import type { SessionStatus } from './TerminalTab';
import { isLightTheme, type ResolvedTheme } from '../theme';
import { terminalTheme } from '../terminalTheme';
import { VaultWorkspacePhase } from '../vaultWorkspace';

interface Props {
  theme?: ResolvedTheme;
  host: HostView;
  hidden: boolean;
  onStatus?: (status: SessionStatus) => void;
  onClose?: () => void;
  onEditHost?: () => void;
  fontSize?: number;
  onFontSizeChange?: (step: number) => void;
}

const FAILURE_TITLES: Record<string, string> = {
  SSH_AUTH_FAILED: 'Authentication failed',
  SSH_DNS: 'Host name not found',
  SSH_CONNECT_TIMEOUT: 'Connection timed out',
  SSH_CONNECT_FAILED: 'Could not reach the server',
  SSH_HOSTKEY_UNKNOWN: 'Server key not trusted',
  SSH_HOSTKEY_CHANGED: 'Server key not trusted',
};

/**
 * One interactive shell. The session stays open while this pane is mounted, so
 * the parent must keep panes mounted when their tab is hidden. A connect that
 * stops at an unknown or changed server key asks for approval here and then
 * connects again; nothing is sent to the server before the key is trusted.
 */
export function TerminalPane({ host, hidden, onStatus, onClose, onEditHost, fontSize = 13.5, onFontSizeChange, theme = 'dark' }: Props) {
  const vaultPhase = useContext(VaultWorkspacePhase);
  const phaseRef = useRef(vaultPhase);
  phaseRef.current = vaultPhase;
  const resumeOutput = useRef<(() => void) | null>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitRef = useRef<FitAddon | null>(null);
  const initialFontSize = useRef(fontSize);
  const initialTheme = useRef(theme);
  const sessionRef = useRef<string | null>(null);
  const hiddenRef = useRef(hidden);
  hiddenRef.current = hidden;
  const [status, setStatus] = useState<SessionStatus>('connecting');
  const [failure, setFailure] = useState<CommandError | null>(null);
  const [keyProblem, setKeyProblem] = useState<HostKeyProblem | null>(null);
  const [exitStatus, setExitStatus] = useState<number | null>(null);
  const [attempt, setAttempt] = useState(0);
  const statusCallback = useRef(onStatus);
  statusCallback.current = onStatus;
  const closeCallback = useRef(onClose);
  closeCallback.current = onClose;

  useEffect(() => {
    statusCallback.current?.(status);
  }, [status]);

  // The terminal itself lives as long as the pane.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const term = new Terminal({
      cursorBlink: !hidden,
      scrollback: 5000,
      fontFamily: "'JetBrains Mono', 'Cascadia Code', 'SF Mono', Menlo, Consolas, 'DejaVu Sans Mono', monospace",
      fontSize: initialFontSize.current,
      lineHeight: 1.25,
      letterSpacing: 0,
      theme: terminalTheme(initialTheme.current),
      minimumContrastRatio: isLightTheme(initialTheme.current) ? 4.5 : 1,
      allowProposedApi: false,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(container);
    fit.fit();
    termRef.current = term;
    fitRef.current = fit;

    const inputSub = term.onData((data) => {
      const id = sessionRef.current;
      if (id && phaseRef.current === 'unlocked') void terminalInput(id, data).catch(() => undefined);
    });
    const resizeSub = term.onResize(({ cols, rows }) => {
      const id = sessionRef.current;
      if (id && phaseRef.current === 'unlocked') void terminalResize(id, cols, rows).catch(() => undefined);
    });
    const observer =
      typeof ResizeObserver === 'undefined'
        ? undefined
        : new ResizeObserver(() => {
            if (container.offsetWidth > 0) fit.fit();
          });
    observer?.observe(container);

    return () => {
      observer?.disconnect();
      inputSub.dispose();
      resizeSub.dispose();
      term.dispose();
      termRef.current = null;
      fitRef.current = null;
    };
  }, []);

  useEffect(() => {
    if (termRef.current) {
      termRef.current.options.theme = terminalTheme(theme);
      termRef.current.options.minimumContrastRatio = isLightTheme(theme) ? 4.5 : 1;
    }
  }, [theme]);

  useEffect(() => {
    const term = termRef.current;
    if (term && term.options.fontSize !== fontSize) {
      term.options.fontSize = fontSize;
      if (!hiddenRef.current) fitRef.current?.fit();
    }
  }, [fontSize]);

  // One connection per attempt. Retry and "Trust & connect" start a new attempt.
  useEffect(() => {
    const term = termRef.current;
    if (!term) return;
    let disposed = false;
    let ended = false;
    let pendingWrites = 0;
    let closeSent = false;
    let sessionId: string | null = null;
    let pendingAck = 0;
    const standbyOutput: Uint8Array[] = [];
    let ackTimer: number | null = null;
    const acknowledge = () => {
      if (ackTimer !== null) window.clearTimeout(ackTimer);
      ackTimer = null;
      if (!sessionId || !pendingAck || disposed || ended) return;
      const bytes = pendingAck;
      pendingAck = 0;
      void terminalAck(sessionId, bytes).catch(() => {
        // An IPC failure must not silently leave the output pump blocked forever.
        if (!disposed && !ended && sessionId) {
          void terminalDisconnect(sessionId).catch(() => undefined);
          setFailure({ code: 'ERROR', message: 'Terminal output acknowledgement failed. Reconnect to continue.' });
          setStatus('failed');
        }
      });
    };
    setStatus('connecting');
    setFailure(null);
    setKeyProblem(null);
    setExitStatus(null);
    if (attempt > 0) term.write('\r\n\x1b[2m── reconnecting ──\x1b[0m\r\n');

    const closeFinished = () => {
      if (disposed || closeSent || !ended || pendingWrites > 0) return;
      closeSent = true;
      setStatus('closed');
      closeCallback.current?.();
    };
    const finish = (code: number | null) => {
      if (ended) return;
      ended = true;
      sessionRef.current = null;
      setExitStatus(code);
      // Keep the pane alive until its final queued output has been parsed.
      closeFinished();
    };

    const writeOutput = (bytes: Uint8Array) => {
      pendingWrites += 1;
      term.write(bytes, () => {
        pendingWrites -= 1;
        if (disposed) return;
        if (ended) { closeFinished(); return; }
        pendingAck += bytes.length;
        if (pendingAck >= 32 * 1024) acknowledge();
        else if (ackTimer === null) ackTimer = window.setTimeout(acknowledge, 8);
      });
    };
    resumeOutput.current = () => {
      if (disposed || ended) return;
      for (const bytes of standbyOutput.splice(0)) writeOutput(bytes);
    };

    const onEvent = (event: TerminalEvent) => {
      if (disposed || ended) return;
      switch (event.type) {
        case 'data':
          {
            const bytes = decodeBase64(event.data);
            // Credits bound this queue to the backend's output window. Defer
            // parsing while locked so terminal protocol replies wait for unlock.
            if (phaseRef.current !== 'unlocked') standbyOutput.push(bytes);
            else writeOutput(bytes);
          }
          break;
        case 'exit':
          finish(event.status);
          break;
        case 'closed':
          finish(event.exitStatus);
          break;
        case 'error':
          term.writeln(`\r\n\x1b[31m[${event.code}] ${event.message}\x1b[0m`);
          break;
      }
    };

    terminalConnect(host.id, term.cols, term.rows, onEvent)
      .then((id) => {
        if (disposed || ended) {
          void terminalDisconnect(id).catch(() => undefined);
          return;
        }
        sessionId = id;
        acknowledge();
        sessionRef.current = id;
        setStatus('connected');
        fitRef.current?.fit();
        if (!hiddenRef.current) term.focus();
      })
      .catch((err: unknown) => {
        if (disposed) return;
        const problem = hostKeyProblem(err);
        if (problem) {
          setKeyProblem(problem);
          setStatus('hostkey');
          return;
        }
        setFailure(isCommandError(err) ? err : { code: 'ERROR', message: String(err) });
        setStatus('failed');
      });

    return () => {
      disposed = true;
      resumeOutput.current = null;
      if (ackTimer !== null) window.clearTimeout(ackTimer);
      if (sessionId) void terminalDisconnect(sessionId).catch(() => undefined);
      sessionRef.current = null;
    };
  }, [host.id, attempt]);

  useEffect(() => {
    if (vaultPhase === 'unlocked') resumeOutput.current?.();
  }, [vaultPhase]);

  // A pane that was hidden has no size; fit it again when it is shown.
  useEffect(() => {
    if (termRef.current) termRef.current.options.cursorBlink = !hidden;
    if (!hidden) {
      fitRef.current?.fit();
      if (status === 'connected') termRef.current?.focus();
    }
  }, [hidden]);

  const retry = () => setAttempt((n) => n + 1);

  function disconnect() {
    const id = sessionRef.current;
    if (!id) return;
    void terminalDisconnect(id)
      .catch(() => undefined)
      .finally(() => {
        sessionRef.current = null;
        setStatus('closed');
      });
  }

  const address =
    host.kind === 'local'
      ? host.wslDistribution ? `WSL · ${host.wslDistribution}` : 'Local shell'
      : `${host.username}@${host.hostname}${host.port !== 22 ? `:${host.port}` : ''}`;
  const statusText: Record<SessionStatus, string> = {
    connecting: 'Connecting…',
    connected: 'Connected',
    hostkey: 'Waiting for key approval',
    failed: 'Not connected',
    closed: exitStatus === null ? 'Session ended' : `Session ended (exit ${exitStatus})`,
  };

  return (
    <div className="terminal-pane" hidden={hidden}>
      <div className="term-bar">
        <span className="dot" data-status={status} />
        <strong>{host.label}</strong>
        <span>{address}</span>
        <span className="grow" />
        <span aria-live="polite">{statusText[status]}</span>
        {status === 'connected' && (
          <button type="button" className="btn btn-ghost btn-sm" onClick={disconnect}>
            Disconnect
          </button>
        )}
        {status === 'closed' && (
          <button type="button" className="btn btn-sm" onClick={retry}>
            <RefreshIcon size={13} /> Reconnect
          </button>
        )}
      </div>
      <div
        className="terminal"
        ref={containerRef}
        aria-label={`Terminal ${host.label}`}
        onKeyDownCapture={(event) => {
          if (hidden || !onFontSizeChange || !event.ctrlKey || event.metaKey || event.altKey) return;
          const increase = event.key === '+' || event.key === '=' || event.code === 'NumpadAdd';
          const decrease = event.key === '-' || event.key === '_' || event.code === 'NumpadSubtract';
          if (!increase && !decrease) return;
          event.preventDefault();
          event.stopPropagation();
          onFontSizeChange(increase ? 1 : -1);
        }}
      />

      {status === 'connecting' && (
        <div className="term-overlay">
          <div className="term-card" role="status">
            <span className="spinner spinner-lg" />
            <h2>Connecting to {host.label}</h2>
            <span>{address}</span>
          </div>
        </div>
      )}

      {status === 'failed' && failure && (
        <div className="term-overlay">
          <div className="term-card" role="alert">
            <AlertIcon size={28} style={{ color: 'var(--danger)' }} />
            <h2>{FAILURE_TITLES[failure.code] ?? 'Connection failed'}</h2>
            <p>{failure.message}</p>
            <span className="code">{failure.code}</span>
            <div className="actions">
              {onClose && (
                <button type="button" className="btn btn-ghost" onClick={onClose}>
                  Close tab
                </button>
              )}
              {onEditHost && (
                <button type="button" className="btn" onClick={onEditHost}>
                  <EditIcon size={14} /> Edit host
                </button>
              )}
              <button type="button" className="btn btn-primary" onClick={retry}>
                <RefreshIcon size={14} /> Retry
              </button>
            </div>
          </div>
        </div>
      )}

      {status === 'hostkey' && keyProblem && (
        <HostKeyDialog
          contained
          hostId={host.id}
          hostLabel={host.label}
          address={`${host.hostname}:${host.port}`}
          problem={keyProblem}
          onTrusted={retry}
          onCancel={() => {
            setKeyProblem(null);
            setFailure({
              code: keyProblem.kind === 'changed' ? 'SSH_HOSTKEY_CHANGED' : 'SSH_HOSTKEY_UNKNOWN',
              message: 'The server key was not approved, so nothing was sent to the server.',
            });
            setStatus('failed');
          }}
        />
      )}
    </div>
  );
}
