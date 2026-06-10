# Changelog

All notable changes to `css-lens` are documented here.

Format: [Semantic Versioning](https://semver.org) — `MAJOR.MINOR.PATCH`
- `MINOR` bump — new features
- `PATCH` bump — bug fixes

---

## [0.7.1] — 2026-06-09

### Fixed
- **Stale "Unknown CSS class" after save** — the LSP server now registers a
  `workspace/didChangeWatchedFiles` file watcher for `**/*.css` during
  initialization; previously no watcher was registered so the editor never
  notified the server when a CSS file was saved, leaving the class map and HTML
  diagnostics permanently stale until the LSP was restarted

---

## [0.7.0] — 2026-06-09

### Fixed
- **Rename word-boundary bug** — renaming `.btn` no longer corrupts sibling
  classes like `.btn-primary` that appear on the same CSS selector line; the
  rename edit now checks that the match is a complete token before applying
- **UTF-16 column crash** — `pos.character` (a UTF-16 code-unit offset as
  required by the LSP spec) was previously used directly as a UTF-8 byte
  offset, causing a panic on any document line containing non-ASCII characters
  (accented letters, em-dashes, etc.); a proper `utf16_offset_to_byte`
  conversion is now applied throughout all position-dependent helpers
- **Duplicate-parse via `@import`** — `scan_directory` now shares a single
  `visited` set across all files; previously a file that appeared both directly
  in the workspace and in an `@import` chain was parsed twice, generating
  false-positive "already defined" duplicate-selector warnings
- **Digit-start identifiers** — `is_valid_css_ident` now rejects names that
  start with a digit (e.g. `2col`), which the CSS Syntax Level 3 spec
  disallows as class/ID selectors; renaming to such a name previously wrote
  invalid CSS silently
- **`extract_quoted` multi-token messages** — the code-action name extractor
  used `find` + `rfind`, which would return a garbage span if a diagnostic
  message ever contained two quoted tokens; it now uses `find` twice to always
  extract the first quoted span
- **Extension fallback tag** — the hard-coded fallback release tag in the Zed
  extension was still pointing to `v0.6.0` after the previous release; updated
  to `v0.7.0`

### Improved
- **Completion performance** — `build_insert_text` no longer calls
  `cursor_context` once per completion candidate; the cursor line and column
  are computed once before the candidate map, reducing work from
  O(candidates × document\_lines) to O(document\_lines) per request
- **Shared cursor context** — all position-dependent helpers (`in_attr`,
  `style_prefix`, `completion_context`, `id_completion_context`, `word_at`,
  `build_insert_text`) now share a single `cursor_context` helper that builds
  the before-cursor string in one pass, eliminating redundant line iteration
- **Color deduplication** — `color_summary` now uses a `HashSet` for O(1)
  membership checks instead of `Vec::contains` (O(n) per check)

---

## [0.6.0] — 2026-06-09

### Added
- **Find all references** — from a class or ID in an HTML `class=""` / `id=""`
  attribute, finds every other HTML file in the workspace that uses the same
  name (`textDocument/references`)
- **Rename** — renaming a class or ID updates its CSS selector definition and
  every HTML attribute reference across the workspace (`textDocument/rename`)
- **Code action: Create class/ID** — when a class or ID is flagged as undefined
  in HTML, a Quick Fix offer appears to append the rule to the nearest CSS file
  (`textDocument/codeAction`)
- **CSS variable completions** — CSS custom properties (`--name`) are extracted
  from parsed CSS and offered as completions when typing `--` inside a
  `style="..."` attribute
- **CSS variable hover** — hovering over a `--variable-name` inside `style=""`
  shows its declared value
- **Color values in hover** — hex, `rgb()`, and `hsl()` values found in a
  rule's properties are highlighted inline in the tooltip
- **Unused-selector hint** — CSS selectors not referenced in any currently-open
  HTML file are flagged with a soft hint; suppressed when no HTML files are open
  so JS-driven classes are never falsely flagged
- **File size cap** — CSS files over 500 KB are skipped during scanning to
  prevent stalling on minified bundles

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
