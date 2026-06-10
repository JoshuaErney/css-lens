# Roadmap

Planned features and improvements for the CSS Class Mapper extension.
See [CHANGELOG.md](CHANGELOG.md) for what has already shipped.

---

- [ ] "Remove unused class or ID" — remove a CSS rule that has no HTML references *(deferred — destructive operation, needs confidence mechanism)*
- [ ] TOCTOU in rename/code-action — CSS file can change between the read and write steps of a rename or code action *(deferred — would need a full in-memory document buffer for CSS)*
- [ ] Scope suggestions to CSS reachable from the current HTML file — `<link>` tags, inline `<style>` blocks, and `@import` chains *(deferred — requires per-document state)*
- [ ] Sort completions by frequency of use — user-configurable *(deferred — requires usage-tracking infrastructure)*
- [ ] Expand shorthand properties (`margin: 8px 16px` → all four sides) *(deferred — complex CSS expansion rules)*
- [ ] Extract class — select CSS properties and turn them into a new named class *(deferred — requires editor-specific selection APIs)*
- [ ] Lazy-load — only scan CSS files when an HTML file is first opened *(deferred — startup scan is fast enough for most projects)*
- [ ] Unclosed attribute swallows rest of line — a `class="` with no closing quote treats the remainder of the line as part of the value *(known limitation — inherent in line-by-line parsing during active editing)*
