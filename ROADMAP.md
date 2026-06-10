# Roadmap

Planned features and improvements for the CSS Class Mapper extension.
See [CHANGELOG.md](CHANGELOG.md) for what has already shipped.

---

## HTML Context

- [ ] Scope suggestions to CSS reachable from the current HTML file — `<link>` tags, inline `<style>` blocks, and `@import` chains *(deferred — requires per-document state)*
- [x] Multi-line class attributes ignored — `class="btn\n  foo"` split across lines is not recognised *(v0.8.0)*
- [ ] Unclosed attribute swallows rest of line — a `class="` with no closing quote treats the remainder of the line as part of the value *(known limitation — inherent in line-by-line parsing during active editing)*

---

## Completions

- [ ] Sort by frequency of use — user-configurable *(deferred — requires usage-tracking infrastructure)*

---

## Hover Tooltips

- [ ] Expand shorthand properties (`margin: 8px 16px` → all four sides) *(deferred — complex CSS expansion rules)*

---

## Refactoring

- [ ] Extract class — select CSS properties and turn them into a new named class *(deferred — requires editor-specific selection APIs)*
- [ ] TOCTOU in rename/code-action — CSS file can change between the read and write steps of a rename or code action *(deferred — would need a full in-memory document buffer for CSS)*

---

## Code Actions

- [ ] "Remove unused class or ID" — remove a CSS rule that has no HTML references *(deferred — destructive operation, needs confidence mechanism)*

---

## Performance

- [ ] Lazy-load — only scan CSS files when an HTML file is first opened *(deferred — startup scan is fast enough for most projects)*
- [x] `hover_handler` rebuilds cursor context up to 4× per request *(v0.8.0)*
