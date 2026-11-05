use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::{Error, Result};
use crate::{Input, LangName, LineRange, Output, theme};

pub(super) const VERSION: &str = "6";
pub(super) const STOP_LANGUAGE: &str = "lighter-internal-stop";

pub(super) const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

const HEADER_TERMINATOR: u8 = b'\n';
const MAX_HEADER_SIZE: u64 = 1024 * 1024;
const TOKEN_BYTES: usize = 32;

/// Secret shared through the daemon's private runtime directory. Only a client
/// that can read it may submit requests, which may name configs whose server
/// commands the daemon will run.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(super) struct AuthToken(String);

impl AuthToken {
    pub(super) fn generate() -> io::Result<Self> {
        let mut bytes = [0; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Self(
            bytes.iter().map(|byte| format!("{byte:02x}")).collect(),
        ))
    }

    pub(super) fn new(token: &str) -> Self {
        Self(token.trim().to_owned())
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Compares in constant time so response timing does not leak the token.
impl PartialEq for AuthToken {
    fn eq(&self, other: &Self) -> bool {
        let (left, right) = (self.0.as_bytes(), other.0.as_bytes());
        left.len() == right.len()
            && left
                .iter()
                .zip(right)
                .fold(0, |difference, (left, right)| difference | (left ^ right))
                == 0
    }
}

impl Eq for AuthToken {}

impl std::fmt::Debug for AuthToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("AuthToken(..)")
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct RequestOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Output>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<theme::Config>,
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
            config: None,
            output: None,
            theme: None,
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
    #[serde(default)]
    token: Option<AuthToken>,
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
    token: Option<AuthToken>,
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
    match input
        .by_ref()
        .take(MAX_HEADER_SIZE)
        .read_until(HEADER_TERMINATOR, &mut bytes)?
    {
        0 => Ok(None),
        _ => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(Error::from),
    }
}

fn write_request(
    mut output: impl Write,
    id: u64,
    token: &AuthToken,
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
            token: Some(token.clone()),
            options,
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
    let mut source = vec![0; checked_length(header.length)?];
    input.read_exact(&mut source)?;
    Ok(Some(Request {
        version: header.version,
        id: header.id,
        language: header.lang,
        path: header.path,
        project: header.project,
        token: header.token,
        options: header.options,
        source,
    }))
}

fn checked_length(length: usize) -> Result<usize> {
    match length <= MAX_MESSAGE_SIZE {
        true => Ok(length),
        false => Err(Error::MessageTooLarge),
    }
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
    let mut body = vec![0; checked_length(header.length)?];
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

fn handle_request<F, E>(
    request: Request,
    token: &AuthToken,
    mut output: impl Write,
    highlight: &mut F,
) -> Result<bool>
where
    F: FnMut(Input<'_>, RequestOptions) -> std::result::Result<String, E>,
    E: std::fmt::Display,
{
    let id = request.id;
    if request.token.as_ref() != Some(token) {
        write_error_response(&mut output, id, &Error::Unauthorized.to_string())?;
        return Ok(false);
    }
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
    token: &AuthToken,
    highlight: &mut F,
) -> Result<bool>
where
    F: FnMut(Input<'_>, RequestOptions) -> std::result::Result<String, E>,
    E: std::fmt::Display,
{
    match read_request(&mut BufReader::new(&mut *stream))? {
        Some(request) => handle_request(request, token, stream, highlight),
        None => Ok(false),
    }
}

pub(super) fn exchange(
    stream: &mut (impl Read + Write),
    id: u64,
    token: &AuthToken,
    input: Input<'_>,
    options: RequestOptions,
) -> Result<String> {
    write_request(&mut *stream, id, token, input, options)?;
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
    const THEME: &str = "Catppuccin Mocha";
    const TOKEN: &str = "secret";

    fn token() -> AuthToken {
        AuthToken::new(TOKEN)
    }

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
            config: Some(PathBuf::from("lighter.toml")),
            output: Some(Output::Html),
            theme: Some(theme::Config::Builtin(THEME.to_owned())),
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
            token: Some(token()),
            options: RequestOptions::default(),
            source: SOURCE.as_bytes().to_vec(),
        }
    }

    #[test]
    fn request_uses_json_header_and_exact_utf8_byte_length() {
        let mut bytes = Vec::new();
        let options = request_options();

        write_request(&mut bytes, REQUEST_ID, &token(), input(), options.clone()).unwrap();

        let request = read_request(&mut BufReader::new(Cursor::new(bytes)))
            .unwrap()
            .unwrap();
        assert_eq!(request.version, VERSION);
        assert_eq!(request.id, REQUEST_ID);
        assert_eq!(request.language, LANGUAGE);
        assert_eq!(request.path, Some(PathBuf::from(PATH)));
        assert_eq!(request.project, Some(PathBuf::from(PROJECT)));
        assert_eq!(request.token, Some(token()));
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

        handle_request(
            request(VERSION),
            &token(),
            &mut output,
            &mut |_input, _options| Err::<String, _>(HIGHLIGHT_ERROR),
        )
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
            &token(),
            &mut output,
            &mut |_input, _options| Ok::<_, std::convert::Infallible>(OUTPUT.to_owned()),
        )
        .unwrap();

        let error =
            read_response_result(&mut BufReader::new(Cursor::new(output)), REQUEST_ID).unwrap_err();
        assert!(matches!(error, Error::Response(message) if message.contains(UNSUPPORTED_VERSION)));
    }

    #[test]
    fn request_handler_rejects_missing_or_wrong_tokens() {
        for (token, language) in [
            (None, LANGUAGE),
            (Some(AuthToken::new("wrong")), LANGUAGE),
            (Some(AuthToken::new("wrong")), STOP_LANGUAGE),
        ] {
            let mut output = Vec::new();
            let mut highlighted = false;
            let request = Request {
                token,
                language: language.to_owned(),
                ..request(VERSION)
            };

            let stop = handle_request(request, &self::token(), &mut output, &mut |_, _| {
                highlighted = true;
                Ok::<_, std::convert::Infallible>(OUTPUT.to_owned())
            })
            .unwrap();

            assert!(!stop);
            assert!(!highlighted);
            let error = read_response_result(&mut BufReader::new(Cursor::new(output)), REQUEST_ID)
                .unwrap_err();
            assert!(
                matches!(error, Error::Response(message) if message == Error::Unauthorized.to_string())
            );
        }
    }

    #[test]
    fn rejects_oversized_lengths_before_allocating() {
        let mut request = serde_json::to_vec(&RequestHeader {
            version: VERSION.to_owned(),
            id: REQUEST_ID,
            lang: LANGUAGE.to_owned(),
            length: MAX_MESSAGE_SIZE + 1,
            path: None,
            project: None,
            token: Some(token()),
            options: RequestOptions::default(),
        })
        .unwrap();
        request.push(HEADER_TERMINATOR);
        let mut response = Vec::new();
        write_header(
            &mut response,
            &ResponseHeader {
                version: VERSION.to_owned(),
                length: MAX_MESSAGE_SIZE + 1,
                id: REQUEST_ID,
                error: None,
            },
        )
        .unwrap();

        let request_error = read_request(&mut BufReader::new(Cursor::new(request))).unwrap_err();
        let response_error = read_response(&mut BufReader::new(Cursor::new(response))).unwrap_err();

        assert!(matches!(request_error, Error::MessageTooLarge));
        assert!(matches!(response_error, Error::MessageTooLarge));
    }

    #[test]
    fn tokens_compare_by_value_and_hide_their_contents() {
        let generated = AuthToken::generate().unwrap();

        assert_eq!(generated.as_str().len(), TOKEN_BYTES * 2);
        assert_eq!(
            AuthToken::new(&format!("{}\n", generated.as_str())),
            generated
        );
        assert_ne!(AuthToken::generate().unwrap(), generated);
        assert!(!format!("{generated:?}").contains(generated.as_str()));
    }
}
