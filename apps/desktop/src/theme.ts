import { useEffect, useState } from 'react';
import { CATPPUCCIN, type CatppuccinPalette, type CatppuccinTheme } from './catppuccin';

export type Theme = 'dark' | 'light' | 'system' | CatppuccinTheme;
export type ResolvedTheme = Exclude<Theme, 'system'>;
export const THEME_KEY = 'selai.theme';
export const THEME_OPTIONS: { value: Theme; label: string }[] = [
  { value: 'dark', label: 'Dark' },
  { value: 'light', label: 'Light' },
  { value: 'system', label: 'System' },
  { value: 'catppuccin-latte', label: 'Catppuccin Latte' },
  { value: 'catppuccin-frappe', label: 'Catppuccin Frappé' },
  { value: 'catppuccin-macchiato', label: 'Catppuccin Macchiato' },
  { value: 'catppuccin-mocha', label: 'Catppuccin Mocha' },
];

export function readThemePreference(): Theme {
  try {
    const saved = window.localStorage.getItem(THEME_KEY);
    return THEME_OPTIONS.find((option) => option.value === saved)?.value ?? 'dark';
  } catch { return 'dark'; }
}

export function isLightTheme(theme: ResolvedTheme): boolean {
  return theme === 'light' || theme === 'catppuccin-latte';
}

function catppuccinVariables(p: CatppuccinPalette, light: boolean): Record<string, string> {
  return {
    '--bg': p.base, '--sidebar': p.mantle, '--surface': p.base,
    '--surface-2': p.mantle, '--surface-3': p.surface0,
    '--border': p.surface0, '--border-strong': p.surface1,
    '--fg': p.text, '--fg-strong': p.text, '--muted': p.subtext1, '--faint': p.subtext0,
    '--accent': p.mauve, '--accent-hover': `color-mix(in srgb, ${p.mauve} 85%, ${p.text})`,
    '--accent-soft': `${p.mauve}1a`, '--accent-fg': light ? p.base : p.crust,
    '--success': light ? `color-mix(in srgb, ${p.green} 55%, ${p.text})` : p.green,
    '--success-soft': `${p.green}1a`,
    '--warning': light ? `color-mix(in srgb, ${p.yellow} 45%, ${p.text})` : p.yellow,
    '--warning-soft': `${p.yellow}1a`, '--danger': p.red, '--danger-soft': `${p.red}1a`,
    '--term-bg': p.base, '--terminal-bg': p.base, '--terminal-fg': p.text,
    '--terminal-overlay': `${p.base}eb`,
    '--shadow': `0 16px 48px ${p.crust}${light ? '24' : '66'}`,
    'color-scheme': light ? 'light' : 'dark',
  };
}

const CUSTOM_THEME_PROPERTIES = Object.keys(catppuccinVariables(CATPPUCCIN['catppuccin-mocha'], false));

/** Also used before React mounts so the unlock screen uses the saved palette. */
export function applyTheme(theme: Theme): void {
  const root = document.documentElement;
  if (theme === 'system') delete root.dataset.theme;
  else root.dataset.theme = theme;
  for (const name of CUSTOM_THEME_PROPERTIES) root.style.removeProperty(name);
  if (theme in CATPPUCCIN) {
    const variables = catppuccinVariables(CATPPUCCIN[theme as CatppuccinTheme], theme === 'catppuccin-latte');
    for (const [name, value] of Object.entries(variables)) root.style.setProperty(name, value);
  }
}

/** One system-theme subscription for the workbench, shared by all terminals. */
export function useResolvedTheme(theme: Theme): ResolvedTheme {
  const [systemLight, setSystemLight] = useState(() => window.matchMedia?.('(prefers-color-scheme: light)').matches ?? false);
  useEffect(() => {
    const media = window.matchMedia?.('(prefers-color-scheme: light)');
    if (!media) return;
    const changed = () => setSystemLight(media.matches);
    changed();
    media.addEventListener('change', changed);
    return () => media.removeEventListener('change', changed);
  }, []);
  return theme === 'system' ? (systemLight ? 'light' : 'dark') : theme;
}
