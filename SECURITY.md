# Security Policy

## Supported versions

Only the latest release is supported. If a security issue is fixed, the fix
lands on `main` and ships in the next release — there are no backports to
older versions.

## Reporting a vulnerability

This is a small, single-maintainer project, and the reporting channel is
plainly GitHub issues: open one at
<https://github.com/johndfowler/otterm/issues>. That's fine here, and said
plainly — there is no private security mailing list.

One judgment call: if you believe the report itself would put users at risk
(for example, a working exploit for the lifeguard), say so in the issue title
and keep technical detail minimal until the maintainer responds.

## Trust model

Otterm is built around a few deliberate boundaries. If you're assessing a
report or a patch, these are the invariants that matter:

- **Captures are data, never code.** `output.log` is treated as untrusted
  terminal output: it is only ever written back to a terminal or decoded
  with `ansi-to-tui`. Nothing the store reads is executed or eval'd.
- **The lifeguard is read-only.** `otterm serve` never mutates capture data
  (the one store write on the serving path is garbage-collecting leaked
  `running/<id>` markers — the same sweep the TUI does). Every run id is
  validated by `valid_id` before it touches the filesystem, so no `../`
  games, and captured command lines are HTML-escaped on the index page.
- **The lifeguard binds the Tailscale IPv4 by default.** Loopback is
  available only as an explicit `--bind`. Never `--bind 0.0.0.0` (or `[::]`)
  unless you understand what the startup warning is telling you: anyone who
  can reach the machine can read every capture in the den. There is no
  authentication — the tailnet *is* the access boundary.
- **The zsh hook must never alter semantics.** The ambient-capture widget
  bails out rather than wrap anything whose meaning wrapping would change
  (pipes, redirects, `&&`/`;`, builtins, functions, aliases, env-assignment
  prefixes). When in doubt, it doesn't wrap.
- **No shell strings from tailnet data.** Peer names and addresses from
  `tailscale status --json` are passed to `ssh` as `Command` argv, never
  interpolated into a shell command.
- **No credentials, no telemetry.** The only network code is the lifeguard,
  and it only listens.
