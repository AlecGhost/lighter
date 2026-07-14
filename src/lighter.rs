use std::path::Path;

use arborium::advanced::{Span, spans_to_ansi, spans_to_html};
use arborium::theme::builtin;
use lsp_types::{SemanticToken, SemanticTokenType, SemanticTokensLegend};
use thiserror::Error;

use crate::lsp::{self, LangName};

const CAPTURE_ATTRIBUTE: &str = "attribute";
const CAPTURE_COMMENT: &str = "comment";
const CAPTURE_CONSTANT: &str = "constant";
const CAPTURE_FUNCTION: &str = "function";
const CAPTURE_FUNCTION_MACRO: &str = "function.macro";
const CAPTURE_FUNCTION_METHOD: &str = "function.method";
const CAPTURE_KEYWORD: &str = "keyword";
const CAPTURE_KEYWORD_MODIFIER: &str = "keyword.modifier";
const CAPTURE_NAMESPACE: &str = "namespace";
const CAPTURE_NUMBER: &str = "number";
const CAPTURE_OPERATOR: &str = "operator";
const CAPTURE_PROPERTY: &str = "property";
const CAPTURE_STRING: &str = "string";
const CAPTURE_STRING_REGEXP: &str = "string.regexp";
const CAPTURE_TYPE: &str = "type";
const CAPTURE_TYPE_PARAMETER: &str = "type.parameter";
const CAPTURE_VARIABLE: &str = "variable";
const CAPTURE_VARIABLE_PARAMETER: &str = "variable.parameter";

#[derive(Debug, Default, Clone, clap::ValueEnum)]
pub enum Output {
    #[default]
    Ansi,
    Html,
}

pub struct HighlightOptions {
    pub output: Output,
    pub lsp: bool,
    pub tree_sitter: bool,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Arborium(#[from] arborium::Error),
    #[error(transparent)]
    Server(#[from] lsp::Error),
}

type Result<T> = std::result::Result<T, Error>;

/// Highlight source with tree-sitter syntax and LSP semantic information.
pub fn highlight(
    input: &str,
    path: Option<&Path>,
    lang: LangName,
    registry: &mut lsp::ServerRegistry,
    options: HighlightOptions,
) -> Result<String> {
    let mut spans = if options.tree_sitter {
        arborium::Highlighter::new().highlight_spans(&lang, input)?
    } else {
        Vec::new()
    };

    if options.lsp
        && let Some((tokens, legend)) = registry
            .get_server(lang)?
            .get_semantic_tokens(input, path)?
    {
        let pattern_index = next_pattern_index(&spans);
        spans.extend(semantic_tokens_to_spans(
            input,
            &tokens,
            &legend,
            pattern_index,
        ));
    }

    Ok(render(input, spans, options.output))
}

fn next_pattern_index(spans: &[Span]) -> u32 {
    spans
        .iter()
        .map(|span| span.pattern_index)
        .max()
        .unwrap_or_default()
        .saturating_add(1)
}

fn semantic_tokens_to_spans(
    source: &str,
    tokens: &[SemanticToken],
    legend: &SemanticTokensLegend,
    pattern_index: u32,
) -> Vec<Span> {
    let lines = LineIndex::new(source);
    let mut line = 0_u32;
    let mut character = 0_u32;

    tokens
        .iter()
        .filter_map(|token| {
            line = line.checked_add(token.delta_line)?;
            character = if token.delta_line == 0 {
                character.checked_add(token.delta_start)?
            } else {
                token.delta_start
            };

            let token_type = legend.token_types.get(token.token_type as usize)?;
            let (start, end) = lines.byte_range(line, character, token.length)?;

            Some(Span {
                start: u32::try_from(start).ok()?,
                end: u32::try_from(end).ok()?,
                capture: capture_for_token_type(token_type).to_owned(),
                pattern_index,
            })
        })
        .collect()
}

fn capture_for_token_type(token_type: &SemanticTokenType) -> &str {
    match token_type {
        token_type if token_type == &SemanticTokenType::NAMESPACE => CAPTURE_NAMESPACE,
        token_type
            if token_type == &SemanticTokenType::TYPE
                || token_type == &SemanticTokenType::CLASS
                || token_type == &SemanticTokenType::ENUM
                || token_type == &SemanticTokenType::INTERFACE
                || token_type == &SemanticTokenType::STRUCT =>
        {
            CAPTURE_TYPE
        }
        token_type if token_type == &SemanticTokenType::TYPE_PARAMETER => CAPTURE_TYPE_PARAMETER,
        token_type if token_type == &SemanticTokenType::PARAMETER => CAPTURE_VARIABLE_PARAMETER,
        token_type if token_type == &SemanticTokenType::VARIABLE => CAPTURE_VARIABLE,
        token_type
            if token_type == &SemanticTokenType::PROPERTY
                || token_type == &SemanticTokenType::EVENT =>
        {
            CAPTURE_PROPERTY
        }
        token_type if token_type == &SemanticTokenType::ENUM_MEMBER => CAPTURE_CONSTANT,
        token_type if token_type == &SemanticTokenType::FUNCTION => CAPTURE_FUNCTION,
        token_type if token_type == &SemanticTokenType::METHOD => CAPTURE_FUNCTION_METHOD,
        token_type if token_type == &SemanticTokenType::MACRO => CAPTURE_FUNCTION_MACRO,
        token_type if token_type == &SemanticTokenType::KEYWORD => CAPTURE_KEYWORD,
        token_type if token_type == &SemanticTokenType::MODIFIER => CAPTURE_KEYWORD_MODIFIER,
        token_type if token_type == &SemanticTokenType::COMMENT => CAPTURE_COMMENT,
        token_type if token_type == &SemanticTokenType::STRING => CAPTURE_STRING,
        token_type if token_type == &SemanticTokenType::NUMBER => CAPTURE_NUMBER,
        token_type if token_type == &SemanticTokenType::REGEXP => CAPTURE_STRING_REGEXP,
        token_type if token_type == &SemanticTokenType::OPERATOR => CAPTURE_OPERATOR,
        token_type if token_type == &SemanticTokenType::DECORATOR => CAPTURE_ATTRIBUTE,
        token_type => token_type.as_str(),
    }
}

fn render(source: &str, spans: Vec<Span>, output: Output) -> String {
    match output {
        Output::Ansi => spans_to_ansi(source, spans, &builtin::catppuccin_mocha()),
        Output::Html => spans_to_html(source, spans, &arborium::HtmlFormat::default()),
    }
}

struct LineIndex<'source> {
    source: &'source str,
    starts: Vec<usize>,
}

impl<'source> LineIndex<'source> {
    fn new(source: &'source str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            source
                .bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Self { source, starts }
    }

    fn byte_range(&self, line: u32, character: u32, length: u32) -> Option<(usize, usize)> {
        let line = usize::try_from(line).ok()?;
        let start = *self.starts.get(line)?;
        let mut end = self
            .starts
            .get(line + 1)
            .copied()
            .unwrap_or(self.source.len());

        if self.source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\n') {
            end -= 1;
        }
        if self.source.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
            end -= 1;
        }

        let line_source = self.source.get(start..end)?;
        let token_end = character.checked_add(length)?;
        let relative_start = utf16_column_to_byte(line_source, character)?;
        let relative_end = utf16_column_to_byte(line_source, token_end)?;

        (relative_start < relative_end).then(|| (start + relative_start, start + relative_end))
    }
}

fn utf16_column_to_byte(line: &str, column: u32) -> Option<usize> {
    let mut utf16_column = 0_u32;

    for (byte, character) in line.char_indices() {
        if utf16_column == column {
            return Some(byte);
        }

        utf16_column = utf16_column.checked_add(character.len_utf16() as u32)?;
        if utf16_column > column {
            return None;
        }
    }

    (utf16_column == column).then_some(line.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "let 😀 = value;\r\ncall(😀);";
    const SEMANTIC_PATTERN_INDEX: u32 = 42;

    fn token(delta_line: u32, delta_start: u32, length: u32, token_type: u32) -> SemanticToken {
        SemanticToken {
            delta_line,
            delta_start,
            length,
            token_type,
            token_modifiers_bitset: 0,
        }
    }

    fn legend(token_types: Vec<SemanticTokenType>) -> SemanticTokensLegend {
        SemanticTokensLegend {
            token_types,
            token_modifiers: Vec::new(),
        }
    }

    #[test]
    fn converts_delta_encoded_utf16_positions_to_byte_spans() {
        let tokens = [token(0, 4, 2, 0), token(0, 5, 5, 1), token(1, 0, 4, 1)];
        let legend = legend(vec![
            SemanticTokenType::VARIABLE,
            SemanticTokenType::FUNCTION,
        ]);

        let spans = semantic_tokens_to_spans(SOURCE, &tokens, &legend, SEMANTIC_PATTERN_INDEX);

        assert_eq!(
            spans,
            vec![
                Span {
                    start: 4,
                    end: 8,
                    capture: CAPTURE_VARIABLE.to_owned(),
                    pattern_index: SEMANTIC_PATTERN_INDEX,
                },
                Span {
                    start: 11,
                    end: 16,
                    capture: CAPTURE_FUNCTION.to_owned(),
                    pattern_index: SEMANTIC_PATTERN_INDEX,
                },
                Span {
                    start: 19,
                    end: 23,
                    capture: CAPTURE_FUNCTION.to_owned(),
                    pattern_index: SEMANTIC_PATTERN_INDEX,
                },
            ]
        );
    }

    #[test]
    fn ignores_tokens_with_invalid_ranges_or_legend_indices() {
        let tokens = [token(0, 5, 1, 0), token(0, 4, 2, 1), token(10, 0, 1, 0)];
        let legend = legend(vec![SemanticTokenType::VARIABLE]);

        let spans = semantic_tokens_to_spans(SOURCE, &tokens, &legend, SEMANTIC_PATTERN_INDEX);

        assert!(spans.is_empty());
    }

    #[test]
    fn semantic_pattern_index_follows_tree_sitter_patterns() {
        let spans = [Span {
            start: 0,
            end: 1,
            capture: CAPTURE_VARIABLE.to_owned(),
            pattern_index: SEMANTIC_PATTERN_INDEX,
        }];

        assert_eq!(next_pattern_index(&spans), SEMANTIC_PATTERN_INDEX + 1);
    }
}
