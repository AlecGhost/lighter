use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Error, Result};
use crate::{Input, LangName, LineRange, Output};

pub(super) const VERSION: &str = "2";
pub(super) const STOP_LANGUAGE: &str = "lighter-internal-stop";

const HEADER_TERMINATOR: u8 = b'\n';

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RequestOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Output>,
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub lsp: bool,
    #[serde(default = "enabled", skip_serializing_if = "is_enabled")]
    pub tree_sitter: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<LineRange>,
}

impl Default for RequestOptions {
    fn default() -> Self {
        Self {
            output: None,
            lsp: true,
            tree_sitter: true,
            lines: None,
        }
    }
}

const fn enabled() -> bool {
    true
}

const fn is_enabled(value: &bool) -> bool {
    *value
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct RequestHeader {
    version: String,
    id: u64,
    lang: String,
    length: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project: Option<PathBuf>,
    #[serde(flatten)]
    options: RequestOptions,
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
struct ResponseHeader {
    version: String,
    length: usize,
    id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Eq, PartialEq)]
struct Request {
    version: String,
    id: u64,
    language: String,
    path: Option<PathBuf>,
    project: Option<PathBuf>,
    options: RequestOptions,
    source: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
struct Response {
    header: ResponseHeader,
    body: Vec<u8>,
}

fn write_header(mut output: impl Write, header: &impl Serialize) -> Result<()> {
    serde_json::to_writer(&mut output, header)?;
    output.write_all(&[HEADER_TERMINATOR])?;
    Ok(())
}

fn read_header<T: serde::de::DeserializeOwned>(input: &mut impl BufRead) -> Result<Option<T>> {
    let mut bytes = Vec::new();
    match input.read_until(HEADER_TERMINATOR, &mut bytes)? {
        0 => Ok(None),
        _ => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(Error::from),
    }
}

fn write_request(
    mut output: impl Write,
    id: u64,
    input: Input<'_>,
    options: RequestOptions,
) -> Result<()> {
    write_header(
        &mut output,
        &RequestHeader {
            version: VERSION.to_owned(),
            id,
            lang: input.lang.to_string(),
            length: input.source.len(),
            path: input.path.map(PathBuf::from),
            project: input.project.map(PathBuf::from),
            options: options,
        },
    )?;
    output.write_all(input.source.as_bytes())?;
    output.flush()?;
    Ok(())
}

fn read_request(input: &mut impl BufRead) -> Result<Option<Request>> {
    let Some(header) = read_header::<RequestHeader>(input)? else {
        return Ok(None);
    };
    let mut source = vec![0; header.length];
    input.read_exact(&mut source)?;
    Ok(Some(Request {
        version: header.version,
        id: header.id,
        language: header.lang,
        path: header.path,
        project: header.project,
        options: header.options,
        source,
    }))
}

fn write_response(mut output: impl Write, id: u64, response: &str) -> Result<()> {
    write_header(
        &mut output,
        &ResponseHeader {
            version: VERSION.to_owned(),
            length: response.len(),
            id,
            error: None,
        },
    )?;
    output.write_all(response.as_bytes())?;
    output.flush()?;
    Ok(())
}

fn write_error_response(mut output: impl Write, id: u64, error: &str) -> Result<()> {
    write_header(
        &mut output,
        &ResponseHeader {
            version: VERSION.to_owned(),
            length: 0,
            id,
            error: Some(error.to_owned()),
        },
    )?;
    output.flush()?;
    Ok(())
}

fn read_response(input: &mut impl BufRead) -> Result<Response> {
    let header = read_header::<ResponseHeader>(input)?.ok_or_else(|| {
        Error::Io(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "daemon closed before sending a response",
        ))
    })?;
    let mut body = vec![0; header.length];
    input.read_exact(&mut body)?;
    Ok(Response { header, body })
}

fn read_response_result(input: &mut impl BufRead, request_id: u64) -> Result<String> {
    let response = read_response(input)?;
    match response.header.version.as_str() {
        VERSION => {}
        _ => return Err(Error::UnsupportedVersion(response.header.version)),
    }
    match response.header.id == request_id {
        true => {}
        false => {
            return Err(Error::ResponseId {
                expected: request_id,
                actual: response.header.id,
            });
        }
    }
    match response.header.error {
        Some(error) if response.header.length == 0 => Err(Error::Response(error)),
        Some(_) => Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "error response has a non-zero body length",
        ))),
        None => String::from_utf8(response.body).map_err(Error::from),
    }
}

fn handle_request<F, E>(request: Request, mut output: impl Write, highlight: &mut F) -> Result<bool>
where
    F: FnMut(Input<'_>, RequestOptions) -> std::result::Result<String, E>,
    E: std::fmt::Display,
{
    let id = request.id;
    match (request.version.as_str(), request.language.as_str()) {
        (VERSION, STOP_LANGUAGE) => {
            write_response(&mut output, id, "")?;
            return Ok(true);
        }
        (VERSION, _) => {}
        (version, _) => {
            write_error_response(
                &mut output,
                id,
                &Error::UnsupportedVersion(version.to_owned()).to_string(),
            )?;
            return Ok(false);
        }
    }

    let source = match String::from_utf8(request.source) {
        Ok(source) => source,
        Err(error) => {
            write_error_response(&mut output, id, &Error::InvalidUtf8(error).to_string())?;
            return Ok(false);
        }
    };
    let input = Input {
        source: &source,
        path: request.path.as_deref(),
        project: request.project.as_deref(),
        lang: LangName::from(request.language),
    };
    match highlight(input, request.options) {
        Ok(response) => write_response(&mut output, id, &response)?,
        Err(error) => write_error_response(&mut output, id, &error.to_string())?,
    }
    Ok(false)
}

pub(super) fn serve_connection<F, E>(
    stream: &mut (impl Read + Write),
    highlight: &mut F,
) -> Result<bool>
where
    F: FnMut(Input<'_>, RequestOptions) -> std::result::Result<String, E>,
    E: std::fmt::Display,
{
    match read_request(&mut BufReader::new(&mut *stream))? {
        Some(request) => handle_request(request, stream, highlight),
        None => Ok(false),
    }
}

pub(super) fn exchange(
    stream: &mut (impl Read + Write),
    id: u64,
    input: Input<'_>,
    options: RequestOptions,
) -> Result<String> {
    write_request(&mut *stream, id, input, options)?;
    read_response_result(&mut BufReader::new(stream), id)
}

#[cfg(test)]
mod tests {
    use std::io::{BufReader, Cursor};
    use std::path::{Path, PathBuf};

    use super::*;

    const REQUEST_ID: u64 = 42;
    const LANGUAGE: &str = "python";
    const SOURCE: &str = "print('😀')";
    const OUTPUT: &str = "highlighted 😀";
    const HIGHLIGHT_ERROR: &str = "highlight failed";
    const PATH: &str = "source.py";
    const PROJECT: &str = "project";

    fn input() -> Input<'static> {
        Input {
            source: SOURCE,
            path: Some(Path::new(PATH)),
            project: Some(Path::new(PROJECT)),
            lang: LangName::from(LANGUAGE),
        }
    }

    fn request_options() -> RequestOptions {
        RequestOptions {
            output: Some(Output::Html),
            lsp: false,
            tree_sitter: false,
            lines: Some("2:4".parse().unwrap()),
        }
    }

    fn request(version: &str) -> Request {
        Request {
            version: version.to_owned(),
            id: REQUEST_ID,
            language: LANGUAGE.to_owned(),
            path: None,
            project: None,
            options: RequestOptions::default(),
            source: SOURCE.as_bytes().to_vec(),
        }
    }

    #[test]
    fn request_uses_json_header_and_exact_utf8_byte_length() {
        let mut bytes = Vec::new();
        let options = request_options();

        write_request(&mut bytes, REQUEST_ID, input(), options.clone()).unwrap();

        let request = read_request(&mut BufReader::new(Cursor::new(bytes)))
            .unwrap()
            .unwrap();
        assert_eq!(request.version, VERSION);
        assert_eq!(request.id, REQUEST_ID);
        assert_eq!(request.language, LANGUAGE);
        assert_eq!(request.path, Some(PathBuf::from(PATH)));
        assert_eq!(request.project, Some(PathBuf::from(PROJECT)));
        assert_eq!(request.options, options);
        assert_eq!(request.source, SOURCE.as_bytes());
    }

    #[test]
    fn response_repeats_version_and_request_id() {
        let mut output = Vec::new();

        write_response(&mut output, REQUEST_ID, OUTPUT).unwrap();

        let response = read_response(&mut BufReader::new(Cursor::new(output))).unwrap();
        assert_eq!(response.header.version, VERSION);
        assert_eq!(response.header.id, REQUEST_ID);
        assert_eq!(response.header.length, OUTPUT.len());
        assert_eq!(response.header.error, None);
        assert_eq!(response.body, OUTPUT.as_bytes());
    }

    #[test]
    fn request_handler_returns_highlight_errors_without_a_body() {
        let mut output = Vec::new();

        handle_request(request(VERSION), &mut output, &mut |_input, _options| {
            Err::<String, _>(HIGHLIGHT_ERROR)
        })
        .unwrap();

        let error =
            read_response_result(&mut BufReader::new(Cursor::new(output)), REQUEST_ID).unwrap_err();
        assert!(matches!(error, Error::Response(message) if message == HIGHLIGHT_ERROR));
    }

    #[test]
    fn request_handler_rejects_unsupported_versions() {
        const UNSUPPORTED_VERSION: &str = "unsupported";
        let mut output = Vec::new();

        handle_request(
            request(UNSUPPORTED_VERSION),
            &mut output,
            &mut |_input, _options| Ok::<_, std::convert::Infallible>(OUTPUT.to_owned()),
        )
        .unwrap();

        let error =
            read_response_result(&mut BufReader::new(Cursor::new(output)), REQUEST_ID).unwrap_err();
        assert!(matches!(error, Error::Response(message) if message.contains(UNSUPPORTED_VERSION)));
    }
}
