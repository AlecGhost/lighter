use std::fmt::Write;

use arborium::theme::{Style, Theme};
use arborium_highlight::Span;

use crate::styled::{self, Backend};

// Typst's raw text is 0.8em; scale its default 0.65em paragraph leading likewise.
const BLOCK_START: &str = "#block[#set par(leading: 0.52em);";

pub(crate) fn spans_to_typst(source: &str, spans: Vec<Span>, theme: &Theme) -> String {
    let content = styled::render::<Typst>(source, spans, theme);
    match content.is_empty() {
        true => content,
        false => format!("{BLOCK_START}{content}]"),
    }
}

struct Typst;

impl Backend for Typst {
    const STYLE_END: char = ']';

    fn write_style(output: &mut String, style: &Style) -> usize {
        let mut groups = 0;
        if let Some(color) = style.fg {
            write!(
                output,
                r##"#text(fill: rgb("#{:02X}{:02X}{:02X}"))["##,
                color.r, color.g, color.b
            )
            .expect("writing to a String cannot fail");
            groups += 1;
        }

        [
            (style.modifiers.bold, "#strong["),
            (style.modifiers.italic, "#emph["),
            (style.modifiers.underline, "#underline["),
            (style.modifiers.strikethrough, "#strike["),
        ]
        .into_iter()
        .filter(|(enabled, _)| *enabled)
        .for_each(|(_, typst_command)| {
            output.push_str(typst_command);
            groups += 1;
        });
        groups
    }

    fn write_text(output: &mut String, text: &str) {
        text.split_inclusive('\n')
            .for_each(|segment| match segment.strip_suffix('\n') {
                Some(line) => {
                    write_nonempty_raw(output, line.strip_suffix('\r').unwrap_or(line));
                    output.push_str("#linebreak()");
                }
                None => write_nonempty_raw(output, segment),
            });
    }
}

fn write_nonempty_raw(output: &mut String, text: &str) {
    match text {
        "" => {}
        text => write_raw(output, text),
    }
}

fn write_raw(output: &mut String, text: &str) {
    output.push_str("#raw(\"");
    text.chars().for_each(|character| match character {
        '\\' => output.push_str(r"\\"),
        '"' => output.push_str("\\\""),
        '\r' => output.push_str(r"\r"),
        '\t' => output.push_str(r"\t"),
        character if character.is_control() => {
            write!(output, r"\u{{{:X}}}", character as u32)
                .expect("writing to a String cannot fail");
        }
        character => output.push(character),
    });
    output.push_str("\")");
}

#[cfg(test)]
mod tests {
    use arborium_theme::ThemeSlot;

    use super::*;
    use crate::styled::test_support::{span, theme};

    fn render(source: &str, spans: Vec<Span>) -> String {
        spans_to_typst(source, spans, &theme())
    }

    fn block(content: &str) -> String {
        format!("{BLOCK_START}{content}]")
    }

    #[test]
    fn renders_flat_tokens_and_unstyled_gaps_from_source() {
        const SOURCE: &str = "let value";
        const EXPECTED: &str = concat!(
            r##"#text(fill: rgb("#010203"))[#raw("let")]"##,
            r##"#raw(" ")"##,
            r##"#text(fill: rgb("#040506"))[#raw("value")]"##,
        );
        let spans = vec![
            span(0..3, ThemeSlot::Keyword, 0),
            span(4..SOURCE.len(), ThemeSlot::Variable, 0),
        ];

        assert_eq!(render(SOURCE, spans), block(EXPECTED));
    }

    #[test]
    fn resolves_equal_span_ranges_by_pattern_priority() {
        const SOURCE: &str = "let";
        const EXPECTED: &str = r##"#text(fill: rgb("#010203"))[#raw("let")]"##;
        let spans = vec![
            span(0..SOURCE.len(), ThemeSlot::Variable, 0),
            span(0..SOURCE.len(), ThemeSlot::Keyword, 1),
        ];

        assert_eq!(render(SOURCE, spans), block(EXPECTED));
    }

    #[test]
    fn escapes_typst_strings_and_preserves_other_text() {
        const SOURCE: &str = "\\\"\t\r\u{8}#[]😀";
        const EXPECTED: &str = r##"#text(fill: rgb("#010203"))[#raw("\\\"\t\r\u{8}#[]😀")]"##;
        let spans = vec![span(0..SOURCE.len(), ThemeSlot::Keyword, 0)];

        assert_eq!(render(SOURCE, spans), block(EXPECTED));
    }

    #[test]
    fn renders_each_line_of_a_multiline_token_with_its_own_style() {
        const SOURCE: &str = "first\r\nsecond";
        const EXPECTED: &str = concat!(
            r##"#text(fill: rgb("#010203"))[#raw("first")]"##,
            "#linebreak()",
            r##"#text(fill: rgb("#010203"))[#raw("second")]"##,
        );
        let spans = vec![span(0..SOURCE.len(), ThemeSlot::Keyword, 0)];

        assert_eq!(render(SOURCE, spans), block(EXPECTED));
    }

    #[test]
    fn renders_source_line_endings_and_blank_lines_as_linebreaks() {
        const SOURCE: &str = "first\r\n\nthird";
        const EXPECTED: &str = r#"#raw("first")#linebreak()#linebreak()#raw("third")"#;

        assert_eq!(render(SOURCE, Vec::new()), block(EXPECTED));
    }

    #[test]
    fn renders_all_text_modifiers() {
        const SOURCE: &str = "call";
        const EXPECTED: &str = concat!(
            r##"#text(fill: rgb("#010203"))["##,
            r#"#strong[#emph[#underline[#strike[#raw("call")]]]]]"#,
        );
        let spans = vec![span(0..SOURCE.len(), ThemeSlot::Function, 0)];

        assert_eq!(render(SOURCE, spans), block(EXPECTED));
    }

    #[test]
    fn renders_empty_source_as_an_empty_fragment() {
        assert_eq!(render("", Vec::new()), "");
    }
}
