import { useId, useRef, useState, type ReactNode } from 'react';
import { describeError } from '../ipc';
import { Dialog } from './Overlay';
import { AlertIcon } from './icons';

/** An explicit confirmation with a safe default and errors kept in context. */
export function ConfirmDialog({ title, children, confirmLabel, pendingLabel = 'Working…', icon,
  onConfirm, onClose }: {
  title: string;
  children: ReactNode;
  confirmLabel: string;
  pendingLabel?: string;
  icon?: ReactNode;
  onConfirm: () => Promise<unknown>;
  onClose: () => void;
}) {
  const descriptionId = useId();
  const pending = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function confirm() {
    if (pending.current) return;
    pending.current = true;
    setBusy(true);
    setError(null);
    try { await onConfirm(); onClose(); }
    catch (err) { setError(describeError(err)); }
    finally { pending.current = false; setBusy(false); }
  }

  return <Dialog title={title} role="alertdialog" describedBy={descriptionId} tone="danger"
    icon={icon ?? <AlertIcon size={20} />} closeDisabled={busy} onClose={() => { if (!pending.current) onClose(); }}
    footer={<>
      <button type="button" className="btn" data-autofocus disabled={busy} onClick={onClose}>Cancel</button>
      <button type="button" className="btn btn-danger" disabled={busy} onClick={() => void confirm()}>
        {busy && <span className="spinner" />}{busy ? pendingLabel : confirmLabel}
      </button>
    </>}>
    <div id={descriptionId} className="confirmation-copy">{children}</div>
    {error && <p role="alert" className="banner" data-tone="danger">{error}</p>}
  </Dialog>;
}
