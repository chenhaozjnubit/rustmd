# rustmd

A Typora-style WYSIWYG Markdown editor and reader, written in pure Rust.

No WebView, no JavaScript runtime, no bundled browser. Every extension beyond
plain Markdown is rendered by code in this repository: the editor, the maths
engine, the diagram renderer and the HTML renderer are all native `egui`
drawing.

## What it does

- **Live preview, Typora style** — the block under the caret shows its Markdown
  source, everything else renders. No split pane.
- **Self-contained maths** — `$x^2$` and `$$…$$`, including `\begin{…}`
  environments (`pmatrix`, `bmatrix`, `cases`, `aligned`, `array`, …), typeset
  by a small box-model engine in `src/math.rs`.
- **Self-contained diagrams** — ` ```mermaid ` fences render as real vector
  graphics for eight diagram types: `flowchart`/`graph`, `pie`,
  `sequenceDiagram`, `classDiagram`, `stateDiagram-v2`, `erDiagram`, `gantt`
  and `mindmap` (`src/mermaid.rs`).
- **HTML rendering** — block-level HTML (`<div>`, `<table>`, `<details>`,
  `<ul>`, `<pre>`, `<img>`) is laid out as real structure, and inline tags
  (`<b>`, `<i>`, `<u>`, `<code>`, `<span style="…">`, `<br>`, comments,
  autolinks) take effect instead of being shown as source (`src/html.rs`).
  Markdown still works inside an HTML block.
- **Syntax-highlighted code fences** via `syntect`.
- **Folders, tabs and a file picker** built on `rfd`.
- **Virtualised scrolling** — only the blocks in the viewport are laid out, so a
  hundred-thousand-block document stays interactive.
- **Standalone HTML export** — `--export in.md out.html` writes a single file
  with everything inlined except the `MathJax` and `mermaid` CDN scripts.

## Running

```sh
cargo run --release                       # open the editor
cargo run --release -- notes.md           # open a file
cargo run --release -- --export in.md out.html
cargo run --release -- --bench big.md     # parse + layout timings, no window
```

## Building

### macOS

```sh
scripts/make-app.sh          # -> dist/rustmd.app
```

Produces a real `.app` bundle, so it can be launched from Finder, found by
Spotlight, and registered as a handler for `.md` files. It declares
`LSHandlerRank = Alternate`, so installing it will not steal the default
handler from whatever you already use.

### Windows

```sh
cargo build --release        # -> target\release\rustmd.exe
```

Release builds carry `windows_subsystem = "windows"`, so no console window is
attached.

`rustmd-windows-x64.zip` is attached to every [release](../../releases), and the
[`Windows`](../../actions/workflows/windows.yml) workflow builds and tests the
same `.exe` on every push.

### Linux and other platforms

```sh
cargo build --release
```

The code is portable: `libc` is a unix-only dependency and the Apple-Event
plumbing in `src/macos.rs` is compiled out elsewhere behind a stub.

## Layout

| Path | What lives there |
| --- | --- |
| `src/app.rs` | Application state, commands, menu bar |
| `src/doc.rs` | Document model and undo history |
| `src/parser.rs` | Markdown → blocks, inline spans |
| `src/render.rs` | Block drawing for every block kind |
| `src/editor.rs` | The live-preview widget, caret and selection |
| `src/html.rs` | Tolerant HTML subset renderer |
| `src/html_export.rs` | Static HTML export |
| `src/math.rs` | LaTeX layout engine |
| `src/mermaid.rs` | Diagram parsing, layout and drawing |
| `src/code_hl.rs` | Syntax highlighting |
| `src/picker.rs` | Folder listing and file open dialog |
| `src/macos.rs` | Apple Event handling for "open with" |
| `src/smoke.rs` | Headless end-to-end tests over `samples/` |

`samples/test-render.md` is a stress document that exercises every syntax the
renderer claims to support, with an "expected" note per section. Parts of the
test suite read it directly with `include_str!`, so the fixtures cannot drift
away from the document.

## Tests

```sh
cargo test
```

The suite renders documents without opening a window, so layout regressions
fail in CI rather than in the eye.

## License

MIT — see [LICENSE](LICENSE).
