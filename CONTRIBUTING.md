# Contributing to otterm

Thanks for stopping by the den. Contributions are welcome — bug reports,
fixes, docs, and features alike.

**For anything big, open an issue first**
(<https://github.com/johndfowler/otterm/issues>) so we can agree on shape
before you spend the effort. Small fixes can come straight in as PRs.

## Build and test

```sh
cargo build --release   # binary at target/release/otterm
cargo test              # the whole suite — unit tests + the lifeguard's
                        # HTTP and streaming endpoints over loopback
cargo clippy            # keep it clean; don't add new warnings
cargo fmt               # standard rustfmt, 4-space indent
```

There is no CI — `cargo test` passing locally is the bar, plus manual
verification for anything that touches the pty or the TUI.

## Manual verification culture

pty and TUI behavior doesn't fit in `cargo test`, so verify by hand:

```sh
OTTERM_DATA_DIR=$(mktemp -d) cargo run -- run -- ls -la  # colors preserved
OTTERM_DATA_DIR=$(mktemp -d) cargo run                   # the TUI
cargo run -- run -- false; echo $?                       # child's exit code
cargo run -- run -- cat <<< hi                           # no VEOF echo captured
cargo run -- serve --bind 127.0.0.1                      # the lifeguard, sandboxed
OTTERM_FAKE_PEERS="deck=100.64.0.2,attic=100.64.0.3,offline" cargo run  # fake tailnet
```

The two transparency rules that must always hold:

- `otterm run` exits with the child's exit code — it stays invisible to
  scripts, CI, and `&&` chains.
- Captured commands keep believing they're on a real terminal: stdin
  forwarded, resizes propagated, raw mode always restored.

## Code style

- Errors are plain `io::Result` with `io::Error::other(...)` for
  external-crate errors. No `anyhow`/`thiserror`.
- **No new dependencies without discussion.** Open an issue first. In
  particular, `ratatui` and `crossterm` are kept in lockstep — never bump
  one without the other.
- `cargo fmt` before submitting; standard rustfmt style.
- Module-level `//!` docs explain *why*, not what; inline comments only
  where intent isn't obvious. The wry, otter-flavored naming is part of the
  project — keep it, never at the cost of clarity.
- The store tolerates corruption by design (`list()` skips bad lines,
  `delete()` ignores missing files). Preserve that: one bad run must never
  break the library.
- Terminal safety: raw mode and the alternate screen are always restored,
  via guards and the panic hook — keep it that way.

## Commit style

Short imperative subjects, otter flavor welcome
(`Lifeguard: /stream/<id> — replay, live tail, done frame over WebSocket`).
Update `CHANGELOG.md` and `AGENTS.md` when you change behavior, layout, or
conventions they describe.
