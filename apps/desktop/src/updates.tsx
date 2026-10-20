import { createContext, lazy, Suspense, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import { describeError, updateCheck, updateOpen, type UpdateCheck } from './ipc';
import { CheckIcon, CloseIcon, ImportIcon, RefreshIcon } from './ui/icons';
import './updates.css';

const ReleaseMarkdown = lazy(() => import('react-markdown'));

type Updates = {
  result: UpdateCheck | null;
  checking: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  show: () => void;
};
const UpdateContext = createContext<Updates | null>(null);

/** One check per launch, independent of vault and workspace lifetimes. */
export function UpdateProvider({ children }: { children: ReactNode }) {
  const [result, setResult] = useState<UpdateCheck | null>(null);
  const [checking, setChecking] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  const inFlight = useRef<Promise<UpdateCheck> | null>(null);
  const mounted = useRef(false);
  const failed = useRef(false);

  const refresh = useCallback(async () => {
    setChecking(true);
    setError(null);
    const request = inFlight.current ??= updateCheck();
    try {
      const next = await request;
      if (mounted.current) { setResult(next); failed.current = false; }
    } catch (err) {
      if (mounted.current) { setError(describeError(err)); failed.current = true; }
    } finally {
      if (inFlight.current === request) inFlight.current = null;
      if (mounted.current) setChecking(false);
    }
  }, []);

  useEffect(() => {
    mounted.current = true;
    void refresh();
    const online = () => { if (failed.current) void refresh(); };
    window.addEventListener('online', online);
    return () => { mounted.current = false; window.removeEventListener('online', online); };
  }, [refresh]);

  const value: Updates = { result, checking, error, refresh, show: () => setOpen(true) };
  return <UpdateContext.Provider value={value}>
    {children}
    {open && <UpdateDialog updates={value} close={() => setOpen(false)} />}
  </UpdateContext.Provider>;
}

/** Hidden until a newer version is known. Closing the dialog keeps this entry. */
export function UpdateButton() {
  const updates = useContext(UpdateContext);
  if (!updates?.result?.update) return null;
  return <button type="button" className="update-button" onClick={updates.show}
    title={`Selai ${updates.result.update.version} is available`}>
    <ImportIcon size={14} /><span>Update available</span>
  </button>;
}

export function UpdateVersion({ version }: { version?: string }) {
  const updates = useContext(UpdateContext);
  if (!updates) return <span className="status-item">{version ? `Selai ${version}` : ''}</span>;
  return <button type="button" className="status-item update-version" title="Check for updates" onClick={() => {
    updates.show(); void updates.refresh();
  }}>{version ? `Selai ${version}` : 'Updates'}</button>;
}

function UpdateDialog({ updates, close }: { updates: Updates; close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [opening, setOpening] = useState(false);
  const [opened, setOpened] = useState(false);
  const [openError, setOpenError] = useState<string | null>(null);
  const update = updates.result?.update;
  const download = update?.download;
  useEffect(() => {
    const element = dialog.current;
    const previous = document.activeElement;
    element?.showModal();
    return () => {
      element?.close();
      if (previous instanceof HTMLElement && previous.isConnected) previous.focus();
    };
  }, []);

  async function openTarget(target: 'release' | 'download') {
    setOpening(true); setOpenError(null);
    try { await updateOpen(target); if (target === 'download') setOpened(true); }
    catch (err) { setOpenError(describeError(err)); }
    finally { setOpening(false); }
  }

  return <dialog ref={dialog} className="dialog update-dialog" aria-labelledby="update-title" onCancel={(event) => { event.preventDefault(); close(); }}>
    <header className="dialog-head">
      <span className="dialog-icon">{update ? <ImportIcon size={19} /> : <RefreshIcon size={19} />}</span>
      <div className="grow"><span className="update-eyebrow">SELAI UPDATES</span>
        <h2 id="update-title">{update ? 'A new version is ready' : updates.checking ? 'Checking for updates' : updates.error ? 'Could not check for updates' : 'You’re up to date'}</h2>
      </div>
      <button type="button" className="icon-btn" aria-label="Close updates" onClick={close}><CloseIcon size={17} /></button>
    </header>
    <div className="dialog-body">
      {update ? <>
        <div className="update-versions"><span>Installed <strong>{updates.result?.currentVersion}</strong></span>
          <span aria-hidden="true">→</span><span>Available <strong>{update.version}</strong></span></div>
        <section className="update-notes" aria-label="Release notes">
          <h3>What’s new{update.publishedAt && Number.isFinite(Date.parse(update.publishedAt))
            ? <time dateTime={update.publishedAt}>{new Date(update.publishedAt).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })}</time> : null}</h3>
          {update.notes.trim() ? <Suspense fallback={<p>Loading release notes…</p>}>
            <ReleaseMarkdown skipHtml disallowedElements={['img', 'a']} unwrapDisallowed>{update.notes}</ReleaseMarkdown>
          </Suspense>
            : <p>Release notes are available on GitHub.</p>}
        </section>
        {download ? <div className="update-installer"><ImportIcon size={16} /><div><strong>{download.name}</strong>
          <span>{(download.size / 1048576).toFixed(1)} MB · Download from GitHub</span></div></div>
          : <p>An installer for this device is not available yet. See the release on GitHub for download options.</p>}
        {opened ? <div role="status" className="update-next-step"><CheckIcon size={17} /><div><strong>Download opened in your browser</strong>
          <p>When the download finishes, save your work and close Selai, then run the installer to update.</p></div></div>
          : download && <p className="update-hint">Download the installer in your browser. When you’re ready, save your work and close Selai before installing.</p>}
      </> : updates.checking ? <p role="status" className="history-loading"><span className="spinner" /> Looking for a newer release on GitHub…</p>
        : !updates.error && <p role="status">You’re running the latest available version{updates.result ? ` (${updates.result.currentVersion})` : ''}.</p>}
      {(updates.error || openError) && <p role="alert" className="banner" data-tone="danger">{openError || updates.error}</p>}
    </div>
    <footer className="dialog-foot">
      {update && <button type="button" className="btn btn-ghost" disabled={opening} onClick={() => void openTarget('release')}>View on GitHub</button>}
      <span className="grow" />
      <button type="button" className="btn" onClick={close}>{opened ? 'Keep working' : update ? 'Later' : 'Close'}</button>
      {download ? <button type="button" className="btn btn-primary" disabled={opening} onClick={() => void openTarget('download')}>
        {opening ? <span className="spinner" /> : <ImportIcon size={14} />}{opening ? 'Opening…' : opened ? 'Download again' : 'Download update'}</button>
        : (!update || updates.error) && <button type="button" className="btn btn-primary" disabled={updates.checking} onClick={() => void updates.refresh()}>
          {updates.checking ? 'Checking…' : 'Check again'}</button>}
    </footer>
  </dialog>;
}
