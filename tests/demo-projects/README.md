# Semantic highlighting demo projects

These small, multi-file projects give each language server a real workspace so
the integration snapshots exercise semantic tokens rather than syntax-only
classification.

Install these language servers on `PATH` before running the full suite:

- `rust-analyzer`
- `basedpyright-langserver`
- `typescript-language-server`
- `gopls`

The TypeScript language server also needs the demo project's pinned TypeScript
compiler:

```sh
npm ci --prefix tests/demo-projects/typescript
```

By default, a semantic test is skipped when its server or required project
dependency is not installed. Set `LIGHTER_REQUIRE_LANGUAGE_SERVERS=1` to turn a
missing prerequisite into a test failure:

```sh
LIGHTER_REQUIRE_LANGUAGE_SERVERS=1 cargo test --test semantic_highlighting \
  -- --test-threads=1
```
