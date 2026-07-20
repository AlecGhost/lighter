use std::io::{self, BufRead, Write};

use thiserror::Error;


const STDIO_FRAME_TERMINATOR: u8 = b'\0';
const STDIO_LANGUAGE_SEPARATOR: char = '\n';
const STDIO_DIAGNOSTIC_PREFIX: &str = "lighter stdio:";

#[derive(Debug, Error)]
pub enum RequestFrameError {
    #[error("request frame is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::str::Utf8Error),
    #[error("request frame is missing the language/source newline separator")]
    MissingLanguageSeparator,
    #[error("request frame has an empty language")]
    EmptyLanguage,
    #[error("request frame is missing its NUL terminator")]
    MissingTerminator,
}

fn parse_request_frame(frame: &[u8]) -> std::result::Result<(&str, &str), RequestFrameError> {
    let frame = std::str::from_utf8(frame)?;
    let (language, source) = frame
        .split_once(STDIO_LANGUAGE_SEPARATOR)
        .ok_or(RequestFrameError::MissingLanguageSeparator)?;

    match language.is_empty() {
        true => Err(RequestFrameError::EmptyLanguage),
        false => Ok((language, source)),
    }
}


fn write_stdio_response(mut output: impl Write, response: &str) -> io::Result<()> {
    output.write_all(response.as_bytes())?;
    output.write_all(&[STDIO_FRAME_TERMINATOR])?;
    output.flush()
}

fn report_stdio_error(
    mut diagnostics: impl Write,
    error: &impl std::fmt::Display,
) -> io::Result<()> {
    writeln!(diagnostics, "{STDIO_DIAGNOSTIC_PREFIX} {error}")
}

/// Serve NUL-delimited highlight requests until an empty frame or EOF.
///
/// A malformed request receives an empty response so clients retain one
/// response per non-shutdown frame. Its diagnostic is written separately.
pub fn serve_stdio<R, W, D, F, E>(
    mut input: R,
    mut output: W,
    mut diagnostics: D,
    mut highlight: F,
) -> io::Result<()>
where
    R: BufRead,
    W: Write,
    D: Write,
    F: FnMut(&str, &str) -> std::result::Result<String, E>,
    E: std::fmt::Display,
{
    let mut finished = false;
    let frames = std::iter::from_fn(|| match finished {
        true => None,
        false => {
            let mut frame = Vec::new();
            match input.read_until(STDIO_FRAME_TERMINATOR, &mut frame) {
                Ok(0) => {
                    finished = true;
                    None
                }
                Ok(_) => {
                    let terminated = frame.last() == Some(&STDIO_FRAME_TERMINATOR);
                    match terminated {
                        true => {
                            frame.pop();
                        }
                        false => finished = true,
                    }
                    Some(Ok((frame, terminated)))
                }
                Err(error) => {
                    finished = true;
                    Some(Err(error))
                }
            }
        }
    });

    frames
        .take_while(|frame| match frame {
            Ok((frame, terminated)) => !(frame.is_empty() && *terminated),
            Err(_) => true,
        })
        .try_for_each(|frame| {
            let (frame, terminated) = frame?;
            let response = match terminated {
                false => Err(RequestFrameError::MissingTerminator.to_string()),
                true => parse_request_frame(&frame)
                    .map_err(|error| error.to_string())
                    .and_then(|(language, source)| {
                        highlight(language, source).map_err(|error| error.to_string())
                    }),
            };

            match response {
                Ok(response) => write_stdio_response(&mut output, &response),
                Err(error) => {
                    report_stdio_error(&mut diagnostics, &error)?;
                    write_stdio_response(&mut output, "")
                }
            }
        })
}
