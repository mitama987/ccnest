# File Tree Icons And Colors

## Purpose

The Files sidebar should be scannable at terminal speed. Directory rows and common file types now carry a small icon and a distinct color, similar to modern terminal file explorers.

## Behavior

- Directories keep a folder icon and yellow emphasis.
- Git control files such as `.gitignore` use a git icon and warm warning color.
- Markdown, image, Rust, Python, JavaScript, TypeScript, web, JSON, config, shell, lock, dotfile, text, and generic files each get a stable visual category.
- The active sidebar cursor still overrides row colors with the existing reversed white selection block.
- The source of truth for classification lives in `src/sidebar/filetree.rs`; `src/ui/mod.rs` only turns that metadata into styled `ratatui` spans.

## Test Strategy

Run:

```powershell
cargo test filetree
cargo test
```

The focused tests verify icon, indent, directory suffix, and common file kind classification.

## Version History

ver0.1 - 2026-04-25 - Documented file tree icon and color classification behavior.
