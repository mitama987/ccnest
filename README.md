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
ccnest path\to\project --resume <session-id>=<tab name> --resume <session-id>
ccnest path\to\project --resume <session-id> --resume-prompt-file prompt.txt --resume-permission-mode bypassPermissions
```

`--resume` reopens existing Claude sessions (`claude --resume <id>`), one tab
each, for example after a reboot. The tab name after `=` is optional (the
folder name otherwise). The first session opens right away; the rest open one
per 2 seconds without switching the active tab, so parallel Claude starts do
not write `~/.claude.json` at the same time.

Three options apply to the `--resume` right before them, for a session that was
cut off mid-work (a reboot, for example) or that is handed over to a new one:

- `--resume-prompt-file <path>` sends the text of that file as the session's
  first message as soon as it is reopened (`claude --resume <id> "<text>"`), so
  it picks the work up without anyone typing. The text is folded to one line.
- `--resume-permission-mode <mode>` starts that one session in the given mode
  (`bypassPermissions`, `plan`, `acceptEdits`, ...) instead of the window's
  default (plan). `bypassPermissions` is ignored when
  `CCNEST_CLAUDE_NO_SKIP_PERMISSIONS` is set.
- `--resume-model <model>` starts that one session on the given model
  (`claude --model <model>`: an alias such as `fable` / `opus`, or a full id).
  Without it the session keeps the model it had. Also works through `--add`.

Sessions without these options just open, as before.

### Close your own tab from inside it

```sh
ccnest --close                 # closes the tab this command was typed in, after 5 seconds
ccnest --close --after 10      # after 10 seconds (0 to 600)
```

For Claude to close its own old tab after it has opened a new one. Typed in a
pane's shell (for example Claude's shell tool), `--close` closes the tab that
holds that pane, the same way as middle-clicking it: its process tree ends and
`Ctrl+Shift+T` opens it again. It returns at once; the tab closes `--after`
seconds later (the window checks on its 2 second tick, so up to 2 seconds
more), which leaves time for the last reply to show. The pane is found through
`CCNEST_PID` and `CCNEST_PANE`, which ccnest sets for everything it starts in a
pane (v0.1.51 and later); there is no `--to`, so another window's pane can not
be closed by mistake. Only that pane's tab closes, never any other. It is
refused, with a reason and exit code 1, when the window has only this one tab
(closing it would end ccnest), when the pane is not in the window any more, or
when the environment variables are missing (a window started before v0.1.51, or
a shell outside ccnest: restart that window first).

### Add tabs to a running window

```sh
ccnest --add --resume <session-id>=<tab name> --resume <session-id>
ccnest --add --to <pid> --resume <session-id>
```

`--add` does not start a new window. It asks a ccnest that is already running
to open the sessions as new tabs (same rules as above: one per 2 seconds, the
active tab stays). The target is `--to <pid>`, or the `CCNEST_PID` environment
variable, which ccnest sets for everything it starts in a pane — so from
inside a tab (for example from Claude's shell tool) plain `ccnest --add ...`
reaches that window.

The window answers within a couple of seconds and the command prints one line
per session: opened, or skipped because it is already open in that window, or
skipped because that window's folder has no conversation log for it (a tab
that would close at once). The exit code is 0 only when every requested
session ends up as a tab there. Sessions resume in the window's start folder,
so they must belong to that project. A window started before v0.1.35 has no
request folder and the command fails at once; restart that window first.

Requests are small JSON files under `%APPDATA%\ccnest\requests\<pid>\`, picked
up on the 2 second tick; the folder is removed when the window exits, and
folders of dead pids are removed the next time a ccnest starts.

## Default keybindings

| Key | Action |
|-----|--------|
| `Ctrl+D` | Split the focused pane vertically (new `claude` to the right) |
| `Ctrl+E` | Split the focused pane horizontally (new `claude` below) |
| `Ctrl+T` | Open a new tab with a fresh `claude` pane |
| `Ctrl+W` | Close the focused pane (SIGTERM) |
| `Ctrl+Shift+T` | Reopen the tab you closed last (resumes its Claude conversation; press again for older ones, up to 20). In Windows Terminal, unbind WT's own `ctrl+shift+t` first (see below) |
| `Ctrl+V` | Paste the clipboard into the focused pane: an image (screenshot, a browser's "Copy image", Paint) is handed to Claude Code as `Alt+V` so it shows an `[Image #N]` chip; text is pasted as before. In Windows Terminal, unbind WT's own `ctrl+v` first (see below); `Ctrl+Shift+V` stays WT's text paste |
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
| `Shift+Enter` / `Ctrl+Enter` / `Alt+Enter` | Insert a newline in the prompt instead of submitting (sent as `ESC + CR`). Works with or without Windows Terminal's `shift+enter → sendInput "\u001b\r"` binding (the `/terminal-setup` recipe): that binding reaches ccnest as Alt+Enter |
| Mouse wheel | Scroll history in the pane under the cursor (3 lines per tick) |
| `Ctrl` + Mouse wheel | Resize the focused split — the pane under the cursor grows on scroll-up, shrinks on scroll-down (5% per tick, clamped to 10–90%) |
| `Alt` + Left-click in a pane (`Shift` + Left-click outside Windows Terminal) | Extend the selection to the click, like Shift+click in a browser: with a selection, its end moves to the click; with none, everything from your last plain click to this click is selected. Scroll with the wheel between the two clicks to select a long reply, then `Ctrl+C` to copy. Windows Terminal keeps `Shift` + click for its own selection and never passes it to ccnest, so use `Alt` there |
| Left-click on a tab | Switch to that tab |
| Double-click on a tab (or `Alt+F` for the active tab) | Rename the tab. The current name starts out selected (blue band): type to replace it, `Backspace` clears it, `Enter` or a click anywhere else commits, `Esc` cancels. `Enter` without typing changes nothing; committing an empty name removes your name and brings the automatic one back. `Ctrl+V` and terminal paste go into the name (first line only) |
| `Alt+C` | Put the active tab on hold: it moves to the far right of the tab bar and shrinks to a grey dot and its number (`●5`), and the next tab comes to the front. It stays on hold, whatever Claude does in it, until you open it and press `Alt+C` again. If Claude is idle at an empty prompt, it is stopped to free its memory; any key typed into the held tab starts it again where it left off |
| Drag a tab left / right (left button) | Reorder the tabs — the tab floats under the pointer while you drag and the others slide aside, release to drop |
| Middle-click (wheel click) on a tab | Close that tab and every pane in it (no confirmation — same as `Ctrl+W` per pane; closing the last tab quits) |
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
(e.g. `ccnest` when launched from the repo root).

Double-click a tab to rename it (`Alt+F` does the same for the active
tab). The tab turns into an input that holds the current name, selected
as a whole (white on a blue band) with the cursor (`▎`) after it, as in
Explorer: the first character you type replaces the name, `Backspace`
clears it, and from then on keys append and `Backspace` deletes one
character. `Enter` commits, and so does a left click anywhere outside
the tab being edited (that click does nothing else); `Esc` cancels.
`Enter` without typing anything changes nothing, so the automatic name
is not frozen by accident. Committing an empty name removes the name
you gave and the automatic one comes back. `Ctrl+V` and a paste from
the terminal go into the name (first non-empty line, control
characters dropped) instead of the pane behind it. Tabs cannot be
switched, closed or reordered with the mouse while one is being
renamed.

A name you give this way is shown as `N:<name>` and wins over every
automatic name below. ccnest remembers it per Claude session in
`%APPDATA%\ccnest\tab-names.json` (for every pane of the tab; at most
500 sessions, oldest dropped first), so the same session shows the same
name after `Ctrl+Shift+T`, after closing the window, and when it is
opened again with `ccnest --resume <id>` or `ccnest --add`. It is a
ccnest-only name: Claude Code's own session name (`/rename`, the
`/resume` picker) is not changed. `CCNEST_TAB_NAMES=<path>` moves the
file.

Left-click a tab in the tab bar to switch to it. Middle-click (wheel
click) a tab to close it together with every pane it holds — browser
style, with no confirmation, the same way `Ctrl+W` closes a single pane.
Closing the last tab quits ccnest.

Tabs that are working are parked on the right. A tab whose Claude panes
are all working (🟢, replying or running something in the background) for
4 seconds moves to the right end of the tab bar and shrinks to its
marker and number (`🟢3`); tabs that need you (🟨) and shell-only tabs
stay on the left with their names, so the left side is your to-do list.
A thin divider (`│`) separates the two groups. Each group keeps the tab
order, so a tab that turns 🟨 comes back to its old place among the
named tabs, at once. A parked tab is still one click away, and the
active tab always shows its name, even while parked. Tabs only change
sides on the 2-second status tick — never at the moment you click, and
not while you drag a tab, rename one, or have just clicked one — so a
double-click never lands on a different tab. `Alt+←` / `Alt+→` and
`Ctrl+Tab` follow the order shown in the tab bar. Dragging reorders
within a group only. Tab numbers are the real tab order, so they are
not consecutive on screen. Set `CCNEST_TAB_GROUP=off` to turn this off.

Put a tab on hold with `Alt+C` when it needs you but not now. The tab
moves to the far right, behind a second divider, and shrinks to a grey
dot and its number (`●5`); the front moves on to the neighbouring tab,
as when you archive a mail. A tab on hold keeps its grey dot whatever
happens in it — it does not turn 🟢 or 🟨 and does not come back on its
own. Click it to look at it (the front tab always shows its name), and
press `Alt+C` again to take it off hold: it returns to its old place.
`Ctrl+C` is deliberately not the key: it interrupts Claude, and pressed
twice it replaces Claude with a shell. Holds are remembered per session
in `%APPDATA%\ccnest\tab-holds.json`, so a tab is still on hold after
`Ctrl+Shift+T`, after closing the window, and when its session is
opened again with `--resume` / `--add`. `CCNEST_TAB_HOLDS=<path>` moves
the file. With `CCNEST_TAB_GROUP=off` a tab on hold is drawn small in
place instead of moving.

Holding a tab also frees its memory (a Claude process is 600–850 MB).
When you press `Alt+C`, each Claude pane of the tab is stopped if — and
only if — nothing would be lost: it is not working, nothing runs in the
background (no `1 shell` / `Waiting for … to finish`), the input box is
on screen and empty (so not in the middle of a permission or question
dialog, and no half-written prompt), and no key was sent to it in the
last 5 seconds. Otherwise it is left running, and the bottom line of
the status bar tells you which of the two happened. A stopped pane
keeps its last screen: open the tab to read it, pass through it with
`Alt+←` / `Alt+→` — nothing is started. The frame title says
`保留中: Claude 停止・Alt+C で再開`. Press `Alt+C` again (it releases
the hold), or type any key into the pane (the key itself is dropped), and
Claude starts again with `claude --resume <session id>`: the
conversation is back in a few seconds. `Ctrl+C` does nothing in a
stopped pane. A held session opened with `--resume` / `--add` (for
example when the window is restored after a reboot) comes up without a
process, unless it was given a first prompt. What does not survive a
stop: the permission mode goes back to the window's default, as with
`Ctrl+Shift+T`. Set `CCNEST_HOLD_SLEEP=off` to keep every held tab
running.

Drag a tab with the left button to reorder. As soon as the pointer
leaves the column you pressed, the tab lifts off and floats under the
pointer, on top of the other tabs, leaving its slot empty. When it passes
a neighbour, that neighbour slides into the free slot (about 140 ms);
when you release, the tab glides from where it floats into its new slot.
Only the horizontal position counts, so the pointer may slip below the
tab bar while you drag. Tabs that do not fit on screen (cut off at the
right edge) cannot be reached by dragging. Set `CCNEST_TAB_ANIM=off` to
turn the motion off: the tabs then swap in place, without floating or
sliding.

When the focused pane's Claude session has a current task, the tab label
becomes `N:<task>` (truncated to 16 cells) so you can tell at a glance
what each tab is working on. The task is, in order of preference:

1. the name you gave the session (`/rename` or `claude --name`),
2. a short Japanese name for Claude Code's session title (see below),
3. the OSC window title Claude Code sets,
4. the last user message in the session JSONL.

Claude Code titles sessions in English kebab-case (`fix-user-name-issues`)
and offers no setting to change that, so ccnest asks
`claude -p --model haiku` once per title to rephrase it as a short
Japanese noun phrase of about six full-width characters that always
names what the session is about (`resume-p1-affilia-features` →
`Affilia P1`, not `続きから再開`). Titles already in Japanese are
used as is. The call loads no settings, CLAUDE.md, MCP servers or tools
(about 800 input tokens, 2–3 s), keeps no session, runs one at a time on a
background thread, and waits until `--resume` tabs have opened and the
pane's claude has been up for 10 s. Results are cached in
`%APPDATA%\ccnest\jp-titles.json`, keyed by session, title and prompt
version, so a title is only rephrased again when Claude Code changes it
(or when a new ccnest ships a different prompt).
`CCNEST_JP_TITLE=off` turns this off.

Tabs containing a Claude pane get a status marker emoji at the head of
the label. The marker is the only status signal; the label text keeps
its normal colour:

| Marker | Meaning |
|---|---|
| 🟢 | Claude is running (the spinner line with its elapsed time, `✶ Boondoggling… (6s · ↓302 tokens)` or `✢ Metamorphosing… (2m 24s · ↓ 4.4k tokens · …)`, is visible; the older `esc to interrupt` wording is still recognised) |
| 🟢 | also when the reply ended but something is still running in the background — the footer under the prompt shows `1 shell`, `2 local agents`, `1 monitor`, `3 background tasks`, …, or the line right above the prompt box reads `Waiting for 1 background agent to finish` (Claude is notified when it finishes and carries on by itself) |
| 🟨 | your turn — a question or permission prompt, or the work finished and Claude is waiting for the next instruction |
| (none) | shell-only tab (no Claude pane) |

A tab with several Claude panes shows 🟨 as soon as one of them is
waiting for you, otherwise 🟢.

A reply that ends while you are on another tab is 🟨 until you open that
tab, even when something keeps running in the background (the reply
itself is for you to read). Once you have opened it, the tab is 🟢 while
the background work runs. A reply that ends while you are watching the
tab goes straight to 🟢. The status is always judged from the bottom of
the pane, also while you have scrolled up into its history.

Background work is read from the footer (the text after the prompt box)
and from the last line above the prompt box, so a tab that keeps a dev
server in the background stays 🟢 until the server stops (v0.1.36–39
showed a separate 🔵 for this). A background agent shows no footer pill —
it is listed under the footer instead — so there the `Waiting for …
to finish` line is what counts, and only while it is the last line
above the prompt box (an older one further up is ignored). Indented
lines below it (`  ⎿ …`) and the notice Claude Code adds when it
compacts an idle session (`● Compacted while idle, …`) are stepped
over, since neither starts a new turn. Some turns end without that
line although an agent is still running; then the list under the footer
counts: a row that ends in elapsed time and token count
(`◯ general-purpose  …  2m 17s · ↓ 116.0k tokens`) means an agent is
running (the list disappears when the last agent finishes). If Claude
ends its reply with a question in plain text while a background job is
still running, the tab is 🟢 as well — unless the reply ends with a
"waiting for your decision" section: a short heading line ending in
`判断待ち` (for example `▼ ゆうさんの判断待ち`) directly followed by an
item. Then the tab is 🟨 even though background work is still running;
a first item that says none (`・今はなし`, `なし`, `特になし`, `None`)
keeps it 🟢, and so does a user message after the section (already
answered). Permission prompts hide the footer
and show 🟨. The always-on `Artifact comment monitor` and Claude Code's
own housekeeping (`saving memories`, `dreaming`) do not count.

## Claude launch flags

Panes launch `claude --session-id <uuid> --permission-mode plan
--allow-dangerously-skip-permissions` by default: the session **starts in
plan mode**, and after you approve a plan you can Shift+Tab into
bypass-permissions for fully automatic execution.

- `CCNEST_CLAUDE_PERMISSION_MODE=off` (or `bypass`) restores the legacy
  behavior: start directly in bypass via `--dangerously-skip-permissions`.
- Any other value (e.g. `acceptEdits`) is passed to `--permission-mode`
  as-is.
- `CCNEST_CLAUDE_NO_SKIP_PERMISSIONS=1` still removes every bypass flag
  (plan mode stays unless turned off as above).

## Plan usage in the status bar

The first status-bar row ends with your claude.ai plan usage, the same
numbers `/usage` shows:

```
cwd: C:\work\…\my-app │ Fable 5.1 │ ⎇ main │ 5h 42% ⏳2h10m · wk 18% · Fable 7% ⏳2d9h
```

- `5h` = current session (5-hour window), `wk` = current week (all models),
  then one entry per model-specific weekly limit (e.g. `Fable`).
- Colors: sky below 70%, yellow from 70%, bold red from 90% (by the
  highest of the numbers).
- `⏳` is the countdown to that limit's reset: minutes under an hour
  (`59m`), hours and minutes under a day (`2h10m`), days and hours beyond
  (`2d9h`). Adjacent limits that reset at the same time (the weekly ones
  usually do) share one countdown at the end of the group. A limit whose
  reset time has passed shows `0%` with no countdown until the next poll.
- When the row is too narrow, the cwd and then the branch shrink first;
  then the countdowns are dropped (the %-only text stays), and finally the
  usage part is dropped whole before the model name.
- ccnest asks `https://api.anthropic.com/api/oauth/usage` every 5 minutes
  on a background thread, using the access token Claude Code saved in
  `~/.claude/.credentials.json` (or `$CLAUDE_CONFIG_DIR`). It only reads
  the token and **never refreshes it**; when the token has expired it waits
  for Claude Code to renew it. Results are shared between ccnest windows
  through `%APPDATA%\ccnest\usage-cache.json`, so several windows still
  make one request per interval.
- **This endpoint is undocumented** (it is what Claude Code's `/usage`
  calls). If it changes or fails, the usage part simply disappears (values
  older than 30 minutes are hidden); nothing else in ccnest is affected.
  On macOS the token lives in the Keychain, so the usage part stays hidden.
- `CCNEST_USAGE=off` turns the feature off (no requests at all).
  `CCNEST_USAGE_POLL_SECS` changes the interval (default 300, minimum 60).

## Sidebar

The left sidebar is always available (`Ctrl+B` toggles the whole sidebar,
`Ctrl+F` toggles the file tree view specifically). It has four
sections:

- **Files** — a tree of the focused pane's cwd (depth 3, honors
  `.gitignore`) with type-specific icons and colors for faster scanning.
- **Claude** — per-pane context usage parsed from
  `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. Each pane is
  launched with `claude --session-id <uuid>` so we know which JSONL to
  read. Set `CCNEST_CONTEXT_WINDOW` to override the default 200 000-token
  window (for 1M-context models, set it to `1000000`).
- **Git** — the current branch plus `M / S / ?` counts for the focused
  pane's cwd.
- **Panes** — a list of every pane in the current tab, marking the active
  one and whether Claude is running. When a task is known the row shows
  `— <task>` in full; Claude rows start with the same 🟢 / 🟨 marker as
  the tab bar.

The selection cursor is a solid white reversed block (`bg=white`,
`fg=black`, `REVERSED`), rendered over the full width of the row.

## Claude context detection

- Each pane gets a pre-generated `session_id` and is launched with
  `claude --session-id <uuid>` (`claude --resume <uuid>` for `--resume` tabs, same id).
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

## Version History

ver0.1 - 2026-04-25 - Documented iconized and colorized Files sidebar rows.
ver0.1.4 - 2026-05-16 - Disabled host alternate-scroll (DECSET 1007) and added stall-robust wheel-budget phantom-arrow suppression so the mouse wheel no longer recalls Claude prompt history.
ver0.1.5 - 2026-05-16 - Reverted the host alternate-scroll disable (DECSET 1007) that broke wheel scrolling on Windows Terminal; kept the wheel-budget phantom-arrow suppression and added an opt-in input trace (CCNEST_INPUT_TRACE) for diagnostics.
ver0.1.6 - 2026-05-21 - Kept vertical mouse wheel scrolling local to ccnest even when Claude Code enables mouse reporting, and extended phantom-arrow suppression across heavy render stalls.
ver0.1.7 - 2026-08-11 - Tab labels show the focused pane's current task (`N:<task>`, OSC title with JSONL fallback) with 4-state status colors (busy/attention/done-unseen/idle); the Panes sidebar shows the full task. Claude now launches in plan mode by default (`--permission-mode plan --allow-dangerously-skip-permissions`); `CCNEST_CLAUDE_PERMISSION_MODE=off` restores the legacy bypass start.
ver0.1.8 - 2026-08-11 - Tab labels get a status marker emoji (🟢 running / 🟡 your turn = idle + permission prompts / 🟣 done-unseen) on Claude tabs — color alone was hard to spot in practice. Idle is treated as "your turn" (yellow) per user feedback.
ver0.1.9 - 2026-08-11 - Status markers switched to the square set (🟩 running / 🟨 your turn / 🟪 done-unseen) per user preference.
ver0.1.10 - 2026-08-11 - Fixed a regressed wheel bug where a notch could reopen Claude prompt history: the deferred phantom-arrow flush now runs only immediately after the input queue is drained, so a stalled loop iteration can no longer flush past an unread paired wheel event. The 2s tick no longer blocks on the pane parser lock (try_lock, skip on contention), git branch discovery is cached with a 10s TTL, and the sidebar's full git status walk is skipped while the sidebar is hidden. CCNEST_INPUT_TRACE now logs pending-arrow flushes (`pending_arrow_flush`) so leaks are machine-checkable.
ver0.1.11 - 2026-09-06 - Fixed sluggish, choppy typing in Claude panes. The event loop is now woken by PTY output (input pump thread + per-pane output notifications over one mpsc channel) instead of redrawing only when a 30 ms `event::poll` timed out (31–47 ms on Windows' default 15.6 ms timer), and it redraws only when something changed (output frames coalesced at 8 ms, input frames immediate). The 5 ms paste-detection wait (really ~16 ms) is no longer paid for single keystrokes — only when a drain holds two or more presses or a bracketed paste. Per-frame `ResizePseudoConsole` + vt100 `set_size` are replaced by a size-change check after each draw; the pane renderer reuses one scratch string instead of allocating per cell; stdout is wrapped in a 1 MB BufWriter. New opt-in `CCNEST_LATENCY_TRACE=1` writes per-keystroke `key_write->output / output->draw / burst_wait` timings (plus `draw` / `output` event lines) to `%APPDATA%\ccnest\latency-trace.log`, and `CCNEST_PTY_DUMP=1` records the raw bytes received from each pane's ConPTY in `pty-dump.log`. Measured with 30 injected keystrokes (SendInput, 60 ms hold, 120 ms gap): Claude pane key→screen p50/p90/p99 14.6/46.5/55.8 ms → 9.1/12.0/23.7 ms (≈7 ms of that is Claude Code's own render); cmd.exe pane 14.4/46.7/47.4 ms → 1.3/1.7/2.0 ms; idle CPU 1.6–2.5% → 0.0–0.3%.
ver0.1.12 - 2026-09-07 - Typing while Claude Code is busy: the echo of a keystroke no longer waits for the output-frame cap (a pending-echo window exempts the first frame after a key write), a batch of pointer moves no longer forces a redraw, and pointer moves are only forwarded to a mouse-tracking child when the cell changes. Measured with 19 injected keys per phase (scripts/latency): with the pointer moving, redraws over the same input dropped 376 -> 151 and key->screen p90 22 -> 17 ms; under a busy Claude pane (40 s of live tool output) ccnest's own share stayed at 2 ms p50 / 3-6 ms p90 while the p90 rise to ~28 ms came from Claude Code's own render. New knobs CCNEST_MIN_OUTPUT_FRAME_MS / CCNEST_IDLE_WAIT_MS / CCNEST_ECHO_WINDOW_MS bisect the coalescing constants without a rebuild, and terminal-identity variables (WT_SESSION, TERM_PROGRAM, ...) are no longer leaked into the child.
ver0.1.13 - 2026-09-26 - Middle-click (wheel click) on a tab in the tab bar closes that tab and all of its split panes, browser-style, with no confirmation (like Ctrl+W); closing the last tab quits. The tab hit-test is a pure function shared with left-click switching, and the drawn tab rectangles are invalidated the moment a tab closes so a second click in the same input batch cannot hit a stale index. Input queued behind a quit (Ctrl+Q or the last tab closing) is no longer processed, removing a latent out-of-range panic on tab lookups. CCNEST_INPUT_TRACE logs each close as `close_tab idx=N tabs_left=M`.
ver0.1.14 - 2026-09-26 - Shell panes (cmd.exe, including a pane restarted as a shell with Ctrl+C twice) are drawn without the pane frame and the " [id] command " title row, so they look like a plain console; Claude panes keep the orange/gray frame.
ver0.1.15 - 2026-09-27 - The Claude pane's frame title now shows the pane's folder name (" [1] 30_XTP3 ") instead of the spawned executable name (the CCNEST_CLAUDE_BIN shim, "ccnest-claude-launcher.exe"); the Panes sidebar row still lists the command.
ver0.1.16 - 2026-09-27 - One blank row between the tab bar and the pane area, so the orange top border and its folder-name title sit one row lower instead of directly under the tab row.
ver0.1.17 - 2026-09-27 - The Claude pane frame is drawn with thin one-eighth block lines (McGugan box, `▔ ▕ ▏ ▁`) instead of box-drawing characters: the top edge sits on the top of its row, half a row higher than before, with the folder-name title right below it.
ver0.1.18 - 2026-09-27 - Reverted 0.1.16 and 0.1.17 after trying them: no blank row under the tab bar and the plain box-drawing pane frame again (the 0.1.15 look, keeping the folder-name title).
ver0.1.19 - 2026-10-04 - Copying a long path or URL that Claude Code wrapped onto the next line no longer inserts a newline: when a row ends at the right edge with a path-like word and the next row continues it, ccnest joins them and drops the continuation indent (Ctrl+C and the right-click Copy). List items, a new path on the next row, rule lines and ordinary word-wrapped prose keep their line breaks.
ver0.1.20 - 2026-10-06 - The status bar shows claude.ai plan usage after the branch: `5h 42% · wk 18% · Fable 7%` (session, week, and each model-specific weekly limit), colored yellow from 70% and red from 90%. A background thread polls the undocumented `/api/oauth/usage` endpoint (the one `/usage` uses) every 5 minutes with the token from `~/.claude/.credentials.json`, never refreshing it, and shares the result across windows via `%APPDATA%\ccnest\usage-cache.json`. `CCNEST_USAGE=off` disables it; `CCNEST_USAGE_POLL_SECS` changes the interval.
ver0.1.21 - 2026-10-10 - Copying a long unbroken string (such as a token list) that Claude Code wrapped exactly at the last column no longer keeps the continuation row's indent spaces: when ConPTY continues such a row through the terminal's own auto-wrap instead of a cursor move, the next row's leading spaces are recognized as Claude Code's hanging indent (same width as the paragraph's body position) and dropped, so the copy is one unbroken line. Plain shell soft wraps, list items and rows whose indent differs keep their spaces.
ver0.1.22 - 2026-10-10 - The status bar's plan usage shows the countdown to each limit's reset: `5h 42% ⏳2h10m · wk 18% · Fable 7% ⏳2d9h` (minutes under an hour, hours+minutes under a day, days+hours beyond; weekly limits that reset at the same time share one countdown). When the row is too narrow the countdowns go first and the %-only text stays, then the whole usage part as before.
ver0.1.23 - 2026-10-19 - `ccnest <dir> --resume <session-id>[=<tab name>] ...` reopens saved Claude sessions one tab each (`claude --resume <id>` instead of a new `--session-id`). The first opens at startup, the rest one per 2 s tick without stealing the active tab. Unknown options now exit with an error instead of starting.
ver0.1.24 - 2026-10-21 - Closing a tab (Ctrl+W, middle-click), restarting a pane as a shell, or quitting ccnest now terminates the whole child process tree: each pane's child is placed in a Windows Job Object with kill-on-close, so the claude.exe behind the `CCNEST_CLAUDE_BIN` shim no longer survives as an orphan (closed tabs had left 19 of them, about 5.5 GB). If the Job cannot be created, the descendants are enumerated and terminated one by one.
ver0.1.25 - 2026-10-24 - Tab labels are short Japanese names: Claude Code's English kebab-case session title (`ai-title`) is rephrased once by `claude -p --model haiku` on a background thread and cached in `%APPDATA%\ccnest\jp-titles.json`. A name given with `/rename` wins; the English OSC title is shown until the Japanese one is ready. `CCNEST_JP_TITLE=off` disables it.
ver0.1.26 - 2026-10-25 - The window title (the name on the taskbar) is the start folder name, e.g. `ClaudeCompany`, instead of the launching shell's `C:\Windows\System32\cmd.exe`, so several ccnest windows can be told apart.
ver0.1.30 - 2026-11-03 - `Ctrl+V` pastes the clipboard through ccnest: when the clipboard holds an image (CF_DIB/CF_BITMAP: a Win+Shift+S screenshot, a browser's right-click "Copy image", Paint), ccnest sends Claude Code `ESC v` (its Windows `Alt+V` image paste) so an `[Image #N]` chip appears; text is bracketed-pasted as before; an image wins when both are present; a file copied in Explorer does nothing (drag it in instead). The right-click Paste uses the same rule. Windows Terminal binds `ctrl+v` to its own text paste (so an image never reached ccnest); unbind it with `{ "id": null, "keys": "ctrl+v" }` in WT's `keybindings` (`ctrl+shift+v` remains WT's text paste).
ver0.1.29 - 2026-10-31 - `Ctrl+Shift+T` reopens the tab you closed last, like a browser: closing a tab (Ctrl+W on its last pane, or middle-click) remembers its name, position, split layout and each pane's Claude session; reopening puts it back at the same position with the same splits and starts each pane with `claude --resume <id>` when the conversation log exists (a fresh session otherwise). Up to 20 closed tabs are kept, newest first. Windows Terminal binds `ctrl+shift+t` to its own new tab by default; unbind it by adding `{ "id": null, "keys": "ctrl+shift+t" }` to `keybindings` in WT's settings.json.
ver0.1.28 - 2026-10-31 - Ctrl+click on a URL that Claude Code wrapped at the right edge opens the whole URL: the click target now joins rows with the same rule as copy (Claude Code's own line break with a hanging indent, and the terminal auto-wrap with the indent written as spaces), so clicking either row of `https://docs.google.com/...` opens the full link instead of the first row's part. A URL also ends at the first character that cannot appear in a URL (Japanese text, spaces), so `.../edit)・リンク` opens `.../edit`. Windows Terminal's own hover tooltip still shows only the first row (it reads the screen itself). Dev: `CCNEST_OPEN_URL_LOG=<path>` appends the URL to `<path>` instead of opening a browser.
ver0.1.27 - 2026-10-27 - Copying Claude Code's Japanese replies pastes as plain paragraphs: rows that Claude Code wrapped at the right edge are joined with no newline and no space when the break falls next to a wide (CJK / full-width) character and the next row continues at the paragraph's body column (also when ConPTY continued through the terminal's auto-wrap), so Japanese lines with ASCII words such as `（@sx09s のポスト 20 件を確認）` no longer break. Rows starting a list item (`- `, `1. `, `・`, `1）`, `①` ...) or with a different indent keep their newline, and English word-wrapped prose is unchanged. The copy then drops the indent shared by every non-empty line (Claude Code's 2-column gutter, and the reply's leading `● ` when the selection starts at column 0); blank lines between paragraphs and the relative indent of nested lists stay, and a selection that starts mid-row leaves its first line as is. Dev: `CCNEST_PTY_DUMP=<path>` appends the raw ConPTY output bytes to `<path>` (`CCNEST_PTY_DUMP=1` keeps the escaped `pty-dump.log`).
ver0.1.31 - 2026-11-08 - Two-state status markers: 🟢 (green circle) while Claude is running, 🟧 (orange square) whenever it is your turn — a question or permission prompt, or the work finished and Claude waits for the next instruction. The 🟨 idle / 🟪 done-unseen distinction and the per-status tinting of the tab label text are gone (too many colour patterns to read at a glance); the sidebar Panes rows carry the same marker. Running is now detected from the spinner's elapsed-time suffix `…(6s · …)`: Claude Code 2.1.319 no longer prints `esc to interrupt` on the spinner line, so since that change every tab had looked idle (🟨) even while Claude was working. Pane frame colours and the status bar are unchanged. E2E: `scripts/e2e/status-marker.ps1`.
ver0.1.32 - 2026-11-08 - Shift+Enter inserts a newline in the Claude prompt again when ccnest runs inside Windows Terminal with the usual `shift+enter → sendInput "\u001b\r"` binding: ConPTY turns that ESC CR into an Alt+Enter key event, which ccnest used to forward as a bare CR (submitting the prompt). Alt+Enter is now sent as `ESC CR` like Shift+Enter / Ctrl+Enter. Measured with `scripts/e2e/conpty-input-probe.py`; E2E `scripts/e2e/shift-enter-newline.ps1`.
ver0.1.33 - 2026-11-09 - The "your turn" tab marker is 🟨 (yellow square, the colour used up to v0.1.30) instead of 🟧; meaning and detection are unchanged.
ver0.1.34 - 2026-11-09 - Running detection fixed: the spinner line is `… (2m 24s · …)` — a space between `…` and `(`, and minutes/hours once the reply passes 60 s / 1 h — so the v0.1.31 matcher (`…(` glued, seconds only) never fired and every working tab stayed 🟨. `has_spinner_timer` now allows the space and `<n>h <n>m <n>s`. E2E `scripts/e2e/status-marker.ps1` uses the same pattern; `scripts/e2e/status-marker-wt.ps1` screenshots a disposable ccnest in a new Windows Terminal window (PrintWindow) so the emoji itself can be checked.
ver0.1.35 - 2026-11-11 - `ccnest --add [--to <pid>] --resume <session-id>[=<tab name>] ...` adds tabs to a ccnest window that is already running instead of starting a new one. The request is a small JSON file dropped into `%APPDATA%\ccnest\requests\<pid>\`; the window picks it up on its 2 s tick, queues the sessions like startup `--resume` (one tab per tick, active tab untouched) and answers which were opened and which were skipped (already open there, or no conversation log in that window's folder). Panes now get `CCNEST_PID=<pid of their ccnest>`, so the command needs no `--to` when run from inside a tab. E2E `scripts/e2e/add-tab.ps1`.
ver0.1.36 - 2026-11-14 - Third tab marker 🔵: the reply has ended but something is still running in the background. When a reply ends the spinner line disappears, so a tab waiting on a background shell or agent used to show 🟨 (your turn) although nothing was needed from you. ccnest now reads the footer under the prompt box (the text after the last `─` rule) and shows 🔵 when it carries a background pill — `1 shell`, `2 local agents`, `1 shell, 1 monitor`, `3 background tasks`, `1 team`, `1 background dynamic workflow`, `1 MCP task`, `◇ 1 cloud session` (wording from Claude Code v2.1.325). The done line `… · 1 shell still running` is not used: it is transcript text and can stay on screen after the job has finished. A tab folds its panes as 🟨 > 🟢 > 🔵. A separate marker rather than 🟢 so that a tab that merely keeps a dev server in the background is not mistaken for one that is working (user decision). Dev aid `CCNEST_STATUS_TRACE=<path>` writes each Claude pane's status and the screen text it was judged from on every 2 s tick; the test strings come from it. E2E `scripts/e2e/background-marker.ps1`.
ver0.1.37 - 2026-11-15 - Drag a tab to reorder. Press a tab with the left button (it becomes the active tab, as before) and move left or right: the tab follows the pointer, the tabs in between shift by one, and releasing drops it. Only the pointer column is used, so the drag keeps working when the pointer slips down onto a pane, and a mouse-mode app in that pane (Claude Code) never receives the stray drag / release. A tab moves only once the pointer is on the neighbour and the moved tab would end up under the pointer, so a narrow tab dragged across a wide one (or the reverse) does not flip back and forth. Dragging past either end of the visible tabs moves the tab to the first / last visible position; tabs cut off at the right edge cannot be reached. Nothing happens while a tab is being renamed. CCNEST_INPUT_TRACE logs each move as `move_tab from=A to=B`. E2E `scripts/e2e/drag-reorder-tab.ps1`.
ver0.1.38 - 2026-11-15 - Floating tab drag. While a tab is dragged it now lifts off and floats under the pointer (drawn on top of the other tabs, its slot left empty) instead of jumping from slot to slot; a neighbour it passes slides into the free slot and, on release, the tab glides from where it floats into its slot (140 ms, ease-out, one cell at a time). A plain click does not lift the tab. The floating tab keeps the white band of the active tab (an orange band hid the 🟨 marker). The reorder rule and the hit-test rectangles are unchanged: they use the slots, never the animated positions. While a slide is running the loop redraws about every 16 ms and then goes back to sleep. `CCNEST_TAB_ANIM=off` disables the motion. The tab bar is now drawn tab by tab instead of as one text line. E2E `scripts/e2e/drag-reorder-tab.ps1` reads the tab bar with the button still down (`-Wt` takes Windows Terminal screenshots mid-drag, `-NoAnim` checks the off switch).
ver0.1.39 - 2026-11-15 - 🔵 also while Claude waits for a background agent. A background agent puts no pill in the footer (it is listed under the footer as `◯ general-purpose …`), so a tab whose reply had ended with `✻ Waiting for 1 background agent to finish` showed 🟨. ccnest now also shows 🔵 when the last non-empty line above the prompt box is `Waiting for <n> background agent(s) [and <n> dynamic workflow(s)] to finish` (wording from Claude Code v2.1.325). The line is transcript text that stays on screen, so it only counts while nothing has been printed below it; once the agent finishes Claude starts its next turn and the tab goes back to 🟢 / 🟨. The prompt box is now found as the last run of three or more `─`, so a stray `─` in the list under the footer no longer hides it. E2E `scripts/e2e/background-marker.ps1 -Agent`.
ver0.1.40 - 2026-11-15 - Background work shows 🟢 instead of 🔵: a tab whose reply has ended but which still has a background shell, monitor or agent running now carries the same green circle as a tab that is replying (user decision — two markers, 🟢 working / 🟨 your turn). Detection is unchanged and still tells the two apart internally (`CCNEST_STATUS_TRACE` prints `Busy` / `Background`); only the emoji is shared.
ver0.1.41 - 2026-11-15 - Shorter tab names that say what the tab is about. The tab label is cut at 16 cells instead of 20, and the Japanese name ccnest asks Haiku for is now about six full-width characters (was eight) and must name the subject — the product or proper noun in the session title — instead of only the action: `resume-p1-affilia-features` used to become `続きから再開` ("resume where we left off") because the last request "続きから再開して" drowned out the title, and is now `Affilia P1`. Empty requests such as "続き" / "進めて" are ignored and long katakana words are rephrased (`リダイレクト` → `転送`). Names cached by an older prompt are regenerated once (the cache key carries a prompt version). `scripts/dev/try_title_prompt.py` runs the prompt from the source against sample titles the way ccnest does.
ver0.1.42 - 2026-11-15 - 🟨 when a decision is pending, even with background work. A reply that ends with a "waiting for your decision" section — a short heading line ending in `判断待ち` (`▼ ゆうさんの判断待ち`) directly followed by an item — now marks the tab 🟨 although a background shell or agent is still running (it used to stay 🟢, so a tab that needed an answer looked busy). The section's first item decides: `・今はなし` / `なし` / `特になし` / `None` means nothing to decide and the tab stays 🟢; an empty section, a bullet line that merely mentions the word, or a user message after the section (already answered) do not count. This reads the shape of the report, not question wording, so replies that do not use such a section behave as before. The status is now judged from the screen taken row by row instead of vt100's `contents()`, which glues a row that is filled to the last column onto the next one (a heading and its first item arrived as one line). E2E `scripts/e2e/background-marker.ps1 -Decision`.
ver0.1.43 - 2026-11-18 - 🟢 stays on after an idle compaction. When a session is left idle, Claude Code compacts it without starting a turn and adds lines below the `✻ Waiting for 1 dynamic workflow to finish` line: indented child lines (`  ⎿  Referenced file …`, `  ⎿  Skills restored …`) and the notice `● Compacted while idle, before the prompt cache expired · turn off with "idleCompaction": false in settings.json`. The waiting line was no longer the last line above the prompt box, so a tab whose workflow was still running fell back to 🟨. ccnest now looks at the last line that starts at column 0 (turn markers `●` / `✻` / `❯` always do; child lines and wrapped continuations are indented) and steps over the idle-compaction notice (also `Compacted using a summary written while you were away`). A new turn below the notice still gives 🟨. E2E `scripts/e2e/idle-compaction-waiting.ps1` (a fake claude draws the screen; transcribed from a screenshot).
ver0.1.44 - 2026-11-18 - Extend the selection to the click. `Alt` + left-click (or `Shift` + left-click in terminals that pass it on) selects from the existing selection, or from your last plain click when nothing is selected, to the clicked cell, so a long Claude Code reply can be selected with click, wheel scroll, Alt+click instead of dragging through it. Clicking inside a selection moves its end; clicking outside grows it without dropping what was selected; keep the button down to drag the end. The start point is stored in buffer coordinates, so it stays on the same text while the history scrolls, and it is dropped when the pane is resized, the app enters or leaves the alternate screen, or a fullscreen (alternate-screen) app is scrolled with the wheel. Windows Terminal keeps Shift+click for its own selection and does not send it to the app (checked with real pointer input: Alt+click arrives with the ALT modifier, Shift+click does not arrive), which is why Alt is the trigger there. In a pane whose app turned mouse reporting on, Alt + left button is handled by ccnest and not forwarded. `CCNEST_INPUT_TRACE` now shows mouse modifiers (`Mouse(Down(Left)@31,7,KeyModifiers(ALT))`) and logs `extend_selection pane=N anchor=.. cursor=..`. E2E `scripts/e2e/click-extend-selection.ps1` (`-MouseMode` for a mouse-reporting pane) and `scripts/e2e/click-extend-selection-wt.ps1` (real Windows Terminal).
ver0.1.45 - 2026-11-21 - Double-click a tab to rename it. Two left presses on the same tab within 500 ms (with no reorder in between) open the name input on that tab; the first press still switches to the tab at once. The input works like Explorer's rename: the current name starts out selected (white on a blue band), the first key replaces it, `Backspace` clears it, `Enter` or a left click outside the tab commits, `Esc` cancels, `Enter` without typing changes nothing, and committing an empty name removes the name and brings the automatic one back. `Ctrl+V` and terminal paste now go into the name (first line) instead of the pane behind it. Fixes `Alt+F` rename, which until now only changed the folder-name title and so was invisible on any tab running Claude: a name given by hand is now shown as `N:<name>` ahead of the `/rename` name, the Japanese name, the OSC title and the last message. The name is stored per session id in `%APPDATA%\ccnest\tab-names.json` (all panes of the tab; 500 entries max) and comes back after Ctrl+Shift+T, after the window is closed, and with `--resume` / `--add`; Claude Code's own session name is not touched. `CCNEST_TAB_NAMES` overrides the path. The README said `F2` renames a tab; it is `Alt+F`. CCNEST_INPUT_TRACE logs `begin_rename_tab idx=N` and `rename_tab idx=N name=...`. E2E `scripts/e2e/dblclick-rename-tab.ps1` (console input) and `dblclick-rename-tab-wt.ps1` (real pointer on Windows Terminal).
ver0.1.46 - 2026-11-21 - Reopen a session and tell it to continue. `--resume-prompt-file <path>` after a `--resume` sends the file's text as that session's first message (`claude --resume <id> "<text>"`, the text folded to one line), and `--resume-permission-mode <mode>` starts that one session in the given mode (`bypassPermissions` starts in bypass like `CCNEST_CLAUDE_PERMISSION_MODE=off`; other modes go to `--permission-mode`; `CCNEST_CLAUDE_NO_SKIP_PERMISSIONS` still wins). Both apply only to the `--resume` right before them, also through `--add`; sessions without them open exactly as before. Made for reopening the sessions a Windows Update restart cut off, in the mode they were in. Misuse (option before any `--resume`, unreadable or empty file, a mode that is not a word) exits with an error before a window opens. E2E `scripts/e2e/resume-prompt.ps1` (fake claude: the prompt arrives as one argument) and `scripts/e2e/resume-prompt-real.ps1` (real claude on a light model: the prompt is sent, answered, and the session runs in bypassPermissions).
ver0.1.47 - 2026-11-21 - Working tabs are parked small on the right. A tab that has been 🟢 (working, or running something in the background) for 4 seconds moves to the right end of the tab bar and is drawn as marker + number only (`🟢3`); tabs that need you (🟨) and shell-only tabs stay on the left with their names, behind a `│` divider (user request: "I only look at the orange tabs — gather them on the left, the green ones just need to be small"). A tab that turns 🟨 moves back at once, to its place in the tab order. The active tab always shows its name, also while parked, so clicking a small tab expands it in place. Tabs change sides only on the 2 s tick, and not while a tab is dragged or renamed or within 600 ms of a tab click, so the tab bar never shifts under a click or a double-click. Next / previous tab (`Alt+←→`, `Ctrl+Tab`) follow the tab-bar order; dragging reorders within a group. `CCNEST_TAB_GROUP=off` restores the old layout. E2E `scripts/e2e/park-working-tabs.ps1` (`-Off`, `-Wt`).
ver0.1.48 - 2026-11-21 - Put a tab on hold with `Alt+C`. The active tab moves to the far right of the tab bar, behind a second divider, and shrinks to a grey dot and its number (`●5`); the front moves on to the neighbouring tab (right neighbour in the same group, else the left one, else the nearest tab that is not on hold). A tab on hold ignores its status: it stays a grey dot while Claude works or finishes in it, until you open it and press `Alt+C` again, which puts it back at its old place. The front tab always shows its name, so a held tab you click reads `● 5:name`. `Ctrl+C` is not used for this on purpose (it interrupts Claude; twice replaces it with a shell). Holds are stored per session id in `%APPDATA%\ccnest\tab-holds.json` (all panes of the tab; 500 entries max) and come back after Ctrl+Shift+T, after the window is closed, and with `--resume` / `--add`. `CCNEST_TAB_HOLDS` overrides the path. Dragging still reorders within a group only; with `CCNEST_TAB_GROUP=off` a held tab is drawn small in place. CCNEST_INPUT_TRACE logs `hold_tab idx=N held=true|false active=M`. E2E `scripts/e2e/hold-tab.ps1` (`-Off`, `-Wt`). The tab E2E scripts (`hold-tab`, `park-working-tabs`, `drag-reorder-tab`, `dblclick-rename-tab`, `reopen-tab`) now start their throw-away ccnest with no console window at all (CREATE_NO_WINDOW), so a run shows nothing and never takes the keyboard focus.
ver0.1.49 - 2026-11-21 - Holding a tab frees its memory. `Alt+C` now also stops the Claude of each pane in the tab when that loses nothing: the pane is not working, has no background work (footer pills such as `1 shell`, or `Waiting for … to finish`, even when the tab shows 🟨 because a decision is pending), shows an empty input box (no permission / question dialog, no half-written prompt) and was not sent a key in the last 5 seconds. The screen is read at that moment, not from the 2-second status cache. Anything else is left running. The bottom line of the status bar says which happened for 8 seconds. A stopped pane keeps its last screen and its frame title says so; opening the tab or passing through it with Alt+←/→ starts nothing. A key typed into the pane (dropped), or releasing the hold, starts Claude again with `--resume <session id>` in the same pane; `Ctrl+C` is ignored there. A held session opened with `--resume` / `--add` and no first prompt starts without a process. `CCNEST_HOLD_SLEEP=off` keeps held tabs running. Measured with the real Claude Code 2.1.330: 650 MB freed for one tab, about 1 MB (the pane's pseudo console host) left. CCNEST_INPUT_TRACE logs `hold_tab … stopped=N kept=N woken=N` and `wake_pane id=N woke=..`. E2E `scripts/e2e/hold-sleep.ps1` (`-Off`; fake claude that draws Claude Code's screen shapes) and `hold-sleep-real.ps1` (the real claude, once).
ver0.1.50 - 2026-11-21 - The hints for a stopped (held) pane name the key that wakes it: the frame title says `保留中: Claude 停止・Alt+C で再開`, and the status-bar notice says `もう一度 Alt+C で続きから再開`. Behaviour is unchanged (`Alt+C` releases the hold and wakes; typing into the pane also wakes).
ver0.1.51 - 2026-11-23 - `ccnest --close [--after <seconds>]` closes the tab of the pane the command was typed in, so Claude can close its own old tab after opening a new one with `--add`. ccnest now gives every pane's children `CCNEST_PANE=<pane id>` next to `CCNEST_PID`; the command sends a close request (same request-file channel as `--add`) and returns at once, and the window closes the tab `--after` seconds later (default 5, 0 to 600; checked on the 2 s tick) with `close_tab`, so the process tree ends and `Ctrl+Shift+T` reopens it. Only the tab that holds the pane closes; the window's last tab, an unknown pane and missing `CCNEST_PANE` / `CCNEST_PID` (windows before v0.1.51) are refused at once with exit code 1. A second `--close` for the same pane replaces the deadline. E2E `scripts/e2e/close-tab.ps1` (no console window).
ver0.1.52 - 2026-11-23 - `--resume-model <model>` after a `--resume` starts that one session on the given model (`claude --resume <id> ... --model <model>`, before the first message if there is one), also through `--add`. For handing work over to a new session on a chosen model (Fable, or Opus when Fable is limited): `ccnest --add --resume <id>=<name> --resume-model fable`. Without it the session keeps the model it had (ccnest never passed a model before, so a handed-over tab started on whatever the resumed session or the settings default was). Misuse (before any `--resume`, empty, a leading `-`, spaces or other symbols) exits with an error before a window opens. E2E `scripts/e2e/add-tab.ps1` checks that `--model fable` reaches only the session that asked. `add-tab.ps1` now starts its throw-away ccnest with no console window (CREATE_NO_WINDOW) like the other tab E2E scripts.
ver0.1.53 - 2026-11-23 - Two tab-colour fixes. (1) A pane scrolled up into its history kept the status it had when it was last at the bottom, so a tab that was replying while you scrolled stayed 🟢 after the reply ended and only turned 🟨 once you scrolled back down. The status is now judged from the bottom screen on every tick (the scroll position is restored within the same lock, so nothing moves on screen). (2) A reply that ends with a background shell, monitor or agent still running showed 🟢 at once, although the reply itself was waiting to be read. Now, if the reply ends while you are on another tab, that tab is 🟨 until you open it; after that it is 🟢 while the background work runs. A reply that ends while you watch the tab goes straight to 🟢 (user decision: "orange until I've seen it"). `CCNEST_STATUS_TRACE` now prints `status` (shown), `detected` (read from the screen), `unseen` and `scroll`. E2E `scripts/e2e/unseen-turn-end.ps1` (fake claude `turn-end-shim.ps1`, no window); the inner input helper of `dblclick-rename-tab.ps1` gained `wheelup:c,r,count`.
ver0.1.54 - 2026-11-24 - A tab stayed 🟨 although a background agent was still running: some turns end without the `Waiting for … to finish` line above the prompt box (seen with Claude Code v2.1.331), and a background agent shows no footer pill, so nothing said "background". ccnest now also reads the agent list under the footer: a row ending in elapsed time and token count (`◯ general-purpose  …  2m 17s · ↓ 116.0k tokens`) means an agent is running, and the tab is 🟢 (the list disappears when the agent finishes; a "waiting for your decision" section and the "orange until seen" rule still win). The same row now also keeps a held tab's Claude alive: before, `Alt+C` on such a tab stopped Claude together with its background agent. E2E `scripts/e2e/agent-list-waiting.ps1` (fake claude, no window: verdict and hold) and `scripts/e2e/agent-list-real.ps1` (real claude, no window: captures the screens the tests are built from).
