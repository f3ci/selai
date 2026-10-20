import { act, renderHook } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { applyTheme, readThemePreference, THEME_KEY, useResolvedTheme, type Theme } from './theme';

afterEach(() => { vi.unstubAllGlobals(); localStorage.clear(); applyTheme('dark'); });

it('restores a saved Catppuccin theme and clears custom colors when returning to neutral themes', () => {
  localStorage.setItem(THEME_KEY, 'catppuccin-mocha');
  applyTheme(readThemePreference());
  expect(document.documentElement.dataset.theme).toBe('catppuccin-mocha');
  expect(document.documentElement.style.getPropertyValue('--terminal-bg')).toBe('#1e1e2e');
  expect(document.documentElement.style.getPropertyValue('--fg')).toBe('#cdd6f4');
  expect(document.documentElement.style.colorScheme).toBe('dark');
  applyTheme('catppuccin-latte');
  expect(document.documentElement.style.colorScheme).toBe('light');
  applyTheme('light');
  expect(document.documentElement.style.getPropertyValue('--terminal-bg')).toBe('');
  expect(document.documentElement.dataset.theme).toBe('light');
  applyTheme('system');
  expect(document.documentElement.dataset.theme).toBeUndefined();
  expect(document.documentElement.style.colorScheme).toBe('');
});

it('follows system changes only when System is selected and removes the listener', () => {
  let changed = () => {};
  const media = { matches: false, addEventListener: vi.fn((_name, callback) => { changed = callback; }), removeEventListener: vi.fn() };
  vi.stubGlobal('matchMedia', () => media);
  const { result, rerender, unmount } = renderHook(({ theme }: { theme: Theme }) => useResolvedTheme(theme), { initialProps: { theme: 'system' as Theme } });
  expect(result.current).toBe('dark');
  act(() => { media.matches = true; changed(); });
  expect(result.current).toBe('light');
  rerender({ theme: 'dark' });
  expect(result.current).toBe('dark');
  unmount();
  expect(media.removeEventListener).toHaveBeenCalledWith('change', changed);
});
