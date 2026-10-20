import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { AgentPanel } from './AgentPanel';
import { PROVIDERS_CHANGED } from './ProvidersPanel';
import { flushVaultViews } from '../vault';

afterEach(() => vi.restoreAllMocks());

beforeEach(() => {
  vi.spyOn(ipc, 'chatSessionList').mockResolvedValue([]);
  vi.spyOn(ipc, 'chatSessionSave').mockResolvedValue(undefined);
});

const verified: ipc.ProviderView = {
  id: 'p1',
  name: 'local-vllm',
  protocol: 'openai_chat',
  baseUrl: 'http://127.0.0.1:8000/v1',
  modelId: 'qwen-tools',
  timeoutSeconds: 120,
  maxOutputTokens: 4096,
  keyConfigured: false,
  isLoopback: true,
  allowInsecureHttp: false,
  updatedAtMs: 7,
  toolCalling: { status: 'verified', lastCheckedAtMs: 1 },
};

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

/** Captures the event callback so a test can play the task's events. */
function mockAgent() {
  let emit: ((event: ipc.AgentEvent) => void) | undefined;
  const start = vi.spyOn(ipc, 'agentStart').mockImplementation(async (_h, _p, _r, _history, onEvent) => {
    emit = onEvent;
    return 't1';
  });
  return { start, play: (event: ipc.AgentEvent) => act(() => emit?.(event)) };
}

async function pickProductionWeb() {
  vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
  vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host], nextCursor: null });
  render(<AgentPanel />);
  const textarea = screen.getByLabelText('Request');
  fireEvent.change(textarea, { target: { value: '@prod' } });
  fireEvent.click(await screen.findByRole('button', { name: /production-web/ }));
  await screen.findByLabelText('Remove production-web');
  expect(textarea).toHaveValue('@production-web ');
  return textarea;
}

async function sendRequest(textarea: HTMLElement, text: string) {
  fireEvent.change(textarea, { target: { value: `@production-web ${text}` } });
  await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
  fireEvent.click(screen.getByRole('button', { name: 'Send' }));
}

describe('AgentPanel', () => {
  it('keeps an unsaved session in memory and allows retrying a failed write', async () => {
    vi.mocked(ipc.chatSessionSave).mockRejectedValueOnce(new Error('disk full'));
    render(<AgentPanel />);
    await waitFor(() => expect(screen.getByLabelText('Session title')).toBeEnabled());
    fireEvent.change(screen.getByLabelText('Session title'), { target: { value: 'Recover deployment' } });
    expect(await screen.findByText(/Session history could not be saved.*disk full/, {}, { timeout: 2000 })).toBeInTheDocument();
    expect(screen.getByLabelText('Session title')).toHaveValue('Recover deployment');
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await waitFor(() => expect(ipc.chatSessionSave).toHaveBeenCalledTimes(2));
    expect(vi.mocked(ipc.chatSessionSave).mock.calls[1]![0]).toEqual(vi.mocked(ipc.chatSessionSave).mock.calls[0]![0]);
    await waitFor(() => expect(screen.queryByText(/Session history could not be saved/)).not.toBeInTheDocument());
  });

  it('browses previous prompts with arrow keys and restores the current draft', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
    let emit: ((event: ipc.ChatEvent) => void) | undefined;
    const chat = vi.spyOn(ipc, 'chatSend').mockImplementation(async (_p, _m, onEvent) => {
      emit = onEvent; return 'chat-task';
    });
    render(<AgentPanel />);
    const textarea = screen.getByLabelText('Request') as HTMLTextAreaElement;
    for (const text of ['First request', 'Second request']) {
      fireEvent.change(textarea, { target: { value: text } });
      await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
      fireEvent.click(screen.getByRole('button', { name: 'Send' }));
      await waitFor(() => expect(chat).toHaveBeenCalledWith('p1', expect.any(Array), expect.any(Function), 'qwen-tools'));
      act(() => emit?.({ type: 'completed' }));
    }
    const draft = 'Unfinished draft\nsecond line';
    fireEvent.change(textarea, { target: { value: draft } });
    textarea.setSelectionRange(draft.length, draft.length);
    fireEvent.keyDown(textarea, { key: 'ArrowUp' });
    expect(textarea).toHaveValue(draft);
    textarea.setSelectionRange(0, 0);
    fireEvent.keyDown(textarea, { key: 'ArrowUp' });
    expect(textarea).toHaveValue('Second request');
    fireEvent.keyDown(textarea, { key: 'ArrowUp' });
    expect(textarea).toHaveValue('First request');
    fireEvent.keyDown(textarea, { key: 'ArrowUp' });
    expect(textarea).toHaveValue('First request');
    fireEvent.keyDown(textarea, { key: 'ArrowDown' });
    expect(textarea).toHaveValue('Second request');
    fireEvent.keyDown(textarea, { key: 'ArrowDown' });
    expect(textarea).toHaveValue(draft);
    expect(chat).toHaveBeenCalledTimes(2);
    fireEvent.click(screen.getByRole('button', { name: 'New session' }));
    fireEvent.keyDown(textarea, { key: 'ArrowUp' });
    expect(textarea).toHaveValue('');
  });

  it('restores server mentions with a recalled prompt without executing it', async () => {
    const agent = mockAgent();
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'check disk usage');
    await waitFor(() => expect(agent.start).toHaveBeenCalledOnce());
    agent.play({ taskId: 't1', type: 'completed', text: '' });
    fireEvent.keyDown(textarea, { key: 'ArrowUp' });
    expect(textarea).toHaveValue('@production-web check disk usage');
    expect(screen.getByLabelText('Remove production-web')).toBeInTheDocument();
    expect(agent.start).toHaveBeenCalledOnce();
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(agent.start).toHaveBeenCalledTimes(2));
    expect(agent.start.mock.calls[1]?.slice(0, 3)).toEqual(['h1', 'p1', 'check disk usage']);
  });

  it('requires unavailable recalled server mentions to be removed or reselected', async () => {
    const agent = mockAgent();
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'check disk usage');
    await waitFor(() => expect(agent.start).toHaveBeenCalledOnce());
    agent.play({ taskId: 't1', type: 'completed', text: '' });
    vi.mocked(ipc.hostList).mockResolvedValue({ items: [], nextCursor: null });
    await act(async () => { window.dispatchEvent(new Event(ipc.HOSTS_CHANGED)); });
    fireEvent.keyDown(textarea, { key: 'ArrowUp' });
    expect(await screen.findByText(/A server from this message is unavailable/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Send' })).toBeDisabled();
    fireEvent.change(textarea, { target: { value: 'check disk usage' } });
    expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled();
    expect(agent.start).toHaveBeenCalledOnce();
  });

  it('saves a titled conversation and restores it with context after reopening', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
    let emit: ((event: ipc.ChatEvent) => void) | undefined;
    const chat = vi.spyOn(ipc, 'chatSend').mockImplementation(async (_p, _m, onEvent) => {
      emit = onEvent;
      return 'chat-task';
    });
    const view = render(<AgentPanel />);
    const textarea = screen.getByLabelText('Request');
    fireEvent.change(textarea, { target: { value: 'Explain SSH keepalive' } });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(chat).toHaveBeenCalled());
    act(() => {
      emit?.({ type: 'textDelta', text: 'Keepalive checks whether the connection is alive.' });
      emit?.({ type: 'completed' });
    });
    fireEvent.change(screen.getByLabelText('Session title'), { target: { value: 'SSH troubleshooting' } });
    await act(async () => { await flushVaultViews(); });
    expect(ipc.chatSessionSave).toHaveBeenCalled();
    const saved = vi.mocked(ipc.chatSessionSave).mock.calls.at(-1)![0];
    expect(saved.title).toBe('SSH troubleshooting');
    expect(JSON.parse(saved.transcript).turns[0].items).toContainEqual({ kind: 'text', text: 'Keepalive checks whether the connection is alive.' });
    view.unmount();

    const summary = { id: saved.id, title: saved.title, createdAtMs: 1, updatedAtMs: 2 };
    vi.mocked(ipc.chatSessionList).mockResolvedValue([summary]);
    vi.spyOn(ipc, 'chatSessionGet').mockResolvedValue({ ...summary, transcript: saved.transcript });
    render(<AgentPanel />);
    expect(await screen.findByText('Keepalive checks whether the connection is alive.')).toBeInTheDocument();
    expect(screen.getByLabelText('Session title')).toHaveValue('SSH troubleshooting');
    fireEvent.change(screen.getByLabelText('Request'), { target: { value: 'What interval should I use?' } });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(chat).toHaveBeenCalledTimes(2));
    expect(chat.mock.calls[1]![1]).toEqual([
      { role: 'user', content: 'Explain SSH keepalive' },
      { role: 'assistant', content: 'Keepalive checks whether the connection is alive.' },
      { role: 'user', content: 'What interval should I use?' },
    ]);
  });

  it('keeps live replies in their original session when a new session is opened', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
    const callbacks: Array<(event: ipc.ChatEvent) => void> = [];
    const chat = vi.spyOn(ipc, 'chatSend').mockImplementation(async (_p, _m, onEvent) => {
      callbacks.push(onEvent);
      return `chat-${callbacks.length}`;
    });
    render(<AgentPanel />);
    fireEvent.change(screen.getByLabelText('Request'), { target: { value: 'Explain systemd' } });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(chat).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByRole('button', { name: 'New session' }));
    fireEvent.change(screen.getByLabelText('Request'), { target: { value: 'Explain nginx' } });
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(chat).toHaveBeenCalledTimes(2));
    expect(chat.mock.calls[1]![1]).toEqual([{ role: 'user', content: 'Explain nginx' }]);
    act(() => {
      callbacks[0]!({ type: 'textDelta', text: 'systemd manages services.' });
      callbacks[0]!({ type: 'completed' });
      callbacks[1]!({ type: 'textDelta', text: 'nginx serves HTTP requests.' });
      callbacks[1]!({ type: 'completed' });
    });
    expect(await screen.findByText('nginx serves HTTP requests.')).toBeInTheDocument();
    expect(screen.queryByText('systemd manages services.')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Session history' }));
    fireEvent.click(screen.getByRole('button', { name: /Explain systemd/ }));
    expect(await screen.findByText('systemd manages services.')).toBeInTheDocument();
    expect(screen.queryByText('nginx serves HTTP requests.')).not.toBeInTheDocument();
  });

  it('chooses the host with @ and starts the task on that host and provider', async () => {
    const agent = mockAgent();
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'check disk usage');

    await waitFor(() =>
      expect(agent.start).toHaveBeenCalledWith('h1', 'p1', 'check disk usage', [], expect.any(Function), 'qwen-tools'),
    );
    // The sent message stays in the thread with the mention highlighted.
    expect(await screen.findByText('@production-web', { selector: '.mention' })).toBeInTheDocument();
  });

  it('mentions a label with spaces and binds the task to that host id', async () => {
    const agent = mockAgent();
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
    const list = vi
      .spyOn(ipc, 'hostList')
      .mockResolvedValue({ items: [{ ...host, id: 'h7', label: 'Production Web' }], nextCursor: null });
    render(<AgentPanel />);
    const textarea = screen.getByLabelText('Request');
    fireEvent.change(textarea, { target: { value: 'restart @Production W' } });
    fireEvent.click(await screen.findByRole('button', { name: /Production Web/ }));
    expect(list).toHaveBeenCalledTimes(1);
    expect(list).toHaveBeenCalledWith(undefined, false, undefined);
    expect(textarea).toHaveValue('restart @Production Web ');
    expect(screen.queryByRole('list', { name: 'Matching hosts' })).not.toBeInTheDocument();

    fireEvent.change(textarea, { target: { value: 'restart @Production Web nginx' } });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
    fireEvent.keyDown(textarea, { key: 'Enter' });
    await waitFor(() =>
      expect(agent.start).toHaveBeenCalledWith('h7', 'p1', 'restart nginx', [], expect.any(Function), 'qwen-tools'),
    );
  });

  it('runs one parallel task per mentioned server and shows the shared request once', async () => {
    const agent = mockAgent();
    const second = { ...host, id: 'h2', label: 'staging-db', hostname: '10.0.0.6' };
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host, second], nextCursor: null });
    render(<AgentPanel />);
    const textarea = screen.getByLabelText('Request');

    fireEvent.change(textarea, { target: { value: '@prod' } });
    fireEvent.click(await screen.findByRole('button', { name: /production-web/ }));
    fireEvent.change(textarea, { target: { value: '@production-web @stag' } });
    fireEvent.click(await screen.findByRole('button', { name: /staging-db/ }));
    fireEvent.change(textarea, { target: { value: '@production-web @staging-db check disk usage' } });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));

    await waitFor(() => expect(agent.start).toHaveBeenCalledTimes(2));
    expect(agent.start).toHaveBeenCalledWith('h1', 'p1', 'check disk usage', [], expect.any(Function), 'qwen-tools');
    expect(agent.start).toHaveBeenCalledWith('h2', 'p1', 'check disk usage', [], expect.any(Function), 'qwen-tools');
    expect(screen.getAllByText('check disk usage', { selector: '.bubble' })).toHaveLength(1);
    expect(screen.getByText('production-web', { selector: '.host-tag' })).toBeInTheDocument();
    expect(screen.getByText('staging-db', { selector: '.host-tag' })).toBeInTheDocument();
  });

  it('deleting the mention text removes the target, and the message becomes plain chat', async () => {
    const chat = vi.spyOn(ipc, 'chatSend').mockResolvedValue('c1');
    const start = vi.spyOn(ipc, 'agentStart');
    const textarea = await pickProductionWeb();
    fireEvent.change(textarea, { target: { value: 'what does df -h show?' } });
    expect(screen.queryByLabelText('Remove production-web')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() =>
      expect(chat).toHaveBeenCalledWith('p1', [{ role: 'user', content: 'what does df -h show?' }], expect.any(Function), 'qwen-tools'),
    );
    expect(start).not.toHaveBeenCalled();
  });

  it('picks a host from the @ list with the keyboard', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
    vi.spyOn(ipc, 'hostList').mockResolvedValue({
      items: [host, { ...host, id: 'h2', label: 'production-db', hostname: '10.0.0.6' }],
      nextCursor: null,
    });
    render(<AgentPanel />);
    const textarea = screen.getByLabelText('Request');
    fireEvent.change(textarea, { target: { value: 'check @prod' } });
    expect(await screen.findByRole('button', { name: /production-db/ })).toBeInTheDocument();

    fireEvent.keyDown(textarea, { key: 'ArrowDown' });
    fireEvent.keyDown(textarea, { key: 'Enter' });

    expect(await screen.findByLabelText('Remove production-db')).toBeInTheDocument();
    expect(textarea).toHaveValue('check @production-db ');
  });

  it('an unverified provider can chat but cannot work on a host', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({
      items: [{ ...verified, toolCalling: { status: 'not_verified', lastCheckedAtMs: null } }],
      nextCursor: null,
    });
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host], nextCursor: null });
    const openProviders = vi.fn();
    render(<AgentPanel onOpenProviders={openProviders} />);
    const textarea = screen.getByLabelText('Request');
    fireEvent.change(textarea, { target: { value: 'how do I rotate nginx logs?' } });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());

    fireEvent.change(textarea, { target: { value: '@prod' } });
    fireEvent.click(await screen.findByRole('button', { name: /production-web/ }));
    fireEvent.change(textarea, { target: { value: '@production-web check disk usage' } });
    expect(await screen.findByText(/Working on a server needs a provider whose tool calling test passed/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Send' })).toBeDisabled();
    fireEvent.click(screen.getByRole('button', { name: 'Open Providers' }));
    expect(openProviders).toHaveBeenCalled();
  });

  it('plain chat keeps the conversation, renders Markdown and runs nothing', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
    let emit: ((e: ipc.ChatEvent) => void) | undefined;
    const chat = vi.spyOn(ipc, 'chatSend').mockImplementation(async (_p, _m, onEvent) => {
      emit = onEvent;
      return 'c1';
    });
    const start = vi.spyOn(ipc, 'agentStart');
    render(<AgentPanel />);
    const textarea = screen.getByLabelText('Request');
    fireEvent.change(textarea, { target: { value: 'how do I list open ports?' } });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
    fireEvent.keyDown(textarea, { key: 'Enter' });
    await waitFor(() => expect(chat).toHaveBeenCalledTimes(1));

    act(() => emit?.({ type: 'textDelta', text: 'Use this:\n\n```bash\nss -tlnp\n```\n\nIt needs **root** for process names.' }));
    act(() => emit?.({ type: 'usage', inputTokens: 1200, outputTokens: 34 }));
    act(() => emit?.({ type: 'completed' }));

    expect(await screen.findByText('ss -tlnp')).toBeInTheDocument();
    expect(screen.getByText('root').tagName).toBe('STRONG');
    expect(screen.getByText('bash')).toBeInTheDocument();
    expect(screen.queryByText(/in · 34 out/)).not.toBeInTheDocument();
    expect(screen.queryByText(/```/)).not.toBeInTheDocument();

    fireEvent.change(textarea, { target: { value: 'and on macOS?' } });
    fireEvent.keyDown(textarea, { key: 'Enter' });
    await waitFor(() => expect(chat).toHaveBeenCalledTimes(2));
    expect(chat.mock.calls[1]?.[1]).toEqual([
      { role: 'user', content: 'how do I list open ports?' },
      { role: 'assistant', content: 'Use this:\n\n```bash\nss -tlnp\n```\n\nIt needs **root** for process names.' },
      { role: 'user', content: 'and on macOS?' },
    ]);
    expect(start).not.toHaveBeenCalled();
  });

  it('loads the provider model list in the composer and sends the chosen model', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
    vi.spyOn(ipc, 'providerModels').mockResolvedValue({ models: ['qwen-tools', 'qwen-chat'], latencyMs: 12 });
    const chat = vi.spyOn(ipc, 'chatSend').mockResolvedValue('c1');
    render(<AgentPanel />);
    const model = await screen.findByRole('combobox', { name: 'Model' });
    await waitFor(() => expect(model).toHaveAttribute('aria-valuetext', 'qwen-tools'));
    fireEvent.click(model);
    fireEvent.click(screen.getByRole('option', { name: 'qwen-chat' }));
    fireEvent.change(screen.getByLabelText('Request'), { target: { value: 'Explain SSH multiplexing' } });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
    fireEvent.click(screen.getByRole('button', { name: 'Send' }));
    await waitFor(() => expect(chat).toHaveBeenCalledWith(
      'p1', [{ role: 'user', content: 'Explain SSH multiplexing' }], expect.any(Function), 'qwen-chat',
    ));
  });

  it('a host task shows each command with its collapsible output', async () => {
    const agent = mockAgent();
    vi.spyOn(ipc, 'taskTimeline').mockResolvedValue([{
      id: 'e1', taskId: 't1', hostId: 'h1', modelId: 'qwen-tools', tool: 'ssh.run_diagnostic',
      commandRedacted: 'df -hP', commandSha256: 'digest', state: 'completed', policyDecision: null,
      exitCode: 0, errorCode: null, stdoutPreview: 'Filesystem Size Used', stderrPreview: null,
      outputTruncated: false, createdAtMs: 1, startedAtMs: 1, finishedAtMs: 2,
    }]);
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'check disk usage');
    await waitFor(() => expect(agent.start).toHaveBeenCalled());
    agent.play({ taskId: 't1', type: 'toolProposed', executionId: 'e1', tool: 'ssh.run_diagnostic', command: 'df -hP', hostLabel: 'production-web' });
    agent.play({ taskId: 't1', type: 'toolFinished', executionId: 'e1', state: 'completed', exitCode: 0, errorCode: null });
    const line = await screen.findByLabelText('Command');
    expect(line).toHaveTextContent('Randf -hPexit 0');
    const output = await screen.findByText('Filesystem Size Used');
    expect(output.closest('details')).not.toHaveAttribute('open');
    fireEvent.click(screen.getByText('Output'));
    expect(output.closest('details')).toHaveAttribute('open');
    // The mention applied to that message only.
    expect(screen.queryByLabelText('Remove production-web')).not.toBeInTheDocument();
  });

  it('batches streamed text but flushes it before a command event', async () => {
    const agent = mockAgent();
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'check disk usage');
    await waitFor(() => expect(agent.start).toHaveBeenCalled());

    agent.play({ taskId: 't1', type: 'textDelta', text: 'Disk ' });
    agent.play({ taskId: 't1', type: 'textDelta', text: 'usage' });
    expect(screen.queryByText('Disk usage')).not.toBeInTheDocument();

    agent.play({ taskId: 't1', type: 'toolProposed', executionId: 'e1', tool: 'ssh.run_diagnostic', command: 'df -hP', hostLabel: 'production-web' });
    expect(await screen.findByText('Disk usage')).toBeInTheDocument();
    expect(screen.getByLabelText('Command')).toHaveTextContent('df -hP');

    agent.play({ taskId: 't1', type: 'textDelta', text: 'Done.' });
    expect(await screen.findByText('Done.')).toBeInTheDocument();
  });

  it('keeps progress visible after text starts and animates while a command runs', async () => {
    const agent = mockAgent();
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'install apache');
    await waitFor(() => expect(agent.start).toHaveBeenCalled());

    agent.play({ taskId: 't1', type: 'textDelta', text: 'Apache2 is missing. Installing it now.' });
    expect(await screen.findByRole('status', { name: 'Working' })).toBeInTheDocument();

    agent.play({ taskId: 't1', type: 'toolProposed', executionId: 'e1', tool: 'ssh.run_command', command: 'apt-get install apache2', hostLabel: 'production-web' });
    expect(await screen.findByRole('status', { name: 'Running command' })).toBeInTheDocument();
    expect(screen.queryByRole('status', { name: 'Working' })).not.toBeInTheDocument();

    agent.play({ taskId: 't1', type: 'toolFinished', executionId: 'e1', state: 'completed', exitCode: 0, errorCode: null });
    expect(await screen.findByRole('status', { name: 'Working' })).toBeInTheDocument();
    agent.play({ taskId: 't1', type: 'state', state: 'completed', errorCode: null });
    await waitFor(() => expect(screen.queryByRole('status', { name: 'Working' })).not.toBeInTheDocument());
  });

  it('shows the exact command and sends the operator decision for an approval', async () => {
    const agent = mockAgent();
    vi.spyOn(ipc, 'taskTimeline').mockResolvedValue([{
      id: 'e1', taskId: 't1', hostId: 'h1', modelId: 'qwen-tools', tool: 'ssh.run_command',
      commandRedacted: 'systemctl restart nginx', commandSha256: 'digest', state: 'completed', policyDecision: null,
      exitCode: 0, errorCode: null, stdoutPreview: 'service restarted', stderrPreview: null,
      outputTruncated: false, createdAtMs: 1, startedAtMs: 1, finishedAtMs: 2,
    }]);
    const decide = vi.spyOn(ipc, 'approvalDecide').mockResolvedValue(undefined);
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'free text');
    await waitFor(() => expect(agent.start).toHaveBeenCalled());

    agent.play({ taskId: 't1', type: 'toolProposed', executionId: 'e1', tool: 'ssh.run_command', command: 'systemctl restart nginx', hostLabel: 'production-web' });
    agent.play({ taskId: 't1', type: 'approvalRequired', approvalId: 'a1', executionId: 'e1', command: 'systemctl restart nginx', expiresAtMs: Date.now() + 300_000 });

    const card = await screen.findByRole('group', { name: 'Approval required' });
    expect(card).toHaveTextContent('systemctl restart nginx');
    expect(card).toHaveTextContent('Run this command on production-web?');

    fireEvent.click(screen.getByRole('button', { name: 'Approve' }));
    await waitFor(() => expect(decide).toHaveBeenCalledWith('a1', true, undefined, false));
    expect(await screen.findByText('Approved. Waiting for the result.')).toBeInTheDocument();
    agent.play({ taskId: 't1', type: 'toolFinished', executionId: 'e1', state: 'completed', exitCode: 0, errorCode: null });
    const output = await screen.findByText('service restarted');
    expect(card).toContainElement(output);
    expect(output.closest('details')).not.toHaveAttribute('open');
  });

  it('shows why the database refused a decision', async () => {
    const agent = mockAgent();
    vi.spyOn(ipc, 'approvalDecide').mockRejectedValue({ code: 'APPROVAL_EXPIRED', message: 'the approval expired' });
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'free text');
    await waitFor(() => expect(agent.start).toHaveBeenCalled());
    agent.play({ taskId: 't1', type: 'approvalRequired', approvalId: 'a1', executionId: 'e1', command: 'id', expiresAtMs: Date.now() + 300_000 });

    fireEvent.click(await screen.findByRole('button', { name: 'Approve' }));
    expect(await screen.findByText('APPROVAL_EXPIRED: the approval expired')).toBeInTheDocument();
  });

  it('cancels a running task with Stop', async () => {
    const agent = mockAgent();
    const cancel = vi.spyOn(ipc, 'agentCancel').mockResolvedValue(true);
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'check disk usage');
    await waitFor(() => expect(agent.start).toHaveBeenCalled());

    fireEvent.click(await screen.findByRole('button', { name: 'Stop' }));
    await waitFor(() => expect(cancel).toHaveBeenCalledWith('t1'));
    agent.play({ taskId: 't1', type: 'state', state: 'cancelled', errorCode: null });
    expect(await screen.findByRole('button', { name: 'Send' })).toBeInTheDocument();
  });

  it('asks to trust the server key when a task cannot connect yet, then starts it', async () => {
    const start = vi
      .spyOn(ipc, 'agentStart')
      .mockRejectedValueOnce({
        code: 'SSH_HOSTKEY_UNKNOWN',
        message: 'host key is not trusted yet',
        details: { algorithm: 'ssh-ed25519', fingerprintSha256: 'SHA256:abc' },
      })
      .mockResolvedValueOnce('t2');
    const trust = vi.spyOn(ipc, 'hostTrust').mockResolvedValue({ outcome: 'pinned', fingerprintSha256: 'SHA256:abc' });
    const textarea = await pickProductionWeb();
    await sendRequest(textarea, 'uptime');

    const dialog = await screen.findByRole('dialog', { name: 'Verify the server' });
    expect(dialog).toHaveTextContent('SHA256:abc');
    fireEvent.click(screen.getByRole('button', { name: 'Trust & connect' }));
    await waitFor(() => expect(trust).toHaveBeenCalledWith('h1', 'SHA256:abc', 'first'));
    await waitFor(() => expect(start).toHaveBeenCalledTimes(2));
    expect(start.mock.calls[1]?.slice(0, 3)).toEqual(['h1', 'p1', 'uptime']);
  });

  it('shows a provider saved elsewhere without restarting the tab', async () => {
    const list = vi
      .spyOn(ipc, 'providerList')
      .mockResolvedValueOnce({ items: [], nextCursor: null })
      .mockResolvedValue({ items: [verified], nextCursor: null });
    render(<AgentPanel />);
    expect(await screen.findByRole('combobox', { name: 'Provider' })).toHaveTextContent('No providers');

    act(() => {
      window.dispatchEvent(new Event(PROVIDERS_CHANGED));
    });

    expect(await screen.findByRole('combobox', { name: 'Provider' })).toHaveTextContent('local-vllm');
    expect(list).toHaveBeenCalledTimes(2);
    // The shown choice must be the selected one, so Send can be enabled.
    expect(screen.getByRole('combobox', { name: 'Provider' })).toHaveAttribute('aria-valuetext', 'local-vllm');
    expect(screen.queryByText(/No verified provider/)).not.toBeInTheDocument();
  });

  it('verifies the audit chain on demand', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [], nextCursor: null });
    vi.spyOn(ipc, 'auditVerify').mockResolvedValue({ ok: true, eventsChecked: 4, brokenAtSeq: null });
    render(<AgentPanel />);
    fireEvent.click(screen.getByRole('button', { name: 'Verify audit log' }));
    expect(await screen.findByText('Audit log intact: 4 event(s) checked.')).toBeInTheDocument();
  });

  it('offers This computer in the @ list and runs the task there', async () => {
    const agent = mockAgent();
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
    vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [], nextCursor: null });
    vi.spyOn(ipc, 'localHost').mockResolvedValue({ ...host, id: 'local', label: 'This computer', kind: 'local' });
    render(<AgentPanel />);
    const textarea = screen.getByLabelText('Request');
    fireEvent.change(textarea, { target: { value: '@this' } });
    const option = await screen.findByRole('button', { name: /This computer/ });
    expect(option).toHaveTextContent('This machine · local shell');
    fireEvent.click(option);
    fireEvent.change(textarea, { target: { value: '@This computer how much disk is free?' } });
    await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
    fireEvent.keyDown(textarea, { key: 'Enter' });
    await waitFor(() =>
      expect(agent.start).toHaveBeenCalledWith('local', 'p1', 'how much disk is free?', [], expect.any(Function), 'qwen-tools'),
    );
  });

  describe('chat server', () => {
    const staging = { ...host, id: 'h2', label: 'staging-db', hostname: '10.0.0.6' };

    function setup() {
      const agent = mockAgent();
      let chatEvent: ((e: ipc.ChatEvent) => void) | undefined;
      const chat = vi.spyOn(ipc, 'chatSend').mockImplementation(async (_p, _m, onEvent) => {
        chatEvent = onEvent;
        return 'c1';
      });
      vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [verified], nextCursor: null });
      vi.spyOn(ipc, 'hostList').mockResolvedValue({ items: [host, staging], nextCursor: null });
      const save = vi.spyOn(ipc, 'preferencesSet').mockResolvedValue({} as ipc.AiPreferences);
      return { agent, chat, save, finishChat: () => act(() => chatEvent?.({ type: 'completed' })) };
    }

    async function send(text: string) {
      const textarea = screen.getByLabelText('Request');
      fireEvent.change(textarea, { target: { value: text } });
      await waitFor(() => expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled());
      fireEvent.keyDown(textarea, { key: 'Enter' });
    }

    it('follows the terminal in focus, so messages need no mention; a mention goes elsewhere for one message', async () => {
      const { agent, chat } = setup();
      const { rerender } = render(<AgentPanel activeHost={host} />);
      expect(await screen.findByRole('button', { name: 'Chat server: production-web. Change' })).toBeInTheDocument();

      // No mention needed: the message goes to the chat's server, and the model
      // decides whether it needs to run anything there.
      await send('what does df -h show?');
      await waitFor(() =>
        expect(agent.start).toHaveBeenLastCalledWith('h1', 'p1', 'what does df -h show?', [], expect.any(Function), 'qwen-tools'),
      );
      agent.play({ taskId: 't1', type: 'state', state: 'completed', errorCode: null });

      // A mention sends one message to another server; the chat's server stays.
      fireEvent.change(screen.getByLabelText('Request'), { target: { value: '@stag' } });
      fireEvent.click(await screen.findByRole('button', { name: /staging-db/ }));
      await send('@staging-db check the logs');
      await waitFor(() =>
        expect(agent.start).toHaveBeenLastCalledWith('h2', 'p1', 'check the logs', expect.any(Array), expect.any(Function), 'qwen-tools'),
      );
      agent.play({ taskId: 't1', type: 'state', state: 'completed', errorCode: null });
      await send('and the memory?');
      await waitFor(() =>
        expect(agent.start).toHaveBeenLastCalledWith('h1', 'p1', 'and the memory?', expect.any(Array), expect.any(Function), 'qwen-tools'),
      );
      agent.play({ taskId: 't1', type: 'state', state: 'completed', errorCode: null });

      // Switching terminals switches the chat's server.
      rerender(<AgentPanel activeHost={staging} />);
      expect(screen.getByRole('button', { name: 'Chat server: staging-db. Change' })).toBeInTheDocument();

      // Removing it makes the chat plain: nothing can run anywhere.
      fireEvent.click(screen.getByRole('button', { name: 'Chat without staging-db' }));
      await send('explain ss -tlnp');
      await waitFor(() => expect(chat).toHaveBeenCalledTimes(1));
      expect(agent.start).toHaveBeenCalledTimes(3);
    });

    it('a pinned server stays when the terminal changes, and is remembered', async () => {
      const { save } = setup();
      const { rerender } = render(<AgentPanel activeHost={host} />);
      const pin = await screen.findByRole('button', { name: 'Pin production-web' });
      expect(pin).toHaveAttribute('aria-pressed', 'false');
      fireEvent.click(pin);
      expect(save).toHaveBeenLastCalledWith({ pinnedHostId: 'h1' });

      rerender(<AgentPanel activeHost={staging} />);
      expect(screen.getByRole('button', { name: 'Chat server: production-web. Change' })).toBeInTheDocument();

      // Unpinning goes back to following the terminal in focus.
      fireEvent.click(screen.getByRole('button', { name: 'Unpin production-web' }));
      expect(save).toHaveBeenLastCalledWith({ pinnedHostId: '' });
      expect(screen.getByRole('button', { name: 'Chat server: staging-db. Change' })).toBeInTheDocument();
    });

    it('restores a remembered pin, and "No server" in the menu makes the chat plain', async () => {
      const { agent, chat, save } = setup();
      vi.spyOn(ipc, 'preferencesGet').mockResolvedValue({ lastProviderId: null, outputRetentionDays: 30, pinnedHostId: 'h2', taskMaxSteps: 30, taskMaxMinutes: 30 });
      render(<AgentPanel activeHost={host} />);
      expect(await screen.findByRole('button', { name: 'Unpin staging-db' })).toHaveAttribute('aria-pressed', 'true');

      fireEvent.click(screen.getByRole('button', { name: 'Chat server: staging-db. Change' }));
      fireEvent.click(screen.getByRole('menuitemradio', { name: /No server/ }));
      expect(save).toHaveBeenLastCalledWith({ pinnedHostId: '' });
      expect(screen.queryByRole('menu')).not.toBeInTheDocument();

      await send('how do I list open ports?');
      await waitFor(() => expect(chat).toHaveBeenCalledTimes(1));
      expect(agent.start).not.toHaveBeenCalled();
    });

    it('picks a server from the menu when none is set, and pins it', async () => {
      const { agent, save } = setup();
      render(<AgentPanel />);
      fireEvent.click(await screen.findByRole('button', { name: /Server/ }));
      fireEvent.click(await screen.findByRole('menuitemradio', { name: /staging-db/ }));
      expect(save).toHaveBeenLastCalledWith({ pinnedHostId: 'h2' });
      await send('uptime');
      await waitFor(() => expect(agent.start).toHaveBeenLastCalledWith('h2', 'p1', 'uptime', [], expect.any(Function), 'qwen-tools'));
    });

    it('a host chosen in another view becomes the pinned chat server', async () => {
      const { save } = setup();
      render(<AgentPanel activeHost={host} contextRequest={{ host: staging, n: 1 }} />);
      expect(await screen.findByRole('button', { name: 'Unpin staging-db' })).toBeInTheDocument();
      await waitFor(() => expect(save).toHaveBeenCalledWith({ pinnedHostId: 'h2' }));
    });
  });

  describe('root commands', () => {
    async function rootApproval(sudo: ipc.SudoNeed) {
      const agent = mockAgent();
      const textarea = await pickProductionWeb();
      await sendRequest(textarea, 'restart nginx');
      await waitFor(() => expect(agent.start).toHaveBeenCalled());
      agent.play({ taskId: 't1', type: 'toolProposed', executionId: 'e1', tool: 'ssh.run_command', command: 'sudo systemctl restart nginx', hostLabel: 'production-web' });
      agent.play({ taskId: 't1', type: 'approvalRequired', approvalId: 'a1', executionId: 'e1', command: 'sudo systemctl restart nginx', expiresAtMs: Date.now() + 300_000, sudo });
      return screen.findByRole('group', { name: 'Approval required' });
    }

    it('reuses the SSH login password when the host has one', async () => {
      vi.spyOn(ipc, 'sudoOptions').mockResolvedValue({ hostLabel: 'production-web', savedPassword: false, loginPassword: true });
      const decide = vi.spyOn(ipc, 'approvalDecide').mockResolvedValue(undefined);
      const card = await rootApproval('password_required');
      expect(card).toHaveTextContent('Run this command as root on production-web?');
      expect(await screen.findByLabelText('Use my SSH login password')).toBeChecked();
      fireEvent.click(screen.getByRole('button', { name: 'Approve as root' }));
      await waitFor(() => expect(decide).toHaveBeenCalledWith('a1', true, { source: 'login' }, false));
    });

    it('needs a typed password before approving, and can remember it', async () => {
      vi.spyOn(ipc, 'sudoOptions').mockResolvedValue({ hostLabel: 'key-box', savedPassword: false, loginPassword: false });
      const decide = vi.spyOn(ipc, 'approvalDecide').mockResolvedValue(undefined);
      await rootApproval('password_required');
      const approve = screen.getByRole('button', { name: 'Approve as root' });
      expect(approve).toBeDisabled();
      fireEvent.change(await screen.findByLabelText('Sudo password'), { target: { value: 's3cret' } });
      fireEvent.click(screen.getByLabelText(/Remember for this host/));
      expect(approve).toBeEnabled();
      fireEvent.click(approve);
      await waitFor(() =>
        expect(decide).toHaveBeenCalledWith('a1', true, { source: 'typed', password: 's3cret', remember: true }, false),
      );
      expect(screen.queryByDisplayValue('s3cret')).not.toBeInTheDocument();
    });

    it('a host without a sudo password approves without asking for one', async () => {
      const options = vi.spyOn(ipc, 'sudoOptions');
      const decide = vi.spyOn(ipc, 'approvalDecide').mockResolvedValue(undefined);
      await rootApproval('no_password');
      expect(screen.getByText(/does not ask for a password/)).toBeInTheDocument();
      fireEvent.click(screen.getByRole('button', { name: 'Approve as root' }));
      await waitFor(() => expect(decide).toHaveBeenCalledWith('a1', true, undefined, false));
      expect(options).not.toHaveBeenCalled();
    });
  });
});

