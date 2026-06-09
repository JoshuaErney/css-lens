# Changelog

All notable changes to `css-class-mapper-lsp` are documented here.

Format: [Semantic Versioning](https://semver.org) — `MAJOR.MINOR.PATCH`
- `MINOR` bump — new features
- `PATCH` bump — bug fixes

---

## [0.5.0] — 2026-06-09

### Added
- **Diagnostics: undefined classes/IDs** — class and ID names in HTML that don't
  exist in any scanned CSS file are now underlined as errors; the squiggle
  clears as soon as the definition is added to CSS and the file is saved
- **Diagnostics: duplicate selectors** — if the same class or ID is defined more
  than once in the same CSS file, every definition after the first is flagged as
  a warning
- **Specificity score in hover** — each hover tooltip now shows the computed
  CSS specificity as `(a,b,c)` (IDs, classes, type selectors)

---

## [0.4.0] — 2026-06-09

### Added
- **`@media` / `@supports` context in hover** — when a rule is nested inside a
  media query, the hover tooltip shows which query it belongs to
  (e.g. _inside `@media (max-width: 768px)`_)
- **ID selector support** — `#id` selectors in CSS are now parsed alongside
  class selectors; completions, hover, and go-to-definition all work inside
  `id="..."` attributes
- **Single-value guard for `id="..."`** — completions stop being offered once
  the attribute already has a value, since IDs are single-value
- **GitHub Actions release workflow** — pushing a `v*` tag now automatically
  builds for all three targets and publishes the release assets

### Changed
- CSS parser rewritten from a flat regex to a proper brace-depth-aware walker.
  This correctly handles `@media` blocks, `@supports`, `@layer`, nested rules,
  and string literals containing `{` or `}` characters.
- `@layer` and other block @-rules are now traversed so selectors inside them
  are found; they inherit the enclosing media query context if any.

---

## [0.3.0] — 2026-06-09

### Added
- **Full selector in hover** — hover tooltips now show the complete selector
  (e.g. `.btn.btn--primary`) instead of just the bare class name
- **Line numbers in hover** — hover tooltips now include the source line number
  (e.g. `styles.css:42`)
- **All definitions in hover** — when a class is defined in more than one file,
  hover lists every definition numbered with its file, line, and CSS block
- **Multi-location go-to-definition** — when a class has multiple definitions,
  go-to-definition returns all locations so the editor presents a picker

### Fixed
- **Last-write-wins bug** — if the same class name appeared in multiple CSS
  files, only the last parsed definition was kept; all definitions are now
  preserved and surfaced in hover and go-to-definition

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
