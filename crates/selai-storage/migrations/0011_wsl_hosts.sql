-- Each WSL distribution is a distinct local target, including its audit history.
ALTER TABLE hosts ADD COLUMN wsl_distribution TEXT
  CHECK (wsl_distribution IS NULL OR (kind = 'local' AND length(wsl_distribution) BETWEEN 1 AND 256));
CREATE UNIQUE INDEX idx_hosts_wsl_distribution
  ON hosts(wsl_distribution COLLATE NOCASE) WHERE wsl_distribution IS NOT NULL;
