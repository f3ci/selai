import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Workbench as App } from './App';
import * as ipc from './ipc';
import { chooseSelectValue } from './ui/selectTestUtils';
import { VaultWorkspacePhase } from './vaultWorkspace';

const windowApi = vi.hoisted(() => ({
  isTauri: vi.fn(() => false),
  getCurrentWindow: vi.fn(),
}));
vi.mock('@tauri-apps/api/core', async (importOriginal) => ({
  ...await importOriginal<typeof import('@tauri-apps/api/core')>(),
  isTauri: windowApi.isTauri,
}));
vi.mock('@tauri-apps/api/window', async (importOriginal) => ({
  ...await importOriginal<typeof import('@tauri-apps/api/window')>(),
  getCurrentWindow: windowApi.getCurrentWindow,
}));

vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    options = { cursorBlink: true };
    cols = 80;
    rows = 24;
    loadAddon() {}
    open() {}
    write() {}
    writeln() {}
    focus() {}
    onData() {
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
    fit() {}
  },
}));

describe('App shell', () => {
  beforeEach(() => {
    vi.spyOn(ipc, 'terminalAck').mockResolvedValue();
    windowApi.isTauri.mockReturnValue(false);
    windowApi.getCurrentWindow.mockReset();
    window.localStorage.clear();
    vi.stubGlobal('innerWidth', 1200);
    vi.spyOn(ipc, 'appInfo').mockResolvedValue({ name: 'Selai', version: '0.1.0' });
    vi.spyOn(ipc, 'storageStatus').mockResolvedValue({
      schemaVersion: 2,
      journalMode: 'wal',
      interruptedTasksAtStartup: 0,
      interruptedExecutionsAtStartup: 0,
    });
    vi.spyOn(ipc, "hostList").mockResolvedValue({ items: [], nextCursor: null });
    vi.spyOn(ipc, 'groupList').mockResolvedValue([]);
    vi.spyOn(ipc, "providerList").mockResolvedValue({ items: [], nextCursor: null });
    vi.spyOn(ipc, "providerTemplates").mockResolvedValue([]);
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('is a desktop workbench: host explorer on the left, chat on the right, welcome in the middle', async () => {
    render(<App />);
    expect(screen.getByRole('button', { name: 'Hosts' })).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('button', { name: 'AI Assistant' })).toHaveAttribute('aria-pressed', 'true');
    expect(screen.getByRole('region', { name: 'Hosts' })).toBeVisible();
    expect(screen.getByLabelText('Request')).toBeVisible();
    expect(screen.getByText('Open a terminal on this computer')).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTitle(/schema v2, wal mode/)).toBeInTheDocument());
  });

  it('toggles the side panels from the activity bar and the keyboard', () => {
    render(<App />);
    const header = document.querySelector('.workspace-head');
    const activitybar = screen.getByRole('navigation', { name: 'Main' });
    expect(header).toContainElement(screen.getByRole('button', { name: 'AI Assistant' }));
    expect(within(activitybar).queryByRole('button', { name: 'AI Assistant' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'AI Assistant' }));
    expect(screen.getByRole('button', { name: 'AI Assistant' })).toHaveAttribute('aria-pressed', 'false');
    expect(screen.getByRole('button', { name: 'AI Assistant' })).toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByLabelText('Request')).not.toBeVisible();
    fireEvent.keyDown(window, { key: 'L', ctrlKey: true, shiftKey: true });
    expect(screen.getByLabelText('Request')).toBeVisible();
    expect(screen.getByRole('button', { name: 'AI Assistant' })).toHaveAttribute('aria-expanded', 'true');

    fireEvent.keyDown(window, { key: 'E', ctrlKey: true, shiftKey: true });
    expect(screen.getByRole('button', { name: 'Hosts' })).toHaveAttribute('aria-pressed', 'false');
  });

  it('opens History and Providers as tabs in the middle', async () => {
    render(<App />);
    fireEvent.click(screen.getByRole('button', { name: 'Providers' }));
    const tabs = screen.getByRole('tablist', { name: 'Open tabs' });
    expect(within(tabs).getByRole('tab', { name: 'Providers' })).toHaveAttribute('aria-selected', 'true');
    fireEvent.click(within(tabs).getByRole('button', { name: 'Close Providers' }));
    expect(screen.queryByRole('tablist', { name: 'Open tabs' })).not.toBeInTheDocument();
  });

  it('opens key management from a profile and returns with the draft and new key available', async () => {
    const key = { id: 'work', name: 'Work key', algorithm: 'ssh-ed25519', publicKey: 'ssh-ed25519 AAAA', fingerprintSha256: 'SHA256:demo', createdAtMs: 1 };
    const list = vi.spyOn(ipc, 'sshKeyList').mockResolvedValue([]);
    render(<App />);
    fireEvent.click(screen.getAllByRole('button', { name: 'New host' })[0]!);
    fireEvent.change(screen.getByLabelText('Label'), { target: { value: 'Draft production' } });
    fireEvent.change(screen.getByLabelText('Host name or IP'), { target: { value: 'example.test' } });
    fireEvent.click(screen.getByRole('button', { name: 'Private key' }));
    await screen.findByText(/No managed SSH keys yet/);
    fireEvent.click(screen.getByRole('button', { name: 'Add SSH key' }));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    await screen.findByRole('heading', { name: 'SSH Keys' });
    expect(screen.getByRole('tab', { name: 'SSH Keys' })).toHaveAttribute('aria-selected', 'true');
    list.mockResolvedValue([key]);
    fireEvent.click(screen.getByRole('button', { name: 'Back to SSH profile' }));
    expect(screen.getByLabelText('Label')).toHaveValue('Draft production');
    expect(screen.getByLabelText('Host name or IP')).toHaveValue('example.test');
    await screen.findByRole('combobox', { name: 'SSH key' });
    chooseSelectValue('SSH key', 'vault://key:work');
    expect(screen.getByRole('combobox', { name: 'SSH key' })).toHaveAttribute('aria-valuetext', 'Work key · ssh-ed25519');
  });

  it('keeps a compact window clear and opens Providers from the assistant', async () => {
    vi.stubGlobal('innerWidth', 800);
    render(<App />);
    expect(screen.getByLabelText('Request')).not.toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: 'AI Assistant' }));
    expect(screen.getByLabelText('Request')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: 'Open Providers' }));
    expect(await screen.findByRole('tab', { name: 'Providers' })).toHaveAttribute('aria-selected', 'true');
    expect(screen.getByLabelText('Request')).not.toBeVisible();
  });

  it('honours the saved assistant visibility in a compact window', () => {
    vi.stubGlobal('innerWidth', 800);
    window.localStorage.setItem('selai.chat', 'open');
    render(<App />);
    expect(screen.getByLabelText('Request')).toBeVisible();
  });

  it('allows the assistant to grow while preserving space for the editor', () => {
    vi.stubGlobal('innerWidth', 1200);
    window.localStorage.setItem('selai.chat.width', '900');
    render(<App />);
    const panel = document.querySelector<HTMLElement>('.chatbar');
    expect(panel?.style.width).toBe('608px');
    fireEvent.click(screen.getByRole('button', { name: 'Hosts' }));
    expect(panel?.style.width).toBe('872px');
  });

  it('puts native window controls and a drag region in the app header', () => {
    const minimize = vi.fn().mockResolvedValue(undefined);
    const toggleMaximize = vi.fn().mockResolvedValue(undefined);
    const close = vi.fn().mockResolvedValue(undefined);
    windowApi.isTauri.mockReturnValue(true);
    windowApi.getCurrentWindow.mockReturnValue({ minimize, toggleMaximize, close });
    render(<App />);
    expect(document.querySelector('.workspace-head')).toHaveAttribute('data-tauri-drag-region');
    fireEvent.click(screen.getByRole('button', { name: 'Minimize window' }));
    fireEvent.click(screen.getByRole('button', { name: 'Maximize or restore window' }));
    fireEvent.click(screen.getByRole('button', { name: 'Close window' }));
    expect(minimize).toHaveBeenCalledTimes(1);
    expect(toggleMaximize).toHaveBeenCalledTimes(1);
    expect(close).toHaveBeenCalledTimes(1);
  });

  it('reports a storage failure with its code instead of hiding it', async () => {
    vi.spyOn(ipc, 'storageStatus').mockRejectedValue({ code: 'STORAGE_BUSY', message: 'database is busy' });
    render(<App />);
    await waitFor(() =>
      expect(screen.getByText('Database unavailable: STORAGE_BUSY: database is busy')).toBeInTheDocument(),
    );
  });

  it('connecting again after a failed login reuses the failed tab', async () => {
    const host = {
      id: 'h1',
      label: 'Production Web',
      hostname: 'web.example.com',
      port: 22,
      username: 'deploy',
      authMethod: 'password',
      credentialConfigured: true,
      archived: false,
    } as ipc.HostView;
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host], nextCursor: null });
    vi.spyOn(ipc, 'terminalConnect').mockRejectedValue({ code: 'SSH_AUTH_FAILED', message: 'authentication failed' });
    render(<App />);

    fireEvent.click(await screen.findByRole('button', { name: 'Connect to Production Web' }));
    expect(await screen.findByText('Authentication failed')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Connect to Production Web' }));
    await waitFor(() => expect(ipc.terminalConnect).toHaveBeenCalledTimes(2));
    const tabs = screen.getByRole('tablist', { name: 'Open tabs' });
    expect(within(tabs).getAllByRole('tab')).toHaveLength(1);
  });

  it('selects and remembers Catppuccin from the theme menu', async () => {
    const first = render(<App />);
    chooseSelectValue('Theme', 'catppuccin-macchiato');
    expect(document.documentElement.dataset.theme).toBe('catppuccin-macchiato');
    expect(localStorage.getItem('selai.theme')).toBe('catppuccin-macchiato');
    first.unmount();
    render(<App />);
    expect(screen.getByRole('combobox', { name: 'Theme' })).toHaveAttribute('aria-valuetext', 'Catppuccin Macchiato');
    chooseSelectValue('Theme', 'dark');
    expect(document.documentElement.style.getPropertyValue('--bg')).toBe('');
    expect(localStorage.getItem('selai.theme')).toBe('dark');
  });

  it('removes both terminal tabs when their shells exit in the same event batch', async () => {
    const host = { id: 'h1', label: 'Production Web', hostname: 'example.test', username: 'ops', port: 22 } as ipc.HostView;
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host], nextCursor: null });
    vi.spyOn(ipc, 'terminalDisconnect').mockResolvedValue();
    const events: ((event: ipc.TerminalEvent) => void)[] = [];
    vi.spyOn(ipc, 'terminalConnect').mockImplementation((_host, _cols, _rows, onEvent) => {
      events.push(onEvent); return Promise.resolve(`session-${events.length}`);
    });
    render(<App />);
    fireEvent.click(await screen.findByRole('button', { name: 'Connect to Production Web' }));
    await screen.findByText('Connected');
    fireEvent.click(screen.getByRole('button', { name: 'Connect to Production Web' }));
    await waitFor(() => expect(events).toHaveLength(2));
    act(() => { for (const onEvent of events) onEvent({ type: 'exit', status: 0 }); });
    await waitFor(() => expect(screen.queryByRole('tab')).not.toBeInTheDocument());
    expect(screen.getByText('Ready when you are.')).toBeInTheDocument();
  });
  it('preserves terminal tabs across vault lock while removing forms and chat', async () => {
    const host = { id: 'h1', label: 'Production Web', hostname: 'example.test', username: 'ops', port: 22 } as ipc.HostView;
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host], nextCursor: null });
    const connect = vi.spyOn(ipc, 'terminalConnect').mockResolvedValue('standby-session');
    const disconnect = vi.spyOn(ipc, 'terminalDisconnect').mockResolvedValue();
    const { rerender } = render(<VaultWorkspacePhase.Provider value="unlocked"><App /></VaultWorkspacePhase.Provider>);
    fireEvent.click(await screen.findByRole('button', { name: 'Connect to Production Web' }));
    await screen.findByText('Connected');
    fireEvent.click(screen.getAllByRole('button', { name: 'New host' })[0]!);
    expect(screen.getByLabelText('Host name or IP')).toBeInTheDocument();
    rerender(<VaultWorkspacePhase.Provider value="locked"><App /></VaultWorkspacePhase.Provider>);
    expect(screen.queryByLabelText('Host name or IP')).not.toBeInTheDocument();
    expect(screen.queryByLabelText('Request')).not.toBeInTheDocument();
    expect(screen.getByText('Connected')).not.toBeVisible();
    expect(disconnect).not.toHaveBeenCalled();
    rerender(<VaultWorkspacePhase.Provider value="unlocked"><App /></VaultWorkspacePhase.Provider>);
    expect(screen.getByText('Connected')).toBeVisible();
    expect(connect).toHaveBeenCalledOnce();
    expect(disconnect).not.toHaveBeenCalled();
  });
});
