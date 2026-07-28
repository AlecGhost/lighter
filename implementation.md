# Lighter implementation specification

This document specifies the observable behavior and internal module boundaries
of the current `lighter` binary. The binary is both:

- a command-line source highlighter; and
- a client and host for an optional background daemon that keeps language
  servers alive between invocations.

The behavioral sections are the contract. The module sections describe the
current division of responsibilities used to implement that contract.

## 1. Binary behavior

### 1.1 Invocation forms

The public command-line forms are:

```text
lighter [OPTIONS] [FILE]
lighter daemon spawn [STARTUP OPTIONS]
lighter daemon kill
```

There is also a hidden `lighter daemon serve [STARTUP OPTIONS]` command. It is
an internal entry point used by `daemon spawn`, not a public user workflow.

Normal highlighting options and daemon subcommands are mutually exclusive.
Clap handles help, version output, argument validation, conflicts, and unknown
arguments before Lighter performs any I/O.

### 1.2 Exit status and streams

On success, the binary exits successfully. A normal highlighting invocation
writes only the rendered fragment to standard output and does not append a
newline of its own.

After successful CLI parsing, an operation failure makes the binary:

1. writes the top-level error display followed by one newline to standard
   error;
2. writes no successful highlight result; and
3. exits with failure status.

Clap owns its own formatted argument-error output and exit status. Lighter does
not print a cause chain for later operation errors. Those errors are grouped by the
operation that failed, such as reading source, loading configuration, loading a
theme, daemon communication, tree-sitter highlighting, or LSP communication.

Language-server messages and debug span dumps also use standard error. A
spawned daemon has its standard input, output, and error connected to the null
device, so its logging is not visible in the spawning terminal.

### 1.3 Normal highlighting options

| Option | Meaning |
| --- | --- |
| `[FILE]` | UTF-8 source file. If absent, read all UTF-8 source from standard input. |
| `-l, --lang LANG` | Explicit language name. Required for standard input and takes precedence over file-name detection. |
| `-p, --project DIR` | Workspace exposed to the language server. It is not inferred from `FILE`. |
| `-c, --config FILE` | Explicit TOML configuration. There is no automatic config-file discovery. |
| `--theme NAME` | Built-in Arborium theme, matched case-insensitively. |
| `--custom-theme FILE` | Custom Arborium TOML theme. Conflicts with `--theme`. |
| `-f, --format FORMAT` | `ansi`, `html`, `latex`, or `typst`; default is `ansi`. |
| `--lines RANGE` | Inclusive, one-based selection in `start:end`, `:end`, or `start:` form. |
| `--no-lsp` | Do not start or query a language server. |
| `--no-tree-sitter` | Do not produce tree-sitter spans. |
| `--log LEVEL` | `ERROR`, `WARN`, `INFO`, or `DEBUG`, parsed case-insensitively. |

Both highlighting engines may be disabled. In that case Lighter still applies
line selection and output-format escaping to the unstyled source.

### 1.4 Input and language resolution

Lighter resolves the language before reading the source:

- An explicit `--lang` value is used verbatim.
- Otherwise, a file path is converted to UTF-8 and passed to Arborium's
  file-name-based language detection.
- Standard input without `--lang` is rejected.
- A non-UTF-8 file path cannot be used for language detection, although an
  explicit `--lang` avoids that detection step.

File input is read in full with `read_to_string`; non-UTF-8 source is therefore
an input error. Standard input is also read completely before highlighting.
There is no streaming render mode.

When LSP highlighting is enabled, an input file is presented to the server at
its canonical real path. Standard-input source is written to a unique temporary
file with a language-appropriate extension, presented to the server as a file
URI, and removed after the document request finishes.

### 1.5 Execution-mode selection

Every normal invocation performs the following high-level sequence:

1. parse and validate the CLI;
2. resolve the language;
3. read all source;
4. test whether the daemon IPC endpoint accepts a connection;
5. either send one request to that daemon or construct a one-shot local
   highlighter;
6. print the returned fragment verbatim.

Daemon discovery is automatic and has no opt-out option. If no connection can
be made, the invocation uses the one-shot path. A connection that succeeds but
later returns a protocol or highlighting error does not fall back to local
highlighting.

### 1.6 Startup defaults and request precedence

Without a daemon, configuration is loaded for the current process. The
effective theme is chosen in this order:

1. `--theme`;
2. `--custom-theme`;
3. `theme` from the selected config file;
4. Catppuccin Mocha.

The effective output format is the CLI format or `ansi`. The effective log
level is the CLI level or `ERROR`.

`daemon spawn` validates its config and theme before starting the child. Its
startup values become session defaults:

- config: explicit file or built-in defaults;
- theme: CLI theme, configured theme, or Catppuccin Mocha;
- format: explicit format or `ansi`;
- log level: explicit level or `ERROR`.

While a daemon is running, a normal invocation supplies per-request values:

| Setting | Request behavior |
| --- | --- |
| Config | An explicit `--config` is sent as an absolute path and becomes the daemon session's active server/capture config. If it equals the active config, the existing registry is retained; otherwise the registry and its language-server processes are replaced. No option means “keep the active daemon config.” |
| Theme | Explicit `--theme` or `--custom-theme` overrides the daemon theme for this request only. No option uses the daemon startup theme. Loading a request config does not adopt that config's theme. |
| Format | An explicit format overrides the daemon default for this request only. |
| Log level | `--log` on a normal daemon-backed invocation has no effect. The daemon startup log level is used. |
| LSP/tree-sitter | The two enabled/disabled booleans are set by every request. |
| Lines | The requested range applies only to that request. |

Changing configuration replaces the server registry only when the newly loaded
`Config` differs from the active value. Equality includes commands, capture
mappings, and the config's theme entry, even though that theme entry is not
automatically applied to daemon rendering.

### 1.7 Highlighting pipeline

The core highlighter operates on an `Input` and `HighlightOptions`:

1. If tree-sitter is enabled, Arborium parses the complete source and returns
   syntax spans.
2. If LSP is enabled, the server registry returns the client for the pair
   `(language, canonical project path)`, starting it lazily when necessary.
3. Lighter requests full-document semantic tokens for the complete source and
   converts them from UTF-16 LSP positions to UTF-8 byte spans.
4. Tree-sitter and semantic spans are concatenated.
5. If `--lines` is present, the source and spans are clipped to the requested
   byte range and spans are rebased to the selected fragment.
6. The selected source and merged spans are rendered as ANSI, HTML, LaTeX, or
   Typst.

The complete document is always analyzed before line selection. Declarations,
imports, and other context outside the output range therefore remain visible to
both highlighting engines.

LSP spans normally receive a pattern priority higher than all tree-sitter
spans, allowing more specific semantic classifications to win on overlap.
Generic LSP `variable` tokens deliberately receive priority zero so they do not
replace more specific tree-sitter classifications.

### 1.8 Line ranges

A line range:

- contains exactly one colon;
- uses positive, one-based decimal line numbers;
- may omit either endpoint but not both; and
- requires the end to be greater than or equal to the start.

The end is inclusive. Internally, selection starts at the first byte of the
start line and ends at the first byte of the following line. Consequently, an
existing newline terminating the last selected line is included before the
renderer applies its own trailing-newline behavior.

A start beyond the end of the file selects an empty fragment. An end beyond the
file is clamped to the end of the source. Spans crossing either boundary are
clipped rather than discarded.

### 1.9 Output formats

#### ANSI

ANSI output is produced by Arborium using the effective theme. It is a terminal
fragment, not a pager protocol; callers that pipe it to a pager are responsible
for preserving escape sequences.

#### HTML

HTML output is produced by Arborium's default HTML formatter. It is an escaped
fragment containing Arborium custom highlight elements, not a complete
document, stylesheet, or `<pre>` wrapper. Theme colors are not embedded in the
fragment; the consumer supplies CSS.

#### LaTeX

LaTeX output is a fragment for an `fvextra` `Verbatim` environment configured
with:

```latex
commandchars=\\\{\}
```

Styled flat tokens may emit:

- `\textcolor[HTML]{RRGGBB}{...}`;
- `\textbf{...}`;
- `\textit{...}`;
- `\underline{...}`; and
- `\sout{...}`.

The characters `\`, `{`, and `}` in source text are escaped with `\`. Other
characters, including non-ASCII characters and ordinary LaTeX special
characters, are preserved because the fragment is intended for `Verbatim`.
Trailing line-feed characters are removed before LaTeX rendering.

#### Typst

Typst output is a `block` containing programmatically constructed `raw` and
`linebreak` elements. Its paragraph leading matches Typst's normal raw code
blocks. Styled flat tokens may be wrapped in:

- `text` with an RGB foreground color;
- `strong`;
- `emph`;
- `underline`; and
- `strike`.

Non-newline source characters are encoded in Typst string literals before
being passed to `raw`. Backslashes, quotes, tabs, carriage returns, and other
control characters are escaped; remaining Unicode text is preserved. Source
newlines become `linebreak` elements. Trailing line-feed characters are removed
before Typst rendering.

### 1.10 Configuration format

The selected TOML file may define `theme`, `servers`, and `captures`.
Unspecified sections use defaults.

Theme forms are:

```toml
theme = "Tokyo Night"
theme = { path = "relative/theme.toml" }
```

A relative configured theme path is resolved against the directory containing
the config file.

Server entries have a shorthand and detailed form:

```toml
[servers]
gleam = "gleam lsp"

[servers.go]
command = "gopls"
config = { gopls = { semanticTokens = true } }
```

Command strings are split with shell-like quoting rules. The first word is the
executable and the remaining words are direct process arguments; no shell is
started. Empty or syntactically incomplete command strings are rejected.
Configured languages replace same-named built-in server entries.

Capture mappings also have two forms:

```toml
[captures]
parameter = "variable.parameter"

[captures.rust]
const = "constant"
```

String entries are general mappings. Table entries are per-language mappings.
When a server is retrieved, general mappings are extended into that language's
map and therefore take precedence over a same-named language-specific entry.
Mapping keys are LSP token-type names and values are Arborium capture names.

The built-in language-server commands are:

| Languages | Program and arguments |
| --- | --- |
| Rust | `rust-analyzer` |
| Python | `basedpyright-langserver --stdio` |
| JavaScript, JSX, TypeScript, TSX | `typescript-language-server --stdio` |
| C, C++ | `clangd` |
| Go | `gopls` with `gopls.semanticTokens = true` configuration |
| Java | `jdtls` |
| Lua | `lua-language-server` |
| Zig | `zls` |
| Ruby | `ruby-lsp` |
| Kotlin | `kotlin-lsp` |
| Swift | `sourcekit-lsp` |
| Haskell | `haskell-language-server-wrapper --lsp` |
| OCaml | `ocamllsp` |
| Dart | `dart language-server` |

### 1.11 Language-server behavior

Language servers are started lazily. A registry caches one process for each
language and canonical project path, including a distinct no-project entry.

When a project is supplied, Lighter:

- canonicalizes it and requires it to be a directory;
- starts the server with that directory as its process working directory; and
- supplies it as both the initialization root URI and workspace folder.

Without a project, no root URI or workspace folder is supplied and the server
inherits Lighter's working directory.

The client advertises relative, full-document semantic tokens. A server that
does not advertise full semantic-token support is rejected. For each highlight
the client:

1. sends `textDocument/didOpen` with version 1;
2. allows initial work-done progress to settle;
3. requests `textDocument/semanticTokens/full`; and
4. sends `textDocument/didClose`, including when a successful token response is
   empty.

The initial progress wait is bounded at five seconds. Once progress has begun,
the completion wait is bounded at fifteen seconds; unfinished progress state is
cleared after the timeout. These waits do not cancel the later semantic-token
request.

Semantic-token positions are interpreted as zero-based UTF-16 line and column
values and converted to UTF-8 byte offsets. Invalid token types, positions,
overflowing deltas, zero-length ranges, and positions splitting a surrogate
pair are omitted. Token modifiers are currently ignored.

Default token-to-capture behavior includes:

- class, interface, and struct → `type`;
- type parameter → `type.parameter`;
- parameter → `variable.parameter`;
- enum → `type.enum`;
- enum member → `type.enum.variant`;
- modifier → `keyword.modifier`; and
- regexp → `string.regexp`.

Other token types use their LSP token name unless configuration overrides it.

The JSON-RPC client handles the server interactions Lighter needs:

- show-message notifications and requests;
- log-message notifications;
- work-done progress creation and updates; and
- workspace configuration requests.

Unknown server requests receive JSON-RPC “method not found.” Incoming messages
larger than 64 MiB are rejected. Language-server standard error is discarded.

When a cached client is dropped, Lighter sends `shutdown`, sends `exit`, and
waits for the child process.

### 1.12 Logging

Log levels are cumulative:

| Level | Visible diagnostics |
| --- | --- |
| `ERROR` | Lighter's own terminal failure only. |
| `WARN` | Server error/warning show-message output. |
| `INFO` | `WARN` output, server informational messages, and progress titles. |
| `DEBUG` | `INFO` output, server log messages, progress details, and Lighter's complete tree-sitter/LSP span dump. |

The debug span dump lists the two span sources separately, sorted by byte
position, and includes each span's source substring. It is generated before
line selection.

### 1.13 Daemon lifecycle

`daemon spawn`:

1. validates startup configuration and theme;
2. rejects an already-connectable daemon;
3. starts the current executable with the hidden `daemon serve` command;
4. disconnects the child from terminal standard streams; and
5. polls every 20 ms for at most 250 attempts, succeeding when the endpoint
   accepts a connection.

It reports early child exit, spawn errors, or a startup timeout.

The daemon is a single-session, sequential server. It acquires a singleton
lock, accepts one request per connection, and processes connections one at a
time. A malformed individual connection is ignored after it closes; listener
I/O failure terminates the server.

On Unix, runtime files live under:

```text
${XDG_RUNTIME_DIR}/lighter/
```

or, when `XDG_RUNTIME_DIR` is absent, under the operating system's temporary
directory:

```text
<temporary-directory>/lighter-<uid>/
```

The directory is created with mode `0700`. It contains `daemon.sock` and
`daemon.lock`; the lock file records the daemon PID. The endpoint is removed
when the server exits. On Windows, the endpoint file stores the loopback TCP
address and the lock uses exclusive file sharing.

`daemon kill` sends a reserved stop request. The daemon acknowledges it with an
empty successful response and then exits. Killing when no daemon is
connectable is an error.

### 1.14 Daemon wire protocol

The internal daemon protocol version is `5`. Each request uses a fresh stream
connection and consists of:

1. one newline-terminated JSON header; and
2. exactly `length` raw UTF-8 source bytes.

The request header contains the version, numeric request id, language, source
byte length, optional path and project, and flattened request options.

A response has the same framing: a newline-terminated JSON header followed by
exactly `length` UTF-8 bytes. It repeats the protocol version and request id.
Failure responses contain an `error` string and a zero-length body. Clients
reject version mismatches, response-id mismatches, invalid UTF-8, truncated
messages, and error headers with non-zero body lengths.

The reserved language name `lighter-internal-stop` stops the daemon only when
sent with the supported protocol version.

## 2. Module interfaces and responsibilities

### 2.1 Dependency direction

The intended dependency flow is:

```text
main + cli
    ├── config ──> lsp types + theme config
    ├── daemon ──> protocol + core highlighter
    └── core highlighter
            ├── Arborium tree-sitter/renderers
            ├── lsp ──> rpc
            ├── shared styled-token renderer
            ├── latex renderer
            └── typst renderer
```

The binary crate owns process policy and user interaction. The library crate
owns highlighting, configuration, daemon mechanics, themes, logging levels,
and language-server integration.

### 2.2 `src/main.rs`: process orchestration

`main.rs` is the binary composition root.

Responsibilities:

- parse the CLI and choose daemon-management or highlighting behavior;
- resolve startup config/theme values;
- read source through `cli`;
- decide between daemon-backed and one-shot highlighting;
- convert CLI options into library input/options;
- print output and map all errors to one-line stderr diagnostics and exit
  status.

Key private interfaces:

```rust
load_startup(&StartupArgs) -> Result<(config::Config, Theme)>
highlight_once(&Options, &str) -> Result<String>
run_daemon(DaemonAction) -> Result<()>
run_once(Options) -> Result<()>
```

It must not implement highlighting, LSP, rendering, or IPC framing itself.

### 2.3 `src/cli.rs`: command-line model and input acquisition

This binary-private module owns Clap declarations and CLI-specific conversion.

Primary types:

- `Interface`: parsed top-level arguments;
- `Command` and `DaemonAction`: daemon subcommand model;
- `StartupArgs`: options valid at startup and on `daemon spawn`;
- `Options`: validated normal-highlighting options;
- `CommandName` and `OptionName`: shared spellings used by declarations,
  tests, and daemon argument serialization.

Primary functions:

```rust
daemon_serve_arguments(&StartupArgs) -> Vec<OsString>
read_input(Option<&Path>) -> Result<String>
```

`TryFrom<Interface> for Options` resolves the language. This module owns
file/stdin errors and language-detection errors, but not config, theme,
highlighting, or daemon behavior.

### 2.4 `src/lib.rs`: core highlighting API

The library root defines the central data model:

```rust
type LangName = Rc<str>;

struct Input<'a> {
    source: &'a str,
    path: Option<&'a Path>,
    project: Option<&'a Path>,
    lang: LangName,
}

struct HighlightOptions {
    output: Output,
    lsp: bool,
    tree_sitter: bool,
    theme: Theme,
    lines: Option<LineRange>,
}

enum Output { Ansi, Html, Latex, Typst }
```

`LineRange` parses the public line-selection syntax and is serializable for
daemon requests.

`Highlighter` is the core service object:

```rust
Highlighter::new(Rc<RefCell<ServerRegistry>>) -> Highlighter
Highlighter::with_options(
    Rc<RefCell<ServerRegistry>>,
    HighlightOptions,
    LogLevel,
) -> Highlighter
Highlighter::set_options(&mut self, HighlightOptions)
Highlighter::highlight(&self, Input<'_>) -> Result<String>
```

Responsibilities:

- request enabled span sources;
- assign semantic priority relative to tree-sitter patterns;
- emit debug span diagnostics;
- merge spans;
- apply line selection; and
- dispatch to the selected renderer.

The registry is injected so one-shot callers can own a short-lived registry
while daemon sessions can share a long-lived one. The `RefCell` enforces mutable
registry access at runtime; reentrant borrowing is considered a programming
error.

### 2.5 `src/config.rs`: TOML-to-runtime configuration

Public interface:

```rust
struct Config {
    commands: lsp::Commands,
    general_mapping: lsp::CaptureMapping,
    lang_mapping: lsp::LangCaptureMapping,
    theme: Option<theme::Config>,
}

Config::load(Option<&Path>) -> Result<Config>
```

Responsibilities:

- load an explicit config or an empty raw config;
- deserialize supported TOML shapes;
- parse command strings into executable plus arguments;
- merge configured commands over built-in commands;
- separate general and per-language capture maps; and
- resolve a configured custom-theme path relative to the config file.

It returns structured configuration and domain-specific errors. It does not
load theme contents, start processes, or mutate an existing registry.

### 2.6 `src/theme.rs`: theme selection and loading

Public model:

```rust
enum Config {
    Builtin(String),
    Custom { path: PathBuf },
}
```

Public functions:

```rust
Config::from_options(Option<&str>, Option<&Path>) -> Result<Option<Config>>
load(
    Option<&str>,
    Option<&Path>,
    Option<&Config>,
) -> Result<Theme>
```

Responsibilities:

- represent serializable built-in/custom theme choices;
- canonicalize a CLI custom-theme path;
- resolve config-relative custom-theme paths;
- implement CLI-over-config-over-default precedence;
- find built-ins case-insensitively; and
- read and parse custom Arborium TOML themes.

The module owns the Catppuccin Mocha default and all theme-specific error
messages.

### 2.7 `src/lsp.rs`: semantic-highlighting service

Public configuration aliases:

```rust
type Commands = HashMap<LangName, CommandEntry>;
type CaptureMapping = HashMap<String, String>;
type LangCaptureMapping = HashMap<LangName, CaptureMapping>;
type ServerConfiguration = serde_json::Value;
```

`CommandEntry` holds an executable, direct arguments, and workspace
configuration. `default_commands()` constructs the built-in command table.

`ServerRegistry` is the process cache:

```rust
ServerRegistry::new(
    Commands,
    CaptureMapping,
    LangCaptureMapping,
    LogLevel,
) -> ServerRegistry

ServerRegistry::get_server(
    &mut self,
    LangName,
    Option<&Path>,
) -> Result<Server<'_>>
```

`Server` is a short-lived borrowed view over a cached client and the effective
capture mapping:

```rust
Server::get_semantic_spans(
    &self,
    source: &str,
    path: Option<&Path>,
    pattern_index: u32,
) -> Result<Vec<Span>>
```

Responsibilities:

- validate and model projects;
- key and lazily populate the process cache;
- spawn, initialize, and shut down language servers;
- manage real and temporary LSP documents;
- request full semantic tokens;
- convert file paths to percent-encoded file URIs;
- convert relative UTF-16 semantic tokens into UTF-8 byte spans; and
- apply capture mappings and span priorities.

It does not parse TOML, decide CLI precedence, merge span sources, select output
lines, or render output.

### 2.8 `src/lsp/rpc.rs`: synchronous JSON-RPC transport

This crate-private submodule provides `lsp` with a synchronous typed facade over
an LSP stdio connection.

The central interface is:

```rust
Connection::new(ChildStdout, ChildStdin, ServerConfiguration, LogLevel)
Connection::request<R: lsp_types::request::Request>(R::Params) -> Result<R::Result>
Connection::notify<N: lsp_types::notification::Notification>(N::Params) -> Result<()>
Connection::wait_for_progress(Duration, Duration) -> Result<()>
```

Responsibilities:

- frame and parse `Content-Length` JSON-RPC messages;
- read server stdout on a dedicated reader thread;
- allocate and verify monotonically increasing numeric request ids;
- interleave responses with supported server notifications and requests;
- answer workspace/configuration and UI/progress requests;
- filter server messages according to `LogLevel`; and
- detect transport, JSON, response, and protocol failures.

It is intentionally unaware of languages, documents, semantic-token
conversion, themes, and rendering.

### 2.9 `src/styled.rs`, `src/latex.rs`, and `src/typst.rs`: styled fragment renderers

The private LaTeX and Typst backends expose one crate-level function each:

```rust
spans_to_latex(source: &str, spans: Vec<Span>, theme: &Theme) -> String
spans_to_typst(source: &str, spans: Vec<Span>, theme: &Theme) -> String
```

Responsibilities:

- `styled` flattens overlapping Arborium spans, resolves token styles, preserves
  unstyled gaps, and removes trailing line feeds;
- `latex` translates styles into LaTeX commands and escapes `fvextra` command
  characters; and
- `typst` translates styles into Typst functions, encodes non-newline source as
  escaped `raw` string data, and emits source newlines as `linebreak` elements.

They do not create a complete document, package preamble, or cache.

### 2.10 `src/daemon.rs`: daemon process and session management

Public types:

```rust
struct Options {
    config: config::Config,
    theme: Theme,
    format: Output,
    log: LogLevel,
}

struct RequestOptions {
    config: Option<PathBuf>,
    output: Option<Output>,
    theme: Option<theme::Config>,
    lsp: bool,
    tree_sitter: bool,
    lines: Option<LineRange>,
}
```

Public operations:

```rust
spawn(&[OsString]) -> Result<()>
serve(Options) -> Result<()>
is_running() -> bool
highlight(Input<'_>, RequestOptions) -> Result<String>
kill() -> Result<()>
```

Responsibilities:

- discover platform-specific runtime paths;
- create and protect the runtime directory;
- enforce a singleton server with a lock;
- spawn and readiness-check the hidden server process;
- connect clients to the active endpoint;
- own long-lived session config, defaults, and server registry;
- apply per-request overrides;
- process connections sequentially; and
- remove the endpoint on server shutdown.

It delegates framing to `daemon::protocol` and highlighting to `Highlighter`.

### 2.11 `src/daemon/protocol.rs`: daemon IPC framing

This private submodule owns protocol versioning and serialization.

Its internal service boundary is:

```rust
exchange(stream, request_id, Input<'_>, RequestOptions) -> Result<String>
serve_connection(stream, highlight_callback) -> Result<bool>
```

The server callback accepts an `Input` plus `RequestOptions` and returns either
a rendered string or a displayable error. `serve_connection` returns `true`
only for an acknowledged stop request.

Responsibilities:

- serialize and parse request/response headers;
- read and write exact byte-counted bodies;
- validate protocol version, ids, UTF-8, and response shape;
- turn callback failures into protocol error responses; and
- recognize the reserved stop language.

It does not discover endpoints, manage sessions, or perform highlighting.

### 2.12 `src/logging.rs`: shared verbosity policy

`LogLevel` is an ordered enum:

```rust
Error < Warn < Info < Debug
```

`includes(required)` implements cumulative filtering, and `as_str()` provides
the stable uppercase name. The module defines policy vocabulary only; LSP RPC
and the core highlighter decide which events belong at each level.

## 3. Cross-module invariants

- All source offsets in Arborium spans and rendered selections are UTF-8 byte
  offsets. UTF-16 exists only at the LSP boundary.
- A source document is analyzed in full; selection is a render-stage concern.
- Process reuse belongs to `ServerRegistry`; neither the CLI nor `Highlighter`
  owns language-server command selection.
- Config and custom-theme paths on normal daemon-backed requests are made
  absolute before crossing the daemon boundary. A spawned daemon inherits the
  spawning process's working directory and re-loads its startup paths there.
- Config parsing produces values; only daemon session management decides
  whether a changed config requires replacing live processes.
- Renderers receive already merged, clipped, and rebased spans.
- Transport modules return data or structured errors and do not print
  user-facing diagnostics directly, except for explicitly configured
  language-server messages and debug logging.
- The auxiliary package under `latex/` is a consumer of the binary's LaTeX
  fragment contract. Its cache and TeX environment behavior are outside the
  Rust binary and daemon interfaces specified here.
