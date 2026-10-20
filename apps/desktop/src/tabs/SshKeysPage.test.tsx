import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { SshKeysPage } from './SshKeysPage';
import { chooseSelectValue } from '../ui/selectTestUtils';

const key: ipc.SshKeyView = { id: 'one', name: 'Work laptop', algorithm: 'ssh-ed25519',
  publicKey: 'ssh-ed25519 AAAA-demo-public-key', fingerprintSha256: 'SHA256:demo', createdAtMs: 1 };

beforeEach(() => { vi.spyOn(ipc, 'sshKeyList').mockResolvedValue([]); });
afterEach(() => vi.restoreAllMocks());

describe('SSH Keys', () => {
  it.each(['ed25519', 'rsa-2048', 'rsa-3072', 'rsa-4096', 'rsa-8192', 'ecdsa-p256', 'ecdsa-p384', 'ecdsa-p521'])(
    'offers %s and sends the selected algorithm to the generator', async (algorithm) => {
      const generate = vi.spyOn(ipc, 'sshKeyGenerate').mockResolvedValue(key);
      render(<SshKeysPage visible />);
      await screen.findByText('No SSH keys yet');
      fireEvent.click(screen.getByRole('button', { name: 'Generate key' }));
      const dialog = screen.getByRole('dialog');
      expect(within(dialog).getByRole('combobox', { name: 'Algorithm' })).toHaveAttribute('aria-valuetext', 'Ed25519');
      fireEvent.change(within(dialog).getByLabelText('Key name'), { target: { value: 'My key' } });
      chooseSelectValue('Algorithm', algorithm);
      fireEvent.click(within(dialog).getByRole('button', { name: 'Generate key' }));
      await waitFor(() => expect(generate).toHaveBeenCalledWith('My key', algorithm));
    },
  );
  it('generates a named key and keeps its public key collapsed until requested', async () => {
    const generate = vi.spyOn(ipc, 'sshKeyGenerate').mockResolvedValue(key);
    render(<SshKeysPage visible />);
    await screen.findByText('No SSH keys yet');
    fireEvent.click(screen.getByRole('button', { name: 'Generate key' }));
    const dialog = screen.getByRole('dialog');
    fireEvent.change(within(dialog).getByLabelText('Key name'), { target: { value: ' Work laptop ' } });
    chooseSelectValue('Algorithm', 'ecdsa-p384');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Generate key' }));
    await screen.findByRole('heading', { name: key.name });
    expect(generate).toHaveBeenCalledWith('Work laptop', 'ecdsa-p384');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(screen.getByText(key.publicKey)).not.toBeVisible();
    fireEvent.click(screen.getByText('Public key', { selector: 'summary' }));
    expect(screen.getByText(key.publicKey)).toBeVisible();
  });

  it('imports an encrypted file with its passphrase and removes the secret form after success', async () => {
    const importKey = vi.spyOn(ipc, 'sshKeyImport').mockResolvedValue(key);
    render(<SshKeysPage visible />);
    fireEvent.click(screen.getByRole('button', { name: 'Import key' }));
    const dialog = screen.getByRole('dialog');
    const file = new File(['demo-key'], 'id_ed25519');
    Object.defineProperty(file, 'arrayBuffer', { value: async () => new TextEncoder().encode('demo-key').buffer });
    fireEvent.change(within(dialog).getByLabelText('Key name'), { target: { value: 'Imported key' } });
    fireEvent.change(within(dialog).getByLabelText('Private key file'), { target: { files: [file] } });
    fireEvent.change(within(dialog).getByLabelText('Key passphrase'), { target: { value: 'demo-passphrase' } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Import key' }));
    await waitFor(() => expect(importKey).toHaveBeenCalledWith('Imported key', btoa('demo-key'), 'demo-passphrase'));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(screen.queryByDisplayValue('demo-passphrase')).not.toBeInTheDocument();
  });

  it('opens the native ~/.ssh picker and imports extensionless key files directly into the vault', async () => {
    const importLocal = vi.spyOn(ipc, 'sshKeyImportFromFile').mockResolvedValue(key);
    render(<SshKeysPage visible />);
    fireEvent.click(screen.getByRole('button', { name: 'Import key' }));
    const dialog = screen.getByRole('dialog');
    fireEvent.change(within(dialog).getByLabelText('Key name'), { target: { value: 'Work key' } });
    fireEvent.change(within(dialog).getByLabelText('Key passphrase'), { target: { value: 'passphrase' } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Choose from ~/.ssh' }));
    await waitFor(() => expect(importLocal).toHaveBeenCalledWith('Work key', 'passphrase'));
    expect(await screen.findByRole('heading', { name: key.name })).toBeInTheDocument();
    expect(screen.queryByDisplayValue('passphrase')).not.toBeInTheDocument();
  });

  it('scans ~/.ssh, imports discovered keys, and reports encrypted or unsupported files', async () => {
    vi.spyOn(ipc, 'sshKeyScanLocal').mockResolvedValue({ imported: [key], alreadySaved: 1, skipped: 2 });
    render(<SshKeysPage visible />);
    fireEvent.click(await screen.findByRole('button', { name: 'Scan ~/.ssh' }));
    expect(await screen.findByRole('status')).toHaveTextContent('Imported 1 key. 1 already saved. 2 unsupported or encrypted files skipped; import encrypted keys individually with their passphrase.');
    expect(screen.getByRole('heading', { name: key.name })).toBeInTheDocument();
    expect(ipc.sshKeyScanLocal).toHaveBeenCalledOnce();
  });

  it('shows a deletion conflict without removing a key that is assigned to a profile', async () => {
    vi.spyOn(ipc, 'sshKeyList').mockResolvedValue([key]);
    const remove = vi.spyOn(ipc, 'sshKeyDelete').mockRejectedValue({ code: 'CONFLICT', message: 'SSH key is assigned to one or more host profiles' });
    render(<SshKeysPage visible />);
    fireEvent.click(await screen.findByRole('button', { name: `Delete ${key.name}` }));
    fireEvent.click(screen.getByRole('button', { name: 'Delete key' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('assigned to one or more host profiles');
    expect(remove).toHaveBeenCalledWith(key.id);
    expect(screen.getByRole('heading', { name: key.name })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    vi.spyOn(ipc, 'sshKeyDelete').mockResolvedValue(undefined);
    fireEvent.click(screen.getByRole('button', { name: `Delete ${key.name}` }));
    fireEvent.click(screen.getByRole('button', { name: 'Delete key' }));
    await screen.findByText('No SSH keys yet');
  });

  it('loads only when visible and clears an import draft when leaving the page', async () => {
    const list = vi.mocked(ipc.sshKeyList);
    const { rerender } = render(<SshKeysPage visible={false} />);
    expect(list).not.toHaveBeenCalled();
    rerender(<SshKeysPage visible />);
    await screen.findByText('No SSH keys yet');
    fireEvent.click(screen.getByRole('button', { name: 'Import key' }));
    fireEvent.change(screen.getByLabelText('Key passphrase'), { target: { value: 'demo-secret' } });
    rerender(<SshKeysPage visible={false} />);
    expect(screen.queryByDisplayValue('demo-secret')).not.toBeInTheDocument();
    rerender(<SshKeysPage visible />);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});
