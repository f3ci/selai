// Turns the task's event stream into the transcript the operator reads. Pure:
// the Rust core decides what happens; this only arranges what is shown.
import type { ExecutionView, SudoNeed, TaskEvent } from '../ipc';

export type ApprovalState = 'pending' | 'sending' | 'approved' | 'denied';

export type TranscriptItem =
  | { kind: 'text'; text: string }
  | { kind: 'status'; text: string }
  | { kind: 'tool'; executionId: string; command: string; hostLabel: string; status: string; exitCode: number | null; stdoutPreview?: string | null; stderrPreview?: string | null; outputTruncated?: boolean }
  | {
      kind: 'approval';
      approvalId: string;
      executionId: string;
      command: string;
      expiresAtMs: number;
      state: ApprovalState;
      error: string | null;
      /** Set for a root command. */
      sudo: SudoNeed | null;
    }
  | { kind: 'failed'; code: string; message: string };

const QUIET_STATES = new Set(['running', 'validating_scope', 'ready', 'awaiting_approval', 'completed']);

export function isTerminal(event: TaskEvent): boolean {
  return (
    (event.type === 'state' && ['completed', 'failed', 'cancelled'].includes(event.state)) ||
    event.type === 'completed' ||
    event.type === 'failed'
  );
}

export function applyEvent(items: TranscriptItem[], event: TaskEvent): TranscriptItem[] {
  switch (event.type) {
    case 'textDelta': {
      const last = items.at(-1);
      if (last?.kind === 'text') {
        return [...items.slice(0, -1), { kind: 'text', text: last.text + event.text }];
      }
      return [...items, { kind: 'text', text: event.text }];
    }
    case 'state':
      // Routine steps are noise in a chat; the tool cards and the turn's spinner
      // already show them. Outcomes that need attention (cancelled, failed,
      // interrupted) stay visible.
      if (QUIET_STATES.has(event.state)) return items;
      return [...items, { kind: 'status', text: stateText(event.state, event.errorCode) }];
    case 'toolProposed':
      return [
        ...items,
        {
          kind: 'tool',
          executionId: event.executionId,
          command: event.command,
          hostLabel: event.hostLabel,
          status: 'proposed',
          exitCode: null,
        },
      ];
    case 'approvalRequired':
      return [
        ...items,
        {
          kind: 'approval',
          approvalId: event.approvalId,
          executionId: event.executionId,
          command: event.command,
          expiresAtMs: event.expiresAtMs,
          state: 'pending',
          error: null,
          sudo: event.sudo ?? null,
        },
      ];
    case 'toolFinished':
      return items.map((item) =>
        item.kind === 'tool' && item.executionId === event.executionId
          ? { ...item, status: event.state, exitCode: event.exitCode }
          : item,
      );
    case 'usage':
      // Token totals belong to the turn, not the transcript; the panel keeps them.
      return items;
    case 'completed':
      return items;
    case 'failed':
      return [...items, { kind: 'failed', code: event.code, message: event.message }];
  }
}

export function setApprovalState(
  items: TranscriptItem[],
  approvalId: string,
  state: ApprovalState,
  error: string | null = null,
): TranscriptItem[] {
  return items.map((item) =>
    item.kind === 'approval' && item.approvalId === approvalId ? { ...item, state, error } : item,
  );
}

export function setExecutionOutput(items: TranscriptItem[], execution: ExecutionView): TranscriptItem[] {
  return items.map((item) => item.kind === 'tool' && item.executionId === execution.id
    ? { ...item, stdoutPreview: execution.stdoutPreview, stderrPreview: execution.stderrPreview, outputTruncated: execution.outputTruncated }
    : item);
}

function stateText(state: string, errorCode: string | null): string {
  const label = state.replace(/_/g, ' ');
  return errorCode ? `${label} (${errorCode})` : label;
}
