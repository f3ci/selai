import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { HostGroups } from './HostGroups';

afterEach(() => vi.restoreAllMocks());

it('creates, renames, and deletes a group through its dedicated commands', async () => {
  const group = { id: 'prod', name: 'Production', sortOrder: 0 };
  const create = vi.spyOn(ipc, 'groupCreate').mockResolvedValue(group);
  const rename = vi.spyOn(ipc, 'groupRename').mockResolvedValue({ ...group, name: 'Servers' });
  const remove = vi.spyOn(ipc, 'groupDelete').mockResolvedValue();
  const onChanged = vi.fn().mockResolvedValue(undefined);
  render(<HostGroups groups={[group]} onChanged={onChanged} onClose={vi.fn()} />);
  fireEvent.change(screen.getByLabelText('New group name'), { target: { value: 'Lab' } });
  fireEvent.click(screen.getByRole('button', { name: 'Create group' }));
  await waitFor(() => expect(create).toHaveBeenCalledWith('Lab'));
  await waitFor(() => expect(screen.getByRole('button', { name: 'Rename Production' })).toBeEnabled());
  fireEvent.click(screen.getByRole('button', { name: 'Rename Production' }));
  fireEvent.change(screen.getByRole('textbox', { name: 'Rename Production' }), { target: { value: 'Servers' } });
  fireEvent.click(screen.getByRole('button', { name: 'Save name' }));
  await waitFor(() => expect(rename).toHaveBeenCalledWith('prod', 'Servers'));
  await waitFor(() => expect(screen.getByRole('button', { name: 'Delete Production' })).toBeEnabled());
  fireEvent.click(screen.getByRole('button', { name: 'Delete Production' }));
  expect(remove).not.toHaveBeenCalled();
  expect(screen.getByRole('alertdialog', { name: 'Delete SSH group?' })).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'Delete group' }));
  await waitFor(() => expect(remove).toHaveBeenCalledWith('prod'));
  await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(3));
});
