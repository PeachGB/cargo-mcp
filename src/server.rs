use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::diagnostics::{self, Diagnostic};
use crate::runner;
use crate::test_output::{self, TestFailure};

/// Per-call timeouts above this are clamped.
const MAX_TIMEOUT_SECS: u64 = 60 * 60;
/// How much of cargo's stderr is returned when it explains a failure.
const MAX_STDERR: usize = 4 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CargoParams {
    /// Path to the project directory (must contain a Cargo.toml).
    pub path: String,
    /// Package to operate on within the workspace (`-p`).
    pub package: Option<String>,
    /// Features to activate.
    pub features: Option<Vec<String>>,
    /// Timeout in seconds; defaults to the server's configured timeout.
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct BuildReport {
    pub success: bool,
    pub error_count: usize,
    pub warning_count: usize,
    pub diagnostics: Vec<Diagnostic>,
    /// True when more diagnostics were emitted than returned.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// Tail of cargo's stderr, when the failure is not explained otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TestReport {
    pub success: bool,
    pub passed: usize,
    pub failed: usize,
    pub ignored: usize,
    pub failures: Vec<TestFailure>,
    pub error_count: usize,
    pub warning_count: usize,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CargoServer {
    default_timeout: Duration,
}

#[tool_router]
impl CargoServer {
    pub fn new(default_timeout: Duration) -> Self {
        Self { default_timeout }
    }

    #[tool(description = "Run `cargo check` and return structured compiler diagnostics.")]
    async fn cargo_check(
        &self,
        Parameters(params): Parameters<CargoParams>,
    ) -> Result<Json<BuildReport>, String> {
        self.build("check", params)
            .await
            .map(Json)
            .map_err(|e| format!("{e:#}"))
    }

    #[tool(description = "Run `cargo clippy` and return structured lint diagnostics.")]
    async fn cargo_clippy(
        &self,
        Parameters(params): Parameters<CargoParams>,
    ) -> Result<Json<BuildReport>, String> {
        self.build("clippy", params)
            .await
            .map(Json)
            .map_err(|e| format!("{e:#}"))
    }

    #[tool(
        description = "Run `cargo test` and return passed/failed/ignored counts, the output of \
                       each failing test, and any compiler diagnostics."
    )]
    async fn cargo_test(
        &self,
        Parameters(params): Parameters<CargoParams>,
    ) -> Result<Json<TestReport>, String> {
        self.test(params)
            .await
            .map(Json)
            .map_err(|e| format!("{e:#}"))
    }
}

#[tool_handler(
    name = "cargo-mcp",
    instructions = "Runs cargo check, clippy and test on a local Rust project and returns \
                    structured results. `path` must point at a directory with a Cargo.toml."
)]
impl ServerHandler for CargoServer {}

impl CargoServer {
    async fn build(&self, subcommand: &str, params: CargoParams) -> Result<BuildReport> {
        let (dir, args) = prepare(subcommand, &params)?;
        let output = runner::run_cargo(&dir, &args, self.timeout(&params)).await?;
        let parsed = diagnostics::parse(&output.stdout);
        let success = output.status.success() && parsed.build_success != Some(false);
        let stderr = (!success && parsed.error_count == 0).then(|| stderr_tail(&output.stderr));
        Ok(BuildReport {
            success,
            error_count: parsed.error_count,
            warning_count: parsed.warning_count,
            diagnostics: parsed.diagnostics,
            truncated: parsed.truncated,
            stderr,
        })
    }

    async fn test(&self, params: CargoParams) -> Result<TestReport> {
        let (dir, mut args) = prepare("test", &params)?;
        // Run every test binary even if an earlier one fails.
        args.push("--no-fail-fast".into());
        let output = runner::run_cargo(&dir, &args, self.timeout(&params)).await?;
        let parsed = diagnostics::parse(&output.stdout);
        let tests = test_output::parse(&parsed.text_lines);
        let success = output.status.success();
        let explained = parsed.error_count > 0 || !tests.failures.is_empty();
        let stderr = (!success && !explained).then(|| stderr_tail(&output.stderr));
        Ok(TestReport {
            success,
            passed: tests.passed,
            failed: tests.failed,
            ignored: tests.ignored,
            failures: tests.failures,
            error_count: parsed.error_count,
            warning_count: parsed.warning_count,
            diagnostics: parsed.diagnostics,
            truncated: parsed.truncated,
            stderr,
        })
    }

    fn timeout(&self, params: &CargoParams) -> Duration {
        params
            .timeout_secs
            .map(|secs| Duration::from_secs(secs.clamp(1, MAX_TIMEOUT_SECS)))
            .unwrap_or(self.default_timeout)
    }
}

/// Validates the parameters and builds the cargo command line.
fn prepare(subcommand: &str, params: &CargoParams) -> Result<(PathBuf, Vec<String>)> {
    let dir = validate_project_dir(Path::new(&params.path))?;
    let mut args = vec![
        subcommand.to_owned(),
        "--message-format=json".to_owned(),
        format!("--manifest-path={}", dir.join("Cargo.toml").display()),
    ];
    if let Some(package) = &params.package {
        ensure!(
            is_valid_package_spec(package),
            "invalid package name: {package:?}"
        );
        args.push(format!("--package={package}"));
    }
    if let Some(features) = params.features.as_deref().filter(|f| !f.is_empty()) {
        for feature in features {
            ensure!(
                is_valid_feature(feature),
                "invalid feature name: {feature:?}"
            );
        }
        args.push(format!("--features={}", features.join(",")));
    }
    Ok((dir, args))
}

fn validate_project_dir(path: &Path) -> Result<PathBuf> {
    let dir = path.canonicalize().with_context(|| {
        format!(
            "path does not exist or is not accessible: {}",
            path.display()
        )
    })?;
    ensure!(dir.is_dir(), "path is not a directory: {}", dir.display());
    if !dir.join("Cargo.toml").is_file() {
        bail!("no Cargo.toml found in {}", dir.display());
    }
    Ok(dir)
}

/// Package names plus the optional `@version` / `registry-url#name` spec forms;
/// anything that could be read as another flag is rejected.
fn is_valid_package_spec(spec: &str) -> bool {
    !spec.is_empty()
        && !spec.starts_with('-')
        && spec
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_@.+:/#".contains(c))
}

/// `feature` or `dependency/feature`.
fn is_valid_feature(feature: &str) -> bool {
    !feature.is_empty()
        && !feature.starts_with('-')
        && feature
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_/+.".contains(c))
}

fn stderr_tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let text = text.trim();
    let mut start = text.len().saturating_sub(MAX_STDERR);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_flag_like_names() {
        assert!(is_valid_package_spec("serde_json@1.0.0"));
        assert!(!is_valid_package_spec("--config=x"));
        assert!(!is_valid_package_spec("a b"));
        assert!(is_valid_feature("tokio/full"));
        assert!(!is_valid_feature("a,b"));
        assert!(!is_valid_feature("-x"));
    }

    #[test]
    fn validates_project_dir() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(validate_project_dir(&tmp.path().join("missing")).is_err());
        assert!(validate_project_dir(tmp.path()).is_err());
        std::fs::write(tmp.path().join("Cargo.toml"), "").unwrap();
        assert!(validate_project_dir(tmp.path()).is_ok());
        assert!(validate_project_dir(&tmp.path().join("Cargo.toml")).is_err());
    }
}
