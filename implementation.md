# Lighter — Implementation Design

CLI code highlighter that merges tree-sitter syntactical highlighting (arborium) with LSP semantic tokens.

## Flow

```
Input + Language
    │
    ▼
arborium advanced::parse(language, source)
    │
    ▼
ParseResult { spans, injections }
    │
    ├─► for each injection: parse(injection.language, injection_text)
    │     └─► collect all spans (offset-adjusted) + recurse injections
    │
    ├─► for main language + each injection language:
    │     open virtual file with matching LSP → semanticTokens/full
    │     decode response → convert (line, char_utf16) to byte offsets
    │     map LSP token types to arborium capture names
    │     produce Vec<Span> with pattern_index = max_pattern_index + 1
    │
    ▼
Merged ParseResult (all spans from all parsers + all LSPs)
    │
    ▼
arborium render (spans_to_ansi / spans_to_html)
    │
    ▼
stdout
```

## Modules

### `main.rs`

Argument parsing (clap, similar to arborium-cli). Reads input from file/stdin. Detects language from file extension or `--lang` flag.

The `daemon` subcommand is dispatched to `server.rs`. Its `spawn` action accepts
the config, theme, custom-theme, format, no-LSP, no-tree-sitter, and logging
options as daemon defaults. File, project, language, and line selection remain
exclusive to normal file/stdin invocations.

### `server.rs`

`lighter daemon spawn` launches a detached singleton process. While it is
running, normal CLI invocations send their language and source to it over a Unix
domain socket (or loopback IPC on Windows), allowing its LSP processes to remain
alive between invocations. `lighter daemon kill` requests a clean shutdown.
Without a live daemon, the CLI retains its standalone behavior.

Daemon messages consist of a single-line JSON header followed by exactly the
number of body bytes declared by `length`. Request headers contain `version`,
`id`, `lang`, and `length`, plus optional project, lines, no-tree-sitter,
no-LSP, format, and absolute config-path overrides. Response headers contain
`version`, the matching `id`, and `length`; failed responses additionally
contain `error` and always have a zero length. A config override replaces the
daemon's current config and drops its highlighter cache, cleanly shutting down
the associated LSP registries before subsequent requests create replacements.

Spawns LSP server processes based on which languages are encountered (main language + injection languages). Servers are started as child processes with piped stdio. Built-in table maps language names to commands (e.g. `rust` → `rust-analyzer`, `python` → `pylsp`).

Calls into `lib.rs` for the highlight pipeline. Prints result.

### `lib.rs`

The highlight pipeline. Takes source, language, and an `&mut LspClient` (or `Option` for no-LSP mode).

1. **Parse**: Use arborium's advanced API (`CompiledGrammar`, `ParseContext`) to parse source → `ParseResult`.
2. **Recurse injections**: For each `Injection` in the result, parse the injected content with arborium for that language. Offset-adjust resulting spans back to the parent coordinate space. Collect recursively.
3. **Enrich with LSP**: For the main language and each injection language (where an LSP is available), call `LspClient::get_semantic_tokens()`. Convert the returned tokens to `Vec<Span>`:
   - Decode the delta-encoded `u32[]` to absolute `(line, char, length)`.
   - Convert `(line, char_utf16)` → byte offset (handling UTF-16).
   - Map LSP token type string → arborium capture name string (e.g. `function` → `function`, `parameter` → `variable.parameter`, `struct` → `type`). User-defined mappings from the config's `[captures]` table take precedence (e.g. `const = "constant"`), and language-specific mappings such as `[captures.rust]` take precedence over global mappings.
   - Set `pattern_index` to `1 + max(all_existing_spans.pattern_index)` so semantic tokens win in arborium's deduplication.
4. **Merge**: Append all semantic spans into the collected span vector.
5. **Render**: Call `spans_to_ansi(source, spans, &theme)` or `spans_to_html(source, spans, &format)`.

### `lsp.rs`

LSP client. Uses `lsp-types` crate for type definitions. JSON-RPC over stdio with `Content-Length` framing.

**`LspClient`** manages one or more server processes. Keyed by language — lazily spawns a server on first request for a given language.

Each server goes through:
1. `initialize` request (declare semantic token capabilities) → extract `legend` from response
2. `initialized` notification

Then per document:
1. `textDocument/didOpen` (virtual file URI, full source text)
2. `textDocument/semanticTokens/full` → get `data: Vec<u32>`
3. `textDocument/didClose`

On drop: `shutdown` request + `exit` notification for each server, kill child processes.

Public interface:

```rust
impl LspClient {
    /// Spawn a new client with a table of language → server command mappings.
    pub fn new(servers: HashMap<String, ServerCommand>) -> Self;

    /// Get semantic tokens for source text in the given language.
    /// Lazily starts the server. Returns None if no server configured
    /// or server doesn't support semantic tokens.
    pub fn get_semantic_tokens(
        &mut self,
        language: &str,
        source: &str,
    ) -> Result<Option<(Vec<SemanticToken>, SemanticTokensLegend)>>;
}
```

`SemanticToken` and `SemanticTokensLegend` come from `lsp-types`.

## Key Dependencies

```toml
arborium = "2.18"
clap = { version = "4", features = ["derive"] }
lsp-types = "0.97"
serde_json = "1"
thiserror = "2"
```
