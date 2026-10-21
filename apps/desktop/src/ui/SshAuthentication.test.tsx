import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { VaultWorkspacePhase } from '../vaultWorkspace';
import { SshAuthentication } from './SshAuthentication';
let receive: (item: ipc.SshAuthChallenge) => void;
const challenge: ipc.SshAuthChallenge = { id: 'first', hostId: 'host', hostLabel: 'Production', prompt: 'Verification code:', kind: 'secret' };
beforeEach(() => {
  vi.spyOn(ipc, 'sshAuthWatch').mockImplementation(async (sink) => { receive = sink; return []; });
  vi.spyOn(ipc, 'sshAuthAnswer').mockResolvedValue();
});
afterEach(() => vi.restoreAllMocks());
describe('SSH authentication', () => {
  it('queues MFA prompts, masks responses and clears each answer', async () => {
    render(<SshAuthentication />);
    act(() => { receive(challenge); receive({ ...challenge, id: 'second', prompt: 'Token PIN:' }); });
    const response = screen.getByLabelText('Response');
    expect(response).toHaveAttribute('type', 'password');
    fireEvent.change(response, { target: { value: '123456' } });
    act(() => receive({ ...challenge, instructions: 'Use a code from your authenticator app.' }));
    expect(screen.getByText('Use a code from your authenticator app.')).toBeInTheDocument();
    expect(screen.getByLabelText('Response')).toHaveValue('123456');
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await screen.findByText('Token PIN:');
    expect(ipc.sshAuthAnswer).toHaveBeenCalledWith('first', '123456');
    expect(screen.getByLabelText('Response')).toHaveValue('');
  });
  it('cancels without submitting a typed secret', async () => {
    render(<SshAuthentication />); act(() => receive(challenge));
    fireEvent.change(screen.getByLabelText('Response'), { target: { value: 'discard-me' } });
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(ipc.sshAuthAnswer).toHaveBeenCalledWith('first', null);
  });
  it('clears challenges when the vault locks and when requests expire', () => {
    const { rerender } = render(<VaultWorkspacePhase.Provider value="unlocked"><SshAuthentication /></VaultWorkspacePhase.Provider>);
    act(() => receive(challenge)); expect(screen.getByRole('dialog')).toBeInTheDocument();
    act(() => receive({ ...challenge, kind: 'dismiss' })); expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    act(() => receive(challenge));
    rerender(<VaultWorkspacePhase.Provider value="locking"><SshAuthentication /></VaultWorkspacePhase.Provider>);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    rerender(<VaultWorkspacePhase.Provider value="unlocked"><SshAuthentication /></VaultWorkspacePhase.Provider>);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});
