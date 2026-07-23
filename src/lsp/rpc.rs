use lsp_types::notification::{LogMessage, Notification, Progress, ShowMessage};
use lsp_types::request::{
    Request, ShowMessageRequest, WorkDoneProgressCreate, WorkspaceConfiguration,
};
use lsp_types::{
    ConfigurationParams, LogMessageParams, MessageActionItem, MessageType, ProgressParams,
    ProgressParamsValue, ProgressToken, ShowMessageParams, ShowMessageRequestParams,
    WorkDoneProgress, WorkDoneProgressCreateParams,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{ChildStdin, ChildStdout};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};
use thiserror::Error;

use super::ServerConfiguration;
use crate::logging::LogLevel;

const JSON_RPC_VERSION: &str = "2.0";
const CONTENT_LENGTH: &str = "Content-Length";
const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;
const METHOD_NOT_FOUND_CODE: i64 = -32601;
const METHOD_NOT_FOUND_MESSAGE: &str = "Method not found";

#[derive(Error, Debug)]
pub enum Error {
    #[error("Language server I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Language server sent invalid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("Language server protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("Language server returned error {code}: {message}")]
    Response { code: i64, message: String },
}

pub(crate) type Result<T> = std::result::Result<T, Error>;

#[derive(Error, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    #[error("Request id overflow")]
    RequestIdOverflow,
    #[error("Response has no numeric id")]
    MissingResponseId,
    #[error("Received response {received} while waiting for {expected}")]
    MismatchedResponse { received: u64, expected: u64 },
    #[error("Received a response while waiting for progress")]
    ResponseDuringProgress,
    #[error("Language server reader disconnected")]
    ReaderDisconnected,
    #[error("Server request has no id")]
    MissingServerRequestId,
    #[error("Malformed header: {0:?}")]
    MalformedHeader(String),
    #[error("Invalid Content-Length")]
    InvalidContentLength,
    #[error("Missing Content-Length header")]
    MissingContentLength,
    #[error("Message exceeds {MAX_MESSAGE_SIZE} byte limit")]
    MessageTooLarge,
}

pub(crate) struct Connection {
    incoming: Receiver<Result<IncomingMessage>>,
    writer: Box<dyn Write + Send>,
    message_output: Box<dyn Write + Send>,
    configuration: ServerConfiguration,
    log: LogLevel,
    next_request_id: u64,
    active_progress: HashSet<ProgressToken>,
}

#[derive(Serialize)]
struct OutgoingMessage<'a, P> {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    method: &'static str,
    params: &'a P,
}

#[derive(Deserialize, Debug)]
struct IncomingMessage {
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Value,
    #[serde(default)]
    result: Value,
    #[serde(default)]
    error: Option<ResponseError>,
}

#[derive(Deserialize, Debug)]
struct ResponseError {
    code: i64,
    message: String,
}

#[derive(Serialize)]
struct ResponseMessage<'a, R> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: R,
}

#[derive(Serialize)]
struct ErrorResponseMessage<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    error: ResponseErrorMessage,
}

#[derive(Serialize)]
struct ResponseErrorMessage {
    code: i64,
    message: &'static str,
}

impl fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("log", &self.log)
            .field("next_request_id", &self.next_request_id)
            .field("active_progress", &self.active_progress)
            .finish_non_exhaustive()
    }
}

impl Connection {
    pub(crate) fn new(
        stdout: ChildStdout,
        stdin: ChildStdin,
        configuration: ServerConfiguration,
        log: LogLevel,
    ) -> Self {
        Self::from_io_with_configuration(stdout, stdin, std::io::stderr(), configuration, log)
    }

    #[cfg(test)]
    fn from_io(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        message_output: impl Write + Send + 'static,
        log: LogLevel,
    ) -> Self {
        Self::from_io_with_configuration(reader, writer, message_output, serde_json::json!({}), log)
    }

    fn from_io_with_configuration(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
        message_output: impl Write + Send + 'static,
        configuration: ServerConfiguration,
        log: LogLevel,
    ) -> Self {
        let (sender, incoming) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            loop {
                let message = read_message(&mut reader);
                let read_failed = message.is_err();
                if sender.send(message).is_err() || read_failed {
                    break;
                }
            }
        });

        Self {
            incoming,
            writer: Box::new(writer),
            message_output: Box::new(message_output),
            configuration,
            log,
            next_request_id: 1,
            active_progress: HashSet::new(),
        }
    }

    pub(crate) fn request<R: Request>(&mut self, params: R::Params) -> Result<R::Result> {
        let id = self.next_request_id;
        self.next_request_id = self
            .next_request_id
            .checked_add(1)
            .ok_or(ProtocolError::RequestIdOverflow)?;

        write_message(
            &mut self.writer,
            &OutgoingMessage {
                jsonrpc: JSON_RPC_VERSION,
                id: Some(id),
                method: R::METHOD,
                params: &params,
            },
        )?;

        loop {
            let message = self.receive()?;
            let Some(message) = self.handle_incoming(message)? else {
                continue;
            };

            let response_id = message
                .id
                .as_ref()
                .and_then(Value::as_u64)
                .ok_or(ProtocolError::MissingResponseId)?;
            if response_id != id {
                return Err(ProtocolError::MismatchedResponse {
                    received: response_id,
                    expected: id,
                }
                .into());
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
            &OutgoingMessage {
                jsonrpc: JSON_RPC_VERSION,
                id: None,
                method: N::METHOD,
                params: &params,
            },
        )
    }

    pub(crate) fn wait_for_progress(
        &mut self,
        initial_timeout: Duration,
        progress_timeout: Duration,
    ) -> Result<()> {
        let initial_deadline = Instant::now() + initial_timeout;
        while self.receive_progress_until(initial_deadline)? {}

        let progress_deadline = Instant::now() + progress_timeout;
        while !self.active_progress.is_empty() && self.receive_progress_until(progress_deadline)? {}
        if !self.active_progress.is_empty() {
            self.active_progress.clear();
        }

        Ok(())
    }

    fn receive_progress_until(&mut self, deadline: Instant) -> Result<bool> {
        let Some(message) = self.receive_until(deadline)? else {
            return Ok(false);
        };
        if self.handle_incoming(message)?.is_some() {
            return Err(ProtocolError::ResponseDuringProgress.into());
        }
        Ok(true)
    }

    fn receive(&self) -> Result<IncomingMessage> {
        self.incoming
            .recv()
            .map_err(|_| Error::from(ProtocolError::ReaderDisconnected))?
    }

    fn receive_until(&self, deadline: Instant) -> Result<Option<IncomingMessage>> {
        let Some(timeout) = deadline.checked_duration_since(Instant::now()) else {
            return Ok(None);
        };
        match self.incoming.recv_timeout(timeout) {
            Ok(message) => message.map(Some),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(ProtocolError::ReaderDisconnected.into()),
        }
    }

    fn handle_incoming(&mut self, message: IncomingMessage) -> Result<Option<IncomingMessage>> {
        let Some(method) = message.method.as_deref() else {
            return Ok(Some(message));
        };

        match method {
            ShowMessage::METHOD => {
                let params: ShowMessageParams = serde_json::from_value(message.params)?;
                self.display_typed_message(params.typ, &params.message)?;
            }
            LogMessage::METHOD => {
                let params: LogMessageParams = serde_json::from_value(message.params)?;
                if self.log.includes(LogLevel::Debug) {
                    self.display_message(&params.message)?;
                }
            }
            Progress::METHOD => {
                let params: ProgressParams = serde_json::from_value(message.params)?;
                self.handle_progress(params)?;
            }
            ShowMessageRequest::METHOD => {
                let id = request_id(&message)?.clone();
                let params: ShowMessageRequestParams = serde_json::from_value(message.params)?;
                self.display_typed_message(params.typ, &params.message)?;
                self.respond(&id, Option::<MessageActionItem>::None)?;
            }
            WorkDoneProgressCreate::METHOD => {
                let id = request_id(&message)?.clone();
                let _: WorkDoneProgressCreateParams = serde_json::from_value(message.params)?;
                self.respond(&id, ())?;
            }
            WorkspaceConfiguration::METHOD => {
                let id = request_id(&message)?.clone();
                let params: ConfigurationParams = serde_json::from_value(message.params)?;
                let configuration = params
                    .items
                    .iter()
                    .map(|item| self.configuration_for(item.section.as_deref()))
                    .collect::<Vec<_>>();
                self.respond(&id, configuration)?;
            }
            _ => {
                if let Some(id) = message.id.as_ref() {
                    self.respond_method_not_found(id)?;
                }
            }
        }

        Ok(None)
    }

    fn configuration_for(&self, section: Option<&str>) -> Value {
        match section {
            Some(section) => self
                .configuration
                .get(section)
                .cloned()
                .unwrap_or(Value::Null),
            None => self.configuration.clone(),
        }
    }

    fn handle_progress(&mut self, params: ProgressParams) -> Result<()> {
        match params.value {
            ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(begin)) => {
                self.active_progress.insert(params.token);
                if self.log.includes(LogLevel::Info) {
                    self.display_message(&begin.title)?;
                }
                if self.log.includes(LogLevel::Debug) {
                    self.display_optional_message(begin.message.as_deref())?;
                }
            }
            ProgressParamsValue::WorkDone(WorkDoneProgress::Report(report)) => {
                if self.log.includes(LogLevel::Debug) {
                    self.display_optional_message(report.message.as_deref())?;
                }
            }
            ProgressParamsValue::WorkDone(WorkDoneProgress::End(end)) => {
                self.active_progress.remove(&params.token);
                if self.log.includes(LogLevel::Debug) {
                    self.display_optional_message(end.message.as_deref())?;
                }
            }
        }
        Ok(())
    }

    fn display_optional_message(&mut self, message: Option<&str>) -> Result<()> {
        if let Some(message) = message {
            self.display_message(message)?;
        }
        Ok(())
    }

    fn display_typed_message(&mut self, typ: MessageType, message: &str) -> Result<()> {
        if self.log.includes(show_message_level(typ)) {
            self.display_message(message)?;
        }
        Ok(())
    }

    fn display_message(&mut self, message: &str) -> Result<()> {
        writeln!(self.message_output, "{message}")?;
        self.message_output.flush()?;
        Ok(())
    }

    fn respond<R: Serialize>(&mut self, id: &Value, result: R) -> Result<()> {
        write_message(
            &mut self.writer,
            &ResponseMessage {
                jsonrpc: JSON_RPC_VERSION,
                id,
                result,
            },
        )
    }

    fn respond_method_not_found(&mut self, id: &Value) -> Result<()> {
        write_message(
            &mut self.writer,
            &ErrorResponseMessage {
                jsonrpc: JSON_RPC_VERSION,
                id,
                error: ResponseErrorMessage {
                    code: METHOD_NOT_FOUND_CODE,
                    message: METHOD_NOT_FOUND_MESSAGE,
                },
            },
        )
    }
}

fn show_message_level(typ: MessageType) -> LogLevel {
    if typ == MessageType::ERROR || typ == MessageType::WARNING {
        LogLevel::Warn
    } else {
        LogLevel::Info
    }
}

fn request_id(message: &IncomingMessage) -> Result<&Value> {
    message
        .id
        .as_ref()
        .ok_or_else(|| ProtocolError::MissingServerRequestId.into())
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
            .ok_or_else(|| ProtocolError::MalformedHeader(line.clone()))?;
        if name.eq_ignore_ascii_case(CONTENT_LENGTH) {
            content_length = Some(
                value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| ProtocolError::InvalidContentLength)?,
            );
        }
    }

    let content_length = content_length.ok_or(ProtocolError::MissingContentLength)?;
    if content_length > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge.into());
    }

    let mut body = vec![0; content_length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{
        ConfigurationItem, MessageType, WorkDoneProgressBegin, WorkDoneProgressEnd,
        WorkDoneProgressReport,
    };
    use serde_json::json;
    use std::io::{Cursor, Read};
    use std::sync::{Arc, Mutex};

    const CLIENT_REQUEST_ID: u64 = 1;
    const SHOW_MESSAGE_REQUEST_ID: u64 = 40;
    const CREATE_PROGRESS_REQUEST_ID: u64 = 41;
    const CONFIGURATION_REQUEST_ID: u64 = 42;
    const OUTGOING_MESSAGE_COUNT: usize = 3;
    const TEST_REQUEST_METHOD: &str = "test/request";
    const TEST_PAYLOAD_TEXT: &str = "response payload";
    const ERROR_MESSAGE_TEXT: &str = "error message";
    const WARNING_MESSAGE_TEXT: &str = "warning message";
    const SHOW_MESSAGE_TEXT: &str = "show notification";
    const LOG_MESSAGE_TEXT: &str = "log notification";
    const SHOW_MESSAGE_REQUEST_TEXT: &str = "show request";
    const PROGRESS_TOKEN: &str = "tracked progress";
    const OTHER_PROGRESS_TOKEN: &str = "other progress";
    const PROGRESS_TITLE: &str = "Analyzing workspace";
    const PROGRESS_BEGIN_MESSAGE: &str = "begin message";
    const PROGRESS_REPORT_MESSAGE: &str = "report message";
    const SHORT_PROGRESS_REPORT_MESSAGE: &str = "done";
    const OTHER_PROGRESS_END_MESSAGE: &str = "other end message";
    const PROGRESS_END_MESSAGE: &str = "end message";
    const TEST_WAIT_TIMEOUT: Duration = Duration::from_millis(10);
    const TEST_READER_DELAY: Duration = Duration::from_millis(50);
    #[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
    struct Payload {
        text: String,
    }

    enum TestRequest {}

    impl Request for TestRequest {
        type Params = Payload;
        type Result = Payload;
        const METHOD: &'static str = TEST_REQUEST_METHOD;
    }

    #[derive(Clone, Default)]
    struct SharedBuffer(Arc<Mutex<Vec<u8>>>);

    impl SharedBuffer {
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().unwrap().clone()
        }

        fn text(&self) -> String {
            String::from_utf8(self.bytes()).unwrap()
        }
    }

    impl Write for SharedBuffer {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct DelayedEofReader {
        reader: Cursor<Vec<u8>>,
        delay: Duration,
    }

    impl DelayedEofReader {
        fn new(bytes: Vec<u8>, delay: Duration) -> Self {
            Self {
                reader: Cursor::new(bytes),
                delay,
            }
        }
    }

    impl Read for DelayedEofReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.reader.read(buffer)?;
            if read == 0 {
                std::thread::sleep(self.delay);
            }
            Ok(read)
        }
    }

    fn frame(value: &impl Serialize) -> Vec<u8> {
        let mut bytes = Vec::new();
        write_message(&mut bytes, value).unwrap();
        bytes
    }

    fn frames(values: &[Value]) -> Vec<u8> {
        values.iter().flat_map(frame).collect()
    }

    fn notification<N: Notification>(params: N::Params) -> Value
    where
        N::Params: Serialize,
    {
        json!({
            "jsonrpc": JSON_RPC_VERSION,
            "method": N::METHOD,
            "params": params,
        })
    }

    fn server_request<R: Request>(id: u64, params: R::Params) -> Value
    where
        R::Params: Serialize,
    {
        json!({
            "jsonrpc": JSON_RPC_VERSION,
            "id": id,
            "method": R::METHOD,
            "params": params,
        })
    }

    fn client_response(id: u64, result: &impl Serialize) -> Value {
        json!({
            "jsonrpc": JSON_RPC_VERSION,
            "id": id,
            "result": result,
        })
    }

    fn progress(token: &str, progress: WorkDoneProgress) -> Value {
        notification::<Progress>(ProgressParams {
            token: ProgressToken::String(token.to_owned()),
            value: ProgressParamsValue::WorkDone(progress),
        })
    }

    fn progress_begin(token: &str, title: &str, message: &str) -> Value {
        progress(
            token,
            WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: title.to_owned(),
                message: Some(message.to_owned()),
                ..Default::default()
            }),
        )
    }

    fn progress_report(token: &str, message: &str) -> Value {
        progress(
            token,
            WorkDoneProgress::Report(WorkDoneProgressReport {
                message: Some(message.to_owned()),
                ..Default::default()
            }),
        )
    }

    fn progress_end(token: &str, message: &str) -> Value {
        progress(
            token,
            WorkDoneProgress::End(WorkDoneProgressEnd {
                message: Some(message.to_owned()),
            }),
        )
    }

    fn show_message(typ: MessageType, message: &str) -> Value {
        notification::<ShowMessage>(ShowMessageParams {
            typ,
            message: message.to_owned(),
        })
    }

    fn output_lines(lines: &[&str]) -> String {
        format!("{}\n", lines.join("\n"))
    }

    fn read_values(bytes: Vec<u8>, count: usize) -> Vec<Value> {
        let mut reader = Cursor::new(bytes);
        (0..count)
            .map(|_| read_message(&mut reader).unwrap())
            .collect()
    }

    fn assert_protocol_error(error: Error, expected: ProtocolError) {
        assert!(matches!(error, Error::Protocol(actual) if actual == expected));
    }

    fn test_connection(
        incoming: Vec<u8>,
        log: LogLevel,
    ) -> (Connection, SharedBuffer, SharedBuffer) {
        test_connection_with_configuration(incoming, json!({}), log)
    }

    fn test_connection_with_configuration(
        incoming: Vec<u8>,
        configuration: ServerConfiguration,
        log: LogLevel,
    ) -> (Connection, SharedBuffer, SharedBuffer) {
        let protocol_output = SharedBuffer::default();
        let message_output = SharedBuffer::default();
        let connection = Connection::from_io_with_configuration(
            Cursor::new(incoming),
            protocol_output.clone(),
            message_output.clone(),
            configuration,
            log,
        );
        (connection, protocol_output, message_output)
    }

    fn server_message_output(messages: &[Value], log: LogLevel) -> String {
        let message_output = SharedBuffer::default();
        let mut connection = Connection::from_io(
            Cursor::new(Vec::new()),
            SharedBuffer::default(),
            message_output.clone(),
            log,
        );
        for message in messages {
            let incoming = serde_json::from_value(message.clone()).unwrap();
            assert!(connection.handle_incoming(incoming).unwrap().is_none());
        }
        message_output.text()
    }

    fn progress_notifications() -> Vec<Value> {
        vec![
            progress_begin(PROGRESS_TOKEN, PROGRESS_TITLE, PROGRESS_BEGIN_MESSAGE),
            progress_report(PROGRESS_TOKEN, PROGRESS_REPORT_MESSAGE),
            progress_report(PROGRESS_TOKEN, SHORT_PROGRESS_REPORT_MESSAGE),
            progress_end(OTHER_PROGRESS_TOKEN, OTHER_PROGRESS_END_MESSAGE),
            progress_end(PROGRESS_TOKEN, PROGRESS_END_MESSAGE),
        ]
    }

    fn wait_for_test_progress(log: LogLevel) -> (Connection, String) {
        let incoming = frames(&progress_notifications());
        let message_output = SharedBuffer::default();
        let mut connection = Connection::from_io(
            DelayedEofReader::new(incoming, TEST_READER_DELAY),
            SharedBuffer::default(),
            message_output.clone(),
            log,
        );

        connection
            .wait_for_progress(TEST_WAIT_TIMEOUT, TEST_WAIT_TIMEOUT)
            .unwrap();

        (connection, message_output.text())
    }

    #[test]
    fn framing_uses_json_byte_length() {
        let message = Payload {
            text: String::from("Grüße 👋"),
        };
        let body = serde_json::to_vec(&message).unwrap();
        let encoded = frame(&message);
        let expected_header = format!("{CONTENT_LENGTH}: {}\r\n\r\n", body.len());

        assert_eq!(encoded, [expected_header.as_bytes(), &body].concat());
    }

    #[test]
    fn reads_independently_constructed_frame_and_case_insensitive_header() {
        const BODY: &[u8] = br#"{"text":"manual frame"}"#;
        let header = format!("content-length: {}\r\n\r\n", BODY.len());
        let input = [header.as_bytes(), BODY].concat();

        let payload: Payload = read_message(&mut Cursor::new(input)).unwrap();

        assert_eq!(payload.text, "manual frame");
    }

    #[test]
    fn rejects_malformed_content_length_headers() {
        let cases = [
            (b"\r\n".as_slice(), ProtocolError::MissingContentLength),
            (
                b"Content-Length: nope\r\n\r\n".as_slice(),
                ProtocolError::InvalidContentLength,
            ),
            (
                b"Content-Length 2\r\n\r\n".as_slice(),
                ProtocolError::MalformedHeader("Content-Length 2\r\n".to_owned()),
            ),
        ];

        for (input, expected) in cases {
            let error = read_message::<Value>(&mut Cursor::new(input)).unwrap_err();
            assert_protocol_error(error, expected);
        }

        let oversized = format!("{CONTENT_LENGTH}: {}\r\n\r\n", MAX_MESSAGE_SIZE + 1);
        let error = read_message::<Value>(&mut Cursor::new(oversized)).unwrap_err();
        assert_protocol_error(error, ProtocolError::MessageTooLarge);
    }

    #[test]
    fn end_of_stream_is_an_io_error() {
        let error = read_message::<Value>(&mut Cursor::new(Vec::new())).unwrap_err();
        assert!(matches!(error, Error::Io(_)));
    }

    #[test]
    fn filters_server_messages_by_log_level() {
        let messages = [
            show_message(MessageType::ERROR, ERROR_MESSAGE_TEXT),
            show_message(MessageType::WARNING, WARNING_MESSAGE_TEXT),
            show_message(MessageType::INFO, SHOW_MESSAGE_TEXT),
            notification::<LogMessage>(LogMessageParams {
                typ: MessageType::LOG,
                message: LOG_MESSAGE_TEXT.to_owned(),
            }),
            progress_begin(PROGRESS_TOKEN, PROGRESS_TITLE, PROGRESS_BEGIN_MESSAGE),
            progress_report(PROGRESS_TOKEN, PROGRESS_REPORT_MESSAGE),
            progress_end(PROGRESS_TOKEN, PROGRESS_END_MESSAGE),
        ];

        assert!(server_message_output(&messages, LogLevel::Error).is_empty());
        assert_eq!(
            server_message_output(&messages, LogLevel::Warn),
            output_lines(&[ERROR_MESSAGE_TEXT, WARNING_MESSAGE_TEXT])
        );
        assert_eq!(
            server_message_output(&messages, LogLevel::Info),
            output_lines(&[
                ERROR_MESSAGE_TEXT,
                WARNING_MESSAGE_TEXT,
                SHOW_MESSAGE_TEXT,
                PROGRESS_TITLE,
            ])
        );
        assert_eq!(
            server_message_output(&messages, LogLevel::Debug),
            output_lines(&[
                ERROR_MESSAGE_TEXT,
                WARNING_MESSAGE_TEXT,
                SHOW_MESSAGE_TEXT,
                LOG_MESSAGE_TEXT,
                PROGRESS_TITLE,
                PROGRESS_BEGIN_MESSAGE,
                PROGRESS_REPORT_MESSAGE,
                PROGRESS_END_MESSAGE,
            ])
        );
    }

    #[test]
    fn handles_message_notifications_and_server_requests_with_debug_enabled() {
        let expected_response = Payload {
            text: TEST_PAYLOAD_TEXT.to_owned(),
        };
        let incoming = frames(&[
            notification::<ShowMessage>(ShowMessageParams {
                typ: MessageType::INFO,
                message: SHOW_MESSAGE_TEXT.to_owned(),
            }),
            notification::<LogMessage>(LogMessageParams {
                typ: MessageType::LOG,
                message: LOG_MESSAGE_TEXT.to_owned(),
            }),
            server_request::<ShowMessageRequest>(
                SHOW_MESSAGE_REQUEST_ID,
                ShowMessageRequestParams {
                    typ: MessageType::WARNING,
                    message: SHOW_MESSAGE_REQUEST_TEXT.to_owned(),
                    actions: None,
                },
            ),
            server_request::<WorkDoneProgressCreate>(
                CREATE_PROGRESS_REQUEST_ID,
                WorkDoneProgressCreateParams {
                    token: ProgressToken::String(PROGRESS_TOKEN.to_owned()),
                },
            ),
            client_response(CLIENT_REQUEST_ID, &expected_response),
        ]);
        let (mut connection, protocol_output, message_output) =
            test_connection(incoming, LogLevel::Debug);

        let response = connection
            .request::<TestRequest>(Payload {
                text: TEST_PAYLOAD_TEXT.to_owned(),
            })
            .unwrap();

        assert_eq!(response, expected_response);
        assert_eq!(
            message_output.text(),
            output_lines(&[
                SHOW_MESSAGE_TEXT,
                LOG_MESSAGE_TEXT,
                SHOW_MESSAGE_REQUEST_TEXT,
            ])
        );

        let outgoing = read_values(protocol_output.bytes(), OUTGOING_MESSAGE_COUNT);
        assert_eq!(outgoing[0]["method"], TestRequest::METHOD);
        assert_eq!(outgoing[1]["id"], SHOW_MESSAGE_REQUEST_ID);
        assert!(outgoing[1]["result"].is_null());
        assert_eq!(outgoing[2]["id"], CREATE_PROGRESS_REQUEST_ID);
        assert!(outgoing[2]["result"].is_null());
    }

    #[test]
    fn suppresses_log_messages_below_debug() {
        let expected_response = Payload {
            text: TEST_PAYLOAD_TEXT.to_owned(),
        };
        let incoming = frames(&[
            notification::<LogMessage>(LogMessageParams {
                typ: MessageType::LOG,
                message: LOG_MESSAGE_TEXT.to_owned(),
            }),
            client_response(CLIENT_REQUEST_ID, &expected_response),
        ]);
        let (mut connection, _, message_output) = test_connection(incoming, LogLevel::Info);

        let response = connection
            .request::<TestRequest>(expected_response.clone())
            .unwrap();

        assert_eq!(response, expected_response);
        assert!(message_output.text().is_empty());
    }

    #[test]
    fn unknown_server_requests_receive_method_not_found() {
        const REQUEST_ID: u64 = 99;
        let message: IncomingMessage = serde_json::from_value(json!({
            "jsonrpc": JSON_RPC_VERSION,
            "id": REQUEST_ID,
            "method": TestRequest::METHOD,
            "params": {},
        }))
        .unwrap();
        let (mut connection, protocol_output, _) = test_connection(Vec::new(), LogLevel::Error);

        assert!(connection.handle_incoming(message).unwrap().is_none());

        let response = read_values(protocol_output.bytes(), 1).remove(0);
        assert_eq!(response["id"], REQUEST_ID);
        assert_eq!(response["error"]["code"], METHOD_NOT_FOUND_CODE);
        assert_eq!(response["error"]["message"], METHOD_NOT_FOUND_MESSAGE);
    }

    #[test]
    fn returns_requested_workspace_configuration_in_order() {
        const CONFIGURATION_SECTION: &str = "language-server";
        const UNKNOWN_SECTION: &str = "unknown";
        let configured_value = json!({ "semanticTokens": true });
        let configuration = json!({ (CONFIGURATION_SECTION): configured_value });
        let request = server_request::<WorkspaceConfiguration>(
            CONFIGURATION_REQUEST_ID,
            ConfigurationParams {
                items: vec![
                    ConfigurationItem {
                        section: Some(CONFIGURATION_SECTION.to_owned()),
                        ..Default::default()
                    },
                    ConfigurationItem {
                        section: Some(UNKNOWN_SECTION.to_owned()),
                        ..Default::default()
                    },
                    ConfigurationItem::default(),
                ],
            },
        );
        let message = serde_json::from_value(request).unwrap();
        let (mut connection, protocol_output, _) =
            test_connection_with_configuration(Vec::new(), configuration, LogLevel::Error);

        assert!(connection.handle_incoming(message).unwrap().is_none());

        let response = read_values(protocol_output.bytes(), 1).remove(0);
        assert_eq!(response["id"], CONFIGURATION_REQUEST_ID);
        assert_eq!(
            response["result"],
            json!([
                configured_value,
                null,
                { (CONFIGURATION_SECTION): configured_value },
            ])
        );
    }

    #[test]
    fn server_requests_require_an_id() {
        let message: IncomingMessage = serde_json::from_value(json!({
            "jsonrpc": JSON_RPC_VERSION,
            "method": ShowMessageRequest::METHOD,
            "params": {
                "type": MessageType::INFO,
                "message": SHOW_MESSAGE_REQUEST_TEXT,
            },
        }))
        .unwrap();
        let (mut connection, _, _) = test_connection(Vec::new(), LogLevel::Error);

        let error = connection.handle_incoming(message).unwrap_err();

        assert_protocol_error(error, ProtocolError::MissingServerRequestId);
    }

    #[test]
    fn rejects_mismatched_response_ids() {
        let response = Payload {
            text: TEST_PAYLOAD_TEXT.to_owned(),
        };
        let incoming = frame(&client_response(CLIENT_REQUEST_ID + 1, &response));
        let (mut connection, _, _) = test_connection(incoming, LogLevel::Error);

        let error = connection.request::<TestRequest>(response).unwrap_err();

        assert_protocol_error(
            error,
            ProtocolError::MismatchedResponse {
                received: CLIENT_REQUEST_ID + 1,
                expected: CLIENT_REQUEST_ID,
            },
        );
    }

    #[test]
    fn rejects_responses_while_waiting_for_progress() {
        let incoming = frame(&client_response(CLIENT_REQUEST_ID, &Value::Null));
        let (mut connection, _, _) = test_connection(incoming, LogLevel::Error);

        let error = connection
            .wait_for_progress(TEST_WAIT_TIMEOUT, TEST_WAIT_TIMEOUT)
            .unwrap_err();

        assert_protocol_error(error, ProtocolError::ResponseDuringProgress);
    }

    #[test]
    fn progress_timeout_clears_unfinished_tokens() {
        let incoming = frame(&progress_begin(
            PROGRESS_TOKEN,
            PROGRESS_TITLE,
            PROGRESS_BEGIN_MESSAGE,
        ));
        let mut connection = Connection::from_io(
            DelayedEofReader::new(incoming, TEST_READER_DELAY),
            SharedBuffer::default(),
            SharedBuffer::default(),
            LogLevel::Error,
        );

        connection
            .wait_for_progress(TEST_WAIT_TIMEOUT, TEST_WAIT_TIMEOUT)
            .unwrap();

        assert!(connection.active_progress.is_empty());
    }

    #[test]
    fn info_waits_for_matching_progress_end_and_only_shows_title() {
        let (connection, output) = wait_for_test_progress(LogLevel::Info);

        assert!(connection.active_progress.is_empty());
        assert_eq!(output, output_lines(&[PROGRESS_TITLE]));
    }

    #[test]
    fn shows_progress_details_with_debug() {
        let (connection, output) = wait_for_test_progress(LogLevel::Debug);

        assert!(connection.active_progress.is_empty());
        assert_eq!(
            output,
            output_lines(&[
                PROGRESS_TITLE,
                PROGRESS_BEGIN_MESSAGE,
                PROGRESS_REPORT_MESSAGE,
                SHORT_PROGRESS_REPORT_MESSAGE,
                OTHER_PROGRESS_END_MESSAGE,
                PROGRESS_END_MESSAGE,
            ])
        );
    }
}
