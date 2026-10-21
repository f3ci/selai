import type { AuthMethod, HostInput, HostView } from '../ipc';

export const MAX_IMPORT_BYTES = 2 * 1024 * 1024;
export const MAX_KDBX_BYTES = 10 * 1024 * 1024;
export const MAX_IMPORT_ROWS = 500;

export const TEMPLATE = 'label,hostname,port,username,auth_method,password,private_key_path,key_passphrase,notes\r\nExample,example.com,22,ops,password,,,,\r\n';

export const FIELDS = [
  'label', 'hostname', 'uri', 'port', 'username', 'authMethod',
  'password', 'privateKeyPath', 'keyPassphrase', 'notes', 'type',
] as const;
export type ImportField = typeof FIELDS[number];
export type ColumnMapping = Record<ImportField, string>;

export interface ProfileTable {
  headers: string[];
  rows: Record<string, string>[];
}

export interface ProfileCandidate {
  rowNumber: number;
  label: string;
  hostname: string;
  username: string;
  authMethod: AuthMethod;
  hasSecret: boolean;
  issue: string | null;
  input: HostInput | null;
}

const ALIASES: Record<ImportField, string[]> = {
  label: ['label', 'name', 'title', 'displayname'],
  hostname: ['hostname', 'host', 'address', 'server', 'ipaddress', 'ip'],
  uri: ['uri', 'url', 'loginuri', 'uris'],
  port: ['port', 'sshport'],
  username: ['username', 'loginusername', 'user', 'login'],
  authMethod: ['authmethod', 'authentication', 'auth'],
  password: ['password', 'loginpassword'],
  privateKeyPath: ['privatekeypath', 'privatekey', 'identityfile', 'identitypath', 'keypath'],
  keyPassphrase: ['keypassphrase', 'passphrase'],
  notes: ['notes', 'description'],
  type: ['type', 'itemtype'],
};

const normalized = (value: string) => value.toLowerCase().replace(/[^a-z0-9]/g, '');

export function guessMapping(headers: string[]): ColumnMapping {
  return Object.fromEntries(FIELDS.map((field) => [
    field,
    headers.find((header) => ALIASES[field].includes(normalized(header))) ?? '',
  ])) as ColumnMapping;
}

function sshConfigFile(filename: string): boolean {
  const base = filename.replace(/\\/g, '/').split('/').at(-1)?.toLowerCase() ?? '';
  return base === 'config' || base === 'ssh_config' || base === 'ssh-config' || base.endsWith('.sshconfig');
}

function sshConfigWords(line: string): string[] {
  const uncommented: string[] = [];
  let quote = '';
  let escaped = false;
  for (let i = 0; i < line.length; i += 1) {
    const char = line[i]!;
    if (escaped) {
      uncommented.push(char);
      escaped = false;
    } else if (char === '\\') {
      uncommented.push(char);
      escaped = true;
    } else if (quote) {
      uncommented.push(char);
      if (char === quote) quote = '';
    } else if (char === '"' || char === "'") {
      quote = char;
      uncommented.push(char);
    } else if (char === '#' && (i === 0 || /\s/.test(line[i - 1]!))) {
      break;
    } else {
      uncommented.push(char);
    }
  }
  const source = uncommented.join('').replace(/^(\s*[A-Za-z][A-Za-z0-9]*)\s*=\s*/, '$1 ');
  const words: string[] = [];
  let word = '';
  quote = '';
  escaped = false;
  for (let i = 0; i < source.length; i += 1) {
    const char = source[i]!;
    if (escaped) {
      word += char;
      escaped = false;
    } else if (char === '\\') {
      const next = source[i + 1];
      if (next && (/[\s"'#\\]/.test(next))) escaped = true;
      else word += char;
    } else if (quote) {
      if (char === quote) quote = '';
      else word += char;
    } else if (char === '"' || char === "'") {
      quote = char;
    } else if (/\s/.test(char)) {
      if (word) words.push(word);
      word = '';
    } else {
      word += char;
    }
  }
  if (escaped) word += '\\';
  if (word) words.push(word);
  return words;
}

function globMatches(pattern: string, host: string): boolean {
  const escaped = [...pattern].map((char) => {
    if (char === '*') return '.*';
    if (char === '?') return '.';
    return '.^$+()[]{}|\\'.includes(char) ? `\\${char}` : char;
  }).join('');
  return new RegExp(`^${escaped}$`, 'i').test(host);
}

interface SshConfigBlock {
  patterns: string[];
  options: Map<string, string>;
  identityFiles: string[];
}

function setSshOption(block: SshConfigBlock, key: string, value: string) {
  if (key === 'identityfile') block.identityFiles.push(value);
  else if (!block.options.has(key)) block.options.set(key, value);
}

function expandSshTokens(value: string, alias: string, hostname: string, username: string): string {
  return value
    .replaceAll('%%', '\u0000')
    .replaceAll('%d', '~')
    .replaceAll('%n', alias)
    .replaceAll('%h', hostname)
    .replaceAll('%r', username)
    .replaceAll('\u0000', '%');
}

/** Converts common OpenSSH Host blocks to the same reviewable import table. */
export function parseSshConfig(text: string): ProfileTable {
  if (new TextEncoder().encode(text).length > MAX_IMPORT_BYTES) throw new Error('Import file is larger than 2 MB.');
  const logicalLines: string[] = [];
  let continuation = '';
  for (const raw of text.replace(/^\uFEFF/, '').split(/\r?\n/)) {
    const line = raw.trimEnd();
    let slashes = 0;
    for (let i = line.length - 1; i >= 0 && line[i] === '\\'; i -= 1) slashes += 1;
    if (slashes % 2 === 1) {
      continuation += `${line.slice(0, -1)} `;
      continue;
    }
    logicalLines.push(continuation + line);
    continuation = '';
  }
  if (continuation) logicalLines.push(continuation);

  const global: SshConfigBlock = { patterns: [], options: new Map(), identityFiles: [] };
  const blocks: SshConfigBlock[] = [];
  let current: SshConfigBlock | null = global;
  const aliases: string[] = [];
  const seenAliases = new Set<string>();
  for (const line of logicalLines) {
    const [rawKey, ...values] = sshConfigWords(line);
    if (!rawKey || !values.length) continue;
    const key = rawKey.toLowerCase();
    if (key === 'host') {
      current = { patterns: values, options: new Map(), identityFiles: [] };
      blocks.push(current);
      for (const pattern of values) {
        if (pattern.startsWith('!') || /[*?]/.test(pattern)) continue;
        const folded = pattern.toLowerCase();
        if (!seenAliases.has(folded)) {
          seenAliases.add(folded);
          aliases.push(pattern);
        }
      }
      continue;
    }
    if (key === 'match') {
      current = null;
      continue;
    }
    if (current) setSshOption(current, key, values.join(' '));
  }

  if (!aliases.length) throw new Error('No concrete Host aliases found. Host patterns, Include files, and Match blocks are not imported.');
  if (aliases.length > MAX_IMPORT_ROWS) throw new Error(`Import is limited to ${MAX_IMPORT_ROWS} rows.`);

  const headers = [
    'label', 'hostname', 'port', 'username', 'auth_method', 'private_key_path',
    'connect_timeout_seconds', 'keep_alive_seconds', 'ssh_config_issue',
  ];
  const rows = aliases.map((alias) => {
    const options = new Map(global.options);
    const identityFiles = [...global.identityFiles];
    for (const block of blocks) {
      const excluded = block.patterns.some((p) => p.startsWith('!') && globMatches(p.slice(1), alias));
      const included = block.patterns.some((p) => !p.startsWith('!') && globMatches(p, alias));
      if (excluded || !included) continue;
      for (const [key, value] of block.options) if (!options.has(key)) options.set(key, value);
      identityFiles.push(...block.identityFiles);
    }
    const username = options.get('user') ?? '';
    const rawHostname = options.get('hostname') ?? alias;
    const hostname = expandSshTokens(rawHostname, alias, rawHostname, username);
    const identityFile = identityFiles.find((path) => path.toLowerCase() !== 'none') ?? '';
    const expandedKeyPath = expandSshTokens(identityFile, alias, hostname, username);
    const keyPath = expandedKeyPath && !/^(?:\/|~[\\/]|[A-Za-z]:[\\/]|\\\\)/.test(expandedKeyPath)
      ? `~/${expandedKeyPath}` : expandedKeyPath;
    const proxyJump = options.get('proxyjump');
    const proxyCommand = options.get('proxycommand');
    const unsupportedProxy = (proxyJump && proxyJump.toLowerCase() !== 'none') || (proxyCommand && proxyCommand.toLowerCase() !== 'none');
    const unresolvedToken = /%[a-z]/i.test(keyPath) || /%[a-z]/i.test(hostname) || /%[a-z]/i.test(username);
    const port = options.get('port') ?? '22';
    const connectTimeout = options.get('connecttimeout') ?? '15';
    const keepAlive = options.get('serveraliveinterval') ?? '30';
    let issue = '';
    if (unsupportedProxy) issue = 'ProxyJump/ProxyCommand is not supported by Selai; profile skipped.';
    else if (unresolvedToken) issue = 'SSH config contains an unsupported % token in the host, user, or key path.';
    else if (!username) issue = 'No User is configured for this host.';
    else if (!/^\d+$/.test(port) || Number(port) < 1 || Number(port) > 65535) issue = 'Port must be a number from 1 to 65535.';
    else if (!/^\d+$/.test(connectTimeout) || Number(connectTimeout) < 1 || Number(connectTimeout) > 120) issue = 'ConnectTimeout must be from 1 to 120 seconds.';
    else if (!/^\d+$/.test(keepAlive) || Number(keepAlive) > 300) issue = 'ServerAliveInterval must be from 0 to 300 seconds.';
    return {
      label: alias,
      hostname,
      port,
      username,
      auth_method: identityFile ? 'private_key' : 'password',
      private_key_path: keyPath,
      connect_timeout_seconds: connectTimeout,
      keep_alive_seconds: keepAlive,
      ssh_config_issue: issue,
    };
  });
  return { headers, rows };
}

/** Parse RFC 4180-style CSV, including quoted commas, quotes and newlines. */
function csvRecords(text: string): string[][] {
  const records: string[][] = [];
  let row: string[] = [];
  let field = '';
  let quoted = false;
  let closedQuote = false;
  const finishRow = () => {
    row.push(field);
    if (row.some((value) => value.trim())) records.push(row);
    row = [];
    field = '';
    closedQuote = false;
  };
  for (let i = 0; i < text.length; i += 1) {
    const char = text[i];
    if (quoted) {
      if (char === '"' && text[i + 1] === '"') {
        field += '"';
        i += 1;
      } else if (char === '"') {
        quoted = false;
        closedQuote = true;
      } else {
        field += char;
      }
    } else if (char === '"' && !field && !closedQuote) {
      quoted = true;
    } else if (char === ',') {
      row.push(field);
      field = '';
      closedQuote = false;
    } else if (char === '\n' || char === '\r') {
      finishRow();
      if (char === '\r' && text[i + 1] === '\n') i += 1;
    } else if (char === '"' || (closedQuote && char !== ' ' && char !== '\t')) {
      throw new Error(`Invalid CSV quoting near row ${records.length + 1}.`);
    } else if (!closedQuote) {
      field += char;
    }
  }
  if (quoted) throw new Error('CSV ends inside a quoted field.');
  if (row.length || field) finishRow();
  return records;
}

function object(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? value as Record<string, unknown> : null;
}

function string(value: unknown): string {
  return typeof value === 'string' || typeof value === 'number' ? String(value) : '';
}

function jsonRow(item: unknown): Record<string, string> {
  const source = object(item);
  if (!source) throw new Error('Every JSON profile must be an object.');
  const row: Record<string, string> = {};
  for (const [key, value] of Object.entries(source)) {
    if (typeof value === 'string' || typeof value === 'number') row[key] = String(value);
  }
  // Bitwarden plaintext JSON vault exports.
  const login = object(source.login);
  if (login) {
    row.login_username = string(login.username);
    row.login_password = string(login.password);
    const uris = Array.isArray(login.uris) ? login.uris : [];
    row.login_uri = uris.map((entry) => string(object(entry)?.uri)).find((uri) => /^(ssh|sftp):\/\//i.test(uri)) ?? '';
  }
  // A host object shaped like the documented Termius API Bridge payload.
  const ssh = object(source.ssh);
  if (ssh) {
    row.ssh_port = string(ssh.port);
    const credentials = object(ssh.credentials);
    if (credentials) {
      row.username = string(credentials.username);
      row.password = string(credentials.password);
      if (string(object(credentials.key)?.private)) row.embedded_private_key = 'yes';
    }
  }
  return row;
}

export function parseProfileTable(text: string, filename: string): ProfileTable {
  if (new TextEncoder().encode(text).length > MAX_IMPORT_BYTES) throw new Error('Import file is larger than 2 MB.');
  const source = text.replace(/^\uFEFF/, '');
  if (/\.kdbx$/i.test(filename)) throw new Error('Enter the KDBX passphrase to unlock this file.');
  if (sshConfigFile(filename)) return parseSshConfig(source);
  if (/\.json$/i.test(filename)) {
    let data: unknown;
    try { data = JSON.parse(source); } catch { throw new Error('Invalid JSON file.'); }
    const root = object(data);
    if (root?.encrypted === true) throw new Error('Encrypted vault exports are not supported. Choose a plaintext export.');
    const items = Array.isArray(data) ? data : Array.isArray(root?.items) ? root.items : Array.isArray(root?.hosts) ? root.hosts : null;
    if (!items) throw new Error('JSON must contain an array of profiles, items, or hosts. Encrypted vault exports are not supported.');
    if (!items.length) throw new Error('No profiles found in the JSON file.');
    if (items.length > MAX_IMPORT_ROWS) throw new Error(`Import is limited to ${MAX_IMPORT_ROWS} rows.`);
    const rows = items.map(jsonRow);
    return { headers: [...new Set(rows.flatMap((row) => Object.keys(row)))], rows };
  }
  if (!/\.csv$/i.test(filename)) throw new Error('Choose a CSV or plaintext JSON file.');
  const records = csvRecords(source);
  if (!records.length) throw new Error('CSV is empty.');
  const headers = records[0]!.map((value) => value.trim());
  if (headers.length > 100 || headers.some((header) => !header || headers.filter((name) => name === header).length > 1)) {
    throw new Error('CSV headers must be non-empty and unique (up to 100 columns).');
  }
  if (records.length === 1) throw new Error('CSV has no profile rows.');
  if (records.length - 1 > MAX_IMPORT_ROWS) throw new Error(`Import is limited to ${MAX_IMPORT_ROWS} rows.`);
  const rows = records.slice(1).map((cells, index) => {
    if (cells.length > headers.length) throw new Error(`CSV row ${index + 2} has more fields than the header.`);
    return Object.fromEntries(headers.map((header, column) => [header, cells[column] ?? '']));
  });
  return { headers, rows };
}

function endpoint(raw: string, requireScheme: boolean): { hostname: string; port: string; username: string; password: string } | null {
  if (!raw || (requireScheme && !/^(ssh|sftp):\/\//i.test(raw))) return null;
  if (/^[a-z][a-z0-9+.-]*:\/\//i.test(raw) && !/^(ssh|sftp):\/\//i.test(raw)) return null;
  try {
    const parsed = new URL(/^(ssh|sftp):\/\//i.test(raw) ? raw : `ssh://${raw}`);
    if (!['ssh:', 'sftp:'].includes(parsed.protocol) || !parsed.hostname || !['', '/'].includes(parsed.pathname) || parsed.search || parsed.hash) return null;
    return {
      hostname: parsed.hostname.replace(/^\[|\]$/g, ''), port: parsed.port,
      username: decodeURIComponent(parsed.username), password: decodeURIComponent(parsed.password),
    };
  } catch { return null; }
}

function safeLabel(raw: string, hostname: string): string {
  const clean = (value: string) => value.replace(/[^A-Za-z0-9 ._-]/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 48).trim();
  const label = clean(raw);
  return label.length >= 2 && /^[A-Za-z0-9]/.test(label) ? label : clean(`SSH ${hostname}`);
}

function validHostname(hostname: string): boolean {
  if (hostname.includes(':')) return true; // URL parsing already validated the IPv6 literal.
  return hostname.length <= 253 && hostname.split('.').every((label) =>
    label.length > 0 && label.length <= 63 && /^[A-Za-z0-9](?:[A-Za-z0-9-]*[A-Za-z0-9])?$/.test(label));
}

export function prepareProfiles(table: ProfileTable, mapping: ColumnMapping, existing: HostView[]): ProfileCandidate[] {
  const labels = new Set(existing.map((host) => host.label.toLocaleLowerCase()));
  const targets = new Set(existing.map((host) => `${host.hostname.toLocaleLowerCase()}|${host.port}|${host.username}`));
  return table.rows.map((row, index) => {
    const cell = (field: ImportField) => row[mapping[field]] ?? '';
    const sourceLabel = cell('label').trim();
    const rawHost = cell('hostname').trim();
    const rawUri = cell('uri').trim();
    const address = endpoint(rawHost || rawUri, !rawHost);
    const hostname = address?.hostname ?? '';
    const username = cell('username').trim() || address?.username || '';
    const port = Number(cell('port').trim() || address?.port || '22');
    const password = cell('password') || address?.password || '';
    const privateKeyPath = cell('privateKeyPath').trim();
    const keyPassphrase = cell('keyPassphrase');
    const rawAuth = normalized(cell('authMethod'));
    const methods: Record<string, AuthMethod> = { password: 'password', privatekey: 'private_key', key: 'private_key', auto: 'auto', keyboardinteractive: 'keyboard_interactive', agent: 'agent', certificate: 'certificate', securitykey: 'security_key', smartcard: 'smart_card', gssapi: 'gssapi', hostbased: 'host_based' };
    const authMethod: AuthMethod = methods[rawAuth] ?? (!rawAuth && privateKeyPath ? 'private_key' : 'password');
    const type = normalized(cell('type'));
    const displayLabel = safeLabel(sourceLabel || hostname, hostname);
    let issue: string | null = row.ssh_config_issue || null;
    if (!issue && type && !['1', 'login', 'ssh', 'password', 'host'].includes(type)) issue = 'Not a login or SSH profile.';
    else if (row.embedded_private_key || /-----BEGIN [A-Z ]*PRIVATE KEY-----/.test(privateKeyPath)) issue = 'Embedded private key cannot be imported; provide a local key file path.';
    else if (!hostname) issue = rawUri || rawHost ? 'Only SSH/SFTP addresses or hostnames are supported.' : 'Missing hostname or SSH URI.';
    else if (!validHostname(hostname)) issue = 'Invalid hostname or IP address.';
    else if (!username) issue = 'Missing username.';
    else if (username.length > 64 || !/^[!-~]+$/.test(username)) issue = 'Username must be printable ASCII without spaces (up to 64 characters).';
    else if (!Number.isInteger(port) || port < 1 || port > 65535) issue = 'Port must be 1 to 65535.';
    else if (rawAuth && !methods[rawAuth]) issue = 'Unsupported authentication method.';
    else if ((authMethod === 'private_key' || authMethod === 'certificate') && !privateKeyPath) issue = 'Key authentication needs a local private key path.';
    else if (authMethod === 'private_key' && password) issue = 'Password conflicts with key authentication.';
    else if (!['password', 'private_key'].includes(authMethod) && (password || keyPassphrase)) issue = 'Native SSH credentials are requested when connecting; remove saved secrets from this row.';
    else if (authMethod === 'password' && keyPassphrase) issue = 'Key passphrase needs key authentication.';
    else if (labels.has(displayLabel.toLocaleLowerCase())) issue = 'Label already exists.';
    else if (targets.has(`${hostname.toLocaleLowerCase()}|${port}|${username}`)) issue = 'Host and username already exist.';

    const configuredConnectTimeout = Number(row.connect_timeout_seconds);
    const configuredKeepAlive = Number(row.keep_alive_seconds);
    const input: HostInput | null = issue ? null : {
      label: displayLabel, hostname, port, username, authMethod,
      privateKeyPath: authMethod !== 'password' && privateKeyPath ? privateKeyPath : null,
      connectTimeoutSeconds: Number.isInteger(configuredConnectTimeout) ? configuredConnectTimeout : 15,
      keepAliveSeconds: Number.isInteger(configuredKeepAlive) ? configuredKeepAlive : 30,
      terminalType: 'xterm-256color',
      notes: cell('notes') || null,
      password: authMethod === 'password' && password ? password : undefined,
      keyPassphrase: authMethod === 'private_key' && keyPassphrase ? keyPassphrase : undefined,
    };
    if (input) {
      labels.add(displayLabel.toLocaleLowerCase());
      targets.add(`${hostname.toLocaleLowerCase()}|${port}|${username}`);
    }
    return { rowNumber: index + 2, label: displayLabel, hostname, username, authMethod,
      hasSecret: Boolean(password || keyPassphrase), issue, input };
  });
}
