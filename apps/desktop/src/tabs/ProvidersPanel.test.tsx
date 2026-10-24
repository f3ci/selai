import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import * as ipc from '../ipc';
import { ProvidersPanel } from './ProvidersPanel';
import { chooseSelectValue } from '../ui/selectTestUtils';

afterEach(() => vi.restoreAllMocks());

const provider: ipc.ProviderView = {
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
  toolCalling: { status: 'not_verified', lastCheckedAtMs: null },
};

function mockTemplates() {
  vi.spyOn(ipc, 'providerTemplates').mockResolvedValue([
    {
      id: 'vllm',
      displayName: 'vLLM',
      protocol: 'openai_chat',
      defaultBaseUrl: 'http://localhost:8000/v1',
      note: 'Needs the tool parser.',
    },
    {
      id: 'custom',
      displayName: 'Custom (OpenAI-compatible)',
      protocol: 'openai_chat',
      defaultBaseUrl: '',
      note: 'Use your own endpoint.',
    },
  ]);
}

describe('ProvidersPanel', () => {
  it('uses a themed confirmation and deletes only after explicit approval', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [provider], nextCursor: null });
    mockTemplates();
    const remove = vi.spyOn(ipc, 'providerDelete').mockResolvedValue();
    render(<ProvidersPanel />);
    fireEvent.click(await screen.findByRole('button', { name: 'Delete local-vllm' }));
    expect(screen.getByRole('alertdialog', { name: 'Delete provider?' })).toHaveTextContent('local-vllm');
    expect(remove).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Delete local-vllm' }));
    fireEvent.click(screen.getByRole('button', { name: 'Delete provider' }));
    await waitFor(() => expect(remove).toHaveBeenCalledWith('p1'));
    await waitFor(() => expect(screen.queryByRole('alertdialog')).not.toBeInTheDocument());
  });

  it('shows verification status as not verified until the Rust probe says otherwise', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [provider], nextCursor: null });
    mockTemplates();
    render(<ProvidersPanel />);
    expect(await screen.findByText('local-vllm')).toBeInTheDocument();
    expect(screen.getByText('Not verified')).toBeInTheDocument();
  });

  it('one Test button checks the connection, then tool calling, and shows the Rust results', async () => {
    vi.spyOn(ipc, 'providerList')
      .mockResolvedValueOnce({ items: [provider], nextCursor: null })
      .mockResolvedValue({
        items: [{ ...provider, toolCalling: { status: 'verified', lastCheckedAtMs: 9 } }],
        nextCursor: null,
      });
    mockTemplates();
    const test = vi.spyOn(ipc, 'providerTest').mockImplementation(async (_id, kind) => ({
      kind,
      ok: true,
      verified: kind === 'tool_calling',
      code: null,
      message: kind === 'tool_calling' ? 'tool call and tool result round trip completed' : 'reachable',
      latencyMs: 12,
      replyPreview: null,
    }));
    render(<ProvidersPanel />);
    fireEvent.click(await screen.findByRole('button', { name: 'Test' }));
    await waitFor(() => expect(test).toHaveBeenCalledTimes(2));
    expect(test.mock.calls.map((c) => c[1])).toEqual(['connectivity', 'tool_calling']);
    expect(await screen.findByText(/Tool calling: passed/)).toBeInTheDocument();
    expect(screen.getByText(/Connection: passed/)).toBeInTheDocument();
    expect(await screen.findByText('Verified')).toBeInTheDocument();
  });

  it('skips the tool-calling check when the connection fails', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [provider], nextCursor: null });
    mockTemplates();
    const test = vi.spyOn(ipc, 'providerTest').mockResolvedValue({
      kind: 'connectivity',
      ok: false,
      verified: false,
      code: 'LLM_UNREACHABLE',
      message: 'connection refused',
      latencyMs: 3,
      replyPreview: null,
    });
    render(<ProvidersPanel />);
    fireEvent.click(await screen.findByRole('button', { name: 'Test' }));
    expect(await screen.findByText(/Connection: failed \(LLM_UNREACHABLE\)/)).toBeInTheDocument();
    expect(screen.getByText(/skipped because the connection failed/)).toBeInTheDocument();
    expect(test).toHaveBeenCalledTimes(1);
  });

  it('offers plain HTTP to a remote gateway only as an explicit opt-in', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [], nextCursor: null });
    mockTemplates();
    const save = vi.spyOn(ipc, 'providerSave').mockResolvedValue(provider);
    render(<ProvidersPanel />);
    fireEvent.click((await screen.findAllByRole('button', { name: 'Add provider' }))[0]!);
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'litellm' } });
    fireEvent.change(screen.getByLabelText('Base URL'), { target: { value: 'https://llm.internal/v1' } });
    expect(screen.queryByLabelText('Allow plain HTTP')).not.toBeInTheDocument();

    fireEvent.change(screen.getByLabelText('Base URL'), { target: { value: 'http://10.1.2.3:4000/v1' } });
    fireEvent.click(screen.getByLabelText('Allow plain HTTP'));
    fireEvent.click(screen.getByRole('button', { name: 'Save provider' }));
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0]?.[0]).toMatchObject({ baseUrl: 'http://10.1.2.3:4000/v1', allowInsecureHttp: true });
  });

  it('applies a template without asking for a model and sends the key only once', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [], nextCursor: null });
    mockTemplates();
    const save = vi.spyOn(ipc, 'providerSave').mockResolvedValue(provider);
    render(<ProvidersPanel />);
    fireEvent.click((await screen.findAllByRole('button', { name: 'Add provider' }))[0]!);
    chooseSelectValue('Provider template', 'vllm');
    expect(screen.getByLabelText('Base URL')).toHaveValue('http://localhost:8000/v1');
    expect(screen.queryByLabelText('Model ID')).not.toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(/API key/), { target: { value: 'k-1' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save provider' }));
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0]?.[0]).toMatchObject({ modelId: 'default', apiKey: 'k-1' });
    await waitFor(() => expect(screen.queryByLabelText(/API key/)).not.toBeInTheDocument());
  });

  it('loads the catalog without claiming key verification and saves the chosen model', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [], nextCursor: null });
    mockTemplates();
    const models = vi
      .spyOn(ipc, 'providerModels')
      .mockRejectedValueOnce({ code: 'PROVIDER_UNAUTHORIZED', message: 'the provider rejected the API key' })
      .mockResolvedValueOnce({ models: ['gpt-4o', 'gpt-4o-mini'], latencyMs: 40 });
    const save = vi.spyOn(ipc, 'providerSave').mockResolvedValue(provider);
    render(<ProvidersPanel />);
    fireEvent.click((await screen.findAllByRole('button', { name: 'Add provider' }))[0]!);
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'openai' } });
    fireEvent.change(screen.getByLabelText('Base URL'), { target: { value: 'https://api.openai.com/v1' } });
    fireEvent.change(screen.getByLabelText('API key'), { target: { value: 'sk-wrong' } });

    fireEvent.click(screen.getByRole('button', { name: 'Load models' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('PROVIDER_UNAUTHORIZED');
    expect(screen.getByLabelText('Model ID')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('radio', { name: 'Choose from API' }));
    fireEvent.change(screen.getByLabelText('API key'), { target: { value: 'sk-right' } });
    fireEvent.click(screen.getByRole('button', { name: 'Load models' }));
    expect(await screen.findByText(/2 models listed/)).toBeInTheDocument();
    expect(screen.queryByText(/Key works/)).not.toBeInTheDocument();
    expect(screen.getByText(/Some providers publish their model catalog without checking your key/)).toBeInTheDocument();
    expect(models).toHaveBeenLastCalledWith(
      expect.objectContaining({ baseUrl: 'https://api.openai.com/v1', apiKey: 'sk-right', protocol: 'openai_chat' }),
    );

    chooseSelectValue('Model', 'gpt-4o-mini');
    fireEvent.click(screen.getByRole('button', { name: 'Save provider' }));
    await waitFor(() => expect(save).toHaveBeenCalled());
    expect(save.mock.calls[0]?.[0]).toMatchObject({ modelId: 'gpt-4o-mini', apiKey: 'sk-right' });
  });

  it('offers API discovery and manual model input without requiring a check', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [], nextCursor: null });
    mockTemplates();
    const models = vi.spyOn(ipc, 'providerModels');
    const save = vi.spyOn(ipc, 'providerSave').mockResolvedValue(provider);
    render(<ProvidersPanel />);
    fireEvent.click((await screen.findAllByRole('button', { name: 'Add provider' }))[0]!);
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'GLM' } });
    fireEvent.change(screen.getByLabelText('Base URL'), { target: { value: 'http://127.0.0.1:4000/v1' } });
    expect(screen.getByRole('radio', { name: 'Choose from API' })).toBeChecked();
    fireEvent.click(screen.getByRole('radio', { name: 'Enter manually' }));
    fireEvent.change(screen.getByLabelText('Model ID'), { target: { value: '  custom-glm-model  ' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save provider' }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ modelId: 'custom-glm-model' })));
    expect(models).not.toHaveBeenCalled();
  });

  it.each(['empty', 'failed'])('allows saving a manual model after an %s model check', async (result) => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [], nextCursor: null });
    mockTemplates();
    const models = vi.spyOn(ipc, 'providerModels');
    if (result === 'empty') models.mockResolvedValue({ models: [], latencyMs: 20 });
    else models.mockRejectedValue({ code: 'LLM_UNREACHABLE', message: 'Model listing unavailable' });
    const save = vi.spyOn(ipc, 'providerSave').mockResolvedValue(provider);
    render(<ProvidersPanel />);
    fireEvent.click((await screen.findAllByRole('button', { name: 'Add provider' }))[0]!);
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'GLM' } });
    fireEvent.change(screen.getByLabelText('Base URL'), { target: { value: 'https://llm.example/v1' } });
    fireEvent.click(screen.getByRole('button', { name: 'Load models' }));
    const input = await screen.findByLabelText('Model ID');
    expect(screen.getByRole('radio', { name: 'Enter manually' })).toBeChecked();
    if (result === 'empty') expect(screen.getByRole('status')).toHaveTextContent('No models returned');
    else expect(screen.getByRole('alert')).toHaveTextContent('Model listing unavailable');
    fireEvent.change(input, { target: { value: 'custom-glm-model' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save provider' }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ modelId: 'custom-glm-model' })));
  });

  it('preserves an edited model when a successful check does not list it', async () => {
    vi.spyOn(ipc, 'providerList').mockResolvedValue({ items: [provider], nextCursor: null });
    mockTemplates();
    vi.spyOn(ipc, 'providerModels').mockResolvedValue({ models: ['another-model'], latencyMs: 20 });
    const save = vi.spyOn(ipc, 'providerSave').mockResolvedValue(provider);
    render(<ProvidersPanel />);
    fireEvent.click(await screen.findByRole('button', { name: 'Edit local-vllm' }));
    expect(screen.getByLabelText('Model ID')).toHaveValue('qwen-tools');
    fireEvent.change(screen.getByLabelText('Model ID'), { target: { value: 'custom-glm-model' } });
    fireEvent.click(screen.getByRole('button', { name: 'Load models' }));
    await screen.findByText(/1 model listed/);
    expect(screen.getByLabelText('Model ID')).toHaveValue('custom-glm-model');
    fireEvent.click(screen.getByRole('button', { name: 'Save provider' }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ id: 'p1', modelId: 'custom-glm-model' })));
  });

  it('saves multiple custom profiles with their own names at the same endpoint', async () => {
    const profiles: ipc.ProviderView[] = [];
    vi.spyOn(ipc, 'providerList').mockImplementation(async () => ({ items: [...profiles], nextCursor: null }));
    mockTemplates();
    const save = vi.spyOn(ipc, 'providerSave').mockImplementation(async (input) => {
      const saved = { ...provider, id: `p${profiles.length + 1}`, name: input.name, baseUrl: input.baseUrl, modelId: input.modelId };
      profiles.push(saved);
      return saved;
    });
    render(<ProvidersPanel />);
    for (const name of ['Zen Personal', 'Zen Work']) {
      fireEvent.click((await screen.findAllByRole('button', { name: 'Add provider' }))[0]!);
      chooseSelectValue('Provider template', 'custom');
      expect(screen.getByLabelText('Name')).toHaveValue('');
      fireEvent.change(screen.getByLabelText('Name'), { target: { value: name } });
      fireEvent.change(screen.getByLabelText('Base URL'), { target: { value: 'https://opencode.ai/zen/v1' } });
      fireEvent.click(screen.getByRole('radio', { name: 'Enter manually' }));
      fireEvent.change(screen.getByLabelText('Model ID'), { target: { value: 'my-model' } });
      fireEvent.click(screen.getByRole('button', { name: 'Save provider' }));
      await screen.findByRole('button', { name: `Edit ${name}` });
    }
    expect(save.mock.calls.map(([input]) => input.name)).toEqual(['Zen Personal', 'Zen Work']);
    expect(screen.getAllByText('Model: my-model')).toHaveLength(2);
    fireEvent.click(screen.getByRole('button', { name: 'Edit Zen Work' }));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Zen Team' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save provider' }));
    await waitFor(() => expect(save).toHaveBeenLastCalledWith(expect.objectContaining({ id: 'p2', name: 'Zen Team' })));
  });
});

