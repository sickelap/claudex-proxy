mod application;
mod auth;
mod chat;
mod codecs;
mod config;
mod domain;
mod providers;
mod proxy;

use std::fmt;
use std::process::ExitCode;

#[derive(Debug)]
pub struct AppError(pub String);

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for AppError {}

impl From<String> for AppError {
    fn from(message: String) -> Self {
        AppError(message)
    }
}

impl From<&str> for AppError {
    fn from(message: &str) -> Self {
        AppError(message.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(error: std::io::Error) -> Self {
        AppError(error.to_string())
    }
}

struct ProxyArgs {
    host: String,
    port: u16,
    config: Option<std::path::PathBuf>,
}

impl Default for ProxyArgs {
    fn default() -> Self {
        ProxyArgs {
            host: "127.0.0.1".to_string(),
            port: 8000,
            config: None,
        }
    }
}

fn parse_proxy_args(argv: &[String]) -> Result<ProxyArgs, AppError> {
    let mut args = ProxyArgs::default();
    let mut iter = argv.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--host" => {
                args.host = iter
                    .next()
                    .ok_or_else(|| AppError("--host requires a value".to_string()))?
                    .clone();
            }
            "--port" => {
                let value = iter
                    .next()
                    .ok_or_else(|| AppError("--port requires a value".to_string()))?;
                args.port = value
                    .parse()
                    .map_err(|_| AppError(format!("invalid --port value: {value}")))?;
            }
            "--config" => {
                args.config =
                    Some(std::path::PathBuf::from(iter.next().ok_or_else(|| {
                        AppError("--config requires a value".to_string())
                    })?));
            }
            other => return Err(AppError(format!("unrecognized argument: {other}"))),
        }
    }
    Ok(args)
}

fn run() -> Result<(), AppError> {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    match argv.first().map(String::as_str) {
        Some("chat") => chat::run(),
        Some("proxy") => {
            let args = parse_proxy_args(&argv[1..])?;
            let registry = config::load_registry(args.config.as_deref())?;
            proxy::run(&args.host, args.port, registry)
        }
        Some(other) if other.starts_with('-') => {
            let args = parse_proxy_args(&argv)?;
            let registry = config::load_registry(args.config.as_deref())?;
            proxy::run(&args.host, args.port, registry)
        }
        None => {
            let args = parse_proxy_args(&argv)?;
            let registry = config::load_registry(args.config.as_deref())?;
            proxy::run(&args.host, args.port, registry)
        }
        Some(other) => Err(AppError(format!("unrecognized subcommand: {other}"))),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("fatal: {error}");
            ExitCode::FAILURE
        }
    }
}
