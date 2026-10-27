import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { SettingsPage } from './SettingsPage';

afterEach(() => vi.restoreAllMocks());

const preferences: ipc.AiPreferences = {
  lastProviderId: null,
  outputRetentionDays: 30,
  pinnedHostId: null,
  taskMaxSteps: 30,
  taskMaxMinutes: 30,
};

describe('SettingsPage', () => {
  it('loads and saves the AI task budget', async () => {
    vi.spyOn(ipc, 'preferencesGet').mockResolvedValue(preferences);
    const save = vi.spyOn(ipc, 'preferencesSet').mockResolvedValue({
      ...preferences,
      taskMaxSteps: 120,
      taskMaxMinutes: 90,
    });
    render(<SettingsPage />);

    const steps = await screen.findByLabelText('Maximum steps');
    const minutes = screen.getByLabelText('Time limit (minutes)');
    fireEvent.change(steps, { target: { value: '120' } });
    fireEvent.change(minutes, { target: { value: '90' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save task budget' }));

    await waitFor(() => expect(save).toHaveBeenCalledWith({ taskMaxSteps: 120, taskMaxMinutes: 90 }));
    expect(await screen.findByRole('status')).toHaveTextContent('120 steps or 90 minutes');
  });

  it('opens vault security settings', () => {
    const dispatch = vi.spyOn(window, 'dispatchEvent');
    render(<SettingsPage />);
    fireEvent.click(screen.getByRole('button', { name: 'Open vault security' }));
    expect(dispatch).toHaveBeenCalledWith(expect.objectContaining({ type: 'selai:security' }));
  });
});
