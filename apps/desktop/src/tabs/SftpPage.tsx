import { useCallback, useEffect, useId, useMemo, useRef, useState } from 'react';
import { describeError, hostKeyProblem, type HostKeyProblem } from '../ipc';
import {
  bytes,
  filesList,
  filesAction,
  sftpConnect,
  sftpDisconnect,
  sftpList,
  transferStart,
  transferCancel,
  transferClear,
  transferWatch,
  type DirectoryView,
  type FileEntry,
  type SftpConnection,
  type TransferView,
  type TransferInput,
} from '../network';
import { useSshHosts } from './useSshHosts';
import { ThemedSelect } from '../ui/ThemedSelect';
import { HostKeyDialog } from '../ui/HostKeyDialog';
import { Dialog } from '../ui/Overlay';
import {
  FolderIcon,
  FileIcon,
  FilesIcon,
  RefreshIcon,
  UpIcon,
  PlusIcon,
  EditIcon,
  TrashIcon,
  UploadIcon,
  ImportIcon,
  CloseIcon,
  SearchIcon,
  ChevronIcon,
} from '../ui/icons';

type Side = 'local' | 'remote';
const activeTransfer = (job: TransferView) => ['queued', 'scanning', 'transferring'].includes(job.status);
interface FilePrompt {
  side: Side;
  action: 'mkdir' | 'rename' | 'delete';
  path: string;
  name: string;
}
export function SftpPage({ visible }: { visible: boolean }) {
  const { hosts, error: hostError } = useSshHosts(visible);
  const [hostId, setHostId] = useState('');
  const [connection, setConnection] = useState<SftpConnection | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [localPath, setLocalPath] = useState('');
  const [remotePath, setRemotePath] = useState('.');
  const [directories, setDirectories] = useState<Partial<Record<Side, DirectoryView>>>({});
  const [selected, setSelected] = useState<Record<Side, FileEntry[]>>({ local: [], remote: [] });
  const [refresh, setRefresh] = useState(0);
  const [jobs, setJobs] = useState<TransferView[]>([]);
  const [transfersExpanded, setTransfersExpanded] = useState(true);
  const [clearing, setClearing] = useState(false);
  const transferPanelId = useId();
  const clearedJobs = useRef(new Set<string>());
  const [overwrite, setOverwrite] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [trust, setTrust] = useState<HostKeyProblem | null>(null);
  const [prompt, setPrompt] = useState<FilePrompt | null>(null);
  const [promptBusy, setPromptBusy] = useState(false);
  const [promptError, setPromptError] = useState<string | null>(null);
  const [replace, setReplace] = useState<TransferInput | null>(null);
  const [disconnectConfirm, setDisconnectConfirm] = useState(false);
  const retryInputs = useRef(new Map<string, TransferInput>());
  const connectionRef = useRef(connection);
  connectionRef.current = connection;
  const updateJob = useCallback((job: TransferView) => {
    if (clearedJobs.current.has(job.id)) return;
    setJobs((rows) =>
      rows.some((j) => j.id === job.id)
        ? rows.map((j) => (j.id === job.id ? job : j))
        : [job, ...rows].slice(0, 100),
    );
    if (job.status === 'completed' || job.status === 'cancelled' || job.status === 'failed')
      setRefresh((n) => n + 1);
  }, []);
  useEffect(() => {
    if (!visible) return;
    let live = true;
    transferWatch((job) => {
      if (live) updateJob(job);
    })
      .then((rows) => {
        if (live) setJobs(rows.filter((job) => !clearedJobs.current.has(job.id)));
      })
      .catch((e) => {
        if (live) setError(describeError(e));
      });
    sftpList()
      .then((rows) => {
        if (live && !connectionRef.current && rows[0]) {
          setConnection(rows[0]);
          setHostId(rows[0].hostId);
          setRemotePath(rows[0].home);
        }
      })
      .catch((e) => {
        if (live) setError(describeError(e));
      });
    return () => {
      live = false;
    };
  }, [visible, updateJob]);
  useEffect(() => {
    if (!hostId && hosts[0]) setHostId(hosts[0].id);
  }, [hosts, hostId]);
  const host = hosts.find((h) => h.id === hostId);
  const activeJobs = jobs.filter(activeTransfer);
  const currentJobs = [...activeJobs, ...jobs.filter((j) => !activeTransfer(j))];
  const running = jobs.filter((j) => j.connectionId === connection?.id && activeTransfer(j));
  async function clearTransfers() {
    setClearing(true);
    try {
      const cleared = await transferClear();
      for (const id of cleared) {
        clearedJobs.current.add(id);
        retryInputs.current.delete(id);
      }
      setJobs((rows) => rows.filter((job) => !clearedJobs.current.has(job.id)));
    } catch (e) {
      setError(describeError(e));
    } finally {
      setClearing(false);
    }
  }
  async function connect() {
    if (!hostId) return;
    setConnecting(true);
    setError(null);
    try {
      const next = await sftpConnect(hostId);
      setConnection(next);
      setRemotePath(next.home);
      setSelected((s) => ({ ...s, remote: [] }));
    } catch (e) {
      const problem = hostKeyProblem(e);
      if (problem) setTrust(problem);
      else setError(describeError(e));
    } finally {
      setConnecting(false);
    }
  }
  async function disconnect() {
    if (!connection) return;
    try {
      await sftpDisconnect(connection.id);
      setConnection(null);
      setDirectories((d) => ({ ...d, remote: undefined }));
      setSelected((s) => ({ ...s, remote: [] }));
      setDisconnectConfirm(false);
    } catch (e) {
      setError(describeError(e));
    }
  }
  async function begin(input: TransferInput) {
    setError(null);
    setReplace(null);
    try {
      const job = await transferStart(input, updateJob);
      if (clearedJobs.current.has(job.id)) return;
      retryInputs.current.set(job.id, input);
      if (retryInputs.current.size > 100) {
        const oldest = retryInputs.current.keys().next().value;
        if (oldest) retryInputs.current.delete(oldest);
      }
      setJobs((rows) => (rows.some((r) => r.id === job.id) ? rows : [job, ...rows]));
    } catch (e) {
      setError(describeError(e));
    }
  }
  function transfer(side: Side) {
    if (!connection) return;
    const destination = directories[side === 'local' ? 'remote' : 'local'];
    if (!destination) return;
    const input: TransferInput = {
      connectionId: connection.id,
      direction: side === 'local' ? 'upload' : 'download',
      sources: selected[side].map((e) => e.path),
      destination: destination.path,
      overwrite,
    };
    if (overwrite) setReplace(input);
    else void begin(input);
  }
  async function act() {
    if (!prompt) return;
    setPromptBusy(true);
    setPromptError(null);
    try {
      await filesAction(
        prompt.side,
        prompt.action,
        prompt.path,
        connection?.id,
        prompt.action === 'delete' ? undefined : prompt.name,
      );
      setPrompt(null);
      setRefresh((n) => n + 1);
    } catch (e) {
      setPromptError(describeError(e));
    } finally {
      setPromptBusy(false);
    }
  }
  const loaded = useCallback((side: Side, directory: DirectoryView) => {
    setDirectories((d) => ({ ...d, [side]: directory }));
    setSelected((s) => ({ ...s, [side]: [] }));
  }, []);
  const operations = (side: Side, action: FilePrompt['action']) => {
    const entry = selected[side][0];
    const directory = directories[side];
    if (!directory || (action !== 'mkdir' && !entry)) return;
    setPromptError(null);
    setPrompt({
      side,
      action,
      path: action === 'mkdir' ? directory.path : entry!.path,
      name: action === 'rename' ? entry!.name : '',
    });
  };
  return (
    <section className="sftp-page">
      <header className="sftp-header">
        <div>
          <p className="eyebrow">FILES</p>
          <h1>SFTP</h1>
        </div>
        <div className="sftp-connection">
          <ThemedSelect label="SFTP server" value={hostId} disabled={Boolean(connection) || connecting}
            options={[{ value: '', label: 'Choose SSH server' }, ...hosts.map((h) => ({ value: h.id, label: h.label }))]} onChange={setHostId} />
          <span className="network-status" data-state={connection ? 'running' : 'stopped'}>
            <i />
            {connection ? 'Connected' : connecting ? 'Connecting…' : 'Disconnected'}
          </span>
          {connection ? (
            <button
              className="btn"
              onClick={() => (running.length ? setDisconnectConfirm(true) : void disconnect())}
            >
              Disconnect
            </button>
          ) : (
            <button
              className="btn btn-primary"
              disabled={!hostId || connecting}
              onClick={() => void connect()}
            >
              {connecting && <span className="spinner" />}Connect
            </button>
          )}
        </div>
      </header>
      {(error || hostError) && (
        <p className="banner sftp-banner" data-tone="danger" role="alert">
          {error || hostError}
        </p>
      )}
      <div className="sftp-transfer-actions">
        <div className="row">
          <button
            className="btn"
            disabled={
              !connection || !selected.local.length || selected.local.length > 256 || !directories.remote
            }
            onClick={() => transfer('local')}
          >
            <UploadIcon /> Upload {selected.local.length > 0 ? `(${selected.local.length})` : ''}
          </button>
          <button
            className="btn"
            disabled={
              !connection ||
              !selected.remote.length ||
              selected.remote.length > 256 ||
              !directories.local ||
              directories.local.path === '@drives'
            }
            onClick={() => transfer('remote')}
          >
            <ImportIcon /> Download {selected.remote.length > 0 ? `(${selected.remote.length})` : ''}
          </button>
        </div>
        <label className="sftp-conflicts">
          Existing files
          <ThemedSelect label="Existing files" value={overwrite ? 'replace' : 'skip'}
            options={[{ value: 'skip', label: 'Skip matching files' }, { value: 'replace', label: 'Replace matching files' }]}
            onChange={(value) => setOverwrite(value === 'replace')} />
        </label>
      </div>
      <div className="file-panels">
        <FilePane
          side="local"
          title="This computer"
          subtitle="Local files"
          path={localPath}
          revision={refresh}
          onNavigate={setLocalPath}
          onLoaded={loaded}
          selected={selected.local}
          onSelect={(entries) => setSelected((s) => ({ ...s, local: entries }))}
          onAction={(action) => operations('local', action)}
        />
        <FilePane
          side="remote"
          title={connection ? (host?.label ?? 'Remote server') : 'Remote server'}
          subtitle={connection && host ? `${host.username}@${host.hostname}` : 'SFTP over SSH'}
          path={remotePath}
          connectionId={connection?.id}
          revision={refresh}
          onNavigate={setRemotePath}
          onLoaded={loaded}
          selected={selected.remote}
          onSelect={(entries) => setSelected((s) => ({ ...s, remote: entries }))}
          onAction={(action) => operations('remote', action)}
        />
      </div>
      <section className="transfer-tray" aria-label="File transfers">
        <header>
          <h2>
            <button
              className="transfer-toggle"
              aria-expanded={transfersExpanded}
              aria-controls={transferPanelId}
              onClick={() => setTransfersExpanded((expanded) => !expanded)}
            >
              <ChevronIcon size={14} />
              Transfers
              <span className="count">{activeJobs.length ? `${activeJobs.length} active` : currentJobs.length}</span>
            </button>
          </h2>
          <div className="transfer-tray-actions">
            <span className="hint">2 transfers at a time · symbolic links are skipped</span>
            <button
              className="btn btn-sm"
              aria-label="Clear finished transfers"
              title="Clear completed, failed, and cancelled transfers"
              disabled={clearing || !jobs.some((job) => !activeTransfer(job))}
              onClick={() => void clearTransfers()}
            >
              <TrashIcon size={13} /> {clearing ? 'Clearing…' : 'Clear'}
            </button>
          </div>
        </header>
        <div id={transferPanelId} hidden={!transfersExpanded}>
          {!currentJobs.length ? (
            <p className="transfer-empty">
              Select files or folders in either panel, then choose Upload or Download.
            </p>
          ) : (
            <div className="transfer-list">
              {currentJobs.slice(0, 30).map((job) => {
                const progress =
                  job.status === 'completed'
                    ? 100
                    : job.totalBytes
                      ? Math.min(99, Math.round((job.transferredBytes / job.totalBytes) * 100))
                      : 0;
                const seconds =
                  job.bytesPerSecond > 0
                    ? Math.ceil(Math.max(0, job.totalBytes - job.transferredBytes) / job.bytesPerSecond)
                    : null;
                return (
                  <article className="transfer-job" key={job.id}>
                    <span className="transfer-direction">
                      {job.direction === 'upload' ? <UploadIcon /> : <ImportIcon />}
                    </span>
                    <div className="grow">
                      <div className="transfer-job-title">
                        <strong title={job.currentFile}>
                          {job.currentFile
                            ? job.currentFile.split(/[\\/]/).pop()
                            : job.direction === 'upload'
                              ? 'Upload'
                              : 'Download'}
                        </strong>
                        <span data-state={job.status}>
                          {job.status === 'transferring' ? `${progress}%` : job.status}
                        </span>
                      </div>
                      <progress
                        aria-label={`${job.direction} progress`}
                        max={100}
                        value={['queued', 'scanning'].includes(job.status) ? undefined : progress}
                      />
                      <div className="transfer-job-detail">
                        <span>
                          {bytes(job.transferredBytes)} / {bytes(job.totalBytes)} · {job.completedFiles}/
                          {job.totalFiles} files{job.skippedFiles > 0 ? ` · ${job.skippedFiles} skipped` : ''}
                        </span>
                        {job.status === 'transferring' && (
                          <span>
                            {bytes(job.bytesPerSecond)}/s
                            {seconds !== null
                              ? ` · ${seconds < 60 ? `${seconds}s` : `${Math.ceil(seconds / 60)}m`} left`
                              : ''}
                          </span>
                        )}
                      </div>
                      {job.error && (
                        <p role="alert" className="transfer-error">
                          {job.error}
                        </p>
                      )}
                    </div>
                    {activeTransfer(job) ? (
                      <button
                        className="icon-btn"
                        aria-label="Cancel transfer"
                        title="Cancel transfer"
                        onClick={() => void transferCancel(job.id).catch((e) => setError(describeError(e)))}
                      >
                        <CloseIcon />
                      </button>
                    ) : (
                      job.status !== 'completed' &&
                      retryInputs.current.has(job.id) && (
                        <button
                          className="btn btn-sm"
                          onClick={() => void begin(retryInputs.current.get(job.id)!)}
                        >
                          Retry
                        </button>
                      )
                    )}
                  </article>
                );
              })}
            </div>
          )}
        </div>
      </section>
      {trust && host && (
        <HostKeyDialog
          hostId={host.id}
          hostLabel={host.label}
          address={`${host.hostname}:${host.port}`}
          problem={trust}
          onCancel={() => setTrust(null)}
          onTrusted={() => {
            setTrust(null);
            void connect();
          }}
        />
      )}
      {prompt && (
        <Dialog
          title={
            prompt.action === 'mkdir'
              ? 'New folder'
              : prompt.action === 'rename'
                ? 'Rename entry'
                : 'Delete entry?'
          }
          onClose={() => !promptBusy && setPrompt(null)}
          tone={prompt.action === 'delete' ? 'danger' : 'default'} closeDisabled={promptBusy}
          footer={
            <>
              <button className="btn" data-autofocus={prompt.action === 'delete' || undefined} disabled={promptBusy} onClick={() => setPrompt(null)}>
                Cancel
              </button>
              <button
                form="file-operation"
                type="submit"
                className={`btn ${prompt.action === 'delete' ? 'btn-danger' : 'btn-primary'}`}
                disabled={promptBusy}
              >
                {promptBusy ? 'Working…' : prompt.action === 'delete' ? 'Delete' : 'Save'}
              </button>
            </>
          }
        >
          <form
            id="file-operation"
            onSubmit={(e) => {
              e.preventDefault();
              void act();
            }}
          >
            <p className="file-dialog-path">{prompt.path}</p>
            {prompt.action === 'delete' ? (
              <p>Delete this entry permanently. Folders must be empty.</p>
            ) : (
              <label className="field">
                Name
                <input
                  autoFocus
                  required
                  value={prompt.name}
                  onChange={(e) => setPrompt({ ...prompt, name: e.target.value })}
                />
              </label>
            )}
            {promptError && (
              <p className="banner" data-tone="danger" role="alert">
                {promptError}
              </p>
            )}
          </form>
        </Dialog>
      )}
      {replace && (
        <Dialog
          title="Replace matching files?"
          onClose={() => setReplace(null)}
          footer={
            <>
              <button className="btn" onClick={() => setReplace(null)}>
                Cancel
              </button>
              <button className="btn btn-primary" onClick={() => void begin(replace)}>
                Transfer & replace
              </button>
            </>
          }
        >
          <p>Transfer {replace.sources.length} selected entries to:</p>
          <p className="file-dialog-path">{replace.destination}</p>
          <p>
            Existing files with the same names will be replaced after each new file finishes transferring.
          </p>
        </Dialog>
      )}
      {disconnectConfirm && (
        <Dialog
          title="Disconnect from SFTP?"
          onClose={() => setDisconnectConfirm(false)}
          footer={
            <>
              <button className="btn" onClick={() => setDisconnectConfirm(false)}>
                Keep connection
              </button>
              <button className="btn btn-danger" onClick={() => void disconnect()}>
                Disconnect & cancel
              </button>
            </>
          }
        >
          <p>
            {running.length} transfer jobs are active. Disconnecting cancels these jobs; completed files are
            kept.
          </p>
        </Dialog>
      )}
    </section>
  );
}

interface PaneProps {
  side: Side;
  title: string;
  subtitle: string;
  path: string;
  connectionId?: string;
  revision: number;
  onNavigate: (path: string) => void;
  onLoaded: (side: Side, dir: DirectoryView) => void;
  selected: FileEntry[];
  onSelect: (rows: FileEntry[]) => void;
  onAction: (action: FilePrompt['action']) => void;
}
function FilePane({
  side,
  title,
  subtitle,
  path,
  connectionId,
  revision,
  onNavigate,
  onLoaded,
  selected,
  onSelect,
  onAction,
}: PaneProps) {
  const [directory, setDirectory] = useState<DirectoryView | null>(null);
  const [address, setAddress] = useState(path);
  const [query, setQuery] = useState('');
  const [hidden, setHidden] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [localRevision, refresh] = useState(0);
  const [scroll, setScroll] = useState(0);
  const [sort, setSort] = useState<'name' | 'size' | 'modified'>('name');
  const scrollRef = useRef<HTMLDivElement>(null);
  const anchor = useRef<string | null>(null);
  const unavailable = side === 'remote' && !connectionId;
  useEffect(() => {
    if (unavailable) {
      setDirectory(null);
      return;
    }
    let live = true;
    setBusy(true);
    setError(null);
    filesList(side, path, connectionId)
      .then((dir) => {
        if (live) {
          setDirectory(dir);
          setAddress(dir.path);
          onLoaded(side, dir);
          setScroll(0);
          if (scrollRef.current) scrollRef.current.scrollTop = 0;
        }
      })
      .catch((e) => {
        if (live) setError(describeError(e));
      })
      .finally(() => {
        if (live) setBusy(false);
      });
    return () => {
      live = false;
    };
  }, [side, path, connectionId, revision, localRevision, unavailable, onLoaded]);
  const entries = useMemo(
    () =>
      (directory?.entries ?? [])
        .filter(
          (entry) =>
            (hidden || !entry.name.startsWith('.')) && entry.name.toLowerCase().includes(query.toLowerCase()),
        )
        .sort(
          (a, b) =>
            (a.kind !== 'directory' ? 1 : 0) - (b.kind !== 'directory' ? 1 : 0) ||
            (sort === 'size'
              ? b.size - a.size
              : sort === 'modified'
                ? (b.modifiedMs ?? 0) - (a.modifiedMs ?? 0)
                : a.name.localeCompare(b.name)),
        ),
    [directory, hidden, query, sort],
  );
  const selectedPaths = useMemo(() => new Set(selected.map((e) => e.path)), [selected]);
  const start = Math.max(0, Math.floor(scroll / 36) - 8);
  const visible = entries.slice(start, start + 60);
  function toggle(entry: FileEntry, range = false, additive = true) {
    if (range && anchor.current) {
      const a = entries.findIndex((e) => e.path === anchor.current);
      const b = entries.indexOf(entry);
      if (a >= 0) {
        onSelect(entries.slice(Math.min(a, b), Math.max(a, b) + 1));
        return;
      }
    }
    anchor.current = entry.path;
    onSelect(
      additive
        ? selectedPaths.has(entry.path)
          ? selected.filter((e) => e.path !== entry.path)
          : [...selected, entry]
        : [entry],
    );
  }
  return (
    <section className="file-pane" aria-label={side === 'local' ? 'Local files' : 'Remote files'}>
      <header className="file-pane-head">
        <span className="network-symbol">
          <FilesIcon size={19} />
        </span>
        <div className="grow">
          <h2>{title}</h2>
          <span className="hint">{subtitle}</span>
        </div>
        <button
          className="icon-btn"
          aria-label={`Refresh ${side} files`}
          disabled={unavailable || busy}
          onClick={() => refresh((n) => n + 1)}
        >
          <RefreshIcon />
        </button>
      </header>
      <form
        className="file-path"
        onSubmit={(e) => {
          e.preventDefault();
          if (path === address) refresh((n) => n + 1);
          else onNavigate(address);
        }}
      >
        <button
          type="button"
          className="icon-btn"
          title="Parent folder"
          aria-label={`Parent ${side} folder`}
          disabled={!directory?.parent || busy || unavailable}
          onClick={() => directory?.parent && onNavigate(directory.parent)}
        >
          <UpIcon />
        </button>
        <input
          aria-label={`${side === 'local' ? 'Local' : 'Remote'} path`}
          disabled={unavailable}
          value={address}
          onChange={(e) => setAddress(e.target.value)}
        />
        <button className="btn btn-sm" disabled={unavailable || busy}>
          Go
        </button>
      </form>
      <div className="file-tools">
        <label className="file-search">
          <SearchIcon size={14} />
          <input
            aria-label={`Filter ${side} files`}
            placeholder="Filter files"
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setScroll(0);
              if (scrollRef.current) scrollRef.current.scrollTop = 0;
            }}
            disabled={unavailable}
          />
        </label>
        <button
          className="icon-btn"
          aria-label={`New ${side} folder`}
          title="New folder"
          disabled={!directory || busy || directory.path === '@drives'}
          onClick={() => onAction('mkdir')}
        >
          <PlusIcon />
        </button>
        <button
          className="icon-btn"
          aria-label={`Rename ${side} entry`}
          title="Rename"
          disabled={selected.length !== 1 || busy}
          onClick={() => onAction('rename')}
        >
          <EditIcon />
        </button>
        <button
          className="icon-btn"
          aria-label={`Delete ${side} entry`}
          title="Delete"
          disabled={selected.length !== 1 || busy}
          onClick={() => onAction('delete')}
        >
          <TrashIcon />
        </button>
      </div>
      {error && (
        <p role="alert" className="banner" data-tone="danger">
          {error}
        </p>
      )}
      <div
        className="file-table-scroll"
        ref={scrollRef}
        onScroll={(e) => {
          const top = e.currentTarget.scrollTop;
          if (Math.abs(top - scroll) >= 36) setScroll(top);
        }}
        aria-busy={busy}
      >
        {unavailable ? (
          <div className="file-pane-empty">
            <FilesIcon size={30} />
            <h3>Connect to browse remote files</h3>
            <p>
              Choose an SSH server above.
              <br />
              Your local files are ready on the left.
            </p>
          </div>
        ) : busy && !directory ? (
          <div className="file-pane-empty">
            <span className="spinner spinner-lg" />
            <p>Loading files…</p>
          </div>
        ) : (
          <table className="file-table">
            <thead>
              <tr>
                <th>
                  <input
                    aria-label={`Select all ${side} entries`}
                    type="checkbox"
                    checked={entries.length > 0 && entries.every((e) => selectedPaths.has(e.path))}
                    onChange={(e) => onSelect(e.target.checked ? entries : [])}
                  />
                </th>
                <th>
                  <button onClick={() => setSort('name')}>Name</button>
                </th>
                <th>
                  <button onClick={() => setSort('size')}>Size</button>
                </th>
                <th>
                  <button onClick={() => setSort('modified')}>Modified</button>
                </th>
              </tr>
            </thead>
            <tbody>
              {start > 0 && (
                <tr aria-hidden="true" className="file-spacer" style={{ height: start * 36 }}>
                  <td colSpan={4} />
                </tr>
              )}
              {visible.map((entry) => (
                <tr
                  key={entry.path}
                  data-selected={selectedPaths.has(entry.path)}
                  onClick={(e) => toggle(entry, e.shiftKey, e.ctrlKey || e.metaKey)}
                  onDoubleClick={() =>
                    (entry.kind === 'directory' || entry.kind === 'symlink') && onNavigate(entry.path)
                  }
                >
                  <td>
                    <input
                      aria-label={`Select ${entry.name}`}
                      type="checkbox"
                      checked={selectedPaths.has(entry.path)}
                      onClick={(e) => e.stopPropagation()}
                      onChange={() => toggle(entry)}
                    />
                  </td>
                  <td>
                    <button
                      className="file-name"
                      title={entry.name}
                      onClick={(e) => {
                        if (entry.kind === 'directory') {
                          e.stopPropagation();
                          onNavigate(entry.path);
                        }
                      }}
                    >
                      <span className={entry.kind === 'directory' ? 'file-folder' : ''}>
                        {entry.kind === 'directory' ? <FolderIcon /> : <FileIcon />}
                      </span>
                      <span>{entry.name}</span>
                      {entry.kind === 'symlink' && <span className="hint">↗</span>}
                    </button>
                  </td>
                  <td>{entry.kind === 'directory' ? '—' : bytes(entry.size)}</td>
                  <td>{entry.modifiedMs ? new Date(entry.modifiedMs).toLocaleDateString() : '—'}</td>
                </tr>
              ))}
              {entries.length > start + visible.length && (
                <tr
                  aria-hidden="true"
                  className="file-spacer"
                  style={{ height: (entries.length - start - visible.length) * 36 }}
                >
                  <td colSpan={4} />
                </tr>
              )}
            </tbody>
          </table>
        )}
        {!unavailable && !busy && !entries.length && (
          <div className="file-pane-empty">
            <FolderIcon size={26} />
            <p>{query ? 'No files match this filter.' : 'This folder is empty.'}</p>
          </div>
        )}
      </div>
      <footer className="file-pane-foot">
        <span>{selected.length ? `${selected.length} selected` : `${entries.length} entries`}</span>
        <label>
          <input type="checkbox" checked={hidden} onChange={(e) => setHidden(e.target.checked)} /> Hidden
          files
        </label>
      </footer>
    </section>
  );
}
