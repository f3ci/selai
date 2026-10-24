import { useState } from 'react';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ConfirmDialog } from './ConfirmDialog';
import { Drawer } from './Overlay';

afterEach(() => vi.restoreAllMocks());

function Harness({ confirm = vi.fn().mockResolvedValue(undefined) }: { confirm?: () => Promise<unknown> }) {
  const [open, setOpen] = useState(false);
  return <>
    <button onClick={() => setOpen(true)}>Delete item</button>
    {open && <ConfirmDialog title="Delete item?" confirmLabel="Delete permanently" pendingLabel="Deleting…"
      onConfirm={confirm} onClose={() => setOpen(false)}>
      <p>Delete <strong>Example</strong> and its saved key?</p>
    </ConfirmDialog>}
  </>;
}

describe('ConfirmDialog', () => {
  it('defaults to Cancel, traps focus, cancels with Escape and restores the trigger', () => {
    const confirm = vi.fn().mockResolvedValue(undefined);
    render(<Harness confirm={confirm} />);
    const trigger = screen.getByRole('button', { name: 'Delete item' });
    trigger.focus();
    fireEvent.click(trigger);
    const dialog = screen.getByRole('alertdialog', { name: 'Delete item?' });
    expect(dialog).toHaveAccessibleDescription('Delete Example and its saved key?');
    expect(screen.getByRole('button', { name: 'Cancel' })).toHaveFocus();
    screen.getByRole('button', { name: 'Delete permanently' }).focus();
    fireEvent.keyDown(document, { key: 'Tab' });
    expect(screen.getByRole('button', { name: 'Close dialog' })).toHaveFocus();
    fireEvent.keyDown(document, { key: 'Tab', shiftKey: true });
    expect(screen.getByRole('button', { name: 'Delete permanently' })).toHaveFocus();
    trigger.focus();
    expect(screen.getByRole('button', { name: 'Cancel' })).toHaveFocus();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    expect(confirm).not.toHaveBeenCalled();
    expect(trigger).toHaveFocus();
  });

  it('waits for confirmation to finish, blocks duplicates and stays open on errors for retry', async () => {
    let reject: (error: Error) => void = () => undefined;
    const confirm = vi.fn().mockImplementationOnce(() => new Promise<void>((_resolve, fail) => { reject = fail; }))
      .mockResolvedValue(undefined);
    render(<Harness confirm={confirm} />);
    fireEvent.click(screen.getByRole('button', { name: 'Delete item' }));
    fireEvent.click(screen.getByRole('button', { name: 'Delete permanently' }));
    expect(screen.getByRole('button', { name: 'Deleting…' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Close dialog' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Deleting…' }));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.getByRole('alertdialog')).toBeInTheDocument();
    expect(confirm).toHaveBeenCalledOnce();
    await act(async () => reject(new Error('The key is still assigned to a profile')));
    expect(screen.getByRole('alert')).toHaveTextContent('The key is still assigned');
    fireEvent.click(screen.getByRole('button', { name: 'Delete permanently' }));
    await waitFor(() => expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument());
    expect(confirm).toHaveBeenCalledTimes(2);
  });

  it('closes only the top confirmation when it is opened over a drawer', () => {
    function Nested() {
      const [open, setOpen] = useState(false);
      const [drawer, setDrawer] = useState(true);
      return drawer && <Drawer title="SSH groups" onClose={() => setDrawer(false)}>
        <button onClick={() => setOpen(true)}>Remove group</button>
        {open && <ConfirmDialog title="Delete group?" confirmLabel="Delete group"
          onConfirm={async () => undefined} onClose={() => setOpen(false)}>Keep the hosts.</ConfirmDialog>}
      </Drawer>;
    }
    render(<Nested />);
    const trigger = screen.getByRole('button', { name: 'Remove group' });
    trigger.focus();
    fireEvent.click(trigger);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    expect(screen.getByRole('dialog', { name: 'SSH groups' })).toBeInTheDocument();
    expect(trigger).toHaveFocus();
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});
