import { useEffect, useId, useRef, type ReactNode } from 'react';
import { CloseIcon } from './icons';

interface DrawerProps {
  title: string;
  subtitle?: string;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
}

const modalStack: HTMLElement[] = [];
let previousOverflow = '';

/** Only the top overlay handles keyboard input and owns focus. */
function useModal(onClose: () => void) {
  const root = useRef<HTMLDivElement>(null);
  const close = useRef(onClose);
  useEffect(() => { close.current = onClose; }, [onClose]);
  useEffect(() => {
    const element = root.current;
    if (!element) return;
    const previousFocus = document.activeElement;
    if (modalStack.length === 0) {
      previousOverflow = document.body.style.overflow;
      document.body.style.overflow = 'hidden';
    }
    modalStack.push(element);
    const isTop = () => modalStack.at(-1) === element;
    const controls = () => [...element.querySelectorAll<HTMLElement>(
      'button, input, select, textarea, a[href], summary, [tabindex], [contenteditable="true"]',
    )].filter((control) => control.tabIndex >= 0 && !control.matches(':disabled')
      && !control.closest('[hidden], [inert], details:not([open]) > :not(summary)')
      && getComputedStyle(control).display !== 'none' && getComputedStyle(control).visibility !== 'hidden');
    const focusInside = () => {
      const preferred = element.querySelector<HTMLElement>('[data-autofocus], [autofocus]');
      (preferred && !preferred.matches(':disabled') ? preferred : controls()[0] ?? element).focus();
    };
    if (!element.contains(document.activeElement)) focusInside();
    const onKey = (e: KeyboardEvent) => {
      if (!isTop() || e.defaultPrevented) return;
      if (e.key === 'Escape') {
        e.preventDefault();
        e.stopPropagation();
        close.current();
      } else if (e.key === 'Tab') {
        const items = controls();
        const first = items[0];
        const last = items.at(-1);
        if (!first) { e.preventDefault(); element.focus(); }
        else if (e.shiftKey && (document.activeElement === first || document.activeElement === element)) {
          e.preventDefault(); last?.focus();
        } else if (!e.shiftKey && (document.activeElement === last || document.activeElement === element)) {
          e.preventDefault(); first.focus();
        }
      }
    };
    const onFocus = (event: FocusEvent) => {
      if (isTop() && event.target instanceof Node && !element.contains(event.target)) focusInside();
    };
    document.addEventListener('keydown', onKey);
    document.addEventListener('focusin', onFocus);
    return () => {
      const wasTop = isTop();
      modalStack.splice(modalStack.indexOf(element), 1);
      document.removeEventListener('keydown', onKey);
      document.removeEventListener('focusin', onFocus);
      if (modalStack.length === 0) document.body.style.overflow = previousOverflow;
      if (wasTop && previousFocus instanceof HTMLElement && previousFocus.isConnected) previousFocus.focus();
    };
  }, []);
  return root;
}

/** A panel that slides in from the right edge for create and edit forms. */
export function Drawer({ title, subtitle, onClose, children, footer }: DrawerProps) {
  const titleId = useId();
  const root = useModal(onClose);
  return (
    <div ref={root} tabIndex={-1} className="scrim" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <aside className="drawer" role="dialog" aria-modal="true" aria-labelledby={titleId}>
        <header className="drawer-head">
          <div>
            <h2 id={titleId}>{title}</h2>
            {subtitle && <p className="muted">{subtitle}</p>}
          </div>
          <button type="button" className="icon-btn" aria-label="Close panel" onClick={onClose}>
            <CloseIcon />
          </button>
        </header>
        <div className="drawer-body">{children}</div>
        {footer && <footer className="drawer-foot">{footer}</footer>}
      </aside>
    </div>
  );
}

interface DialogProps {
  title: string;
  tone?: 'default' | 'danger';
  icon?: ReactNode;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  /** `contained` keeps the dialog inside its positioned parent, such as one terminal pane. */
  contained?: boolean;
  className?: string;
  eyebrow?: string;
  closeLabel?: string;
  closeDisabled?: boolean;
  role?: 'dialog' | 'alertdialog';
  describedBy?: string;
}

export function Dialog({ title, tone = 'default', icon, onClose, children, footer, contained,
  className = '', eyebrow, closeLabel = 'Close dialog', closeDisabled, role = 'dialog', describedBy }: DialogProps) {
  const titleId = useId();
  const root = useModal(onClose);
  return (
    <div ref={root} tabIndex={-1} className={contained ? 'scrim scrim-contained' : 'scrim'}>
      <div className={`dialog ${className}`} data-tone={tone} role={role} aria-modal="true" aria-labelledby={titleId} aria-describedby={describedBy}>
        <header className="dialog-head">
          {icon && <span className="dialog-icon">{icon}</span>}
          <div className="dialog-heading">
            {eyebrow && <span className="dialog-eyebrow">{eyebrow}</span>}
            <h2 id={titleId}>{title}</h2>
          </div>
          <button type="button" className="icon-btn" aria-label={closeLabel} disabled={closeDisabled} onClick={onClose}>
            <CloseIcon size={16} />
          </button>
        </header>
        <div className="dialog-body">{children}</div>
        {footer && <footer className="dialog-foot">{footer}</footer>}
      </div>
    </div>
  );
}
