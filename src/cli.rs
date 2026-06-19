use clap::{Parser, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Run a command and inspect its logs in a scrollable terminal viewer",
    trailing_var_arg = true
)]
pub(crate) struct Cli {
    /// Log parser to use. Auto currently recognizes bunyan, env_logger, logfmt, and tracing fmt defaults.
    #[arg(
        short,
        long,
        value_enum,
        default_value_t = LogFormat::Auto,
        env = "TRACEVIEWER_FORMAT"
    )]
    pub(crate) format: LogFormat,

    /// Optional maximum number of log lines to keep in memory. By default the buffer is unbounded.
    #[arg(long, env = "TRACEVIEWER_MAX_LINES")]
    pub(crate) max_lines: Option<usize>,

    /// Maximum bytes retained from a single log line.
    #[arg(
        long,
        default_value_t = crate::process::DEFAULT_MAX_LINE_BYTES,
        env = "TRACEVIEWER_MAX_LINE_BYTES"
    )]
    pub(crate) max_line_bytes: usize,

    /// Command to run, followed by its arguments. Use `--` before the command when needed.
    #[arg(required = true)]
    pub(crate) command: Vec<String>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum LogFormat {
    Auto,
    Bunyan,
    Plain,
    EnvLogger,
    Logfmt,
    Tracing,
}
