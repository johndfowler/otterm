# Otterm ~( o.o )~

**The output librarian.** Run commands through Otterm and it captures every
byte they print — colors included — so "what did that migration say twenty
minutes ago" is a search, not a scrollback archaeology dig.

Since 0.3, the library is **live**: captures register the moment they start,
show up in the TUI with a spinner and a real-time byte count, and the viewer
tails them as they run — open a deploy mid-flight and watch it. Captures
whose process died without finishing are flagged, and `R` re-runs any past
command (in its original directory) as a new live capture you can follow
immediately.

```sh
# capture a run (output streams to your terminal exactly as normal)
otterm run -- cargo test
otterm run -- npm run build

# reprint the most recent capture (pipeable)
otterm last | grep error

# browse and search the library
otterm
```

## Why a pty?

`otterm run` spawns the command on a pseudo-terminal, so tools still believe
they're talking to a real terminal: colors stay on, progress bars render,
prompts work. Stdin is forwarded (interactively when you're on a TTY, with
proper EOF signaling when piped), terminal resizes propagate, and the child's
exit code is passed through — `otterm run` is transparent to scripts, CI,
and `&&` chains.

## Ambient capture (zsh)

Add to `~/.zshrc`:

```sh
eval "$(otterm init zsh)"
```

Every eligible command you type is then captured automatically — no
`otterm run` needed. Eligible means a plain, single, external command:
pipes, redirects, `&&`/`;` chains, builtins, functions, aliases, and
env-assignment prefixes run untouched, because wrapping would change what
they mean. Editors, pagers, ssh, multiplexers, and friends are blocklisted
(extend with `OTTERM_IGNORE="foo bar"`). Your history keeps exactly what you
typed. Opt out per command with a leading space, or per session with
`otterm-off` / `otterm-on`.

## The library (TUI)

| Key | Action |
|---|---|
| `Enter` | view a run's output (ANSI rendered; tails live runs) |
| `R` | re-run the selected command as a new live capture |
| `f` | in a live viewer: toggle follow (pin to the tail) |
| `/` | filter runs by command / directory (live) |
| `s` | full-text search across **all** captured output |
| `x` then `y` | delete a run |
| `j k d u g G` | move / page / jump (scrolling up unpins follow; `G` re-pins) |
| `n` `N` | next / previous match in the viewer |
| `r` | reload the index |
| `q` / `Esc` | back / quit |

Run states in the list: `✓`/`✗` finished (exit 0 / nonzero), a spinner with a
counting duration for **running** captures, and `!` for captures whose
process died before finishing.

Search results show `command · line: matching text`; `Enter` opens the run
scrolled to that line with the match highlighted.

## Storage

Runs live under your platform data dir (`~/Library/Application Support/otterm`
on macOS, `~/.local/share/otterm` on Linux), one directory per run: the raw
`output.log` plus a `meta.json` (command, cwd, timing, exit code, size).
`index.jsonl` is the append-only catalog, and `running/` holds one marker
file per in-flight capture so discovering live runs never scans the whole
library. Set `OTTERM_DATA_DIR` to relocate
everything. Logs over 16 MB are viewed/searched tail-first; nothing is ever
truncated on disk. Prune from the TUI with `x`.

## Build

```sh
cargo build --release   # binary at target/release/otterm
```
