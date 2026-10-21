-- Keep the original constrained column for migration compatibility. All new
-- readers use the expanded method when present; no host table is rebuilt, so
-- pins, credentials, history and foreign keys remain intact.
ALTER TABLE hosts ADD COLUMN auth_method_v2 TEXT CHECK (auth_method_v2 IN (
  'password', 'private_key', 'auto', 'keyboard_interactive', 'agent',
  'certificate', 'security_key', 'smart_card', 'gssapi', 'host_based'
));
