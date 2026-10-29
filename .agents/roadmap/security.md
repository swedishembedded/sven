# security

Scope: Hardening `sven rc` so that a compromised remote-control server (which chooses tool calls and relays approvals) gets no more than the machine owner allowed locally.

## Done

- Non-hosted `sven rc`: file tools confined to the working directory, sven's
  own sign-in/credentials/session store refused, `web_fetch` public-only, gdb
  host commands refused; `--wide-access` opts out; `tools.only` /
  `tools.disabled` are the operator allow list
  (`docs/guides/remote-control.md`).

- Hosted: Landlock ABI 6 (TCP connect limited to 443, 80, 53 and the
  sign-in's ports, no bind, signal and abstract-socket scopes), failing
  closed (`sven_remote::sandbox`).
- Hosted: configuration is read from `--config` alone
  (`ConfigDocument::load_explicit`).
- Sign-in: https required except loopback, discovery URLs must be on the
  server's origin; an unreadable command is skipped, not retried forever.

## Left

- The `shell` tool is not path-confined in non-hosted `sven rc`; the
  recommendation is a container with only the project mounted. A
  Landlock profile for non-hosted `sven rc` would close it.
- `start_server` with `--wide-access` off still refuses only model-spelled
  commands; a discovered server command comes from project files.
