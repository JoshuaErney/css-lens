# Changelog

All notable changes to `css-class-mapper-lsp` are documented here.

Format: [Semantic Versioning](https://semver.org) — `MAJOR.MINOR.PATCH`
- `MINOR` bump — new features
- `PATCH` bump — bug fixes

---

## [0.2.0] — 2026-06-08

### Added
- **Multi-class completions** — completions now trigger correctly when a `class`
  attribute already contains one or more class names (e.g. `class="btn |"`)
- **Class filtering** — classes already present in the attribute are excluded
  from suggestions so you only see what you can still add
- **Case-insensitive matching** — typing `BTN` or `Btn` will match `btn`
- **Smart spacing** — inserted class names include a leading or trailing space
  automatically when the cursor is flush against another class name
- **`@import` following** — the CSS scanner now follows `@import` statements
  recursively, so classes defined in imported files appear in completions
- **Cycle detection** — circular `@import` chains are detected and skipped
- **Go to definition** — `Cmd+Click` on a class name inside a `class="..."`
  attribute jumps to its definition in the source CSS file

### Changed
- Completions are now sorted alphabetically for a stable, predictable list
- CSS comment stripping now preserves newlines, keeping definition line numbers
  accurate even in files with multi-line block comments

### Fixed
- Regexes are now compiled once at startup via `OnceLock` instead of on every
  keystroke, improving responsiveness in large files

---

## [0.1.0] — 2026-06-08

### Added
- Initial release
- Scans workspace `.css` files for class selectors on startup
- Autocomplete inside `class="..."` attributes in HTML files
- Hover tooltips showing CSS property declarations for a class
- Incremental file watching — only the changed `.css` file is re-parsed on save
- `node_modules` and hidden directories excluded from the initial scan
- Symlink following disabled to prevent scanning outside the workspace
- Same-filename collision fix — files in different directories with the same
  name are tracked independently
