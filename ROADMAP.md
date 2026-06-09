# Roadmap

Planned features and improvements for the CSS Class Mapper extension.
Checked items have been shipped — see [CHANGELOG.md](CHANGELOG.md) for details.

---

## CSS Parsing

- [x] Handle `@import` — follow imported CSS files automatically *(v0.2.0)*
- [x] Support `@media` — capture which breakpoint a class lives in *(v0.4.0)*
- [x] Handle multiple definitions of the same class across files *(v0.3.0)*
- [x] Extract ID selectors (`#id`) alongside class selectors *(v0.4.0)*

---

## HTML Context

- [x] Complete when `class` attribute already has values (`class="btn |"`) *(v0.2.0)*
- [x] Filter out classes already used in the same `class` attribute *(v0.2.0)*
- [x] Trigger completions and hover inside `id="..."` attributes — `id` is single-value only, stop suggesting once a value exists *(v0.4.0)*
- [ ] Scope suggestions to CSS reachable from the current HTML file — `<link>` tags, inline `<style>` blocks, and `@import` chains

---

## Completions

- [x] Sort suggestions alphabetically *(v0.2.0)*
- [ ] Sort by frequency of use — user-configurable

---

## Hover Tooltips

- [x] Show the full original selector (`.btn.btn--primary`) not just the class name *(v0.3.0)*
- [x] Show which file the rule comes from *(v0.1.0)*
- [x] Show which line number the rule comes from *(v0.3.0)*
- [x] Show the media query context if the rule is inside one (`@media (max-width: 768px)`) *(v0.4.0)*
- [x] Show all definitions when multiple files define the same class or ID *(v0.3.0)*
- [ ] Expand shorthand properties (`margin: 8px 16px` → all four sides)
- [ ] Color swatches next to color values
- [x] Show computed specificity score — IDs score `(1,0,0)`, classes `(0,1,0)` *(v0.5.0)*

---

## Diagnostics

- [x] Highlight class names in HTML that don't exist in any CSS file *(v0.5.0)*
- [ ] Show a soft hint (faint underline, not an error) for classes and IDs that appear unused — leaves the developer to decide if they're JS-driven
- [x] Warn when the same class or ID is defined twice in the same CSS file *(v0.5.0)*

---

## Navigation

- [x] Go to definition — `Cmd+Click` a class in HTML to jump to its definition in the CSS file *(v0.2.0)*
- [ ] Find all references — from a CSS definition, find every HTML file that uses it

---

## Refactoring

- [ ] Rename — rename a class or ID in one place and update it across all CSS and HTML files
- [ ] Extract class — select CSS properties and turn them into a new named class

---

## Code Actions

- [ ] "Create class" or "Create ID" — when an undefined class or ID is used in HTML, offer to generate it in the nearest CSS file
- [ ] "Remove unused class or ID" — remove a CSS rule that has no HTML references

---

## CSS Variable Support

- [ ] Recognize and complete CSS custom properties (`--primary-color`) inside `style="..."` attributes
- [ ] Show the declared value of a CSS variable in hover tooltips

---

## Performance

- [ ] Lazy-load — only scan CSS files when an HTML file is first opened
- [ ] Cap file size — skip CSS files over a configurable limit to handle minified files

---

## Distribution

- [x] GitHub Actions workflow to auto-compile and publish release assets on new tags *(v0.4.0)*
