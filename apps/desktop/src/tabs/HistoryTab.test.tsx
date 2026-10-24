import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { chooseSelectValue } from '../ui/selectTestUtils';
import { HistoryTab, HostHistoryTab } from './HistoryTab';

afterEach(() => vi.restoreAllMocks());

const host: ipc.HostView = {
  id: 'h1',
  label: 'production-web',
  groupId: null,
  hostname: '10.0.0.5',
  port: 22,
  username: 'ops',
  authMethod: 'password',
  privateKeyPath: null,
  connectTimeoutSeconds: 10,
  keepAliveSeconds: 30,
  terminalType: 'xterm-256color',
  notes: null,
  credentialConfigured: true,
  updatedAtMs: 1,
  lastConnectedAtMs: null,
  archived: false,
  kind: 'ssh',
};

const archived: ipc.HostView = { ...host, id: 'h2', label: 'retired-db', archived: true };

function execution(overrides: Partial<ipc.ExecutionView> = {}): ipc.ExecutionView {
  return {
    id: 'e1',
    taskId: 't1',
    hostId: 'h1',
    modelId: 'qwen-tools',
    tool: 'ssh.run_diagnostic',
    commandRedacted: 'df -hP',
    commandSha256: 'ab'.repeat(32),
    state: 'completed',
    policyDecision: 'allow',
    exitCode: 0,
    errorCode: null,
    stdoutPreview: 'Filesystem  Size  Used',
    stderrPreview: null,
    outputTruncated: false,
    createdAtMs: Date.UTC(2026, 9, 16, 9, 0),
    startedAtMs: null,
    finishedAtMs: null,
    ...overrides,
  };
}

function task(overrides: Partial<ipc.TaskView> = {}): ipc.TaskView {
  return {
    id: 't1',
    hostId: 'h1',
    state: 'completed',
    modelId: 'qwen-tools',
    errorCode: null,
    createdAtMs: Date.UTC(2026, 9, 16, 9, 0),
    finishedAtMs: Date.UTC(2026, 9, 16, 9, 0, 12),
    promptPreview: 'check disk usage on the web node',
    inputTokens: 12345,
    outputTokens: 678,
    modelTurns: 2,
    executions: 1,
    ...overrides,
  };
}

function mockHistory(rows: ipc.ExecutionView[] = [execution()], tasks: ipc.TaskView[] = [task()]) {
  vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host, archived], nextCursor: null });
  vi.spyOn(ipc, 'historyTasks').mockResolvedValue({ items: tasks, nextCursor: null });
  vi.spyOn(ipc, 'historySummary').mockResolvedValue({
    tasks: tasks.length,
    executions: rows.length,
    failedExecutions: 1,
    inputTokens: 12345,
    outputTokens: 678,
    lastActivityMs: Date.UTC(2026, 9, 16, 9, 0),
  });
  return vi
    .spyOn(ipc, 'historyQuery')
    .mockResolvedValue({ items: rows, nextCursor: null });
}

/** The command table lives in the Commands view. */
async function showCommands() {
  fireEvent.click(await screen.findByRole('button', { name: 'Commands' }));
  return screen.findByText('df -hP');
}

describe('HostHistoryTab', () => {
  it('lets the operator configure the task step and time limits', async () => {
    mockHistory();
    vi.spyOn(ipc, 'preferencesGet').mockResolvedValue({
      lastProviderId: null,
      outputRetentionDays: 30,
      pinnedHostId: null,
      taskMaxSteps: 30,
      taskMaxMinutes: 30,
    });
    const save = vi.spyOn(ipc, 'preferencesSet').mockResolvedValue({
      lastProviderId: null,
      outputRetentionDays: 30,
      pinnedHostId: null,
      taskMaxSteps: 12,
      taskMaxMinutes: 15,
    });
    render(<HostHistoryTab visible />);

    const steps = await screen.findByLabelText('Maximum steps');
    const minutes = screen.getByLabelText('Time limit (minutes)');
    fireEvent.change(steps, { target: { value: '12' } });
    fireEvent.change(minutes, { target: { value: '15' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save task limits' }));

    await waitFor(() => expect(save).toHaveBeenCalledWith({ taskMaxSteps: 12, taskMaxMinutes: 15 }));
    expect(await screen.findByRole('status')).toHaveTextContent('12 steps or 15 minutes');
  });

  it('lists the chosen host history, including archived hosts', async () => {
    const query = mockHistory();
    render(<HostHistoryTab visible />);

    expect(await showCommands()).toBeInTheDocument();
    expect(query).toHaveBeenCalledWith('h1', { states: [] , sinceMs: null, untilMs: null }, undefined);
    fireEvent.click(screen.getByRole('combobox', { name: 'Host' }));
    expect(screen.getByRole('option', { name: 'retired-db (archived)' })).toBeInTheDocument();
  });

  it('loads every command page with one action and supports oldest-first sorting', async () => {
    const older = execution({ id: 'older', commandRedacted: 'older command', createdAtMs: 1 });
    const query = mockHistory();
    query.mockImplementation(async (_hostId, _filter, cursor) => cursor === 'next-page'
      ? { items: [older], nextCursor: null }
      : { items: [execution()], nextCursor: 'next-page' });
    render(<HostHistoryTab visible />);
    await waitFor(() => expect(query).toHaveBeenCalled());
    fireEvent.click(await screen.findByRole('button', { name: 'Commands' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Load all' }));
    expect(await screen.findByText('older command')).toBeInTheDocument();
    expect(query).toHaveBeenCalledWith('h1', { states: [], sinceMs: null, untilMs: null }, 'next-page');
    fireEvent.click(screen.getByRole('combobox', { name: 'Sort by' }));
    fireEvent.click(screen.getByRole('option', { name: 'Oldest command first' }));
    const rows = screen.getByRole('table').querySelectorAll('tbody tr');
    expect(rows[0]).toHaveTextContent('older command');
  });

  it('loads again when the tab is shown, so runs finished while hidden appear', async () => {
    const query = mockHistory();
    const { rerender } = render(<HostHistoryTab visible={false} />);
    await waitFor(() => expect(ipc.hostList).toHaveBeenCalled());
    expect(query).not.toHaveBeenCalled();
    expect(ipc.historyTasks).not.toHaveBeenCalled();

    rerender(<HostHistoryTab visible />);
    expect(await showCommands()).toBeInTheDocument();
    expect(query).toHaveBeenCalledTimes(1);
    expect(ipc.historyTasks).toHaveBeenCalledTimes(1);
  });

  it('sends the chosen states when the filter is applied', async () => {
    const query = mockHistory();
    render(<HostHistoryTab visible />);
    await showCommands();

    fireEvent.click(screen.getByRole('checkbox', { name: 'denied' }));
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));

    await waitFor(() =>
      expect(query).toHaveBeenLastCalledWith(
        'h1',
        expect.objectContaining({ states: ['denied'] }),
        undefined,
      ),
    );
  });

  it('shows the timeline of the task an execution belongs to', async () => {
    mockHistory();
    const timeline = vi.spyOn(ipc, 'taskTimeline').mockResolvedValue([
      execution({ id: 'e1', commandRedacted: 'uptime' }),
      execution({ id: 'e2', commandRedacted: 'df -hP', state: 'denied' }),
    ]);
    render(<HostHistoryTab visible />);
    fireEvent.click(await showCommands());

    expect(timeline).toHaveBeenCalledWith('t1');
    expect(await screen.findByText('Task timeline')).toBeInTheDocument();
    expect(screen.getByText(/uptime/)).toBeInTheDocument();
  });

  it('reports where the export went and that it was cut at the row limit', async () => {
    mockHistory();
    vi.spyOn(ipc, 'historyExport').mockResolvedValue({
      path: '/data/exports/selai-history-h1-1.json',
      executions: 1000,
      truncatedRows: true,
      extraRedactions: 0,
      disclosure: 'x',
    });
    render(<HostHistoryTab visible />);
    await screen.findByText('check disk usage on the web node');
    fireEvent.click(screen.getByRole('button', { name: 'Export encrypted' }));

    expect(await screen.findByRole('status')).toHaveTextContent(
      'Exported 1000 row(s) to /data/exports/selai-history-h1-1.json. The export was cut at the row limit.',
    );
  });

  it('clears stored output only after the operator confirms', async () => {
    mockHistory();
    const purge = vi.spyOn(ipc, 'historyPurgeOutput').mockResolvedValue({ executions: 1 });
    render(<HostHistoryTab visible />);
    await screen.findByText('check disk usage on the web node');

    fireEvent.click(screen.getByRole('button', { name: 'Clear stored output' }));
    expect(purge).not.toHaveBeenCalled();
    expect(screen.getByRole('alertdialog', { name: 'Clear stored output?' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(purge).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole('button', { name: 'Clear stored output' }));
    fireEvent.click(screen.getByRole('button', { name: 'Clear output' }));
    await waitFor(() => expect(purge).toHaveBeenCalledWith('h1'));
  });

  it('opens on tasks with the request, token usage and a host summary', async () => {
    mockHistory();
    render(<HostHistoryTab visible />);

    expect(await screen.findByText('check disk usage on the web node')).toBeInTheDocument();
    const summary = screen.getByRole('group', { name: 'Usage summary' });
    await waitFor(() => expect(summary).toHaveTextContent('Input tokens12,345'));
    expect(summary).toHaveTextContent('Output tokens678');
    expect(summary).toHaveTextContent('1 failed');
    const tasks = screen.getByRole('list', { name: 'Tasks' });
    expect(tasks).toHaveTextContent('12.3k in');
    expect(tasks).toHaveTextContent('678 out');
  });

  it('shows unknown token counts as unknown, not zero', async () => {
    mockHistory([execution()], [task({ inputTokens: null, outputTokens: null, promptPreview: null })]);
    render(<HostHistoryTab visible />);
    const tasks = await screen.findByRole('list', { name: 'Tasks' });
    await waitFor(() => expect(tasks).toHaveTextContent('— in'));
    expect(tasks).toHaveTextContent('Request not recorded');
  });

  it('opens a task with its tokens and the commands it ran', async () => {
    mockHistory();
    const timeline = vi
      .spyOn(ipc, 'taskTimeline')
      .mockResolvedValue([execution({ commandRedacted: 'du -sh /var' })]);
    render(<HostHistoryTab visible />);
    fireEvent.click(await screen.findByText('check disk usage on the web node'));

    expect(timeline).toHaveBeenCalledWith('t1');
    const usage = await screen.findByRole('group', { name: 'Token usage' });
    expect(usage).toHaveTextContent('Input12,345');
    expect(usage).toHaveTextContent('Total13,023');
    expect(await screen.findByText('du -sh /var')).toBeInTheDocument();
  });
});

const session = { id: 's1', title: 'Investigate web cluster', createdAtMs: 1, updatedAtMs: 20 };

function savedTurn(taskId: string | null, hostId = 'h1', overrides = {}) {
  return { id: taskId ?? 'chat', hostId, hostLabel: hostId === 'h1' ? 'production-web' : 'staging-web',
    taskId, taskPrompt: 'Check disk usage', prompt: 'Check disk usage', showPrompt: true,
    mentionText: null, items: [], running: false, notice: null, error: null, ...overrides };
}

function mockSessions(turns = [savedTurn('t1')]) {
  const list = vi.spyOn(ipc, 'chatSessionList').mockResolvedValue([
    session, { ...session, id: 's2', title: 'Deploy database' },
  ]);
  const get = vi.spyOn(ipc, 'chatSessionGet').mockResolvedValue({ ...session,
    transcript: JSON.stringify({ version: 1, turns }) });
  const timeline = vi.spyOn(ipc, 'taskTimeline').mockResolvedValue([execution()]);
  return { list, get, timeline };
}

describe('Session history', () => {
  it('sorts sessions by activity and loads all matching sessions with one action', async () => {
    const { list, get } = mockSessions();
    list.mockResolvedValue(Array.from({ length: 65 }, (_, index) => ({
      ...session, id: `s${index}`, title: `Session ${index}`, updatedAtMs: index + 1,
    })));
    render(<HistoryTab visible />);
    const sessions = screen.getByRole('list', { name: 'Sessions' });
    await waitFor(() => expect(within(sessions).getAllByRole('listitem')).toHaveLength(30));
    expect(within(sessions).getAllByRole('button')[0]).toHaveTextContent('Session 64');
    fireEvent.click(screen.getByRole('button', { name: 'Load more sessions' }));
    expect(within(sessions).getAllByRole('listitem')).toHaveLength(60);
    fireEvent.click(screen.getByRole('button', { name: 'Load all sessions' }));
    expect(within(sessions).getAllByRole('listitem')).toHaveLength(65);
    expect(screen.queryByRole('button', { name: 'Load all sessions' })).not.toBeInTheDocument();
    chooseSelectValue('Sort sessions by', 'oldest');
    expect(within(sessions).getAllByRole('button')[0]).toHaveTextContent('Session 0');
    expect(within(sessions).getAllByRole('listitem')).toHaveLength(30);
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search sessions' }), { target: { value: 'Session 6' } });
    expect(within(sessions).getAllByRole('listitem')).toHaveLength(6);
    expect(get).not.toHaveBeenCalled();
  });

  it('shows newest requests and commands first and can switch to oldest first', async () => {
    const { timeline } = mockSessions([
      savedTurn('old', 'h1', { taskPrompt: 'Earlier request' }),
      savedTurn('new', 'h1', { taskPrompt: 'Recent request' }),
    ]);
    timeline.mockImplementation(async (id) => [
      execution({ id: `${id}-1`, taskId: id, commandRedacted: `${id} earlier command`, createdAtMs: 1 }),
      execution({ id: `${id}-2`, taskId: id, commandRedacted: `${id} latest command`, createdAtMs: 2 }),
    ]);
    render(<HistoryTab visible />);
    fireEvent.click(await screen.findByText(session.title));
    await screen.findByText('new latest command');
    const requests = screen.getByRole('list', { name: 'Session requests' });
    const firstRequest = () => within(requests).getAllByRole('listitem').filter((item) => item.classList.contains('session-request'))[0]!;
    expect(within(firstRequest()).getByRole('heading')).toHaveTextContent('Recent request');
    expect(within(firstRequest()).getAllByRole('button')[0]).toHaveTextContent('new latest command');
    chooseSelectValue('Sort by', 'oldest');
    expect(within(firstRequest()).getByRole('heading')).toHaveTextContent('Earlier request');
    expect(within(firstRequest()).getAllByRole('button')[0]).toHaveTextContent('old earlier command');
    expect(timeline).toHaveBeenCalledTimes(2);
  });

  it('opens on searchable session titles and loads no transcripts or command records before selection', async () => {
    const { get, timeline } = mockSessions();
    render(<HistoryTab visible />);
    expect(await screen.findByText(session.title)).toBeInTheDocument();
    expect(screen.getByText('Deploy database')).toBeInTheDocument();
    expect(get).not.toHaveBeenCalled();
    expect(timeline).not.toHaveBeenCalled();
    fireEvent.change(screen.getByRole('searchbox', { name: 'Search sessions' }), { target: { value: 'CLUSTER' } });
    expect(screen.getByText(session.title)).toBeInTheDocument();
    expect(screen.queryByText('Deploy database')).not.toBeInTheDocument();
  });

  it('shows audited commands from multiple servers inside their saved session and opens output details', async () => {
    const { get, timeline } = mockSessions([savedTurn('t1'), savedTurn('t2', 'h2'), savedTurn('t1')]);
    timeline.mockImplementation(async (id) => [execution({ id: `e-${id}`, taskId: id,
      hostId: id === 't1' ? 'h1' : 'h2', commandRedacted: id === 't1' ? 'df -hP' : 'uptime' })]);
    render(<HistoryTab visible />);
    fireEvent.click(await screen.findByText(session.title));
    expect(await screen.findByText('df -hP')).toBeInTheDocument();
    expect(await screen.findByText('uptime')).toBeInTheDocument();
    expect(screen.getByText('production-web')).toBeInTheDocument();
    expect(screen.getByText('staging-web')).toBeInTheDocument();
    expect(screen.getByText('2 servers')).toBeInTheDocument();
    expect(timeline).toHaveBeenCalledTimes(2);
    expect(get).toHaveBeenCalledWith('s1');
    fireEvent.click(screen.getByText('df -hP'));
    const details = await screen.findByRole('region', { name: 'Execution details' });
    expect(within(details).getByText(/Filesystem\s+Size\s+Used/)).toBeInTheDocument();
    expect(within(details).getByText('Exit code')).toBeInTheDocument();
    expect(within(details).getByText('0')).toBeInTheDocument();
  });

  it('does not mistake a failed command lookup for an empty history and can retry it', async () => {
    const { timeline } = mockSessions();
    timeline.mockRejectedValueOnce(new Error('Database unavailable'));
    render(<HistoryTab visible />);
    fireEvent.click(await screen.findByText(session.title));
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not load commands');
    expect(screen.queryByText('No command execution recorded for this request.')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Retry commands' }));
    expect(await screen.findByText('df -hP')).toBeInTheDocument();
    expect(screen.queryByRole('alert')).not.toBeInTheDocument();
  });

  it('loads all remaining request pages with one action and limits concurrent command lookups to four', async () => {
    const { timeline } = mockSessions(Array.from({ length: 27 }, (_, index) => savedTurn(`t${index}`)));
    let resolveBatch: (() => void)[] = [];
    let active = 0;
    let peak = 0;
    timeline.mockImplementation(() => new Promise((resolve) => {
      active++;
      peak = Math.max(peak, active);
      resolveBatch.push(() => { active--; resolve([]); });
    }));
    render(<HistoryTab visible />);
    fireEvent.click(await screen.findByText(session.title));
    await waitFor(() => expect(timeline).toHaveBeenCalledTimes(4));
    expect(timeline).toHaveBeenNthCalledWith(1, 't26');
    expect(screen.getByRole('button', { name: 'Load all requests' })).toBeDisabled();
    for (let batch = 0; batch < 3; batch++) {
      await act(async () => {
        const complete = resolveBatch;
        resolveBatch = [];
        complete.forEach((resolve) => resolve());
      });
    }
    expect(peak).toBe(4);
    expect(timeline).toHaveBeenCalledTimes(12);
    expect(screen.getAllByText('No command execution recorded for this request.')).toHaveLength(12);
    fireEvent.click(screen.getByRole('button', { name: 'Load all requests' }));
    await waitFor(() => expect(timeline).toHaveBeenCalledTimes(16));
    for (let batch = 0; batch < 4; batch++) {
      await act(async () => {
        const complete = resolveBatch;
        resolveBatch = [];
        complete.forEach((resolve) => resolve());
      });
    }
    expect(peak).toBe(4);
    expect(timeline).toHaveBeenCalledTimes(27);
    expect(screen.getAllByText('No command execution recorded for this request.')).toHaveLength(27);
    expect(screen.queryByRole('button', { name: 'Load all requests' })).not.toBeInTheDocument();
  });

  it('reloads when shown and refreshes session commands after returning from a hidden tab', async () => {
    const { list, get, timeline } = mockSessions();
    const { rerender } = render(<HistoryTab visible={false} />);
    expect(list).not.toHaveBeenCalled();
    rerender(<HistoryTab visible />);
    fireEvent.click(await screen.findByText(session.title));
    expect(await screen.findByText('df -hP')).toBeInTheDocument();
    rerender(<HistoryTab visible={false} />);
    timeline.mockResolvedValue([execution({ commandRedacted: 'uptime' })]);
    rerender(<HistoryTab visible />);
    expect(await screen.findByText('uptime')).toBeInTheDocument();
    expect(screen.queryByText('df -hP')).not.toBeInTheDocument();
    expect(get).toHaveBeenCalledTimes(2);
  });

  it('keeps older per-host audit records accessible and returns to sessions', async () => {
    mockSessions();
    mockHistory();
    render(<HistoryTab visible />);
    fireEvent.click(screen.getByRole('button', { name: 'Browse by host' }));
    expect(await screen.findByText('check disk usage on the web node')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Export encrypted' })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'All sessions' }));
    expect(await screen.findByText(session.title)).toBeInTheDocument();
  });

  it('reports invalid transcripts without reading commands', async () => {
    const { get, timeline } = mockSessions();
    get.mockResolvedValue({ ...session, transcript: '{"version":999,"turns":[]}' });
    render(<HistoryTab visible />);
    fireEvent.click(await screen.findByText(session.title));
    expect(await screen.findByRole('alert')).toHaveTextContent('Unsupported session history');
    expect(timeline).not.toHaveBeenCalled();
  });
});
