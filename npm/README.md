# ccnest-cli

npm wrapper for [ccnest](https://github.com/mitama987/ccnest) — a Claude Code-aware terminal multiplexer (Rust, TUI).

## Install

```
npm install -g ccnest-cli
```

This downloads the prebuilt `ccnest` binary for your OS/arch from the matching [GitHub Release](https://github.com/mitama987/ccnest/releases) and installs it on your `PATH` as `ccnest`.

Supported platforms:

- Windows x64
- macOS arm64 (Apple Silicon)
- macOS x64 (Intel)
- Linux x64

## Run

```
ccnest                # use cwd as the first pane's directory
ccnest path/to/project
```

Press `Ctrl+Q` to exit, `Ctrl+W` to close the focused pane. Full keybindings and docs at <https://mitama987.github.io/ccnest/>.

## Source / advanced install

Building from source via `cargo install --path .` is also supported — see the [main README](https://github.com/mitama987/ccnest#install) and the [docs site](https://mitama987.github.io/ccnest/#install).

## License

MIT
