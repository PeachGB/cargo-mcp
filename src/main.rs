//! MCP server exposing `cargo check`, `cargo clippy` and `cargo test` over stdio.
//!
//! The default timeout per cargo run is 120 s; override it with
//! `--timeout-secs <N>` or the `CARGO_MCP_TIMEOUT_SECS` environment variable.

mod diagnostics;
mod runner;
mod server;
mod test_output;

use std::time::Duration;

use anyhow::{Context, Result};
use rmcp::ServiceExt;

const DEFAULT_TIMEOUT_SECS: u64 = 120;

#[tokio::main]
async fn main() -> Result<()> {
    let timeout = Duration::from_secs(timeout_secs()?);
    // stdout carries the protocol; anything else must go to stderr.
    let service = server::CargoServer::new(timeout)
        .serve(rmcp::transport::stdio())
        .await
        .context("failed to start MCP server")?;
    service.waiting().await?;
    Ok(())
}

fn timeout_secs() -> Result<u64> {
    let mut args = std::env::args().skip(1);
    let value = match args.next().as_deref() {
        Some("--timeout-secs") => Some(args.next().context("--timeout-secs needs a value")?),
        Some(arg) => anyhow::bail!("unknown argument: {arg}"),
        None => std::env::var("CARGO_MCP_TIMEOUT_SECS").ok(),
    };
    match value {
        Some(v) => match v.parse::<u64>() {
            Ok(secs) if secs > 0 => Ok(secs),
            _ => anyhow::bail!("invalid timeout: {v:?} (expected a positive number of seconds)"),
        },
        None => Ok(DEFAULT_TIMEOUT_SECS),
    }
}
