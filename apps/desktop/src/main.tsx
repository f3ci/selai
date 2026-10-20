import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import { applyTheme, readThemePreference } from './theme';
import './styles.css';
import './network.css';

// Apply the display preference before mounting the vault screen. Workbench
// applies later theme changes after unlock, but is not mounted while locked.
applyTheme(readThemePreference());

const root = document.getElementById('root');
if (!root) {
  throw new Error('missing #root element');
}

createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
