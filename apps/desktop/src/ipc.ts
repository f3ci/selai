// Typed wrappers around Tauri commands. The webview is presentation only:
// every authorization decision happens in the Rust core.
import { Channel, invoke } from '@tauri-apps/api/core';

export interface AppInfo {
  name: string;
  version: string;
}

export interface StorageStatus {
  schemaVersion: number;
  journalMode: string;
  interruptedTasksAtStartup: number;
  interruptedExecutionsAtStartup: number;
}

/** Error shape returned by every failing Rust command. */
export interface CommandError {
  code: string;
  message: string;
  details?: KeyErrorDetails;
}

export interface KeyErrorDetails {
  algorithm: string;
  fingerprintSha256: string;
  pinnedFingerprintSha256?: string | null;
}

export type AuthMethod = 'password' | 'private_key';

export interface HostView {
  id: string;
  label: string;
  groupId: string | null;
  proxyId?: string | null;
  hostname: string;
  port: number;
  username: string;
  authMethod: AuthMethod;
  privateKeyPath: string | null;
  connectTimeoutSeconds: number;
  keepAliveSeconds: number;
  terminalType: string;
  notes: string | null;
  credentialConfigured: boolean;
  updatedAtMs: number;
  lastConnectedAtMs: number | null;
  archived: boolean;
  /** `local` is the built-in "This computer" entry: no SSH, cannot be edited. */
  kind: 'ssh' | 'local';
}

export interface Page<T> {
  items: T[];
  nextCursor: string | null;
}

export interface HostInput {
  id?: string;
  expectedUpdatedAtMs?: number;
  label: string;
  groupId?: string | null;
  proxyId?: string | null;
  hostname: string;
  port: number;
  username: string;
  authMethod: AuthMethod;
  privateKeyPath?: string | null;
  connectTimeoutSeconds: number;
  keepAliveSeconds: number;
  terminalType: string;
  notes?: string | null;
  /** Empty or absent keeps the stored value; viewing it uses an explicit command. */
  password?: string;
  /** Write-only. Empty or absent keeps the stored value. */
  keyPassphrase?: string;
}

export interface SshKeyView { id: string; name: string; algorithm: string; publicKey: string; fingerprintSha256: string; createdAtMs: number }
export function sshKeyList(): Promise<SshKeyView[]> { return invoke<SshKeyView[]>('ssh_key_list'); }
export function sshKeyGenerate(name: string, algorithm: string): Promise<SshKeyView> { return invoke<SshKeyView>('ssh_key_generate', { name, algorithm }); }
export function sshKeyImport(name: string, privateKeyBase64: string, passphrase?: string): Promise<SshKeyView> { return invoke<SshKeyView>('ssh_key_import', { name, privateKeyBase64, passphrase: passphrase ?? null }); }
export function sshKeyImportFromFile(name: string, passphrase?: string): Promise<SshKeyView | null> { return invoke<SshKeyView | null>('ssh_key_import_from_file', { name, passphrase: passphrase ?? null }); }
export interface SshKeyScanResult { imported: SshKeyView[]; alreadySaved: number; skipped: number }
export function sshKeyScanLocal(): Promise<SshKeyScanResult> { return invoke<SshKeyScanResult>('ssh_key_scan_local'); }
export function sshKeyDelete(id: string): Promise<void> { return invoke<void>('ssh_key_delete', { id }); }

export interface ObservedKey {
  algorithm: string;
  fingerprintSha256: string;
  publicKeyBase64: string;
}

export interface ProbeView {
  observed: ObservedKey;
  status: 'trusted' | 'unknown' | 'changed';
  pinnedFingerprintSha256: string | null;
}

export type TrustMode = 'first' | 'replace';

export interface TrustView {
  outcome: 'pinned' | 'rotated' | 'unchanged';
  fingerprintSha256: string;
}

export interface GroupView {
  id: string;
  name: string;
  sortOrder: number;
}

export type TerminalEvent =
  | { type: 'data'; data: string }
  | { type: 'exit'; status: number }
  | { type: 'closed'; exitStatus: number | null }
  | { type: 'error'; code: string; message: string };

export function appInfo(): Promise<AppInfo> {
  return invoke<AppInfo>('app_info');
}

export function storageStatus(): Promise<StorageStatus> {
  return invoke<StorageStatus>('storage_status');
}

export function hostList(query?: string, includeArchived = false, cursor?: string): Promise<Page<HostView>> {
  return invoke<Page<HostView>>('host_list', { query: query ?? null, includeArchived, cursor: cursor ?? null });
}

/** The built-in "This computer" entry, for a local terminal and local agent tasks. */
export async function localHost(): Promise<HostView> {
  // Async so a failure always arrives as a rejected promise, never a throw.
  return invoke<HostView>('local_host');
}

/** Sent on the window after a host is saved or archived, so other views reload. */
export const HOSTS_CHANGED = 'selai:hosts-changed';

export function hostSave(input: HostInput): Promise<HostView> {
  return invoke<HostView>('host_save', { input });
}

/** Moves only the group membership; the backend preserves credentials and settings. */
export function hostMoveGroup(id: string, groupId: string | null, expectedUpdatedAtMs: number): Promise<HostView> {
  return invoke<HostView>('host_move_group', { id, groupId, expectedUpdatedAtMs });
}

/** Only called by the password eye button while the vault is unlocked. */
export function hostRevealPassword(id: string): Promise<string | null> {
  return invoke<string | null>('host_reveal_password', { id });
}

export function groupList(): Promise<GroupView[]> { return invoke<GroupView[]>('group_list'); }
export function groupCreate(name: string): Promise<GroupView> { return invoke<GroupView>('group_create', { name }); }
export function groupRename(id: string, name: string): Promise<GroupView> { return invoke<GroupView>('group_rename', { id, name }); }
export function groupDelete(id: string): Promise<void> { return invoke<void>('group_delete', { id }); }

export interface KdbxProfile {
  title: string;
  url: string;
  username: string;
  password: string;
  notes: string;
}

/** Decrypts a KDBX file locally and returns only entries with SSH/SFTP URLs. */
export function kdbxPreview(fileBase64: string, passphrase: string): Promise<KdbxProfile[]> {
  return invoke<KdbxProfile[]>('kdbx_preview', { fileBase64, passphrase });
}

/** Reads only the current user's default ~/.ssh/config, bounded by the backend. */
export function sshConfigRead(): Promise<string> {
  return invoke<string>('ssh_config_read');
}

export function hostArchive(id: string): Promise<void> {
  return invoke<void>('host_archive', { id });
}

export function hostProbe(id: string): Promise<ProbeView> {
  return invoke<ProbeView>('host_probe', { id });
}

export function hostTrust(id: string, fingerprintSha256: string, mode: TrustMode): Promise<TrustView> {
  return invoke<TrustView>('host_trust', { id, fingerprintSha256, mode });
}

export function terminalConnect(
  hostId: string,
  cols: number,
  rows: number,
  onEvent: (event: TerminalEvent) => void,
): Promise<string> {
  const channel = new Channel<TerminalEvent>();
  channel.onmessage = onEvent;
  return invoke<string>('terminal_connect', { hostId, cols, rows, onEvent: channel });
}

export function terminalInput(sessionId: string, data: string): Promise<void> {
  return invoke<void>('terminal_input', { sessionId, data });
}

export function terminalAck(sessionId: string, bytes: number): Promise<void> {
  return invoke<void>('terminal_ack', { sessionId, bytes });
}

export function terminalResize(sessionId: string, cols: number, rows: number): Promise<void> {
  return invoke<void>('terminal_resize', { sessionId, cols, rows });
}

export function terminalDisconnect(sessionId: string): Promise<void> {
  return invoke<void>('terminal_disconnect', { sessionId });
}

export function isCommandError(err: unknown): err is CommandError {
  return typeof err === 'object' && err !== null && 'code' in err && 'message' in err;
}

/** A refused connect the user can resolve by approving the server key. */
export interface HostKeyProblem {
  kind: 'unknown' | 'changed';
  details: KeyErrorDetails;
}

export function hostKeyProblem(err: unknown): HostKeyProblem | null {
  if (!isCommandError(err) || !err.details) return null;
  if (err.code === 'SSH_HOSTKEY_UNKNOWN') return { kind: 'unknown', details: err.details };
  if (err.code === 'SSH_HOSTKEY_CHANGED') return { kind: 'changed', details: err.details };
  return null;
}

export function describeError(err: unknown): string {
  if (typeof err === 'object' && err !== null && 'code' in err && 'message' in err) {
    const { code, message } = err as CommandError;
    return `${code}: ${message}`;
  }
  return String(err);
}

/** Decodes the base64 payload of a terminal data event. */
export function decodeBase64(data: string): Uint8Array {
  const binary = atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

export type ProviderProtocolName = 'openai_responses' | 'openai_chat' | 'anthropic_messages';

export interface ToolCallingStatus {
  status: 'verified' | 'failed' | 'not_verified';
  lastCheckedAtMs: number | null;
}

export interface ProviderView {
  id: string;
  name: string;
  protocol: ProviderProtocolName;
  baseUrl: string;
  modelId: string;
  timeoutSeconds: number;
  maxOutputTokens: number;
  keyConfigured: boolean;
  isLoopback: boolean;
  allowInsecureHttp: boolean;
  updatedAtMs: number;
  toolCalling: ToolCallingStatus;
}

export interface TemplateView {
  id: string;
  displayName: string;
  protocol: ProviderProtocolName;
  defaultBaseUrl: string;
  note: string;
}

export interface ProviderInput {
  id?: string;
  expectedUpdatedAtMs?: number;
  name: string;
  protocol: ProviderProtocolName;
  baseUrl: string;
  modelId: string;
  timeoutSeconds: number;
  maxOutputTokens: number;
  /** Lets `http://` reach a non-loopback host. The key and prompts then travel unencrypted. */
  allowInsecureHttp?: boolean;
  /** Write-only. Empty or absent keeps the stored key. */
  apiKey?: string;
  clearApiKey?: boolean;
}

export interface ProviderTestView {
  kind: 'connectivity' | 'tool_calling';
  ok: boolean;
  verified: boolean;
  code: string | null;
  message: string;
  latencyMs: number;
  replyPreview: string | null;
}

export function providerTemplates(): Promise<TemplateView[]> {
  return invoke<TemplateView[]>('provider_templates');
}

export function providerList(): Promise<Page<ProviderView>> {
  return invoke<Page<ProviderView>>('provider_list', { cursor: null });
}

export function providerSave(input: ProviderInput): Promise<ProviderView> {
  return invoke<ProviderView>('provider_save', { input });
}

export interface ProviderModelsInput {
  /** With no key typed, the key saved for this provider is used. */
  id?: string;
  protocol: ProviderProtocolName;
  baseUrl: string;
  allowInsecureHttp?: boolean;
  /** Write-only. Used for this one request. */
  apiKey?: string;
}

export interface ModelsView {
  models: string[];
  latencyMs: number;
}

/** Lists the endpoint's models. Also the API key check: a bad key fails here. */
export function providerModels(input: ProviderModelsInput): Promise<ModelsView> {
  return invoke<ModelsView>('provider_models', { input });
}

export function providerDelete(id: string): Promise<void> {
  return invoke<void>('provider_delete', { id });
}

export function providerTest(id: string, kind: 'connectivity' | 'tool_calling'): Promise<ProviderTestView> {
  return invoke<ProviderTestView>('provider_test', { id, kind });
}

// ---- agent (M3) -------------------------------------------------------------

/** One event of a task. Mirrors `agent_service::TaskEventView` in Rust. */
export type TaskEvent =
  | { type: 'state'; state: string; errorCode: string | null }
  | { type: 'textDelta'; text: string }
  | { type: 'toolProposed'; executionId: string; tool: string; command: string; hostLabel: string }
  | {
      type: 'approvalRequired';
      approvalId: string;
      executionId: string;
      command: string;
      expiresAtMs: number;
      /** Set for a root command: whether the host's sudo asks for a password. */
      sudo?: SudoNeed | null;
    }
  | { type: 'toolFinished'; executionId: string; state: string; exitCode: number | null; errorCode: string | null }
  | { type: 'usage'; inputTokens: number | null; outputTokens: number | null }
  | { type: 'completed'; text: string }
  | { type: 'failed'; code: string; message: string };

export type AgentEvent = TaskEvent & { taskId: string };

export interface ExecutionView {
  id: string;
  taskId: string;
  hostId: string;
  modelId: string;
  tool: string;
  commandRedacted: string;
  commandSha256: string;
  state: string;
  policyDecision: string | null;
  exitCode: number | null;
  errorCode: string | null;
  stdoutPreview: string | null;
  stderrPreview: string | null;
  outputTruncated: boolean;
  createdAtMs: number;
  startedAtMs: number | null;
  finishedAtMs: number | null;
}

export interface AuditVerifyView {
  ok: boolean;
  eventsChecked: number;
  brokenAtSeq: number | null;
}

export function agentStart(
  hostId: string,
  providerId: string,
  prompt: string,
  history: ChatMessage[],
  onEvent: (event: AgentEvent) => void,
  modelId?: string,
): Promise<string> {
  const channel = new Channel<AgentEvent>();
  channel.onmessage = onEvent;
  return invoke<string>('agent_start', { hostId, providerId, modelId, prompt, history, onEvent: channel });
}

/** Events of a plain discussion reply. No host and no tools are involved. */
export type ChatEvent =
  | { type: 'textDelta'; text: string }
  | { type: 'usage'; inputTokens: number | null; outputTokens: number | null }
  | { type: 'completed' }
  | { type: 'failed'; code: string; message: string };

export interface ChatMessage {
  role: 'user' | 'assistant';
  content: string;
}

export interface ChatSessionSummary {
  id: string;
  title: string;
  createdAtMs: number;
  updatedAtMs: number;
}

export function chatSessionList(): Promise<ChatSessionSummary[]> {
  return invoke<ChatSessionSummary[]>('chat_session_list');
}

export function chatSessionGet(id: string): Promise<ChatSessionSummary & { transcript: string }> {
  return invoke<ChatSessionSummary & { transcript: string }>('chat_session_get', { id });
}

export function chatSessionSave(input: { id: string; title: string; transcript: string }): Promise<void> {
  return invoke<void>('chat_session_save', { input });
}

/** Streams one reply. The returned id can be cancelled with `agentCancel`. */
export function chatSend(
  providerId: string,
  messages: ChatMessage[],
  onEvent: (event: ChatEvent) => void,
  modelId?: string,
): Promise<string> {
  const channel = new Channel<ChatEvent>();
  channel.onmessage = onEvent;
  return invoke<string>('chat_send', { providerId, modelId, messages, onEvent: channel });
}

export function agentCancel(taskId: string): Promise<boolean> {
  return invoke<boolean>('agent_cancel', { taskId });
}

export type SudoNeed = 'no_password' | 'password_required';

/** How the operator answers sudo's password question. The password is write-only. */
export type SudoAnswer =
  | { source: 'typed'; password: string; remember: boolean }
  | { source: 'saved' }
  | { source: 'login' };

export interface SudoOptions {
  hostLabel: string;
  savedPassword: boolean;
  /** The host signs in with a saved SSH password that can be reused for sudo. */
  loginPassword: boolean;
}

export function approvalDecide(
  approvalId: string,
  approve: boolean,
  sudo?: SudoAnswer,
  approveRootForTask = false,
): Promise<void> {
  return invoke<void>('approval_decide', { approvalId, approve, sudo: sudo ?? null, approveRootForTask });
}

export function sudoOptions(approvalId: string): Promise<SudoOptions> {
  return invoke<SudoOptions>('sudo_options', { approvalId });
}

export function hostForgetSudoPassword(id: string): Promise<void> {
  return invoke<void>('host_forget_sudo_password', { id });
}

export function historyList(hostId: string, cursor?: string): Promise<Page<ExecutionView>> {
  return invoke<Page<ExecutionView>>('history_list', { hostId, cursor: cursor ?? null });
}

export function auditVerify(): Promise<AuditVerifyView> {
  return invoke<AuditVerifyView>('audit_verify');
}

// ---- AI history and preferences (M4) ----------------------------------------

export interface HistoryFilter {
  states: string[];
  sinceMs?: number | null;
  untilMs?: number | null;
}

export interface TaskView {
  id: string;
  hostId: string;
  state: string;
  modelId: string;
  errorCode: string | null;
  createdAtMs: number;
  finishedAtMs: number | null;
  /** The request, redacted before it was stored. Null for tasks from older versions. */
  promptPreview: string | null;
  /** Summed over the task's model turns. Null when the provider never reported it. */
  inputTokens: number | null;
  outputTokens: number | null;
  modelTurns: number;
  executions: number;
}

export interface HostUsageView {
  tasks: number;
  executions: number;
  failedExecutions: number;
  inputTokens: number | null;
  outputTokens: number | null;
  lastActivityMs: number | null;
}

export function historySummary(hostId: string): Promise<HostUsageView> {
  return invoke<HostUsageView>('history_summary', { hostId });
}

export interface ExportResult {
  path: string;
  executions: number;
  truncatedRows: boolean;
  extraRedactions: number;
  disclosure: string;
}

export interface PurgeResult {
  executions: number;
}

export interface AiPreferences {
  lastProviderId: string | null;
  outputRetentionDays: number;
  /** The server the chat is pinned to. Null or "" when none is pinned. */
  pinnedHostId: string | null;
  taskMaxSteps: number;
  taskMaxMinutes: number;
}

export function historyQuery(
  hostId: string,
  filter: HistoryFilter,
  cursor?: string,
): Promise<Page<ExecutionView>> {
  return invoke<Page<ExecutionView>>('history_query', { hostId, filter, cursor: cursor ?? null });
}

export function historyTasks(hostId: string, cursor?: string): Promise<Page<TaskView>> {
  return invoke<Page<TaskView>>('history_tasks', { hostId, cursor: cursor ?? null });
}

export function taskTimeline(taskId: string): Promise<ExecutionView[]> {
  return invoke<ExecutionView[]>('task_timeline', { taskId });
}

export function historyExport(hostId: string, filter: HistoryFilter): Promise<ExportResult> {
  return invoke<ExportResult>('history_export', { hostId, filter });
}

export function historyPurgeOutput(hostId: string): Promise<PurgeResult> {
  return invoke<PurgeResult>('history_purge_output', { hostId });
}

export function preferencesGet(): Promise<AiPreferences> {
  return invoke<AiPreferences>('preferences_get');
}

export function preferencesSet(input: Partial<AiPreferences>): Promise<AiPreferences> {
  return invoke<AiPreferences>('preferences_set', { input });
}
