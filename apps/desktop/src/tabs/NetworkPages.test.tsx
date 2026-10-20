import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { ProxiesPage } from './ProxiesPage';
import { chooseSelectValue } from '../ui/selectTestUtils';
import { ForwardingPage } from './ForwardingPage';
import { SftpPage } from './SftpPage';
import { HostForm } from './HostForm';
import type { ForwardView, TransferView } from '../network';

const api = vi.hoisted(() => ({
  proxyList: vi.fn(),
  proxySave: vi.fn(),
  proxyDelete: vi.fn(),
  forwardWatch: vi.fn(),
  forwardSave: vi.fn(),
  forwardStart: vi.fn(),
  forwardStop: vi.fn(),
  forwardDelete: vi.fn(),
  sftpList: vi.fn(),
  sftpConnect: vi.fn(),
  sftpDisconnect: vi.fn(),
  filesList: vi.fn(),
  filesAction: vi.fn(),
  transferWatch: vi.fn(),
  transferStart: vi.fn(),
  transferCancel: vi.fn(),
  transferClear: vi.fn(),
  hostSave: vi.fn(),
}));
const host = {
  id: 'ssh-1',
  label: 'Staging',
  hostname: '192.0.2.42',
  port: 22,
  username: 'deploy',
  authMethod: 'password' as const,
  privateKeyPath: null,
  groupId: null,
  proxyId: null,
  connectTimeoutSeconds: 15,
  keepAliveSeconds: 30,
  terminalType: 'xterm-256color',
  notes: null,
  credentialConfigured: true,
  updatedAtMs: 1,
  lastConnectedAtMs: null,
  archived: false,
  kind: 'ssh' as const,
};
vi.mock('../network', async (original) => ({ ...(await original<typeof import('../network')>()), ...api }));
vi.mock('../ipc', () => ({
  describeError: (e: Error) => e.message,
  hostKeyProblem: () => null,
  HOSTS_CHANGED: 'hosts-changed',
  hostList: vi.fn(async () => ({ items: [host], nextCursor: null })),
  groupList: vi.fn(async () => []),
  sshKeyList: vi.fn(async () => []),
  hostSave: api.hostSave,
  hostForgetSudoPassword: vi.fn(),
  hostRevealPassword: vi.fn(),
}));
beforeEach(() => {
  vi.clearAllMocks();
  api.transferCancel.mockResolvedValue(undefined);
  api.transferClear.mockResolvedValue([]);
  api.forwardStop.mockResolvedValue(undefined);
  api.proxyList.mockResolvedValue([]);
  api.forwardWatch.mockResolvedValue([]);
  api.sftpList.mockResolvedValue([]);
  api.transferWatch.mockResolvedValue([]);
  api.sftpConnect.mockResolvedValue({ id: 'sftp-1', hostId: host.id, home: '/home/deploy' });
  api.filesList.mockImplementation(async (side: string) => ({
    path: side === 'local' ? 'C:\\Demo' : '/home/deploy',
    parent: '/',
    entries: [
      {
        name: side === 'local' ? 'config.toml' : 'README.md',
        path: side === 'local' ? 'C:\\Demo\\config.toml' : '/home/deploy/README.md',
        kind: 'file',
        size: 2048,
        modifiedMs: null,
        permissions: null,
      },
    ],
  }));
});
describe('network management flows', () => {
  it('collapses transfers and clears finished history while retaining active jobs and ignoring late events', async () => {
    const job: TransferView = {
      id: 'active', connectionId: 'sftp-1', direction: 'upload', status: 'transferring',
      currentFile: 'active.bin', destination: '/home/deploy', totalBytes: 2048,
      transferredBytes: 1024, totalFiles: 1, completedFiles: 0, skippedFiles: 0,
      bytesPerSecond: 1024, error: null,
    };
    const done: TransferView = { ...job, id: 'done', status: 'completed', currentFile: 'done.bin' };
    let update: (job: TransferView) => void = () => {};
    let history = [job, done];
    api.transferWatch.mockImplementation(async (callback) => { update = callback; return history; });
    api.transferClear.mockImplementation(async () => { history = [job]; return [done.id]; });
    const { rerender } = render(<SftpPage visible />);
    await screen.findByText('done.bin');
    const toggle = screen.getByRole('button', { name: /Transfers/ });
    expect(toggle).toHaveAttribute('aria-expanded', 'true');
    fireEvent.click(toggle);
    expect(toggle).toHaveAttribute('aria-expanded', 'false');
    expect(screen.getByText('active.bin')).not.toBeVisible();
    expect(api.transferCancel).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Clear finished transfers' }));
    await waitFor(() => expect(screen.queryByText('done.bin')).not.toBeInTheDocument());
    fireEvent.click(toggle);
    expect(screen.getByText('active.bin')).toBeVisible();
    expect(screen.getByRole('button', { name: 'Cancel transfer' })).toBeEnabled();
    act(() => update(done));
    expect(screen.queryByText('done.bin')).not.toBeInTheDocument();
    rerender(<SftpPage visible={false} />);
    rerender(<SftpPage visible />);
    await waitFor(() => expect(api.transferWatch).toHaveBeenCalledTimes(2));
    expect(screen.queryByText('done.bin')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Clear finished transfers' })).toBeDisabled();
  });
  it('creates an authenticated HTTP proxy from the UI and submits credentials only as input', async () => {
    api.proxySave.mockImplementation(async (input) => ({
      ...input,
      id: 'proxy-1',
      password: undefined,
      passwordConfigured: true,
    }));
    render(<ProxiesPage visible />);
    fireEvent.click(screen.getByRole('button', { name: 'New proxy' }));
    fireEvent.change(screen.getByLabelText('Name', { exact: true }), { target: { value: 'Office gateway' } });
    chooseSelectValue('Proxy protocol', 'http');
    expect(screen.getByLabelText('Port', { exact: true })).toHaveValue(8080);
    fireEvent.change(screen.getByLabelText('Proxy host'), { target: { value: 'proxy.example.com' } });
    fireEvent.click(screen.getByLabelText('Use proxy authentication'));
    fireEvent.change(screen.getByLabelText('Username', { exact: true }), { target: { value: 'user' } });
    fireEvent.change(screen.getByLabelText('Password', { exact: true }), {
      target: { value: 'test-password' },
    });
    fireEvent.submit(document.getElementById('proxy-editor')!);
    await screen.findByRole('heading', { name: 'Office gateway' });
    expect(api.proxySave).toHaveBeenCalledWith(
      expect.objectContaining({
        kind: 'http',
        username: 'user',
        password: 'test-password',
        hostname: 'proxy.example.com',
      }),
    );
    expect(screen.queryByDisplayValue('test-password')).not.toBeInTheDocument();
  });
  it('routes an SSH profile through a saved proxy and preserves the profile fields', async () => {
    api.proxyList.mockResolvedValue([
      {
        id: 'proxy-1',
        name: 'Office gateway',
        kind: 'socks5',
        hostname: '127.0.0.1',
        port: 1080,
        username: null,
        passwordConfigured: false,
      },
    ]);
    api.hostSave.mockResolvedValue({ ...host, proxyId: 'proxy-1' });
    render(<HostForm host={host} onCancel={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.click(screen.getByLabelText('Connect through a proxy'));
    await screen.findByRole('combobox', { name: 'Proxy profile' });
    chooseSelectValue('Proxy profile', 'proxy-1');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() =>
      expect(api.hostSave).toHaveBeenCalledWith(
        expect.objectContaining({
          id: host.id,
          proxyId: 'proxy-1',
          hostname: host.hostname,
          username: host.username,
        }),
      ),
    );
  });
  it('creates a dynamic tunnel without a destination and renders start/stop updates', async () => {
    api.forwardSave.mockImplementation(async (profile) => ({
      profile: { ...profile, id: 'tunnel-1' },
      status: 'stopped',
      actualPort: 0,
      connections: 0,
      sent: 0,
      received: 0,
      connectionErrors: 0,
      error: null,
    }));
    api.forwardStart.mockImplementation(async (_id, callback) => {
      const view: ForwardView = {
        profile: { ...api.forwardSave.mock.calls[0]![0], id: 'tunnel-1' },
        status: 'running',
        actualPort: 49155,
        connections: 1,
        sent: 1024,
        received: 2048,
        connectionErrors: 0,
        error: null,
      };
      callback(view);
      return view;
    });
    render(<ForwardingPage visible />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'New tunnel' })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: 'New tunnel' }));
    fireEvent.change(screen.getByLabelText('Name', { exact: true }), { target: { value: 'SOCKS route' } });
    fireEvent.click(screen.getByRole('radio', { name: /^Dynamic/ }));
    fireEvent.change(screen.getByLabelText('Listen port', { exact: true }), { target: { value: '0' } });
    expect(screen.queryByLabelText('Destination host')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Save & start' }));
    await screen.findByText('running', { exact: true });
    expect(api.forwardSave).toHaveBeenCalledWith(
      expect.objectContaining({ mode: 'dynamic', bindPort: 0, bindAddress: '127.0.0.1', hostId: host.id }),
    );
    fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
    expect(api.forwardStop).toHaveBeenCalledWith('tunnel-1');
  });
  it('uploads selected paths, renders byte progress, and cancels through native IPC', async () => {
    let update: (job: TransferView) => void = () => {};
    const job: TransferView = {
      id: 'job-1',
      connectionId: 'sftp-1',
      direction: 'upload',
      status: 'queued',
      currentFile: 'config.toml',
      destination: '/home/deploy',
      totalBytes: 2048,
      transferredBytes: 0,
      totalFiles: 1,
      completedFiles: 0,
      skippedFiles: 0,
      bytesPerSecond: 1024,
      error: null,
    };
    api.transferStart.mockImplementation(async (_input, callback) => {
      update = callback;
      return job;
    });
    render(<SftpPage visible />);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Connect' })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: 'Connect' }));
    await screen.findByLabelText('Select README.md');
    fireEvent.click(await screen.findByLabelText('Select config.toml'));
    fireEvent.click(screen.getByRole('button', { name: 'Upload (1)' }));
    await waitFor(() =>
      expect(api.transferStart).toHaveBeenCalledWith(
        {
          connectionId: 'sftp-1',
          direction: 'upload',
          sources: ['C:\\Demo\\config.toml'],
          destination: '/home/deploy',
          overwrite: false,
        },
        expect.any(Function),
      ),
    );
    act(() => update({ ...job, status: 'transferring', transferredBytes: 1024 }));
    expect(screen.getByRole('progressbar')).toHaveAttribute('value', '50');
    expect(screen.getByText('1 KiB / 2 KiB · 0/1 files')).toBeVisible();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel transfer' }));
    expect(api.transferCancel).toHaveBeenCalledWith('job-1');
  });
});
