use std::path::Path;
use thiserror::Error;

use crate::lsp::{self, LangName};

#[derive(Debug, Default, Clone, clap::ValueEnum)]
pub enum Output {
    #[default]
    ANSI,
    HTML,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error("{0}")]
    ServerError(lsp::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// The highlight pipeline.
pub fn highlight(
    input: &str,
    path: Option<&Path>,
    lang: LangName,
    registry: &mut lsp::ServerRegistry,
    output: Output,
) -> Result<String> {
    let server = registry.get_server(lang).map_err(Error::ServerError)?;
    let tokens = server
        .get_semantic_tokens(input, path)
        .map_err(Error::ServerError)?;
    //
    // Stub: In the full implementation, this will:
    // 1. Parse source code using arborium advanced module
    // 2. Recurse into language injections and offset-adjust resulting spans.
    // 3. Map LSP tokens to arborium capture names with higher pattern_index and merge spans.
    // 4. Render the merged spans to ANSI (`spans_to_ansi`) or HTML (`spans_to_html`).

    todo!()
}
