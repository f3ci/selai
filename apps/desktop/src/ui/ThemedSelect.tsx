import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { ChevronIcon } from './icons';

export interface SelectOption { value: string; label: ReactNode; detail?: string; disabled?: boolean }

/** A consistent, keyboard-accessible single-select menu for the whole app. */
export function ThemedSelect({ label, title, value, options, disabled, className = '', placeholder, required, onChange }: {
  label: string;
  title?: string;
  value: string;
  options: SelectOption[];
  disabled?: boolean;
  className?: string;
  placeholder?: string;
  required?: boolean;
  onChange: (value: string) => void;
}) {
  const id = useId();
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const [open, setOpen] = useState(false);
  const selected = options.find((option) => option.value === value);
  const enabledOptions = options.filter((option) => !option.disabled);

  useEffect(() => {
    if (!open) return;
    const close = (event: PointerEvent) => {
      if (event.target instanceof Node && !root.current?.contains(event.target)) setOpen(false);
    };
    document.addEventListener('pointerdown', close);
    return () => document.removeEventListener('pointerdown', close);
  }, [open]);

  function choose(option: SelectOption) {
    if (option.disabled) return;
    onChange(option.value);
    setOpen(false);
    trigger.current?.focus();
  }

  function keyDown(event: KeyboardEvent<HTMLButtonElement>) {
    if (disabled || enabledOptions.length === 0) return;
    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      if (!open) { setOpen(true); return; }
      const current = Math.max(0, enabledOptions.findIndex((option) => option.value === value));
      const next = event.key === 'ArrowDown'
        ? (current + 1) % enabledOptions.length
        : (current - 1 + enabledOptions.length) % enabledOptions.length;
      choose(enabledOptions[next]!);
    } else if (event.key === 'Escape' && open) {
      event.preventDefault(); setOpen(false);
    } else if ((event.key === 'Enter' || event.key === ' ') && !open) {
      event.preventDefault(); setOpen(true);
    }
  }

  return <div className={`themed-select ${className}`} ref={root}>
    <button ref={trigger} type="button" role="combobox" aria-label={label} aria-valuetext={selected ? String(selected.label) : undefined}
      aria-expanded={open} aria-controls={id} aria-haspopup="listbox" aria-required={required || undefined}
      disabled={disabled || enabledOptions.length === 0} className="themed-select-trigger"
      title={title ?? (selected ? String(selected.label) : label)} onClick={() => setOpen((current) => !current)} onKeyDown={keyDown}>
      <span>{selected?.label ?? placeholder ?? (options.length ? `Choose ${label.toLowerCase()}` : `No ${label.toLowerCase()}s`)}</span>
      <ChevronIcon size={14} />
    </button>
    {required && <select className="themed-select-validity" tabIndex={-1} aria-hidden="true" value={value} required disabled={disabled}
      onInvalid={(event) => { event.preventDefault(); trigger.current?.focus(); setOpen(true); }} onChange={() => undefined}>
      <option value="" />{options.map((option) => <option key={option.value} value={option.value}>{String(option.label)}</option>)}
    </select>}
    {open && <ul id={id} className="themed-select-menu" role="listbox" aria-label={label}>
      {options.map((option) => <li key={option.value}>
        <button type="button" role="option" data-value={option.value} aria-selected={option.value === value} disabled={option.disabled} onClick={() => choose(option)}>
          <span>{option.label}</span>{option.detail && <small>{option.detail}</small>}
        </button>
      </li>)}
    </ul>}
  </div>;
}
