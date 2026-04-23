# ccnest

A Claude Code-aware terminal multiplexer. Windows-first. Inspired by
[Shin-sibainu/ccmux](https://github.com/Shin-sibainu/ccmux), but with
prefix-less pane creation hotkeys and a structured left sidebar that
surfaces file tree, Claude context usage, git status, and pane list.

**Docs site:** <https://mitama987.github.io/ccnest/>

## Status

v0.1 — Windows only. Rust + `ratatui` + `crossterm` + `portable-pty` + `vt100`.

## Install

Two options — both end up with a `ccnest` binary on your PATH, so you can
run it from any directory just like `ccmux`.

**Recommended (rustup users):**

```sh
git clone https://github.com/mitama987/ccnest
cd ccnest
cargo install --path .
```

This drops `ccnest.exe` into `%USERPROFILE%\.cargo\bin\`, which rustup
already puts on PATH.

**Alternative (PowerShell, installs to `~/.local/bin`):**

```powershell
git clone https://github.com/mitama987/ccnest
cd ccnest
pwsh .\scripts\install.ps1
```

Override the destination with `$env:CCNEST_INSTALL_DIR` before running
`install.ps1`.

Requires a working `claude` CLI on `PATH`. If `claude` isn't found, panes
fall back to the system shell (`%ComSpec%` / `$SHELL`) so the multiplexer
stays usable.

## Launch

From any directory:

```sh
ccnest          # use the current directory as the initial pane cwd
ccnest path\to\project
```

## Default keybindings

| Key | Action |
|-----|--------|
| `Ctrl+D` | Split the focused pane vertically (new `claude` to the right) |
| `Ctrl+E` | Split the focused pane horizontally (new `claude` below) |
| `Ctrl+T` | Open a new tab with a fresh `claude` pane |
| `Ctrl+W` | Close the focused pane (SIGTERM) |
| `Alt + ←` / `Alt + →` | Previous / next tab |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Previous / next tab (alias) |
| `F2` | Rename the current tab (Enter to commit, Esc to cancel) |
| `Ctrl + ← → ↑ ↓` | Move pane focus |
| `Ctrl+F` | Toggle the left sidebar's file tree (opens it focused, closes when already on Files) |
| `Ctrl+B` | Toggle the entire left sidebar |
| `Ctrl+1` .. `Ctrl+4` | Jump to sidebar section (Files / Claude / Git / Panes) |
| `Tab` (sidebar focused) | Cycle sidebar section |
| `↑` / `↓` / `j` / `k` (sidebar focused) | Move selection cursor |
| `Enter` (on a file row) | Open the entry in `$EDITOR` (falls back to `code`) |
| `Ctrl+Q` | Quit |
| `Shift+Tab` | Forwarded as back-tab (`CSI Z`) — drives Claude's mode cycle (default → auto-accept → plan) |
| `Shift+Enter` / `Ctrl+Enter` | Insert a newline in the prompt instead of submitting (sent as `ESC + CR`) |
| Mouse wheel | Scroll history in the pane under the cursor (3 lines per tick) |
| `Shift+PageUp` / `Shift+PageDown` | Scroll the focused pane one screen of history |
| `Shift+↑` / `Shift+↓` | Scroll the focused pane one line of history |
| any keystroke | Snaps the view back to the live tail |
| anything else | Sent to the focused pane |

### Ctrl+D and EOF

`Ctrl+D` is captured by the multiplexer before it reaches `claude`, so it
will no longer send EOF. To exit a Claude session, use `/exit` inside
Claude or `Ctrl+W` to close the pane from the outside.

### Scrollback

Each pane carries 2000 lines of history. The mouse wheel and
`Shift+PageUp` / `Shift+PageDown` / `Shift+↑↓` keys move the view back
through it; the next keystroke you type snaps the view back to the live
tail automatically. Plain `PageUp` / `PageDown` (no Shift) still pass
through to `claude` so the CLI's own paging keeps working.

## Tabs

Each tab's title is initialized from the pane's current folder name
(e.g. `ccnest` when launched from the repo root). Press `F2` to rename
the active tab — type the new title, then `Enter` to commit or `Esc` to
cancel. The cursor (`▎`) is shown inline while editing.

## Sidebar

The left sidebar is always available (`Ctrl+B` toggles the whole sidebar,
`Ctrl+F` toggles the file tree view specifically). It has four
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
