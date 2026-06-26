use std::{env, io, thread};

use tracing_bunyan_formatter::{BunyanFormattingLayer, JsonStorageLayer};
use tracing_subscriber::{filter::LevelFilter, prelude::*, registry::Registry};

#[derive(Clone, Copy)]
enum Format {
    Plain,
    EnvLogger,
    Logfmt,
    Tracing,
    Bunyan,
}

impl Format {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "plain" => Some(Self::Plain),
            "env-logger" | "env_logger" => Some(Self::EnvLogger),
            "logfmt" => Some(Self::Logfmt),
            "tracing" => Some(Self::Tracing),
            "bunyan" => Some(Self::Bunyan),
            _ => None,
        }
    }
}

fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print_help();
        return;
    }

    let Some(format) = args.first().and_then(|arg| Format::parse(arg)) else {
        print_help();
        return;
    };

    let repeat = option_usize(&args, "--repeat").unwrap_or(1);
    let show_threads = args.iter().any(|arg| arg == "--threads");
    match format {
        Format::Plain => emit_plain(repeat),
        Format::EnvLogger => {
            init_env_logger();
            emit_log_records(repeat);
        }
        Format::Logfmt => emit_logfmt(repeat),
        Format::Tracing => {
            init_tracing_fmt(show_threads);
            emit_tracing_records(repeat, show_threads);
        }
        Format::Bunyan => {
            init_bunyan();
            emit_tracing_records(repeat, false);
        }
    }
}

fn option_usize(args: &[String], name: &str) -> Option<usize> {
    args.windows(2)
        .find(|window| window[0] == name)
        .and_then(|window| window[1].parse().ok())
}

fn init_env_logger() {
    let mut builder = env_logger::Builder::new();
    builder
        .filter_level(log::LevelFilter::Trace)
        .format_timestamp_secs()
        .target(env_logger::Target::Stdout)
        .init();
}

fn init_tracing_fmt(show_threads: bool) {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_thread_names(show_threads)
        .with_thread_ids(show_threads)
        .with_target(true)
        .init();
}

fn init_bunyan() {
    let formatting_layer = BunyanFormattingLayer::new("showcase".to_string(), io::stdout);
    let subscriber = Registry::default()
        .with(LevelFilter::TRACE)
        .with(JsonStorageLayer)
        .with(formatting_layer);

    tracing::subscriber::set_global_default(subscriber).expect("install tracing subscriber");
}

fn emit_plain(repeat: usize) {
    for pass in 0..repeat {
        println!("plain: booting demo service pass={}", pass + 1);
        println!("plain: loaded 4 workers from ./config/showcase.toml");
        eprintln!("plain: warning: retry budget is low");
        println!("plain: completed request /api/widgets in 18ms");
    }
}

fn emit_log_records(repeat: usize) {
    for pass in 0..repeat {
        log::info!(
            target: "showcase::server",
            "listening on {} pass={}",
            "127.0.0.1:8080",
            pass + 1
        );

        log::debug!(
            target: "showcase::db",
            "pool checkout took {}ms rows={}",
            7,
            12
        );

        log::warn!(
            target: "showcase::client",
            "upstream returned {}; retrying attempt={} retry_after_ms={}",
            429,
            2,
            250
        );

        log::error!(
            target: "showcase::worker",
            "job failed: {} job_id={}",
            "missing artifact",
            "019b9370-0a9d-7231-825b-3f6f3b80555a"
        );
    }
}

fn emit_logfmt(repeat: usize) {
    for pass in 0..repeat {
        println!(
            "time=2026-06-15T12:01:02Z level=info target=showcase::server msg=\"starting http listener\" addr=127.0.0.1:8080 workers=4 pass={} cold_start={}",
            pass + 1,
            pass == 0
        );
        println!(
            "time=2026-06-15T12:01:03Z level=debug target=showcase::handler msg=\"loaded widgets\" count=12 cached=false user_id=42 latency_ms=18.4"
        );
        println!(
            "time=2026-06-15T12:01:04Z level=warn target=showcase::client msg=\"retrying upstream\" attempt=2 reason=\"rate limited\" retry_after_ms=250"
        );
        println!(
            "time=2026-06-15T12:01:05Z level=error target=showcase::worker msg=\"failed to process job\" error=\"missing artifact\" job_id=019b9370-0a9d-7231-825b-3f6f3b80555a"
        );
    }
}

fn emit_tracing_records(repeat: usize, show_threads: bool) {
    for pass in 0..repeat {
        tracing::info!(
            target: "showcase::server",
            addr = "127.0.0.1:8080",
            workers = 4,
            pass = pass + 1,
            cold_start = pass == 0,
            build.version = "0.1.2",
            features = ?["plain", "env-logger", "logfmt", "tracing", "bunyan"],
            "starting http listener"
        );

        let request = tracing::info_span!(
            target: "showcase::handler",
            "request",
            id = 7,
            method = "GET",
            path = "/api/widgets"
        );
        let _request = request.enter();

        tracing::debug!(
            target: "showcase::handler",
            count = 12,
            cached = false,
            user_id = 42,
            latency_ms = 18.4,
            "loaded widgets"
        );

        {
            let db = tracing::trace_span!(target: "showcase::db", "db", pool = "primary");
            let _db = db.enter();
            tracing::trace!(
                target: "showcase::db",
                rows = 12,
                elapsed_ms = 4.8,
                "SELECT returned"
            );
        }

        {
            let upstream = tracing::warn_span!(
                target: "showcase::client",
                "upstream_call",
                service = "inventory",
                endpoint = "/v1/widgets"
            );
            let _upstream = upstream.enter();
            tracing::warn!(
                target: "showcase::client",
                attempt = 2,
                reason = "rate limited",
                retry_after_ms = 250,
                "retrying upstream"
            );
        }

        {
            let job = tracing::error_span!(
                target: "showcase::worker",
                "process_job",
                job_id = "019b9370-0a9d-7231-825b-3f6f3b80555a",
                queue = "imports"
            );
            let _job = job.enter();
            tracing::error!(
                target: "showcase::worker",
                error = "missing artifact",
                error.sources = ?["cache miss", "upstream timeout"],
                "failed to process job"
            );
        }

        if show_threads {
            emit_threaded_tracing_records(pass);
        }
    }
}

fn emit_threaded_tracing_records(pass: usize) {
    let handles = [
        thread::Builder::new()
            .name("showcase-ingest".to_string())
            .spawn(move || {
                tracing::info!(
                    target: "showcase::ingest",
                    pass = pass + 1,
                    batch_id = "batch-42",
                    records = 256,
                    "accepted ingest batch"
                );
            })
            .expect("spawn ingest showcase thread"),
        thread::Builder::new()
            .name("showcase-export".to_string())
            .spawn(move || {
                tracing::debug!(
                    target: "showcase::export",
                    pass = pass + 1,
                    destination = "warehouse",
                    queued = 32,
                    "queued export work"
                );
            })
            .expect("spawn export showcase thread"),
    ];

    for handle in handles {
        handle.join().expect("join showcase thread");
    }
}

fn print_help() {
    println!(
        "\
traceviewer showcase example

Usage:
  cargo run --example showcase -- <plain|env-logger|logfmt|tracing|bunyan> [--repeat N] [--threads]

Examples:
  cargo run --example showcase -- tracing
  cargo run --example showcase -- tracing --threads
  cargo run --example showcase -- logfmt
  cargo run --example showcase -- bunyan
  cargo run --bin tv -- --format tracing -- cargo run --example showcase -- tracing --threads
  cargo run --bin tv -- --format logfmt -- cargo run --example showcase -- logfmt
  cargo run --bin tv -- --format bunyan -- cargo run --example showcase -- bunyan
  cargo run --bin tv -- --format env-logger -- cargo run --example showcase -- env-logger
"
    );
}
