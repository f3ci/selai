import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { HostsSidebar } from './HostsSidebar';

afterEach(() => vi.restoreAllMocks());
beforeEach(() => {
  localStorage.removeItem('selai.hosts.collapsed-groups');
  vi.spyOn(ipc, 'groupList').mockResolvedValue([]);
});

describe('HostsSidebar', () => {
  it('offers installed WSL distributions beside the native shell with independent identities', async () => {
    const native = { id: 'local', label: 'PowerShell', kind: 'local', hostname: 'localhost' } as ipc.HostView;
    const ubuntu = { ...native, id: 'wsl-ubuntu', label: 'WSL - Ubuntu-24.04', wslDistribution: 'Ubuntu-24.04' };
    const debian = { ...native, id: 'wsl-debian', label: 'WSL - Debian', wslDistribution: 'Debian' };
    const docker = { ...native, id: 'wsl-docker', label: 'WSL - docker-desktop', wslDistribution: 'docker-desktop' };
    vi.spyOn(ipc, 'localHost').mockResolvedValue(native);
    vi.spyOn(ipc, 'localHosts').mockResolvedValue([native, ubuntu, debian, docker]);
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [], nextCursor: null });
    const connect = vi.fn();
    render(<HostsSidebar onConnect={connect} sessions={{ 'wsl-ubuntu': 'connected' }} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Connect to WSL - Ubuntu-24.04' }));
    expect(connect).toHaveBeenCalledWith(ubuntu);
    expect(screen.getByRole('button', { name: 'Connect to PowerShell' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Connect to This computer' })).not.toBeInTheDocument();
    for (const name of ['PowerShell', 'Ubuntu', 'Debian', 'Docker']) {
      expect(document.querySelector(`img[data-environment="${name}"]`)).toBeInTheDocument();
    }
    expect(screen.getByRole('button', { name: 'Connect to WSL - Debian' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'This computer' }));
    fireEvent.change(screen.getByRole('searchbox', { name: 'Filter hosts' }), { target: { value: 'debian' } });
    expect(screen.getByRole('button', { name: 'Connect to WSL - Debian' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Connect to WSL - Ubuntu-24.04' })).not.toBeInTheDocument();
  });

  it('keeps the native shell available when WSL discovery fails', async () => {
    const native = { id: 'local', label: 'PowerShell', kind: 'local' } as ipc.HostView;
    vi.spyOn(ipc, 'localHost').mockResolvedValue(native);
    vi.spyOn(ipc, 'localHosts').mockRejectedValue(new Error('WSL unavailable'));
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [], nextCursor: null });
    const connect = vi.fn();
    render(<HostsSidebar onConnect={connect} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Connect to PowerShell' }));
    expect(connect).toHaveBeenCalledWith(native);
    expect(screen.queryByText('WSL Linux shell')).not.toBeInTheDocument();
  });

  it('creates a group from the SSH hosts header and closes the create dialog', async () => {
    vi.spyOn(ipc, 'localHost').mockRejectedValue(new Error('unavailable'));
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [], nextCursor: null });
    const create = vi.spyOn(ipc, 'groupCreate').mockImplementation(async (name) => {
      const group = { id: 'prod', name, sortOrder: 0 };
      vi.mocked(ipc.groupList).mockResolvedValue([group]);
      return group;
    });
    render(<HostsSidebar onConnect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Create SSH group' }));
    fireEvent.change(screen.getByLabelText('New group name'), { target: { value: 'Production' } });
    fireEvent.click(screen.getByRole('button', { name: 'Create group' }));
    await screen.findByRole('button', { name: 'Group Production' });
    expect(create).toHaveBeenCalledWith('Production');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('moves by dropping on a collapsed group, preserves fields, and allows dropping back to Ungrouped', async () => {
    vi.spyOn(ipc, 'localHost').mockRejectedValue(new Error('unavailable'));
    vi.mocked(ipc.groupList).mockResolvedValue([{ id: 'prod', name: 'Production', sortOrder: 0 }]);
    const host = { id: 'one', label: 'Web', groupId: null, updatedAtMs: 42, hostname: 'web.example.com', username: 'ops', port: 22, credentialConfigured: true, kind: 'ssh' } as ipc.HostView;
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host], nextCursor: null });
    const move = vi.spyOn(ipc, 'hostMoveGroup').mockImplementation(async (_id, groupId, revision) => ({ ...host, groupId, updatedAtMs: revision + 1 }));
    const connect = vi.fn();
    render(<HostsSidebar onConnect={connect} />);
    const group = await screen.findByRole('button', { name: 'Group Production' });
    fireEvent.click(group);
    const data = new Map<string, string>();
    const transfer = { types: ['application/x-selai-host-id'], setData: (type: string, value: string) => data.set(type, value),
      getData: (type: string) => data.get(type), setDragImage: vi.fn(), effectAllowed: '', dropEffect: '' };
    fireEvent.dragStart(screen.getByRole('button', { name: 'Connect to Web' }), { dataTransfer: transfer });
    fireEvent.dragOver(group, { dataTransfer: transfer });
    expect(group.parentElement).toHaveAttribute('data-drop-active', 'true');
    fireEvent.drop(group, { dataTransfer: transfer });
    await screen.findByText('Moved Web to Production.');
    expect(move).toHaveBeenCalledWith('one', 'prod', 42);
    expect(group).toHaveAttribute('aria-expanded', 'true');
    expect(connect).not.toHaveBeenCalled();
    fireEvent.dragStart(screen.getByRole('button', { name: 'Connect to Web' }), { dataTransfer: transfer });
    const ungrouped = screen.getByRole('button', { name: 'Group Ungrouped' });
    fireEvent.drop(ungrouped, { dataTransfer: transfer });
    await screen.findByText('Moved Web to Ungrouped.');
    expect(move).toHaveBeenLastCalledWith('one', null, 43);
  });

  it('ignores external drops and reloads after a failed move without hiding the profile', async () => {
    vi.spyOn(ipc, 'localHost').mockRejectedValue(new Error('unavailable'));
    vi.mocked(ipc.groupList).mockResolvedValue([{ id: 'prod', name: 'Production', sortOrder: 0 }]);
    const host = { id: 'one', label: 'Web', groupId: null, updatedAtMs: 42, hostname: 'web.example.com', username: 'ops', port: 22 } as ipc.HostView;
    const list = vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host], nextCursor: null });
    const move = vi.spyOn(ipc, 'hostMoveGroup').mockRejectedValue(new Error('Changed elsewhere'));
    render(<HostsSidebar onConnect={vi.fn()} />);
    const group = await screen.findByRole('button', { name: 'Group Production' });
    const transfer = { types: ['application/x-selai-host-id'], setData: vi.fn(), getData: () => 'one', setDragImage: vi.fn() };
    fireEvent.drop(group, { dataTransfer: transfer });
    expect(move).not.toHaveBeenCalled();
    fireEvent.dragStart(screen.getByRole('button', { name: 'Connect to Web' }), { dataTransfer: transfer });
    fireEvent.drop(group, { dataTransfer: transfer });
    await screen.findByRole('alert');
    expect(list).toHaveBeenCalledTimes(2);
    expect(screen.getByRole('button', { name: 'Connect to Web' })).toBeInTheDocument();
  });
  it('groups profiles, collapses a group, and expands matching groups during local search', async () => {
    vi.spyOn(ipc, 'localHost').mockRejectedValue(new Error('unavailable'));
    vi.mocked(ipc.groupList).mockResolvedValue([{ id: 'prod', name: 'Production', sortOrder: 0 }]);
    const list = vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [
      { id: 'one', label: 'Web', groupId: 'prod', hostname: 'web.example.com', username: 'ops', port: 22, credentialConfigured: true },
      { id: 'two', label: 'Lab', groupId: null, hostname: 'lab.example.com', username: 'ops', port: 22, credentialConfigured: true },
    ] as ipc.HostView[], nextCursor: null });
    render(<HostsSidebar onConnect={vi.fn()} />);
    const group = await screen.findByRole('button', { name: 'Group Production' });
    expect(screen.getByRole('button', { name: 'Group Ungrouped' })).toBeInTheDocument();
    fireEvent.click(group);
    expect(group).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('button', { name: 'Connect to Web' })).not.toBeInTheDocument();
    fireEvent.change(screen.getByRole('searchbox'), { target: { value: 'production' } });
    expect(screen.getByRole('button', { name: 'Connect to Web' })).toBeInTheDocument();
    expect(screen.queryByText('Lab')).not.toBeInTheDocument();
    expect(list).toHaveBeenCalledTimes(1);
  });
  it('remembers collapsed groups after the sidebar is remounted', async () => {
    vi.spyOn(ipc, 'localHost').mockRejectedValue(new Error('unavailable'));
    vi.mocked(ipc.groupList).mockResolvedValue([{ id: 'prod', name: 'Production', sortOrder: 0 }]);
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [], nextCursor: null });
    const first = render(<HostsSidebar onConnect={vi.fn()} />);
    const group = await screen.findByRole('button', { name: 'Group Production' });
    fireEvent.click(group);
    expect(group).toHaveAttribute('aria-expanded', 'false');
    first.unmount();
    render(<HostsSidebar onConnect={vi.fn()} />);
    expect(await screen.findByRole('button', { name: 'Group Production' })).toHaveAttribute('aria-expanded', 'false');
  });
  it('shows only edit and archive actions, and keeps them separate from connecting', async () => {
    vi.spyOn(ipc, 'localHost').mockRejectedValue(new Error('unavailable'));
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [
      { id: 'one', label: 'Production Web', hostname: 'web.example.com', port: 22, username: 'ops',
        authMethod: 'password', credentialConfigured: true, kind: 'ssh' } as ipc.HostView,
    ], nextCursor: null });
    const connect = vi.fn();
    const archive = vi.spyOn(ipc, 'hostArchive').mockResolvedValue();
    render(<HostsSidebar onConnect={connect} />);
    expect(await screen.findByRole('button', { name: 'Edit Production Web' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Use .* in chat/ })).not.toBeInTheDocument();
    expect(screen.queryByTitle('Use in chat')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Archive Production Web' }));
    expect(screen.getByRole('alertdialog', { name: 'Archive host?' })).toBeInTheDocument();
    expect(archive).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(connect).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Connect to Production Web' }));
    expect(connect).toHaveBeenCalledOnce();
  });

  it('filters loaded hosts locally without querying the database for each keystroke', async () => {
    vi.spyOn(ipc, 'localHost').mockRejectedValue(new Error('unavailable'));
    const hosts = [
      { id: 'one', label: 'Production Web', hostname: 'web.example.com', port: 22, username: 'ops', authMethod: 'password', credentialConfigured: true, kind: 'ssh' },
      { id: 'two', label: 'Staging DB', hostname: 'db.example.com', port: 22, username: 'ops', authMethod: 'password', credentialConfigured: true, kind: 'ssh' },
    ] as ipc.HostView[];
    const list = vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: hosts, nextCursor: null });
    render(<HostsSidebar onConnect={vi.fn()} />);

    expect(await screen.findByText('Production Web')).toBeInTheDocument();
    expect(screen.getByText('Staging DB')).toBeInTheDocument();
    const search = screen.getByRole('searchbox', { name: 'Filter hosts' });
    fireEvent.change(search, { target: { value: 'p' } });
    fireEvent.change(search, { target: { value: 'prod' } });
    expect(screen.getByText('Production Web')).toBeInTheDocument();
    expect(screen.queryByText('Staging DB')).not.toBeInTheDocument();
    expect(list).toHaveBeenCalledTimes(1);

    fireEvent.click(screen.getByRole('button', { name: 'Refresh hosts' }));
    await waitFor(() => expect(list).toHaveBeenCalledTimes(2));
  });
});
