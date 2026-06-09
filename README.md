# css-class-mapper-lsp

A native Rust LSP server that powers the [CSS Class Mapper](https://github.com/joshuaerney/css-class-mapper) Zed extension.

It scans every `.css` file in your workspace and wires up a full set of HTML authoring features — completions, hover documentation, diagnostics, navigation, and refactoring — all backed by a real understanding of your stylesheet.

---

## Features

### Completions

- **Class completions** — typing inside `class="..."` suggests every class name found in your CSS files, filtered by what you've already typed (case-insensitive)
- **Already-used filtering** — classes already present in the same attribute are excluded from suggestions
- **Smart spacing** — inserted names automatically get a leading or trailing space when the cursor is flush against another class name
- **ID completions** — typing inside `id="..."` suggests ID selectors; suggestions stop once a value is present (IDs are single-value)
- **CSS variable completions** — typing `--` inside `style="..."` suggests custom properties (`--primary-color`, etc.) found in your CSS

### Hover Tooltips

Hovering a class name in `class="..."`, an ID in `id="..."`, or a variable name in `style="..."` shows:

- The **full selector** (e.g. `.btn.btn--primary`)
- The **source file and line number** (e.g. `styles.css:42`)
- The **`@media` or `@supports` context** if the rule is inside one
- The **CSS specificity score** as `(a,b,c)`
- **Color values** — hex, `rgb()`, and `hsl()` values in the rule's properties are shown inline
- The **full property block** in a syntax-highlighted code fence
- **All definitions** listed when the same name is defined in multiple files
- **Declared value** of a CSS custom property when hovering a `--variable-name` in `style=""`

### Diagnostics

- **Undefined class/ID** — class and ID names in HTML that don't exist in any CSS file are underlined as errors; the squiggle clears automatically when you save the CSS definition
- **Duplicate selector** — if the same class or ID is defined more than once in the same CSS file, every definition after the first is flagged as a warning
- **Unused-selector hint** — CSS selectors not referenced in any currently-open HTML file receive a soft hint; suppressed when no HTML files are open so JS-driven classes are never falsely flagged

### Navigation

- **Go to definition** — `Cmd+Click` a class in `class="..."` or an ID in `id="..."` to jump to its definition in the source CSS file; when multiple files define the same name the editor presents a picker
- **Find all references** — from a class or ID in an HTML attribute, finds every HTML file in the workspace that uses the same name

### Refactoring

- **Rename** — rename a class or ID in one place; the LSP updates the CSS selector definition and every HTML attribute reference across the workspace atomically
- **Create class/ID** — when an undefined class or ID is flagged in HTML, a Quick Fix code action appends the new rule to the nearest CSS file

### CSS Parsing

- Follows `@import` chains recursively (with cycle detection)
- Handles `@media`, `@supports`, `@layer`, and other block at-rules via a proper brace-depth-aware parser
- Extracts both class selectors (`.btn`) and ID selectors (`#hero`)
- Parses CSS custom properties (`--name: value`) for variable completions and hover
- Strips block comments while preserving line numbers
- Skips files larger than 500 KB to avoid stalling on minified bundles
- Excludes `node_modules` and hidden directories

---

## How it works

The LSP server is a plain Rust binary that speaks the [Language Server Protocol](https://microsoft.github.io/language-server-protocol/) over stdio. It is downloaded automatically by the Zed extension at install time — you do not need to install it manually.

On startup it walks every `.css` file in the workspace, parses all selectors and custom properties into an in-memory map, and responds to LSP requests from the editor. File changes are picked up incrementally via `workspace/didChangeWatchedFiles` — only the changed file is re-parsed. Diagnostics for open HTML files are pushed automatically whenever the CSS map changes.

---

## Building from source

Requires Rust installed via [rustup](https://rustup.rs).

```bash
git clone https://github.com/joshuaerney/css-class-mapper-lsp
cd css-class-mapper-lsp/lsp
cargo build --release
# binary is at target/release/css-class-mapper-lsp
```

### Cross-compilation targets

```bash
# macOS Apple Silicon (native)
cargo build --release --target aarch64-apple-darwin

# macOS Intel
cargo build --release --target x86_64-apple-darwin

# Linux x86_64 (requires cargo-zigbuild or a GNU cross-toolchain)
cargo zigbuild --release --target x86_64-unknown-linux-gnu
```

New releases are built and published automatically by the GitHub Actions workflow in `.github/workflows/release.yml` when a `v*` tag is pushed.

---

## Release asset naming

GitHub release assets must be named exactly as follows for the Zed extension to find them:

| Platform | Asset filename |
|---|---|
| macOS Apple Silicon | `css-class-mapper-lsp-{version}-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `css-class-mapper-lsp-{version}-x86_64-apple-darwin.tar.gz` |
| Linux x86_64 | `css-class-mapper-lsp-{version}-x86_64-unknown-linux-gnu.tar.gz` |

Each `.tar.gz` must contain a single executable named `css-class-mapper-lsp`.

---

## License

MIT — see [LICENSE](LICENSE).
