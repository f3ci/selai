# ADR-0005: Agent tools, policy, approval and audit

- **Status:** Accepted (M3)
- **Scope:** S-030, S-031, S-032, S-033. Required before S-031 by the backlog.

## Context

The agent may run commands on one SSH host. Every command must pass policy and
be recorded before it reaches the network. The model must never choose a host,
and a command must run only with the approval that matches its exact text, host
and task. The spec forbids executing text the model produced in plain prose.

## Decisions

1. **Two tools, fixed names.** Provider function names allow only letters,
   digits, `_` and `-`, so the model sees `ssh_run_diagnostic` and
   `ssh_run_command`. These map to the stored `tool_name` values
   `ssh.run_diagnostic` and `ssh.run_command`.

2. **No host argument.** Neither schema has a host field. Each tool call is
   bound to the task's `target_host_id`. A call carrying any unknown property
   (including `host`, `host_id`, or `hostname`) is rejected without execution.

3. **Diagnostics are fixed templates.** `ssh_run_diagnostic` accepts an enum
   `operation` and typed arguments. Rust builds the command from a template.
   The only interpolated value is a service name, which must match
   `^[A-Za-z0-9@._-]{1,64}$` and is single-quoted. Diagnostics run without
   approval. The spec calls this risk reduction, not a read-only guarantee.

4. **Free-form commands always need approval.** `ssh_run_command` takes a string
   of 1 to 4000 bytes without NUL and a timeout of 1 to 60 seconds. There are no
   trust grants in M3. Autorun for commands is deferred (see C7 in ADR-0001).

5. **Policy is a pure function.** `selai-policy::evaluate` takes the tool
   request, the task scope and the policy version, and returns `Allow`,
   `NeedsApproval`, or `Deny(code)`. A host that differs from the task's host
   is `Deny(TASK_SCOPE_INVALID)`.

6. **Approval is a ticket bound to one execution.** An approval row is created
   after its execution row and carries the execution id, task id, host id,
   command SHA-256 and policy version. The schema's triggers enforce that all of
   these match. Expiry is 5 minutes. Before the execution moves to `running`, the
   runner re-reads the approval and execution from the database and recomputes
   the digest from the command it is about to run. Any mismatch, expiry, or
   denial stops the command.

7. **Audit before network I/O.** Each proposed call first writes an execution
   row in state `proposed` and a `tool_proposed` audit event, in one
   transaction. If that write fails, the command is not run and the task fails
   with `AUDIT_WRITE_FAILED`. The runner does not continue with other tools.

8. **Hash-chained audit log.** `audit_events` rows form a chain. Each `event_hash`
   is SHA-256 over the previous hash and a canonical JSON form of the event. `seq`
   is assigned inside the same transaction. `verify_chain` reports the first
   broken link. The chain is tamper-evident against accidental change only. A
   local administrator can rewrite the whole file (spec, section 8).

9. **Output bounds and redaction.** Each stream is capped at 64 KiB for storage
   (`stdout_preview`, `stderr_preview`, with truncation flags) and 32 KiB for the
   model. Both are redacted for known secret shapes (`password=`, bearer tokens,
   private-key blocks) and for the literal credentials that the task holds.
   **Deferred:** encrypted full-output blobs (`audit_blobs`). M3 stores only the
   redacted, bounded preview and its hash. Full encrypted output is in S-040.

10. **Timeouts and cancellation.** Exec runs on a separate channel with no PTY.
    On timeout or cancellation the channel is closed. The remote process may
    keep running, so the outcome records `remote_termination: unknown`. A
    cancelled task starts no further tool call.

11. **Limits.** Defaults from spec section 6.2: 12 steps, 10 minutes wall time,
    60 seconds per command, one command at a time, at most four tool calls per
    model turn.

12. **Provider failures do not replay commands.** A failed model request is
    retried only as the provider adapter allows (ADR-0004). A command that has
    started is never re-run because the model stream broke.

13. **Root commands (amended).** `ssh_run_command` takes `as_root`. The model
    never writes `sudo` and never handles a password; local code wraps the command
    as `sudo -S -k -p '<nonce>' -- sh -c '<command>'` (or `sudo -n` when the host
    needs no password). The stored, shown and approved text is `sudo <command>`,
    so its digest differs from the plain command's.
    - Before the approval is created, `LC_ALL=C sudo -k -n true` classifies the
      host: no password, password required, not allowed, or not installed. It
      runs nothing as root. "Not allowed" and "not installed" deny the command
      without asking the operator.
    - The password comes with the approval decision: typed, the sudo password
      saved for that host, or (password hosts only) the host's SSH password. It
      is held in memory and taken by the task only after the database confirmed
      the approval; every other outcome drops it. Only a host that asks for a
      password gets one.
    - It is written to stdin only after sudo prints this run's random prompt,
      once, and stdin is then closed. Output that arrives before any prompt
      closes stdin without writing it. A command that reads stdin therefore
      cannot capture it. The prompt is removed from the output and the password
      is redacted from stdout and stderr as a literal.
    - `sudo_used` (with `password_supplied`, never the value) is audited before
      the command runs. A rejected password is `SUDO_AUTH_FAILED`.
14. **This computer (amended).** A built-in host row (`kind = 'local'`, fixed
    id) is the target for a local terminal and for agent tasks on the operator's
    own machine. Tasks bound to it use the same policy, approval tickets, audit
    and limits; only the executor differs (a local process instead of an SSH
    exec channel, with stdin closed). Root commands use the same sudo wrapper on
    the local machine. The row cannot be edited, archived, probed or trusted.
    On Windows, each discovered WSL distribution has another `kind = 'local'`
    row with a stable UUID and `wsl_distribution` (migration 0011). Terminal and
    agent execution select that distribution through `wsl.exe` arguments. Tasks,
    approvals, credentials, and history stay bound to that distribution's host
    identity; the native Windows target retains its original fixed UUID.

## Consequences

- The agent runtime holds no SSH credentials. It receives an executor that
  wraps a session already authenticated for the task's host.
- A test that injects a forged `host_id`, a changed command after approval, a
  denied approval, an expired approval, or an audit write failure must each
  show that no command reaches the executor.
- Restart marks live tasks, executions and pending approvals as interrupted or
  cancelled. Nothing resumes (existing reconciliation).

## Fallback

If the audit chain is too slow on a platform, the chain can be computed per
transaction without changing the schema. Dropping the pre-record is not an option.

## Impact on other agents

- Agent C (providers) provides `ToolCall` and `Message` types only. It does not
  build tool results.
- Agent A (SSH) provides exec with the limits above. The executor never receives
  a credential.
