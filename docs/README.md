# Highlighter comparison demo

This directory is a dependency-free GitHub Pages site. It compares the same
source with Highlight.js, Arborium, and Lighter for Rust, Python, and TypeScript.
The vendored Highlight.js 11.11.1 browser bundle retains its BSD-3-Clause
license header.

The language fixtures are deliberately small projects so each language server
can resolve imports and emit project-aware semantic tokens. Rebuild the checked-in
HTML fragments from the repository root:

```sh
npm install --prefix docs/projects/typescript
docs/scripts/generate.sh
```

Then serve `docs/` from any static file server, or configure GitHub Pages to
publish from the `docs` directory.
