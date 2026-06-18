use std::process::ExitStatus;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    pub(crate) fn indicator(self) -> char {
        match self {
            Self::Stdout => '|',
            Self::Stderr => '!',
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    Unknown,
}

impl Level {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Trace => "TRACE",
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
            Self::Unknown => "",
        }
    }

    pub(crate) fn severity(self) -> u8 {
        match self {
            Self::Error => 5,
            Self::Warn => 4,
            Self::Info => 3,
            Self::Debug => 2,
            Self::Trace => 1,
            Self::Unknown => 0,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LogEntry {
    pub(crate) raw: String,
    pub(crate) timestamp: Option<String>,
    pub(crate) level: Level,
    pub(crate) parsed: bool,
    pub(crate) target: Option<String>,
    pub(crate) spans: Vec<String>,
    pub(crate) values: Vec<TraceValueField>,
    pub(crate) message: String,
    pub(crate) message_parts: Vec<MessagePart>,
    pub(crate) stream: Stream,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct TraceValueField {
    pub(crate) key: String,
    pub(crate) value: TraceValue,
}

impl TraceValueField {
    pub(crate) fn new(key: impl Into<String>, value: TraceValue) -> Self {
        Self {
            key: key.into(),
            value,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TraceValue {
    Bool(String),
    Null(String),
    Number(String),
    String(String),
    Object(String),
    Array(String),
    Other(String),
}

impl TraceValue {
    pub(crate) fn from_tracing_text(value: &str) -> Self {
        let value = value.to_string();
        if matches!(value.as_str(), "true" | "false") {
            Self::Bool(value)
        } else if value == "null" {
            Self::Null(value)
        } else if value.parse::<i64>().is_ok() || value.parse::<f64>().is_ok() {
            Self::Number(value)
        } else if value.starts_with('"') && value.ends_with('"') {
            Self::String(value)
        } else if value.starts_with('{') && value.ends_with('}') {
            Self::Object(value)
        } else if value.starts_with('[') && value.ends_with(']') {
            Self::Array(value)
        } else {
            Self::Other(value)
        }
    }

    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Bool(value)
            | Self::Null(value)
            | Self::Number(value)
            | Self::String(value)
            | Self::Object(value)
            | Self::Array(value)
            | Self::Other(value) => value,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MessageStyle {
    Default,
    JsonArray,
    JsonBool,
    JsonKey,
    JsonNull,
    JsonNumber,
    JsonObject,
    JsonPunctuation,
    JsonString,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MessagePart {
    pub(crate) text: String,
    pub(crate) style: MessageStyle,
}

impl MessagePart {
    pub(crate) fn new(text: impl Into<String>, style: MessageStyle) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }

    pub(crate) fn plain_text(parts: &[Self]) -> String {
        parts.iter().map(|part| part.text.as_str()).collect()
    }
}

#[derive(Debug)]
pub(crate) enum AppEvent {
    Line(Stream, String),
    ProcessExited(ExitStatus),
    ReaderFailed(Stream, String),
}
