# ccnest

A Claude Code-aware terminal multiplexer. Windows-first. Inspired by
[Shin-sibainu/ccmux](https://github.com/Shin-sibainu/ccmux), but with
prefix-less pane creation hotkeys and a structured left sidebar that
surfaces file tree, Claude context usage, git status, and pane list.

## Status

v0.1 — Windows only. Rust + `ratatui` + `crossterm` + `portable-pty` + `vt100`.

## Install

```sh
git clone https://github.com/mitama987/ccnest
cd ccnest
cargo install --path .
```

Requires a working `claude` CLI on `PATH`. If `claude` isn't found, panes
fall back to the system shell (`%ComSpec%` / `$SHELL`) so the multiplexer
stays usable.

## Default keybindings

| Key | Action |
|-----|--------|
| `Ctrl+D` | Split the focused pane horizontally (new `claude` below) |
| `Ctrl+E` | Split the focused pane vertically (new `claude` to the right) |
| `Ctrl+T` | Open a new tab with a fresh `claude` pane |
| `Ctrl+W` | Close the focused pane (SIGTERM) |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Alt + ← → ↑ ↓` / `Alt + h j k l` | Move pane focus |
| `Ctrl+B` | Toggle the left sidebar |
| `Ctrl+1` .. `Ctrl+4` | Jump to sidebar section (Files / Claude / Git / Panes) |
| `Alt+S` / `Alt+C` | Focus sidebar / focus content |
| `Tab` (sidebar focused) | Cycle sidebar section |
| `↑` / `↓` / `j` / `k` (sidebar focused) | Move selection cursor |
| `Enter` (on a file row) | Open the entry in `$EDITOR` (falls back to `code`) |
| `Ctrl+Q` | Quit |
| anything else | Sent to the focused pane |

### Ctrl+D and EOF

`Ctrl+D` is captured by the multiplexer before it reaches `claude`, so it
will no longer send EOF. To exit a Claude session, use `/exit` inside
Claude or `Ctrl+W` to close the pane from the outside.

## Sidebar

The left sidebar is always available (`Ctrl+B` toggles). It has four
sections:

- **Files** — a tree of the focused pane's cwd (depth 3, honors
  `.gitignore`).
- **Claude** — per-pane context usage parsed from
  `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. Each pane is
  launched with `claude --session-id <uuid>` so we know which JSONL to
  read. Set `CCNEST_CONTEXT_WINDOW` to override the default 200 000-token
  window (for 1M-context models, set it to `1000000`).
- **Git** — the current branch plus `M / S / ?` counts for the focused
  pane's cwd.
- **Panes** — a list of every pane in the current tab, marking the active
  one and whether Claude is running.

The selection cursor is a solid white reversed block (`bg=white`,
`fg=black`, `REVERSED`), rendered over the full width of the row.

## Claude context detection

- Each pane gets a pre-generated `session_id` and is launched with
  `claude --session-id <uuid>`.
- The sidebar refreshes every 2 seconds and re-reads the JSONL.
- Usage is computed from the *last* `usage` field in the JSONL: input +
  cache_creation_input + cache_read_input + output tokens.
- If `claude --session-id` isn't supported by the user's Claude version,
  the matching JSONL is not located and the row shows
  `(no session yet)`.

## Dev

```sh
cargo check
cargo test
cargo build --release   # target/release/ccnest.exe
```

## License

MIT
