use std::collections::HashMap;
use std::path::Path;

use arborium::advanced::{Span, spans_to_ansi, spans_to_html};
use arborium::theme::{Theme, builtin};
use lsp_types::{SemanticToken, SemanticTokenType, SemanticTokensLegend};
use thiserror::Error;

use crate::lsp::{self, LangName};

const TREE_SITTER_SPANS_HEADING: &str = "Tree-sitter spans:";
const LSP_SPANS_HEADING: &str = "LSP spans:";

#[derive(Debug, Default, serde::Deserialize)]
pub struct CaptureMappings {
    #[serde(flatten)]
    mappings: HashMap<String, CaptureMapping>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum CaptureMapping {
    Capture(String),
    Language(HashMap<String, String>),
}

impl CaptureMappings {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, capture: &str, language: &str) -> Option<&str> {
        self.mappings
            .get(language)
            .and_then(|mapping| match mapping {
                CaptureMapping::Language(captures) => captures.get(capture),
                CaptureMapping::Capture(_) => None,
            })
            .or_else(|| match self.mappings.get(capture) {
                Some(CaptureMapping::Capture(capture)) => Some(capture),
                Some(CaptureMapping::Language(_)) | None => None,
            })
            .map(String::as_str)
    }
}

#[derive(Debug, Default, Clone, Copy, clap::ValueEnum)]
pub enum Output {
    #[default]
    Ansi,
    Html,
    #[value(alias = "spans")]
    Debug,
}

pub struct HighlightOptions {
    pub output: Output,
    pub lsp: bool,
    pub tree_sitter: bool,
    pub theme: Theme,
    pub captures: CaptureMappings,
}

#[derive(Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Arborium(#[from] arborium::Error),
    #[error(transparent)]
    Server(#[from] lsp::Error),
}

type Result<T> = std::result::Result<T, Error>;

pub fn default_theme() -> Theme {
    builtin::catppuccin_mocha()
}

/// Highlight source with tree-sitter syntax and LSP semantic information.
pub fn highlight(
    input: &str,
    path: Option<&Path>,
    lang: LangName,
    registry: &mut lsp::ServerRegistry,
    options: &HighlightOptions,
) -> Result<String> {
    let tree_sitter_spans = if options.tree_sitter {
        // TODO: get injections and add lsp highlighting for injected languages
        arborium::Highlighter::new().highlight_spans(&lang, input)?
    } else {
        Vec::new()
    };

    let lsp_spans = if options.lsp
        && let Some((tokens, legend)) = registry
            .get_server(lang.clone())?
            .get_semantic_tokens(input, path)?
    {
        let pattern_index = next_pattern_index(&tree_sitter_spans);
        semantic_tokens_to_spans(
            input,
            &tokens,
            &legend,
            pattern_index,
            &lang,
            &options.captures,
        )
    } else {
        Vec::new()
    };

    Ok(render(
        input,
        tree_sitter_spans,
        lsp_spans,
        options.output,
        &options.theme,
    ))
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
    language: &str,
    captures: &CaptureMappings,
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
                capture: capture_for_token_type(token_type, language, captures).to_owned(),
                pattern_index,
            })
        })
        .collect()
}

fn capture_for_token_type<'a>(
    token_type: &'a SemanticTokenType,
    language: &str,
    captures: &'a CaptureMappings,
) -> &'a str {
    captures
        .get(token_type.as_str(), language)
        .unwrap_or_else(|| default_capture_for_token_type(token_type))
}

fn default_capture_for_token_type(token_type: &SemanticTokenType) -> &str {
    match token_type {
        token_type if arborium_theme::CAPTURE_NAMES.contains(&token_type.as_str()) => {
            token_type.as_str()
        }
        token_type
            if token_type == &SemanticTokenType::CLASS
                || token_type == &SemanticTokenType::INTERFACE
                || token_type == &SemanticTokenType::STRUCT =>
        {
            "type"
        }
        token_type if token_type == &SemanticTokenType::TYPE_PARAMETER => "type.parameter",
        token_type if token_type == &SemanticTokenType::PARAMETER => "variable.parameter",
        token_type if token_type == &SemanticTokenType::ENUM => "type.enum",
        token_type if token_type == &SemanticTokenType::ENUM_MEMBER => "type.enum.variant",
        token_type if token_type == &SemanticTokenType::MODIFIER => "keyword.modifier",
        token_type if token_type == &SemanticTokenType::REGEXP => "string.regexp",
        // NOTE: currently unsupported default lsp token types are 'event' and 'decorator'
        token_type => token_type.as_str(),
    }
}

fn render(
    source: &str,
    tree_sitter_spans: Vec<Span>,
    lsp_spans: Vec<Span>,
    output: Output,
    theme: &Theme,
) -> String {
    match output {
        Output::Ansi => spans_to_ansi(source, merge_spans(tree_sitter_spans, lsp_spans), theme),
        Output::Html => spans_to_html(
            source,
            merge_spans(tree_sitter_spans, lsp_spans),
            &arborium::HtmlFormat::default(),
        ),
        Output::Debug => render_debug_spans(source, tree_sitter_spans, lsp_spans),
    }
}

fn merge_spans(mut tree_sitter_spans: Vec<Span>, lsp_spans: Vec<Span>) -> Vec<Span> {
    tree_sitter_spans.extend(lsp_spans);
    tree_sitter_spans
}

fn render_debug_spans(
    source: &str,
    mut tree_sitter_spans: Vec<Span>,
    mut lsp_spans: Vec<Span>,
) -> String {
    sort_spans_by_position(&mut tree_sitter_spans);
    sort_spans_by_position(&mut lsp_spans);

    let tree_sitter_lines = format_span_lines(source, &tree_sitter_spans);
    let lsp_lines = format_span_lines(source, &lsp_spans);
    format!("{TREE_SITTER_SPANS_HEADING}\n{tree_sitter_lines}\n\n{LSP_SPANS_HEADING}\n{lsp_lines}")
}

fn sort_spans_by_position(spans: &mut [Span]) {
    spans.sort_by_key(|span| (span.start, span.end));
}

fn format_span_lines(source: &str, spans: &[Span]) -> String {
    spans
        .iter()
        .map(|span| {
            let text = source
                .get(span.start as usize..span.end as usize)
                .unwrap_or_default();
            format!("{span:?} {text:?}")
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    use arborium_theme::ThemeSlot;

use super::*;

    const SOURCE: &str = "let 😀 = value;\r\ncall(😀);";
    const SEMANTIC_PATTERN_INDEX: u32 = 42;
    const CUSTOM_LSP_TOKEN_TYPE: &str = "const";
    const CUSTOM_CAPTURE: &str = "constant";
    const CUSTOM_LANGUAGE: &str = "rust";
    const OTHER_LANGUAGE: &str = "python";
    const CUSTOM_LANGUAGE_CAPTURE: &str = "constant.rust";
    const CUSTOM_CAPTURES: &str = r#"
const = "constant"

[rust]
const = "constant.rust"
"#;
    const LANGUAGE_ONLY_CAPTURES: &str = r#"
[rust]
const = "constant"
"#;
    const THEME_SOURCE: &str = r##"
name = "Test theme"
variant = "light"

"keyword" = { fg = "mauve" }

[palette]
mauve = "#010203"
"##;

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

    fn semantic_spans(
        tokens: &[SemanticToken],
        legend: &SemanticTokensLegend,
        language: &str,
        captures: &CaptureMappings,
    ) -> Vec<Span> {
        semantic_tokens_to_spans(
            SOURCE,
            tokens,
            legend,
            SEMANTIC_PATTERN_INDEX,
            language,
            captures,
        )
    }

    fn custom_captures() -> CaptureMappings {
        toml::from_str(CUSTOM_CAPTURES).unwrap()
    }

    #[test]
    fn converts_delta_encoded_utf16_positions_to_byte_spans() {
        let tokens = [token(0, 4, 2, 0), token(0, 5, 5, 1), token(1, 0, 4, 1)];
        let legend = legend(vec![
            SemanticTokenType::VARIABLE,
            SemanticTokenType::FUNCTION,
        ]);

        let spans = semantic_spans(&tokens, &legend, CUSTOM_LANGUAGE, &CaptureMappings::new());

        assert_eq!(
            spans,
            vec![
                Span {
                    start: 4,
                    end: 8,
                    capture: ThemeSlot::Variable.name().unwrap().to_owned(),
                    pattern_index: SEMANTIC_PATTERN_INDEX,
                },
                Span {
                    start: 11,
                    end: 16,
                    capture: ThemeSlot::Function.name().unwrap().to_owned().to_owned(),
                    pattern_index: SEMANTIC_PATTERN_INDEX,
                },
                Span {
                    start: 19,
                    end: 23,
                    capture: ThemeSlot::Function.name().unwrap().to_owned().to_owned(),
                    pattern_index: SEMANTIC_PATTERN_INDEX,
                },
            ]
        );
    }

    #[test]
    fn ignores_tokens_with_invalid_ranges_or_legend_indices() {
        let tokens = [token(0, 5, 1, 0), token(0, 4, 2, 1), token(10, 0, 1, 0)];
        let legend = legend(vec![SemanticTokenType::VARIABLE]);

        let spans = semantic_spans(&tokens, &legend, CUSTOM_LANGUAGE, &CaptureMappings::new());

        assert!(spans.is_empty());
    }

    #[test]
    fn applies_configured_lsp_capture_mapping() {
        let tokens = [token(0, 0, 3, 0)];
        let legend = legend(vec![SemanticTokenType::new(CUSTOM_LSP_TOKEN_TYPE)]);
        let captures = custom_captures();

        let spans = semantic_spans(&tokens, &legend, OTHER_LANGUAGE, &captures);

        assert_eq!(spans[0].capture, CUSTOM_CAPTURE);
    }

    #[test]
    fn language_capture_mapping_takes_precedence_over_global_mapping() {
        let tokens = [token(0, 0, 3, 0)];
        let legend = legend(vec![SemanticTokenType::new(CUSTOM_LSP_TOKEN_TYPE)]);
        let captures = custom_captures();

        let spans = semantic_spans(&tokens, &legend, CUSTOM_LANGUAGE, &captures);

        assert_eq!(spans[0].capture, CUSTOM_LANGUAGE_CAPTURE);
    }

    #[test]
    fn language_capture_mapping_only_applies_to_that_language() {
        let tokens = [token(0, 0, 3, 0)];
        let legend = legend(vec![SemanticTokenType::new(CUSTOM_LSP_TOKEN_TYPE)]);
        let captures: CaptureMappings = toml::from_str(LANGUAGE_ONLY_CAPTURES).unwrap();

        let language_spans = semantic_spans(&tokens, &legend, CUSTOM_LANGUAGE, &captures);
        let other_spans = semantic_spans(&tokens, &legend, OTHER_LANGUAGE, &captures);

        assert_eq!(language_spans[0].capture, CUSTOM_CAPTURE);
        assert_eq!(other_spans[0].capture, CUSTOM_LSP_TOKEN_TYPE);
    }

    #[test]
    fn semantic_pattern_index_follows_tree_sitter_patterns() {
        let spans = [Span {
            start: 0,
            end: 1,
            capture: ThemeSlot::Variable.name().unwrap().to_owned().to_owned(),
            pattern_index: SEMANTIC_PATTERN_INDEX,
        }];

        assert_eq!(next_pattern_index(&spans), SEMANTIC_PATTERN_INDEX + 1);
    }

    #[test]
    fn applies_parsed_theme_colors() {
        let theme = Theme::from_toml(THEME_SOURCE).unwrap();
        let source = "let";
        let spans = vec![Span {
            start: 0,
            end: source.len() as u32,
            capture: ThemeSlot::Keyword.name().unwrap().to_owned().to_owned(),
            pattern_index: 0,
        }];

        let rendered = render(source, spans, Vec::new(), Output::Ansi, &theme);

        assert!(rendered.contains("\u{1b}[38;2;1;2;3m"));
    }

    #[test]
    fn debug_output_separates_and_sorts_span_sources() {
        const DEBUG_SOURCE: &str = "let call value";
        let tree_sitter_spans = vec![
            span(9, 14, ThemeSlot::Variable.name().unwrap(), 1),
            span(0, 3, ThemeSlot::Keyword.name().unwrap(), 0),
        ];
        let lsp_spans = vec![
            span(9, 14, ThemeSlot::Variable.name().unwrap(), SEMANTIC_PATTERN_INDEX),
            span(4, 8, ThemeSlot::Function.name().unwrap(), SEMANTIC_PATTERN_INDEX),
        ];

        let rendered = render_debug_spans(DEBUG_SOURCE, tree_sitter_spans, lsp_spans);
        let expected = [
            TREE_SITTER_SPANS_HEADING.to_owned(),
            format!("{:?} {:?}", span(0, 3, ThemeSlot::Keyword.name().unwrap(), 0), "let"),
            format!("{:?} {:?}", span(9, 14, ThemeSlot::Variable.name().unwrap(), 1), "value"),
            String::new(),
            LSP_SPANS_HEADING.to_owned(),
            format!(
                "{:?} {:?}",
                span(4, 8, ThemeSlot::Function.name().unwrap(), SEMANTIC_PATTERN_INDEX),
                "call"
            ),
            format!(
                "{:?} {:?}",
                span(9, 14, ThemeSlot::Variable.name().unwrap(), SEMANTIC_PATTERN_INDEX),
                "value"
            ),
        ]
        .join("\n");

        assert_eq!(rendered, expected);
    }

    fn span(start: u32, end: u32, capture: &str, pattern_index: u32) -> Span {
        Span {
            start,
            end,
            capture: capture.to_owned(),
            pattern_index,
        }
    }
}
