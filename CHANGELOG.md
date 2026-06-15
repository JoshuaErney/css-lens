# Changelog

All notable changes to `css-lens` are documented here.

Format: [Semantic Versioning](https://semver.org) — `MAJOR.MINOR.PATCH`
- `MINOR` bump — new features
- `PATCH` bump — bug fixes

---

## [0.9.0] — 2026-06-15

### Added
- **Duplicate class detection** — a `class="btn btn"` attribute with the same
  token listed more than once now produces a WARNING diagnostic at the duplicate
  occurrence; multi-line and whitespace-padded cases are handled correctly
- **CSS document symbols** — Zed's outline panel now lists all selectors defined
  in a CSS file (`.btn`, `#hero`, etc.), sorted by line; selectors inside
  `@media` blocks show the query in the container-name column; the panel
  supports keyboard navigation and jump-to-selector
- **`@keyframes` name tracking** — animation names defined with `@keyframes`
  are indexed at startup and kept up to date on CSS saves; hover over a keyframe
  name in `style="animation-name: …"` shows file and line; go-to-definition
  jumps to the `@keyframes` rule; completions are offered when the cursor is in
  an `animation-name:` value
- **`style=""` property and value completions** — typing in a `style=""` attribute
  now offers property-name completions (75+ properties, kind: Property) and
  keyword-value completions for ~30 properties with discrete valid values (kind:
  Value); CSS custom properties (`--foo`) are still offered when the prefix
  starts with `--`; animation-name values are sourced from the `@keyframes` map
- **Code lens** — each CSS selector displays an inline "used N times" / "unused"
  count; usage is counted across all workspace HTML files; lenses update when any
  HTML file opens, changes, or closes

### Fixed
- `style=""` context detection now correctly skips semicolons inside quoted
  property values (`content: "a;b"`) when identifying the active declaration,
  preventing completion and hover from misidentifying the property context
- `scan_keyframes` now follows `@import` chains with a visited-set guard,
  matching `scan_directory`'s behaviour; keyframes defined in import-only files
  are no longer invisible to completions and go-to-definition
- `diagnostics_for_duplicate_classes` no longer skips the remainder of a line
  when a malformed multi-line attribute is terminated by a `<` tag boundary;
  `class=""` attributes appearing after the boundary on the same line are now
  correctly duplicate-checked
- Inline `<style>` block diagnostics now work for non-`file://` document URIs
  (untitled and virtual-filesystem documents); previously such documents always
  returned an empty inline map, producing false "Unknown CSS class" errors for
  classes defined in the document's own `<style>` blocks
- CSS variable hover now falls back to the global `var_map` when the
  per-document scoped map does not contain the variable; hovering over a custom
  property defined in an unlinked CSS file no longer silently returns nothing
- "Remove unused rule" code action now reads CSS content from the editor's
  in-memory buffer (when the file is open) rather than always reading from disk;
  previously, unsaved edits to the CSS file caused the deletion range to target
  the wrong lines

### Changed
- Scoped class-map computation (per-document `<link>` resolution + inline
  `<style>` parsing) is now performed once per LSP request in `route_request`
  and passed to handlers, rather than being independently computed inside each
  of `completion_handler`, `hover_handler`, and `definition_handler`
- Usage counts for code lens are pre-built whenever HTML files change and cached
  in server state; `code_lens_handler` no longer walks the workspace on every
  scroll event
- Workspace HTML walk logic is now shared between `workspace_html_refs` and
  `build_usage_counts` via a common `walk_html_files` helper; previously the
  WalkDir setup was duplicated verbatim in both functions
- `parse_css_content_at` merged into `parse_css_content` (new `base_line: u32`
  parameter); callers passing `0` are unchanged
- `collect_reachable` now applies the same `MAX_CSS_BYTES` size guard as
  `parse_css_file_inner`, preventing oversized/minified files from appearing in
  the scoped reachable set when they were excluded from the global class map
- Hint collection in the "Remove unused rule" code action is now a single pass
  over the diagnostics slice (previously two separate filtered iterators)

---

## [0.8.0] — 2026-06-14

### Added
- **Remove unused rule** — an unused CSS selector hint now offers a Quick Fix
  to delete the entire rule block; a multi-selector guard (`a, b { }`) ensures
  the action is only offered when every co-selector in the block is also unused,
  so no live rules are accidentally removed
- **Scope to linked CSS** — completions, hover, and go-to-definition are now
  filtered to CSS files reachable from the current HTML document via
  `<link rel="stylesheet">` tags and their `@import` chains; when no resolvable
  links are found the full workspace map is used as a fallback; diagnostics
  remain lenient (a class defined anywhere in the workspace never triggers an
  error); the "Create class/ID" code action also prefers a linked CSS file as
  its insertion target
- **Inline `<style>` block support** — classes and IDs defined inside `<style>`
  blocks are included in completions, hover, go-to-definition, and diagnostics
  for that HTML document; selector `definition_line` values are offset to the
  correct line in the HTML file so navigation lands in the right place; CSS
  variables declared in inline styles surface in `style=""` completions

### Fixed
- **Unclosed attribute bleeds into adjacent markup** — a `class="` or `id="`
  without a closing quote previously swallowed the remainder of the line and
  leaked into following lines, causing false "Unknown CSS class" errors for
  tokens that were never part of the attribute value; `<` is now treated as a
  hard boundary in both the same-line and continuation-line scanners so
  malformed attributes terminate cleanly; genuine multi-line class attributes
  (no `<` between the opening quote and its closing quote) are unaffected

---

## [0.7.2] — 2026-06-09

### Changed
- **Binary renamed to `css-lens`** — the LSP binary, release asset filenames, and
  all internal identifiers are now `css-lens` to match the extension rebrand;
  release assets are now named `css-lens-{version}-{target}.tar.gz`

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
