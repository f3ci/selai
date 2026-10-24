import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { TerminalPane } from './TerminalPane';
import { VaultWorkspacePhase } from '../vaultWorkspace';

const written: unknown[] = [];
const terminals: { options: { cursorBlink: boolean; fontSize: number; theme?: { background?: string; foreground?: string } }; focus: ReturnType<typeof vi.fn> }[] = [];
const fits: ReturnType<typeof vi.fn>[] = [];
let deferWrites = false;
const writeCallbacks: (() => void)[] = [];
let sendTerminalData: (data: string) => void = () => {};

vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    options = { cursorBlink: true, fontSize: 13.5 };
    cols = 80;
    rows = 24;
    constructor(options: { cursorBlink: boolean; fontSize: number }) {
      this.options.cursorBlink = options.cursorBlink;
      this.options.fontSize = options.fontSize;
      terminals.push(this);
    }
    loadAddon() {}
    open() {}
    focus = vi.fn();
    write(data: unknown, callback?: () => void) {
      written.push(data);
      if (callback) { if (deferWrites) writeCallbacks.push(callback); else callback(); }
    }
    writeln(text: string) {
      written.push(text);
    }
    onData(callback: (data: string) => void) {
      sendTerminalData = callback;
      return { dispose() {} };
    }
    onResize() {
      return { dispose() {} };
    }
    dispose() {}
  },
}));
vi.mock('@xterm/addon-fit', () => ({
  FitAddon: class {
    fit = vi.fn();
    constructor() { fits.push(this.fit); }
  },
}));

const host = {
  id: 'h1',
  label: 'Production Web',
  hostname: 'web.example.com',
  port: 22,
  username: 'deploy',
} as ipc.HostView;

beforeEach(() => {
  deferWrites = false;
  writeCallbacks.length = 0;
  vi.spyOn(ipc, 'terminalAck').mockResolvedValue();
  written.length = 0;
  terminals.length = 0;
  fits.length = 0;
});
afterEach(() => vi.restoreAllMocks());

describe('TerminalPane', () => {
  it('connects using the selected WSL host identity and labels its environment', async () => {
    const wsl = { ...host, id: 'wsl-debian', kind: 'local', label: 'WSL - Debian', wslDistribution: 'Debian' } as ipc.HostView;
    const connect = vi.spyOn(ipc, 'terminalConnect').mockResolvedValue('wsl-session');
    vi.spyOn(ipc, 'terminalDisconnect').mockResolvedValue();
    render(<TerminalPane host={wsl} hidden={false} />);
    await screen.findByText('Connected');
    expect(connect).toHaveBeenCalledWith('wsl-debian', expect.any(Number), expect.any(Number), expect.any(Function));
    expect(screen.getByText('WSL · Debian')).toBeInTheDocument();
  });

  it('blocks input during standby and resumes the same terminal after unlock', async () => {
    let deliver: (event: ipc.TerminalEvent) => void = () => {};
    const connect = vi.spyOn(ipc, 'terminalConnect').mockImplementation((_host, _cols, _rows, onEvent) => {
      deliver = onEvent; return Promise.resolve('standby-session');
    });
    const input = vi.spyOn(ipc, 'terminalInput').mockResolvedValue();
    const disconnect = vi.spyOn(ipc, 'terminalDisconnect').mockResolvedValue();
    const { rerender } = render(<VaultWorkspacePhase.Provider value="unlocked"><TerminalPane host={host} hidden={false} /></VaultWorkspacePhase.Provider>);
    await screen.findByText('Connected');
    sendTerminalData('before');
    expect(input).toHaveBeenCalledWith('standby-session', 'before');
    rerender(<VaultWorkspacePhase.Provider value="locked"><TerminalPane host={host} hidden /></VaultWorkspacePhase.Provider>);
    written.length = 0;
    act(() => deliver({ type: 'data', data: btoa('standby output') }));
    expect(written).toHaveLength(0);
    expect(ipc.terminalAck).not.toHaveBeenCalled();
    sendTerminalData('blocked');
    expect(input).toHaveBeenCalledTimes(1);
    expect(disconnect).not.toHaveBeenCalled();
    rerender(<VaultWorkspacePhase.Provider value="unlocked"><TerminalPane host={host} hidden={false} /></VaultWorkspacePhase.Provider>);
    expect(new TextDecoder().decode(written[0] as Uint8Array)).toBe('standby output');
    await waitFor(() => expect(ipc.terminalAck).toHaveBeenCalledWith('standby-session', 14));
    sendTerminalData('after');
    expect(input).toHaveBeenLastCalledWith('standby-session', 'after');
    expect(connect).toHaveBeenCalledOnce();
    expect(terminals).toHaveLength(1);
  });
  it('changes the palette of an existing terminal without reconnecting or clearing it', async () => {
    const connect = vi.spyOn(ipc, 'terminalConnect').mockResolvedValue('theme-session');
    const { rerender } = render(<TerminalPane host={host} hidden={false} theme="dark" />);
    await screen.findByText('Connected');
    expect(terminals[0]?.options.theme?.background).toBe('#141618');
    rerender(<TerminalPane host={host} hidden={false} theme="light" />);
    expect(terminals[0]?.options.theme).toMatchObject({ background: '#fafaf8', foreground: '#333b35' });
    for (const [theme, background, foreground, red] of [
      ['catppuccin-latte', '#eff1f5', '#4c4f69', '#d20f39'],
      ['catppuccin-frappe', '#303446', '#c6d0f5', '#e78284'],
      ['catppuccin-macchiato', '#24273a', '#cad3f5', '#ed8796'],
      ['catppuccin-mocha', '#1e1e2e', '#cdd6f4', '#f38ba8'],
    ] as const) {
      rerender(<TerminalPane host={host} hidden={false} theme={theme} />);
      expect(terminals[0]?.options.theme).toMatchObject({ background, foreground, red });
    }
    expect(terminals).toHaveLength(1);
    expect(connect).toHaveBeenCalledOnce();
  });

  it('acknowledges decoded bytes only after xterm consumes output, including hidden panes', async () => {
    let deliver: (event: ipc.TerminalEvent) => void = () => {};
    vi.spyOn(ipc, 'terminalConnect').mockImplementation((_host, _cols, _rows, onEvent) => {
      deliver = onEvent;
      return Promise.resolve('flow-session');
    });
    render(<TerminalPane host={host} hidden />);
    await screen.findByText('Connected');
    deferWrites = true;
    act(() => deliver({ type: 'data', data: btoa('hello') }));
    expect(ipc.terminalAck).not.toHaveBeenCalled();
    act(() => writeCallbacks.splice(0).forEach((callback) => callback()));
    await waitFor(() => expect(ipc.terminalAck).toHaveBeenCalledWith('flow-session', 5));
  });
  it('closes a finished shell once without waiting for the channel close', async () => {
    let deliver: (event: ipc.TerminalEvent) => void = () => {};
    vi.spyOn(ipc, 'terminalConnect').mockImplementation((_host, _c, _r, onEvent) => {
      deliver = onEvent; return Promise.resolve('exit-session');
    });
    vi.spyOn(ipc, 'terminalDisconnect').mockResolvedValue();
    const onClose = vi.fn();
    render(<TerminalPane host={host} hidden={false} onClose={onClose} />);
    await screen.findByText('Connected');
    act(() => deliver({ type: 'data', data: btoa('exit\r\nlogout\r\n') }));
    expect(onClose).not.toHaveBeenCalled();
    act(() => deliver({ type: 'exit', status: 0 }));
    expect(onClose).toHaveBeenCalledOnce();
    act(() => deliver({ type: 'closed', exitStatus: 0 }));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('keeps the final output alive until xterm has consumed it before closing', async () => {
    let deliver: (event: ipc.TerminalEvent) => void = () => {};
    vi.spyOn(ipc, 'terminalConnect').mockImplementation((_host, _cols, _rows, onEvent) => {
      deliver = onEvent;
      return Promise.resolve('drain-session');
    });
    vi.spyOn(ipc, 'terminalDisconnect').mockResolvedValue();
    const onClose = vi.fn();
    render(<TerminalPane host={host} hidden={false} onClose={onClose} />);
    await screen.findByText('Connected');
    deferWrites = true;
    act(() => {
      deliver({ type: 'data', data: btoa('final output') });
      deliver({ type: 'exit', status: 0 });
      deliver({ type: 'closed', exitStatus: 0 });
    });
    expect(onClose).not.toHaveBeenCalled();
    act(() => writeCallbacks.splice(0).forEach((callback) => callback()));
    expect(onClose).toHaveBeenCalledOnce();
  });

  it('does not resurrect a shell that exits before connect resolves', async () => {
    vi.spyOn(ipc, 'terminalConnect').mockImplementation((_host, _c, _r, onEvent) => {
      onEvent({ type: 'exit', status: 1 }); return Promise.resolve('already-ended');
    });
    const disconnect = vi.spyOn(ipc, 'terminalDisconnect').mockResolvedValue();
    const onClose = vi.fn();
    render(<TerminalPane host={host} hidden={false} onClose={onClose} />);
    await waitFor(() => expect(disconnect).toHaveBeenCalledWith('already-ended'));
    expect(onClose).toHaveBeenCalledOnce();
    expect(screen.queryByText('Connected')).not.toBeInTheDocument();
  });
  it('uses Ctrl+Plus and Ctrl+Minus in the terminal and refits when the font size changes', () => {
    vi.spyOn(ipc, 'terminalConnect').mockResolvedValue('session-font');
    const onFontSizeChange = vi.fn();
    const { rerender } = render(<TerminalPane host={host} hidden={false} fontSize={13.5} onFontSizeChange={onFontSizeChange} />);
    const terminal = screen.getByLabelText('Terminal Production Web');
    expect(terminals[0]?.options.fontSize).toBe(13.5);

    expect(fireEvent.keyDown(terminal, { key: '+', ctrlKey: true })).toBe(false);
    fireEvent.keyDown(terminal, { key: '=', ctrlKey: true });
    fireEvent.keyDown(terminal, { key: '-', ctrlKey: true });
    fireEvent.keyDown(terminal, { key: '+', altKey: true });
    expect(onFontSizeChange.mock.calls).toEqual([[1], [1], [-1]]);

    const fitCount = fits[0]?.mock.calls.length ?? 0;
    rerender(<TerminalPane host={host} hidden={false} fontSize={14.5} onFontSizeChange={onFontSizeChange} />);
    expect(terminals[0]?.options.fontSize).toBe(14.5);
    expect(fits[0]).toHaveBeenCalledTimes(fitCount + 1);
  });

  it('writes decoded output bytes to the terminal and reports the session status', async () => {
    let deliver: (event: ipc.TerminalEvent) => void = () => {};
    vi.spyOn(ipc, 'terminalConnect').mockImplementation((_host, _c, _r, onEvent) => {
      deliver = onEvent;
      return Promise.resolve('session-1');
    });
    const onStatus = vi.fn();
    render(<TerminalPane host={host} hidden={false} onStatus={onStatus} />);
    expect(screen.getByText('Connecting to Production Web')).toBeInTheDocument();
    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument());
    await waitFor(() => expect(onStatus).toHaveBeenLastCalledWith('connected'));

    act(() => deliver({ type: 'data', data: 'aGk=' })); // "hi"
    const bytes = written.find((w) => w instanceof Uint8Array) as Uint8Array;
    expect(Array.from(bytes)).toEqual([104, 105]);
  });

  it('shows a failed login with its code and retries on request', async () => {
    const connect = vi
      .spyOn(ipc, 'terminalConnect')
      .mockRejectedValueOnce({ code: 'SSH_AUTH_FAILED', message: 'the server rejected the credentials' })
      .mockResolvedValueOnce('session-2');
    const onEditHost = vi.fn();
    render(<TerminalPane host={host} hidden={false} onEditHost={onEditHost} />);
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Authentication failed');
    expect(alert).toHaveTextContent('SSH_AUTH_FAILED');

    fireEvent.click(screen.getByRole('button', { name: 'Edit host' }));
    expect(onEditHost).toHaveBeenCalled();

    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument());
    expect(connect).toHaveBeenCalledTimes(2);
  });

  it('asks to trust an unknown server key inside the connect flow, then connects', async () => {
    const connect = vi
      .spyOn(ipc, 'terminalConnect')
      .mockRejectedValueOnce({
        code: 'SSH_HOSTKEY_UNKNOWN',
        message: 'host key is not trusted yet',
        details: { algorithm: 'ssh-ed25519', fingerprintSha256: 'SHA256:abc' },
      })
      .mockResolvedValueOnce('session-3');
    const trust = vi.spyOn(ipc, 'hostTrust').mockResolvedValue({ outcome: 'pinned', fingerprintSha256: 'SHA256:abc' });
    render(<TerminalPane host={host} hidden={false} />);

    const dialog = await screen.findByRole('dialog', { name: 'Verify the server' });
    expect(dialog).toHaveTextContent('SHA256:abc');
    fireEvent.click(screen.getByRole('button', { name: 'Trust & connect' }));

    await waitFor(() => expect(trust).toHaveBeenCalledWith('h1', 'SHA256:abc', 'first'));
    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument());
    expect(connect).toHaveBeenCalledTimes(2);
  });

  it('a changed key needs explicit confirmation before it is replaced', async () => {
    vi.spyOn(ipc, 'terminalConnect').mockResolvedValue('session-4').mockRejectedValueOnce({
      code: 'SSH_HOSTKEY_CHANGED',
      message: 'host key changed',
      details: { algorithm: 'ssh-ed25519', fingerprintSha256: 'SHA256:new', pinnedFingerprintSha256: 'SHA256:old' },
    });
    const trust = vi.spyOn(ipc, 'hostTrust').mockResolvedValue({ outcome: 'rotated', fingerprintSha256: 'SHA256:new' });
    render(<TerminalPane host={host} hidden={false} />);

    const replace = await screen.findByRole('button', { name: 'Replace key & connect' });
    expect(replace).toBeDisabled();
    expect(screen.getByText('SHA256:old')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('checkbox'));
    fireEvent.click(replace);
    await waitFor(() => expect(trust).toHaveBeenCalledWith('h1', 'SHA256:new', 'replace'));
    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument());
  });

  it('declining the key leaves the session closed and sends nothing', async () => {
    vi.spyOn(ipc, 'terminalConnect').mockRejectedValueOnce({
      code: 'SSH_HOSTKEY_UNKNOWN',
      message: 'host key is not trusted yet',
      details: { algorithm: 'ssh-ed25519', fingerprintSha256: 'SHA256:abc' },
    });
    const trust = vi.spyOn(ipc, 'hostTrust');
    render(<TerminalPane host={host} hidden={false} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Server key not trusted');
    expect(trust).not.toHaveBeenCalled();
  });

  it('closes the remote session when the pane unmounts', async () => {
    vi.spyOn(ipc, 'terminalConnect').mockResolvedValue('session-9');
    const disconnect = vi.spyOn(ipc, 'terminalDisconnect').mockResolvedValue();
    const { unmount } = render(<TerminalPane host={host} hidden={false} />);
    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument());
    unmount();
    expect(disconnect).toHaveBeenCalledWith('session-9');
  });

  it('stops cursor blinking and does not steal focus while hidden', async () => {
    vi.spyOn(ipc, 'terminalConnect').mockResolvedValue('session-hidden');
    const { rerender } = render(<TerminalPane host={host} hidden />);
    await waitFor(() => expect(screen.getByText('Connected')).toBeInTheDocument());
    expect(terminals[0]?.options.cursorBlink).toBe(false);
    expect(terminals[0]?.focus).not.toHaveBeenCalled();

    rerender(<TerminalPane host={host} hidden={false} />);
    expect(terminals[0]?.options.cursorBlink).toBe(true);
    expect(terminals[0]?.focus).toHaveBeenCalledTimes(1);
  });
});
