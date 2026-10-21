import { describe, expect, it } from 'vitest';
import type { HostView } from '../ipc';
import { guessMapping, parseProfileTable, prepareProfiles } from './sshProfileImport';

function candidates(text: string, filename: string, existing: HostView[] = []) {
  const table = parseProfileTable(text, filename);
  return prepareProfiles(table, guessMapping(table.headers), existing);
}

function at<T>(items: T[], index: number): T {
  const item = items[index];
  if (!item) throw new Error(`Missing row ${index}`);
  return item;
}

describe('SSH profile import', () => {
  it('reads the common CSV format with quoted fields and keeps the password for host_save', () => {
    const rows = candidates('label,hostname,port,username,auth_method,password,notes\r\n"Production, east",example.com,2222,ops,password,"a,b","line one\nline two"\r\n', 'hosts.csv');
    expect(rows).toHaveLength(1);
    expect(at(rows, 0).issue).toBeNull();
    expect(at(rows, 0).input).toMatchObject({
      label: 'Production east', hostname: 'example.com', port: 2222,
      username: 'ops', password: 'a,b', notes: 'line one\nline two',
    });
  });

  it('accepts only SSH logins from a mixed Bitwarden CSV', () => {
    const rows = candidates('folder,type,name,notes,login_uri,login_username,login_password\nServers,login,Web,,https://example.com,web,web-secret\nServers,login,Shell,,ssh://ops@db.example.com:2200,,ssh-secret\nServers,note,Private note,secret,,,\n', 'vault.csv');
    expect(at(rows, 0).issue).toContain('SSH/SFTP');
    expect(at(rows, 1).input).toMatchObject({ hostname: 'db.example.com', port: 2200, username: 'ops', password: 'ssh-secret' });
    expect(at(rows, 2).issue).toContain('Not a login');
  });

  it('maps a generic password-manager CSV and a Termius-shaped JSON host to one profile shape', () => {
    const csv = candidates('Name,URI,Username,Password\nDatabase,sftp://db.example.com,admin,secret\n', 'passbolt.csv');
    expect(at(csv, 0).input).toMatchObject({ label: 'Database', hostname: 'db.example.com', username: 'admin', password: 'secret' });
    const json = candidates(JSON.stringify({ hosts: [{
      label: 'Gateway', address: 'gateway.example.com', ssh: { port: 2222, credentials: { username: 'root', password: 'secret' } },
    }] }), 'hosts.json');
    expect(at(json, 0).input).toMatchObject({ label: 'Gateway', hostname: 'gateway.example.com', port: 2222, username: 'root', password: 'secret' });
  });

  it('uses Bitwarden plaintext JSON only for an SSH URI', () => {
    const rows = candidates(JSON.stringify({ items: [
      { type: 1, name: 'Shell', login: { username: 'deploy', password: 'secret', uris: [{ uri: 'https://example.com' }, { uri: 'ssh://example.com' }] } },
      { type: 1, name: 'Website', login: { username: 'web', password: 'web-secret', uris: [{ uri: 'https://example.com' }] } },
    ] }), 'bitwarden.json');
    expect(at(rows, 0).input).toMatchObject({ hostname: 'example.com', username: 'deploy', password: 'secret' });
    expect(at(rows, 1).input).toBeNull();
  });

  it('skips duplicate labels and embedded private key material', () => {
    const existing = [{ label: 'Gateway', hostname: 'old.example.com', port: 22, username: 'root' }] as HostView[];
    const rows = candidates(JSON.stringify({ hosts: [
      { label: 'Gateway', address: 'new.example.com', username: 'root' },
      { label: 'Key host', address: 'key.example.com', ssh: { credentials: { username: 'root', key: { private: 'PRIVATE' } } } },
    ] }), 'hosts.json', existing);
    expect(at(rows, 0).issue).toContain('Label already exists');
    expect(at(rows, 1).issue).toContain('Embedded private key');
  });

  it('rejects malformed or encrypted source data before preview', () => {
    expect(() => parseProfileTable('label,hostname\n"unfinished,example.com', 'hosts.csv')).toThrow('quoted field');
    expect(() => parseProfileTable('{}', 'vault.json')).toThrow('Encrypted vault exports');
    expect(() => parseProfileTable('{"encrypted":true,"items":[]}', 'vault.json')).toThrow('Encrypted vault exports');
    expect(() => parseProfileTable('', 'vault.kdbx')).toThrow('KDBX');
  });
});


it('imports native authentication modes and refuses embedded native credentials', () => {
  const rows = candidates('label,hostname,username,auth_method,private_key_path,password\nCert,cert.test,ops,certificate,/keys/id,\nAgent,agent.test,ops,agent,,\nMFA,mfa.test,ops,keyboard_interactive,,secret\n', 'hosts.csv');
  expect(at(rows, 0).input).toMatchObject({ authMethod: 'certificate', privateKeyPath: '/keys/id' });
  expect(at(rows, 1).input).toMatchObject({ authMethod: 'agent', privateKeyPath: null });
  expect(at(rows, 2).issue).toContain('requested when connecting');
});
