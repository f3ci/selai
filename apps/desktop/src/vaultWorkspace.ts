import { createContext } from 'react';

/** A locked workspace retains terminal buffers only; other views unmount. */
export const VaultWorkspacePhase = createContext<'unlocked' | 'locking' | 'locked'>('unlocked');
