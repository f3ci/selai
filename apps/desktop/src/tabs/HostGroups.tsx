import { useState, type FormEvent } from 'react';
import { describeError, groupCreate, groupDelete, groupRename, type GroupView } from '../ipc';
import { Drawer } from '../ui/Overlay';
import { ConfirmDialog } from '../ui/ConfirmDialog';
import { EditIcon, FolderIcon, PlusIcon, TrashIcon } from '../ui/icons';

export function HostGroups({ groups, onChanged, onClose, createOnly = false }: {
  groups: GroupView[];
  onChanged: () => Promise<void>;
  onClose: () => void;
  createOnly?: boolean;
}) {
  const [name, setName] = useState('');
  const [renaming, setRenaming] = useState<string | null>(null);
  const [renamed, setRenamed] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [deleting, setDeleting] = useState<GroupView | null>(null);

  async function change(action: () => Promise<unknown>, closeAfter = false) {
    setBusy(true); setError(null);
    try { await action(); await onChanged(); if (closeAfter) onClose(); }
    catch (err) { setError(describeError(err)); }
    finally { setBusy(false); }
  }

  function create(e: FormEvent) {
    e.preventDefault();
    if (!name.trim()) return;
    void change(async () => { await groupCreate(name.trim()); setName(''); }, createOnly);
  }

  return <Drawer title={createOnly ? 'New SSH group' : 'SSH groups'} subtitle="Create a group, then drag SSH profiles into it." onClose={() => { if (!busy) onClose(); }}>
    {error && <p role="alert" className="banner" data-tone="danger">{error}</p>}
    <form onSubmit={create} className="form-section">
      <label className="field" htmlFor="group-name">New group name</label>
      <div className="group-select-row">
        <input id="group-name" value={name} maxLength={64} placeholder="Production" autoFocus
          disabled={busy} onChange={(e) => setName(e.target.value)} />
        <button className="btn" type="submit" disabled={busy || !name.trim()}><PlusIcon size={14} /> Create group</button>
      </div>
    </form>
    {!createOnly && <div className="group-manager-list">
      {groups.length === 0 && <p className="hint">No groups yet. Create one, then drag profiles into it from the sidebar.</p>}
      {groups.map((group) => <div className="group-manager-row" key={group.id}>
        <FolderIcon size={16} />
        {renaming === group.id ? <form className="group-select-row" onSubmit={(e) => {
          e.preventDefault();
          if (renamed.trim()) void change(async () => { await groupRename(group.id, renamed.trim()); setRenaming(null); });
        }}>
          <input aria-label={`Rename ${group.name}`} value={renamed} maxLength={64} disabled={busy} autoFocus onChange={(e) => setRenamed(e.target.value)} />
          <button className="btn btn-sm" type="submit" disabled={busy || !renamed.trim()}>Save name</button>
          <button className="btn btn-ghost btn-sm" type="button" disabled={busy} onClick={() => setRenaming(null)}>Cancel</button>
        </form> : <>
          <span className="grow" title={group.name}>{group.name}</span>
          <button className="icon-btn" type="button" aria-label={`Rename ${group.name}`} disabled={busy}
            onClick={() => { setRenaming(group.id); setRenamed(group.name); }}><EditIcon size={14} /></button>
          <button className="icon-btn" type="button" aria-label={`Delete ${group.name}`} disabled={busy}
            onClick={() => setDeleting(group)}><TrashIcon size={14} /></button>
        </>}
      </div>)}
    </div>}
    <p className="hint">Drag profiles onto a group or Ungrouped in the sidebar to move them. Deleting a group keeps its profiles and history.</p>
    {deleting && <ConfirmDialog title="Delete SSH group?" icon={<TrashIcon size={20} />}
      confirmLabel="Delete group" pendingLabel="Deleting…" onClose={() => setDeleting(null)}
      onConfirm={async () => { await groupDelete(deleting.id); await onChanged(); }}>
      <p>Delete the group <strong>{deleting.name}</strong>?</p>
      <p>Its profiles will move to Ungrouped. Hosts, credentials, and history will be kept.</p>
    </ConfirmDialog>}
  </Drawer>;
}
