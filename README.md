# Lighter

Lighter is a command-line source-code highlighter. It combines
[Arborium](https://github.com/bearcove/arborium)'s tree-sitter syntax
highlighting with semantic tokens from language servers, then writes the result
as terminal colors, HTML, or LaTeX.

Use it to inspect highlighted code in a terminal, generate highlighted
fragments for other tools, or add semantic highlighting to scripts and editor
workflows.

See the [live highlighter comparison](https://alecghost.github.io/lighter/) for
side-by-side Highlight.js, Arborium, and Lighter output.

## Features

- Syntax highlighting for the languages supported by Arborium
- Optional semantic highlighting through the Language Server Protocol (LSP)
- ANSI, HTML, and LaTeX output
- Built-in and custom themes
- Inclusive line selection without losing whole-file semantic context
- A background daemon that keeps language servers warm between invocations
- File input with language detection, or standard input with an explicit
  language

## Installation

Lighter is currently installed from source. It requires a recent stable Rust
toolchain.

```sh
git clone https://github.com/AlecGhost/lighter.git
cd lighter
cargo install --path .
```

Language servers are separate programs. Install the server for the language
whose semantic highlighting you want.

## Quick start

If the matching language server is installed, include semantic information and
expose the current directory as its workspace:

```sh
lighter --project . src/main.rs
```

When input comes from standard input, specify the language:

```sh
printf 'fn main() {}\n' | lighter --lang rust
```

## Usage

```text
lighter [OPTIONS] [FILE]
lighter daemon <COMMAND>
```

`FILE` is optional. Lighter infers the language from a file's extension. With
standard input, `--lang` is required.

The most useful options are:

| Option | Purpose |
| --- | --- |
| `-l, --lang <LANG>` | Set the language instead of detecting it |
| `-p, --project <DIR>` | Use a directory as the language server workspace |
| `-f, --format <FORMAT>` | Select `ansi`, `html`, or `latex` |
| `--lines <RANGE>` | Render an inclusive, one-based line range |
| `--theme <THEME>` | Select a built-in theme |
| `--custom-theme <FILE>` | Load an Arborium TOML theme |
| `-c, --config <FILE>` | Load a Lighter TOML configuration |

Run `lighter --help` for the complete interface and the available built-in
themes.

### Select lines

Ranges are inclusive and one-based:

```sh
lighter --lines 10:20 src/main.rs
lighter --lines :20 src/main.rs
lighter --lines 10: src/main.rs
```

Lighter still analyzes the complete source before selecting the requested
lines. This lets a language server use declarations and imports outside the
rendered range.

### Output formats

`ansi` is the default and emits terminal color escape sequences. For paging,
use a pager that preserves colors:

```sh
lighter  src/main.rs | less -R
```

`html` emits an HTML fragment using Arborium custom elements such as `<a-k>`
and `<a-f>`. It does not emit a complete page or CSS, so the embedding page
must style those elements.

```sh
lighter --format html src/main.rs > snippet.html
```

#### Arborium comparison

The following HTML compares the same [Rust source](examples/html-comparison/source.rs)
twice:

```sh
arborium --html --lang rust examples/html-comparison/source.rs
lighter --format html --project . examples/html-comparison/source.rs
```

The first snippet contains Arborium's tree-sitter spans. The second contains
Lighter's merged tree-sitter and rust-analyzer semantic spans. Both use
Arborium's checked-in
[`base-rustdoc.css`](examples/html-comparison/arborium-base.css) and
[`catppuccin-mocha.css`](examples/html-comparison/catppuccin-mocha.css), copied
unchanged from `packages/arborium/src/themes`.

<style>
@import url("examples/html-comparison/arborium-base.css");
@import url("examples/html-comparison/catppuccin-mocha.css");

.html-comparison pre {
  min-width: 34rem;
  margin: 0;
  padding: 1rem;
  overflow: auto;
  background: var(--arb-bg-dark);
  color: var(--arb-fg-dark);
  line-height: 1.5;
}
</style>

<table class="html-comparison">
  <thead>
    <tr>
      <th>Arborium · tree-sitter</th>
      <th>Lighter · tree-sitter + rust-analyzer</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td>
        <pre><code><a-k>use</a-k> std<a-p>::</a-p>fmt<a-p>::</a-p><a-cr>Display</a-cr><a-p>;</a-p>

<a-at>#</a-at><a-p>[</a-p><a-at>derive</a-at><a-p>(</a-p><a-cr>Debug</a-cr><a-p>)]</a-p>
<a-k>struct</a-k> <a-t>User</a-t> <a-p>{</a-p>
    <a-pr>name</a-pr><a-p>:</a-p> <a-t>String</a-t><a-p>,</a-p>
<a-p>}</a-p>

<a-k>impl</a-k> <a-t>User</a-t> <a-p>{</a-p>
    <a-k>fn</a-k> <a-f>label</a-f><a-p>&lt;</a-p><a-t>T</a-t><a-p>:</a-p> <a-t>Display</a-t><a-p>&gt;(</a-p><a-o>&amp;</a-o><a-v>self</a-v><a-p>,</a-p> <a-v>prefix</a-v><a-p>:</a-p> <a-t>T</a-t><a-p>)</a-p> -&gt; <a-t>String</a-t> <a-p>{</a-p>
        <a-m>format!</a-m><a-p>(</a-p><a-s>&quot;{prefix}: {}&quot;</a-s><a-p>,</a-p> <a-v>self</a-v><a-p>.</a-p><a-pr>name</a-pr><a-p>)</a-p>
    <a-p>}</a-p>
<a-p>}</a-p>

<a-k>fn</a-k> <a-f>main</a-f><a-p>()</a-p> <a-p>{</a-p>
    <a-k>let</a-k> user = <a-t>User</a-t> <a-p>{</a-p>
        <a-pr>name</a-pr><a-p>:</a-p> <a-s>&quot;Ada&quot;</a-s><a-p>.</a-p><a-f>to_owned</a-f><a-p>(),</a-p>
    <a-p>};</a-p>
    <a-m>println!</a-m><a-p>(</a-p><a-s>&quot;{}&quot;</a-s><a-p>,</a-p> user<a-p>.</a-p><a-f>label</a-f><a-p>(</a-p><a-s>&quot;user&quot;</a-s><a-p>));</a-p>
<a-p>}</a-p></code></pre>
      </td>
      <td>
        <pre><code><a-k>use</a-k> std<a-o>::</a-o>fmt<a-o>::</a-o><a-cr>Display</a-cr><a-p>;</a-p>

<a-at>#</a-at><a-p>[</a-p><a-at>derive</a-at><a-p>(</a-p><a-cr>Debug</a-cr><a-p>)]</a-p>
<a-k>struct</a-k> <a-t>User</a-t> <a-p>{</a-p>
    <a-pr>name</a-pr><a-p>:</a-p> <a-t>String</a-t><a-p>,</a-p>
<a-p>}</a-p>

<a-k>impl</a-k> <a-t>User</a-t> <a-p>{</a-p>
    <a-k>fn</a-k> <a-f>label</a-f><a-p>&lt;</a-p><a-t>T</a-t><a-p>:</a-p> <a-t>Display</a-t><a-p>&gt;(</a-p><a-o>&amp;</a-o><a-v>self</a-v><a-p>,</a-p> <a-v>prefix</a-v><a-p>:</a-p> <a-t>T</a-t><a-p>)</a-p> <a-o>-&gt;</a-o> <a-t>String</a-t> <a-p>{</a-p>
        <a-m>format!</a-m><a-p>(</a-p><a-s>&quot;{prefix}: {}&quot;</a-s><a-p>,</a-p> <a-k>self</a-k><a-o>.</a-o><a-pr>name</a-pr><a-p>)</a-p>
    <a-p>}</a-p>
<a-p>}</a-p>

<a-k>fn</a-k> <a-f>main</a-f><a-p>()</a-p> <a-p>{</a-p>
    <a-k>let</a-k> <a-v>user</a-v> <a-o>=</a-o> <a-t>User</a-t> <a-p>{</a-p>
        <a-pr>name</a-pr><a-p>:</a-p> <a-s>&quot;Ada&quot;</a-s><a-o>.</a-o><a-f>to_owned</a-f><a-p>(),</a-p>
    <a-p>};</a-p>
    <a-m>println!</a-m><a-p>(</a-p><a-s>&quot;{}&quot;</a-s><a-p>,</a-p> user<a-o>.</a-o><a-f>label</a-f><a-p>(</a-p><a-s>&quot;user&quot;</a-s><a-p>));</a-p>
<a-p>}</a-p></code></pre>
      </td>
    </tr>
  </tbody>
</table>

Lighter's semantic spans distinguish operators such as `::`, `->`, `=`, and
member access, and classify semantic identifiers such as `self`.

`latex` emits commands for an `fvextra` `Verbatim` environment with `xcolor`
enabled and `commandchars=\\\{\}`:

```sh
lighter --format latex src/main.rs > snippet.tex
```

A minimal wrapper looks like this:

```latex
\documentclass{article}
\usepackage{xcolor}
\usepackage{fvextra}
\begin{document}
\begin{Verbatim}[commandchars=\\\{\}]
\input{snippet.tex}
\end{Verbatim}
\end{document}
```

## Semantic highlighting

Lighter enables both tree-sitter and LSP highlighting by default. It lazily
starts a language server the first time a language is used. The server must be
available on `PATH` and support full-document semantic tokens.

Built-in server commands are provided for:

| Languages | Command |
| --- | --- |
| Rust | `rust-analyzer` |
| Python | `basedpyright-langserver --stdio` |
| JavaScript, JSX, TypeScript, TSX | `typescript-language-server --stdio` |
| C, C++ | `clangd` |
| Go | `gopls` |
| Java | `jdtls` |
| Lua | `lua-language-server` |
| Zig | `zls` |
| Ruby | `ruby-lsp` |
| Kotlin | `kotlin-lsp` |
| Swift | `sourcekit-lsp` |
| Haskell | `haskell-language-server-wrapper --lsp` |
| OCaml | `ocamllsp` |
| Dart | `dart language-server` |

If no server is configured, its executable is missing, or it does not provide
semantic tokens, Lighter reports an error.

Pass the source file whenever possible so the language server sees its real
path. For standard input, Lighter creates a temporary document. Use
`--project <DIR>` when the server needs project configuration, dependencies, or
other workspace files.

## Configuration

If you want to supply your own servers or add custom capture mappings, use
`--config`:

```sh
lighter --config lighter.toml src/main.rs
```

A configuration can choose a theme, add or replace language-server commands,
provide server settings, and map LSP token names to Arborium captures:

```toml
theme = "Tokyo Night"

[servers]
gleam = "gleam lsp"

[servers.go]
command = "gopls"
config = { gopls = { semanticTokens = true } }

[captures]
parameter = "variable.parameter"

[captures.rust]
const = "constant"
decorator = "constant"
```

Server command strings use shell-like quoting but are executed directly, not
through a shell.

A custom theme can be selected relative to the configuration file:

```toml
theme = { path = "theme.toml" }
```

Command-line theme options override the configured theme:

```sh
lighter --config lighter.toml --theme "GitHub Light" src/main.rs
lighter --custom-theme ./theme.toml src/main.rs
```

Without a configured or command-line theme, Lighter uses Catppuccin Mocha.

## Daemon

Starting a daemon keeps language-server processes alive, avoiding their startup
cost on every invocation:

```sh
lighter daemon spawn
lighter --project . src/main.rs
lighter daemon kill
```

Normal invocations discover and use the daemon automatically. Startup options
set its defaults:

```sh
lighter daemon spawn \
  --config lighter.toml \
  --theme "Tokyo Night" \
  --format ansi
```

Per-invocation `--format`, `--theme`, and `--custom-theme` options
override the daemon defaults. Passing `--config`
updates the active server and capture configuration for that daemon session.
Pass a theme option explicitly when changing themes on a running daemon.

## Troubleshooting

**`No language server available for …`**

The language has no built-in or configured server. Add an entry under
`[servers]`, or pass ``.

**`Failed to start server for …`**

Install the configured language server and make sure its executable is on
`PATH`.

**The language cannot be detected**

Use an explicit language:

```sh
lighter --lang rust path/to/source
```

**The language server does not understand the project**

Pass its workspace directory:

```sh
lighter --project path/to/project path/to/project/src/main.rs
```
