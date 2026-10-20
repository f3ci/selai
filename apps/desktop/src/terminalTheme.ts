import type { ITheme } from '@xterm/xterm';
import { CATPPUCCIN } from './catppuccin';
import type { ResolvedTheme } from './theme';

const DARK_THEME: ITheme = {
  background: '#141618',
  foreground: '#d8dbe8',
  cursor: '#a8c7b8',
  cursorAccent: '#141618',
  selectionBackground: 'rgba(168, 199, 184, 0.25)',
  black: '#1c2031',
  red: '#ef6b6b',
  green: '#3fcf86',
  yellow: '#f0b84a',
  blue: '#6b8cff',
  magenta: '#b58cff',
  cyan: '#3cc8dc',
  white: '#d8dbe8',
  brightBlack: '#5d6380',
  brightRed: '#ff8a8a',
  brightGreen: '#6be3a4',
  brightYellow: '#ffd27a',
  brightBlue: '#93acff',
  brightMagenta: '#cfb0ff',
  brightCyan: '#7adcea',
  brightWhite: '#ffffff',
};

const LIGHT_THEME: ITheme = {
  background: '#fafaf8', foreground: '#333b35', cursor: '#38634f', cursorAccent: '#fafaf8',
  selectionBackground: 'rgba(56, 99, 79, 0.22)',
  black: '#26302a', red: '#a62e2a', green: '#246b3d', yellow: '#806000',
  blue: '#245cb0', magenta: '#813e9f', cyan: '#006c7a', white: '#58665d',
  brightBlack: '#65736a', brightRed: '#b23730', brightGreen: '#28713e', brightYellow: '#866000',
  brightBlue: '#295cc1', brightMagenta: '#853ba8', brightCyan: '#007080', brightWhite: '#1b241e',
};

export function terminalTheme(theme: ResolvedTheme): ITheme {
  if (theme === 'dark') return DARK_THEME;
  if (theme === 'light') return LIGHT_THEME;
  const palette = CATPPUCCIN[theme];
  return {
    ...palette.ansi,
    background: palette.base,
    foreground: palette.text,
    cursor: palette.rosewater,
    cursorAccent: palette.base,
    selectionBackground: `${palette.surface2}80`,
    selectionForeground: palette.text,
  };
}
