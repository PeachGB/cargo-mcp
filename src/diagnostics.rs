//! Turns cargo's `--message-format=json` stream into compact diagnostics.

use std::collections::HashSet;

use cargo_metadata::Message;
use cargo_metadata::diagnostic::{Diagnostic as RustcDiagnostic, DiagnosticLevel, DiagnosticSpan};
use schemars::JsonSchema;
use serde::Serialize;

/// Upper bound on diagnostics returned, to keep responses a sane size.
pub const MAX_DIAGNOSTICS: usize = 200;

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    /// `error`, `warning`, `note`, `help`, `failure-note` or `ice`.
    pub level: String,
    /// Lint or error code, e.g. `E0308` or `clippy::needless_return`.
    pub code: Option<String>,
    pub message: String,
    pub file: Option<String>,
    pub line_start: Option<usize>,
    pub column_start: Option<usize>,
    /// Machine-applicable fix proposed by the compiler, if any.
    pub suggestion: Option<Suggestion>,
}

#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq, Eq, Hash)]
pub struct Suggestion {
    pub message: String,
    /// Text to put in place of the span below.
    pub replacement: String,
    pub file: String,
    pub line_start: usize,
    pub column_start: usize,
    pub line_end: usize,
    pub column_end: usize,
}

#[derive(Debug, Default)]
pub struct Parsed {
    pub diagnostics: Vec<Diagnostic>,
    pub error_count: usize,
    pub warning_count: usize,
    /// Set when diagnostics were dropped because of [`MAX_DIAGNOSTICS`].
    pub truncated: bool,
    /// Outcome of the `build-finished` message, if cargo emitted one.
    pub build_success: Option<bool>,
    /// Lines of stdout that were not cargo JSON (e.g. libtest output).
    pub text_lines: Vec<String>,
}

pub fn parse(stdout: &[u8]) -> Parsed {
    let mut parsed = Parsed::default();
    let mut seen = HashSet::new();

    for message in Message::parse_stream(stdout).flatten() {
        match message {
            Message::CompilerMessage(msg) => {
                let diag = &msg.message;
                if is_summary(diag) {
                    continue;
                }
                let diag = convert(diag);
                // Targets sharing sources (lib + lib test, …) repeat diagnostics.
                if !seen.insert(diag.clone()) {
                    continue;
                }
                match diag.level.as_str() {
                    "error" | "ice" => parsed.error_count += 1,
                    "warning" => parsed.warning_count += 1,
                    _ => {}
                }
                if parsed.diagnostics.len() < MAX_DIAGNOSTICS {
                    parsed.diagnostics.push(diag);
                } else {
                    parsed.truncated = true;
                }
            }
            Message::BuildFinished(finished) => parsed.build_success = Some(finished.success),
            Message::TextLine(line) => parsed.text_lines.push(line),
            _ => {}
        }
    }
    parsed
}

/// rustc's closing "aborting due to N errors" / "N warnings emitted" /
/// "For more information about this error…" lines.
fn is_summary(diag: &RustcDiagnostic) -> bool {
    diag.spans.is_empty()
        && diag.code.is_none()
        && (diag.level == DiagnosticLevel::FailureNote
            || diag.message.starts_with("aborting due to")
            || diag.message.ends_with("warning emitted")
            || diag.message.ends_with("warnings emitted"))
}

fn convert(diag: &RustcDiagnostic) -> Diagnostic {
    let span = primary_span(&diag.spans);
    Diagnostic {
        level: level_name(diag.level).to_owned(),
        code: diag.code.as_ref().map(|c| c.code.clone()),
        message: diag.message.clone(),
        file: span.map(|s| s.file_name.clone()),
        line_start: span.map(|s| s.line_start),
        column_start: span.map(|s| s.column_start),
        suggestion: find_suggestion(diag),
    }
}

/// The primary span, followed out of macro expansions to the user's code.
fn primary_span(spans: &[DiagnosticSpan]) -> Option<&DiagnosticSpan> {
    let mut span = spans.iter().find(|s| s.is_primary).or(spans.first())?;
    while let Some(expansion) = &span.expansion {
        span = &expansion.span;
    }
    Some(span)
}

fn find_suggestion(diag: &RustcDiagnostic) -> Option<Suggestion> {
    std::iter::once(diag).chain(&diag.children).find_map(|d| {
        d.spans.iter().find_map(|span| {
            let replacement = span.suggested_replacement.as_ref()?;
            Some(Suggestion {
                message: d.message.clone(),
                replacement: replacement.clone(),
                file: span.file_name.clone(),
                line_start: span.line_start,
                column_start: span.column_start,
                line_end: span.line_end,
                column_end: span.column_end,
            })
        })
    })
}

fn level_name(level: DiagnosticLevel) -> &'static str {
    match level {
        DiagnosticLevel::Ice => "ice",
        DiagnosticLevel::Error => "error",
        DiagnosticLevel::Warning => "warning",
        DiagnosticLevel::FailureNote => "failure-note",
        DiagnosticLevel::Note => "note",
        DiagnosticLevel::Help => "help",
        _ => "unknown",
    }
}
