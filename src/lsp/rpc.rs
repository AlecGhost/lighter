use lsp_types::{notification::Notification, request::Request};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::process::{ChildStdin, ChildStdout};
use thiserror::Error;

const JSON_RPC_VERSION: &str = "2.0";
const CONTENT_LENGTH: &str = "Content-Length";
const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

#[derive(Error, Debug)]
pub(crate) enum Error {
    #[error("Language server I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Language server sent invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("Language server protocol error: {0}")]
    Protocol(String),
    #[error("Language server returned error {code}: {message}")]
    Response { code: i64, message: String },
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub(crate) struct Connection {
    reader: BufReader<ChildStdout>,
    writer: ChildStdin,
    next_request_id: u64,
}

#[derive(Serialize)]
struct RequestMessage<'a, P> {
    jsonrpc: &'static str,
    id: u64,
    method: &'static str,
    params: &'a P,
}

#[derive(Serialize)]
struct NotificationMessage<'a, P> {
    jsonrpc: &'static str,
    method: &'static str,
    params: &'a P,
}

#[derive(Deserialize)]
struct IncomingMessage {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Deserialize)]
struct ResponseError {
    code: i64,
    message: String,
}

impl Connection {
    pub(crate) fn new(stdout: ChildStdout, stdin: ChildStdin) -> Connection {
        Connection {
            reader: BufReader::new(stdout),
            writer: stdin,
            next_request_id: 1,
        }
    }

    pub(crate) fn request<R: Request>(&mut self, params: R::Params) -> Result<R::Result> {
        let id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or_else(|| Error::Protocol("Request id overflow".to_string()))?;

        write_message(
            &mut self.writer,
            &RequestMessage {
                jsonrpc: JSON_RPC_VERSION,
                id,
                method: R::METHOD,
                params: &params,
            },
        )?;

        loop {
            let message: IncomingMessage = read_message(&mut self.reader)?;
            if message.method.is_some() {
                continue;
            }

            let response_id = message
                .id
                .as_ref()
                .and_then(Value::as_u64)
                .ok_or_else(|| Error::Protocol("Response has no numeric id".to_string()))?;
            if response_id != id {
                return Err(Error::Protocol(format!(
                    "Received response {response_id} while waiting for {id}"
                )));
            }

            if let Some(error) = message.error {
                return Err(Error::Response {
                    code: error.code,
                    message: error.message,
                });
            }

            return serde_json::from_value(message.result).map_err(Error::from);
        }
    }

    pub(crate) fn notify<N: Notification>(&mut self, params: N::Params) -> Result<()> {
        write_message(
            &mut self.writer,
            &NotificationMessage {
                jsonrpc: JSON_RPC_VERSION,
                method: N::METHOD,
                params: &params,
            },
        )
    }
}

fn write_message(writer: &mut impl Write, message: &impl Serialize) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    write!(writer, "{CONTENT_LENGTH}: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn read_message<T: for<'de> Deserialize<'de>>(reader: &mut impl BufRead) -> Result<T> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Server closed stdout while a response was pending",
            )
            .into());
        }
        if line == "\r\n" || line == "\n" {
            break;
        }

        let (name, value) = line
            .trim_end_matches(['\r', '\n'])
            .split_once(':')
            .ok_or_else(|| Error::Protocol(format!("Malformed header: {line:?}")))?;
        if name.eq_ignore_ascii_case(CONTENT_LENGTH) {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| Error::Protocol("Invalid Content-Length".to_string()))?,
            );
        }
    }

    let content_length = content_length
        .ok_or_else(|| Error::Protocol("Missing Content-Length header".to_string()))?;
    if content_length > MAX_MESSAGE_SIZE {
        return Err(Error::Protocol(format!(
            "Message exceeds {MAX_MESSAGE_SIZE} byte limit"
        )));
    }

    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[derive(Debug, Eq, PartialEq, Deserialize, Serialize)]
    struct Payload {
        text: String,
    }

    fn frame(value: &impl Serialize) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_message(&mut bytes, value).unwrap();
        bytes
    }

    #[test]
    fn framing_uses_json_byte_length() {
        let message = Payload {
            text: String::from("Grüße 👋"),
        };
        let encoded = frame(&message);
        let decoded: Payload = read_message(&mut Cursor::new(encoded)).unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn end_of_stream_is_an_io_error() {
        let error = read_message::<Value>(&mut Cursor::new(Vec::new())).unwrap_err();
        assert!(matches!(error, Error::Io(_)));
    }
}
