import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { HostForm } from './HostForm';
import { chooseSelectValue } from '../ui/selectTestUtils';

afterEach(() => vi.restoreAllMocks());
beforeEach(() => {
  vi.spyOn(ipc, 'groupList').mockResolvedValue([]);
  vi.spyOn(ipc, 'sshKeyList').mockResolvedValue([{
  id: 'key1', name: 'Work key', algorithm: 'ssh-ed25519', publicKey: 'ssh-ed25519 AAAA',
  fingerprintSha256: 'SHA256:example', createdAtMs: 1,
}]);
});

function fill(label: string, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } });
}

describe('HostForm', () => {
  it('preserves an existing key file selection while editing a legacy profile', async () => {
    const host = { id: 'legacy', label: 'Legacy', hostname: 'example.test', username: 'ops', port: 22,
      authMethod: 'private_key', privateKeyPath: 'C:/Users/demo/.ssh/id_ed25519' } as ipc.HostView;
    const save = vi.spyOn(ipc, 'hostSave').mockResolvedValue(host);
    render(<HostForm host={host} onCancel={vi.fn()} onSaved={vi.fn()} />);
    expect(await screen.findByRole('combobox', { name: 'SSH key' })).toHaveAttribute('aria-valuetext', 'Existing key file');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ privateKeyPath: host.privateKeyPath })));
  });
  it('selects an existing group without offering inline group creation', async () => {
    vi.mocked(ipc.groupList).mockResolvedValue([{ id: 'production', name: 'Production', sortOrder: 0 }]);
    const save = vi.spyOn(ipc, 'hostSave').mockResolvedValue({} as ipc.HostView);
    render(<HostForm host={null} onCancel={vi.fn()} onSaved={vi.fn()} />);
    fill('Label', 'Web'); fill('Host name or IP', 'example.test'); fill('Username', 'ops');
    await screen.findByRole('combobox', { name: 'Group' });
    chooseSelectValue('Group', 'production');
    expect(screen.queryByRole('button', { name: 'New group' })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ groupId: 'production' })));
  });

  it('reveals a saved password without resubmitting it as an edit', async () => {
    const host = { id: 'saved-host', label: 'Web', hostname: 'example.test', username: 'ops', port: 22,
      authMethod: 'password', credentialConfigured: true } as ipc.HostView;
    const reveal = vi.spyOn(ipc, 'hostRevealPassword').mockResolvedValue('saved-demo-password');
    const save = vi.spyOn(ipc, 'hostSave').mockResolvedValue(host);
    render(<HostForm host={host} onCancel={vi.fn()} onSaved={vi.fn()} />);
    expect(reveal).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Show password' }));
    await screen.findByDisplayValue('saved-demo-password');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ password: undefined })));
    expect(screen.queryByDisplayValue('saved-demo-password')).not.toBeInTheDocument();
  });
  it('refuses to save an incomplete host without calling Rust', () => {
    const save = vi.spyOn(ipc, 'hostSave');
    render(<HostForm host={null} onCancel={() => {}} onSaved={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(screen.getByRole('alert')).toHaveTextContent('Label is required.');
    expect(save).not.toHaveBeenCalled();
  });

  it('sends the password once and clears it after a successful save', async () => {
    const save = vi.spyOn(ipc, 'hostSave').mockResolvedValue({} as ipc.HostView);
    const onSaved = vi.fn();
    render(<HostForm host={null} onCancel={() => {}} onSaved={onSaved} />);
    fill('Label', 'production-web');
    fill('Host name or IP', 'web.example.com');
    fill('Username', 'deploy');
    fill('Password', 'correct horse');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    expect(save.mock.calls[0]?.[0]).toMatchObject({ label: "production-web", password: "correct horse" });
    expect(onSaved).toHaveBeenCalledWith(expect.anything(), false);
    expect(screen.queryByDisplayValue('correct horse')).not.toBeInTheDocument();
  });

  it('keeps spaces in the label and opens a terminal with Save & connect', async () => {
    const saved = { id: 'h9', label: 'Production Web' } as ipc.HostView;
    const save = vi.spyOn(ipc, 'hostSave').mockResolvedValue(saved);
    const onSaved = vi.fn();
    render(<HostForm host={null} onCancel={() => {}} onSaved={onSaved} />);
    fill('Label', '  Production Web ');
    fill('Host name or IP', 'web.example.com');
    fill('Username', 'deploy');
    fireEvent.click(screen.getByRole('button', { name: 'Save & connect' }));

    await waitFor(() => expect(onSaved).toHaveBeenCalledWith(saved, true));
    expect(save.mock.calls[0]?.[0]).toMatchObject({ label: 'Production Web' });
  });

  it('switches to key authentication without sending a password', async () => {
    const save = vi.spyOn(ipc, 'hostSave').mockResolvedValue({} as ipc.HostView);
    render(<HostForm host={null} onCancel={() => {}} onSaved={() => {}} />);
    fill('Label', 'db');
    fill('Host name or IP', 'db.example.com');
    fill('Username', 'ops');
    fill('Password', 'typed by mistake');
    fireEvent.click(screen.getByRole('button', { name: 'Private key' }));
    await screen.findByRole('combobox', { name: 'SSH key' });
    chooseSelectValue('SSH key', 'vault://key:key1');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0]?.[0]).toMatchObject({ authMethod: 'private_key', privateKeyPath: 'vault://key:key1', password: undefined });
  });

  it('never prefills a saved secret when editing', () => {
    const host = {
      id: '6f1c2c1e-0b8e-4c2a-9a55-3d2f1a7b9c10',
      label: 'web',
      groupId: null,
      hostname: 'web.example.com',
      port: 22,
      username: 'deploy',
      authMethod: 'password',
      privateKeyPath: null,
      connectTimeoutSeconds: 15,
      keepAliveSeconds: 30,
      terminalType: 'xterm-256color',
      notes: null,
      credentialConfigured: true,
      updatedAtMs: 1,
      lastConnectedAtMs: null,
      archived: false,
      kind: 'ssh',
    } satisfies ipc.HostView;
    render(<HostForm host={host} onCancel={() => {}} onSaved={() => {}} />);
    const password = screen.getByLabelText('Password') as HTMLInputElement;
    expect(password.value).toBe('');
    expect(password).toHaveAttribute('placeholder', expect.stringContaining('saved'));
  });
});
