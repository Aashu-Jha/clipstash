# ClipStash

A featherweight macOS menu bar clipboard manager, written in Rust.

Target: single-digit-MB idle RAM and ~0% idle CPU — the numbers you'd expect
from a native tool, not a 500 MB Electron app.

> **Status:** early work-in-progress. Storage, clipboard watcher, and hotkey
> layers are implemented. The custom popover UI (ClipVault-style) is
> scaffolded and actively being built out.

## Why

Existing clipboard managers on macOS are surprisingly heavy. On the author's
machine, one popular option was measured at **559 MB RAM and 99% CPU** while
doing nothing. For a background utility that should cost you almost nothing,
that's absurd. ClipStash aims for a footprint closer to Zed's:

| App         | Idle RAM  | Idle CPU |
|-------------|-----------|----------|
| ClipVault   | ~559 MB   | ~99%     |
| Zed editor  | ~135 MB   | ~0.3%    |
| **ClipStash target** | **~15 MB** | **~0%**  |

## Features

- Text + image clipboard history, persisted to disk (redb)
- Unlimited history; status bar icon changes color after 500 entries
- Pinned favorites
- Global hotkey: ⌘⇧V
- Search + type filter (All / Text / Image)
- Native AppKit popover UI (no web view, no Electron)

## How it stays cheap

1. **NSPasteboard `changeCount` polling**, 250 ms cadence. A single integer
   compare per tick — the pasteboard is only actually read when it changes.
2. **No web view.** The UI is a custom `NSPanel` anchored to an
   `NSStatusItem`, rendered with native AppKit views via `objc2`.
3. **Pure-Rust storage.** [`redb`](https://github.com/cberner/redb) — no C
   dependencies, crash-safe, smaller binary and faster builds than SQLite.
4. **Release profile** is tuned for size: fat LTO, single codegen unit,
   `strip = true`, `panic = abort`.

## Project layout

```
clipstash/
├── Cargo.toml
├── src/
│   ├── main.rs        # event loop wiring
│   ├── clipboard.rs   # NSPasteboard watcher + write-back
│   ├── store.rs       # redb-backed persistent history
│   ├── popover.rs     # status item + custom NSPanel popover UI
│   └── hotkey.rs      # ⌘⇧V global hotkey
└── README.md
```

## Build

```bash
cargo run --release
```

First run creates the database at
`~/Library/Application Support/ClipStash/history.redb` and places a clipboard
glyph in your menu bar.

## Roadmap

- [x] Persistent storage (redb)
- [x] NSPasteboard watcher with dedup
- [x] Global hotkey
- [x] Status item + panel scaffold
- [ ] NSTableView data source + custom row cell
- [ ] Search field + filter dropdown wiring
- [ ] Trash button per row + Clear-all
- [ ] Thumbnail generation for image rows
- [ ] Anchor panel under status button
- [ ] File clipping support
- [ ] Settings (history size cap, ignore-list, launch at login)
- [ ] Signed + notarized release build

## Contributing

Issues and PRs welcome. This is an early-stage project — the fastest way to
help is to try building it, open issues for anything that breaks, and
suggest UX improvements against the ClipVault-style popover.

## License

MIT — see [LICENSE](LICENSE).
