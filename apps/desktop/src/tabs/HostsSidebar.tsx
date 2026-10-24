import { useCallback, useEffect, useMemo, useState, type DragEvent } from 'react';
import { describeError, groupList, HOSTS_CHANGED, hostArchive, hostList, hostMoveGroup, localHost, type GroupView, type HostView } from '../ipc';
import { HostAvatar } from '../ui/HostAvatar';
import { LocalEnvironmentAvatar } from '../ui/LocalEnvironmentIcon';
import { collectPages } from '../ui/collectPages';
import { loadLocalTargets } from '../ui/localTargets';
import { ConfirmDialog } from '../ui/ConfirmDialog';
import { ArchiveIcon, ChevronIcon, EditIcon, FolderIcon, ImportIcon, PlusIcon, RefreshIcon, SearchIcon } from '../ui/icons';
import { HostForm } from './HostForm';
import { HostGroups } from './HostGroups';
import { HostImport } from './HostImport';
import type { SessionStatus } from './TerminalTab';

interface Props {
  onConnect: (host: HostView) => void;
  /** Status of the open terminal for each host, if any. */
  sessions?: Record<string, SessionStatus>;
  /** Another view asked to edit this host, for example after a failed login. */
  editRequest?: { hostId: string; n: number } | null;
  onManageKeys?: () => void;
  onManageProxies?: () => void;
  profilePaused?: boolean;
}

const HOST_DRAG_TYPE = 'application/x-selai-host-id';
const COLLAPSED_GROUPS_KEY = 'selai.hosts.collapsed-groups';

function storedCollapsedGroups(): Set<string> {
  try {
    const value: unknown = JSON.parse(localStorage.getItem(COLLAPSED_GROUPS_KEY) ?? '[]');
    return new Set(Array.isArray(value) ? value.filter((id): id is string => typeof id === 'string') : []);
  } catch { return new Set(); }
}

/** The host explorer in the left sidebar: this computer and the saved SSH hosts. */
export function HostsSidebar({ onConnect, sessions = {}, editRequest, onManageKeys, onManageProxies, profilePaused }: Props) {
  const [hosts, setHosts] = useState<HostView[]>([]);
  const [groups, setGroups] = useState<GroupView[]>([]);
  const [groupDialog, setGroupDialog] = useState<'create' | 'manage' | null>(null);
  const [draggedHost, setDraggedHost] = useState<string | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const [moving, setMoving] = useState(false);
  const [moveNote, setMoveNote] = useState<string | null>(null);
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(storedCollapsedGroups);
  const [query, setQuery] = useState('');
  const [editing, setEditing] = useState<HostView | 'new' | null>(null);
  const [archiving, setArchiving] = useState<HostView | null>(null);
  const [importing, setImporting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [local, setLocal] = useState<HostView[]>([]);
  const [open, setOpen] = useState({ local: true, ssh: true });

  useEffect(() => {
    try { localStorage.setItem(COLLAPSED_GROUPS_KEY, JSON.stringify([...collapsedGroups])); } catch { /* storage can be unavailable */ }
  }, [collapsedGroups]);

  useEffect(() => {
    let active = true;
    localHost()
      .then((h) => active && setLocal((previous) => previous.length ? previous : [h]))
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    void loadLocalTargets().then(setLocal).catch(() => undefined);
    try {
      const [items, hostGroups] = await Promise.all([
        collectPages((cursor) => hostList(undefined, false, cursor)), groupList(),
      ]);
      setHosts(items);
      setGroups(hostGroups);
      setError(null);
    } catch (err) {
      setError(describeError(err));
    } finally {
      setLoaded(true);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (!editRequest) return;
    let active = true;
    // Look the host up without the current filter, so it is found even when hidden.
    collectPages((cursor) => hostList(undefined, false, cursor))
      .then((items) => {
        const host = items.find((h) => h.id === editRequest.hostId);
        if (active && host) setEditing(host);
      })
      .catch(() => undefined);
    return () => {
      active = false;
    };
  }, [editRequest]);

  async function archive(host: HostView) {
    await hostArchive(host.id);
    window.dispatchEvent(new Event(HOSTS_CHANGED));
    await refresh();
  }

  function canDrop(e: DragEvent) {
    return !moving && draggedHost !== null && e.dataTransfer.types.includes(HOST_DRAG_TYPE);
  }

  async function moveToGroup(id: string, groupId: string | null) {
    const host = hosts.find((item) => item.id === id);
    if (!host || moving || (host.groupId ?? null) === groupId) return;
    setMoving(true); setMoveNote(null); setError(null);
    try {
      const saved = await hostMoveGroup(host.id, groupId, host.updatedAtMs);
      setHosts((items) => items.map((item) => item.id === saved.id ? saved : item));
      setCollapsedGroups((prev) => { const next = new Set(prev); next.delete(groupId ?? 'ungrouped'); return next; });
      setQuery('');
      setMoveNote(`Moved ${host.label} to ${groups.find((group) => group.id === groupId)?.name ?? 'Ungrouped'}.`);
      window.dispatchEvent(new Event(HOSTS_CHANGED));
    } catch (err) {
      // Reload after a stale revision or a group removed in another view.
      await refresh(); setError(describeError(err));
    } finally { setMoving(false); }
  }

  const q = query.trim().toLowerCase();
  const { visibleHosts, buckets, ungrouped } = useMemo(() => {
    const byId = new Map(groups.map((group) => [group.id, { group, hosts: [] as HostView[] }]));
    const visibleHosts: HostView[] = [];
    const ungrouped: HostView[] = [];
    for (const host of hosts) {
      const bucket = host.groupId ? byId.get(host.groupId) : undefined;
      if (q && ![host.label, host.hostname, host.username, bucket?.group.name ?? ''].some((text) => text.toLowerCase().includes(q))) continue;
      visibleHosts.push(host);
      if (bucket) bucket.hosts.push(host); else ungrouped.push(host);
    }
    return { visibleHosts, buckets: [...byId.values()].filter((bucket) => !q || bucket.hosts.length > 0 || draggedHost !== null), ungrouped };
  }, [hosts, groups, q, draggedHost]);
  const visibleLocal = local.filter((host) => q === '' || [host.label, host.wslDistribution ?? '', 'local'].some((value) => value.toLowerCase().includes(q)));
  const showLocal = visibleLocal.length > 0;

  const hostRows = (items: HostView[]) => <ul className="tree-list">
    {items.map((host) => <HostRow key={host.id} host={host}
      sub={`${host.username}@${host.hostname}${host.port !== 22 ? `:${host.port}` : ''}`}
      warn={!host.credentialConfigured && host.authMethod === 'password' ? 'No password saved' : undefined}
      status={sessions[host.id]} onConnect={onConnect}
      draggable={!moving} onDragStart={(e) => {
        e.dataTransfer.setData(HOST_DRAG_TYPE, host.id); e.dataTransfer.effectAllowed = 'move';
        const row = e.currentTarget.closest<HTMLElement>('.tree-row');
        if (row) e.dataTransfer.setDragImage(row, 16, 16);
        setDraggedHost(host.id); setMoveNote(null);
      }} onDragEnd={() => { setDraggedHost(null); setDropTarget(null); }}
      onEdit={() => setEditing(host)} onArchive={() => setArchiving(host)} />)}
  </ul>;

  const groupSection = (id: string, name: string, items: HostView[]) => {
    const expanded = !!q || !collapsedGroups.has(id);
    return <div className="host-group" key={id} data-drop-active={dropTarget === id}
      onDragOver={(e) => { if (canDrop(e)) { e.preventDefault(); e.dataTransfer.dropEffect = 'move'; setDropTarget(id); } }}
      onDragLeave={(e) => { if (!(e.relatedTarget instanceof Node) || !e.currentTarget.contains(e.relatedTarget)) setDropTarget((current) => current === id ? null : current); }}
      onDrop={(e) => {
        if (!canDrop(e)) return;
        e.preventDefault(); e.stopPropagation();
        const source = e.dataTransfer.getData(HOST_DRAG_TYPE);
        if (source === draggedHost) void moveToGroup(source, id === 'ungrouped' ? null : id);
        setDraggedHost(null); setDropTarget(null);
      }}>
      <button type="button" className="tree-group tree-subgroup" aria-label={`Group ${name}`}
        aria-expanded={expanded} onClick={() => setCollapsedGroups((prev) => {
          const next = new Set(prev); if (next.has(id)) next.delete(id); else next.add(id); return next;
        })}>
        <ChevronIcon size={12} /><span className="group-label">{name}</span><span className="tree-count">{items.length}</span>
      </button>
      {expanded && (items.length > 0 ? hostRows(items) : <p className="group-empty">No hosts in this group.</p>)}
    </div>;
  };

  return (
    <section aria-label="Hosts" className="side-panel">
      <header className="panel-head">
        <h2>Hosts</h2>
        <span className="grow" />
        <button type="button" className="icon-btn" aria-label="Manage SSH groups" title="Manage SSH groups"
          onClick={() => setGroupDialog('manage')}><FolderIcon size={15} /></button>
        <button type="button" className="icon-btn" aria-label="Import SSH profiles" title="Import SSH profiles" onClick={() => {
          setEditing(null);
          setImporting(true);
        }}>
          <ImportIcon size={15} />
        </button>
        <button type="button" className="icon-btn" aria-label="New host" title="New host" onClick={() => setEditing('new')}>
          <PlusIcon size={15} />
        </button>
        <button type="button" className="icon-btn" aria-label="Refresh hosts" title="Refresh" onClick={() => void refresh()}>
          <RefreshIcon size={14} />
        </button>
      </header>
      <label className="side-search">
        <SearchIcon size={13} />
        <input
          type="search"
          placeholder="Filter hosts"
          aria-label="Filter hosts"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </label>

      {error && (
        <p role="alert" className="side-error">
          {error}
        </p>
      )}

      <div className="tree">
        {showLocal && (
          <>
            <button
              type="button"
              className="tree-group"
              aria-expanded={open.local || !!q}
              onClick={() => setOpen((o) => ({ ...o, local: !o.local }))}
            >
              <ChevronIcon size={12} />
              This computer
            </button>
            {(open.local || !!q) && (
              <ul className="tree-list">
                {visibleLocal.map((host) => <HostRow key={host.id}
                  host={host}
                  sub={host.wslDistribution ? 'WSL Linux shell' : 'local shell'}
                  status={sessions[host.id]}
                  onConnect={onConnect}
                />)}
              </ul>
            )}
          </>
        )}

        <div className="tree-heading">
        <button
          type="button"
          className="tree-group"
          aria-expanded={open.ssh || !!q}
          onClick={() => setOpen((o) => ({ ...o, ssh: !o.ssh }))}
        >
          <ChevronIcon size={12} />
          SSH hosts
          {visibleHosts.length > 0 && <span className="tree-count">{visibleHosts.length}</span>}
        </button>
        <button type="button" className="icon-btn" aria-label="Create SSH group" title="Create SSH group"
          onClick={() => setGroupDialog('create')}><PlusIcon size={14} /></button>
        </div>
        {(open.ssh || !!q) && (groups.length === 0 ? hostRows(visibleHosts) : <>
          {buckets.map(({ group, hosts }) => groupSection(group.id, group.name, hosts))}
          {(ungrouped.length > 0 || draggedHost !== null) && groupSection('ungrouped', 'Ungrouped', ungrouped)}
        </>)}
        {loaded && visibleHosts.length === 0 && !error && (open.ssh || !!q) && (
          <div className="tree-empty">
            {q ? (
              <p>No host matches “{query}”.</p>
            ) : (
              <>
                <p>No SSH hosts yet.</p>
                <button type="button" className="btn btn-sm" onClick={() => setEditing('new')}>
                  <PlusIcon size={13} /> New host
                </button>
              </>
            )}
          </div>
        )}
      </div>

      {archiving && <ConfirmDialog title="Archive host?" icon={<ArchiveIcon size={20} />}
        confirmLabel="Archive host" pendingLabel="Archiving…" onConfirm={() => archive(archiving)} onClose={() => setArchiving(null)}>
        <p>Archive <strong>{archiving.label}</strong>?</p>
        <p>Its command history will be kept. Saved passwords will be removed.</p>
      </ConfirmDialog>}
      {editing && (
        <HostForm
          key={editing === 'new' ? 'new' : editing.id}
          host={editing === 'new' ? null : editing}
          onManageKeys={onManageKeys}
          onManageProxies={onManageProxies}
          suspended={profilePaused}
          onCancel={() => setEditing(null)}
          onSaved={async (saved, connect) => {
            setEditing(null);
            window.dispatchEvent(new Event(HOSTS_CHANGED));
            await refresh();
            if (connect) onConnect(saved);
          }}
        />
      )}
      {moveNote && <p className="host-move-note" role="status">{moveNote}</p>}
      {draggedHost && groups.length > 0 && <p className="host-move-note">Drop on a group to move this profile.</p>}
      {groupDialog && <HostGroups groups={groups} createOnly={groupDialog === 'create'} onChanged={async () => {
        await refresh();
        if (groupDialog === 'create') { setQuery(''); setOpen((prev) => ({ ...prev, ssh: true })); }
      }} onClose={() => setGroupDialog(null)} />}
      {importing && (
        <HostImport
          onClose={() => setImporting(false)}
          onImported={async () => {
            window.dispatchEvent(new Event(HOSTS_CHANGED));
            await refresh();
          }}
        />
      )}
    </section>
  );
}

function HostRow({
  host,
  sub,
  warn,
  status,
  onConnect,
  onEdit,
  onArchive,
  draggable,
  onDragStart,
  onDragEnd,
}: {
  host: HostView;
  sub: string;
  warn?: string;
  status?: SessionStatus;
  onConnect: (host: HostView) => void;
  onEdit?: () => void;
  onArchive?: () => void;
  draggable?: boolean;
  onDragStart?: (e: DragEvent<HTMLButtonElement>) => void;
  onDragEnd?: () => void;
}) {
  return (
    <li className="tree-row" title={warn ? `${sub} · ${warn}` : sub}>
      <button
        type="button"
        className="tree-open"
        aria-label={`Connect to ${host.label}`}
        onClick={() => onConnect(host)}
        draggable={draggable ?? false}
        onDragStart={onDragStart}
        onDragEnd={onDragEnd}
      />
      {host.kind === 'local' ? (
        <LocalEnvironmentAvatar host={host} />
      ) : (
        <HostAvatar label={host.label} size={12} />
      )}
      <span className="tree-text">
        <span className="tree-label">{host.label}</span>
        <span className="tree-sub">{sub}</span>
      </span>
      {warn && <span className="dot" data-status="hostkey" aria-label={warn} />}
      {status && <span className="dot" data-status={status === 'connected' ? 'ok' : status} aria-label={`Terminal ${status}`} />}
      <span className="tree-tools">
        {onEdit && (
          <button type="button" className="icon-btn" aria-label={`Edit ${host.label}`} title="Edit" onClick={onEdit}>
            <EditIcon size={13} />
          </button>
        )}
        {onArchive && (
          <button type="button" className="icon-btn" aria-label={`Archive ${host.label}`} title="Archive" onClick={onArchive}>
            <ArchiveIcon size={13} />
          </button>
        )}
      </span>
    </li>
  );
}
