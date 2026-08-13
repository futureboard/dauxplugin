//! Printing: one place that knows about `--json`, `--quiet`, stdout and stderr.
//!
//! Two rules the whole CLI depends on:
//!
//! * in `--json` mode stdout carries **exactly one** JSON document and nothing else, so a
//!   script can pipe any command straight into a parser;
//! * `--quiet` hides the decorative output and never hides a diagnostic.

use std::fmt::Display;
use std::io::Write as _;

use daux_bundle::{Severity, ValidationIssue};

/// The CLI's output channel. [main-thread]
#[derive(Clone, Copy, Debug)]
pub struct Out {
    json: bool,
    quiet: bool,
}

impl Out {
    /// [main-thread] An output channel for the given global flags.
    pub const fn new(json: bool, quiet: bool) -> Self {
        Self { json, quiet }
    }

    /// [main-thread] Whether the caller asked for machine-readable output.
    pub const fn is_json(self) -> bool {
        self.json
    }

    /// Whether human-readable text should be written at all.
    const fn prose(self) -> bool {
        !self.json && !self.quiet
    }

    /// [main-thread] A line of prose. Suppressed by `--json` and `--quiet`.
    pub fn line(self, text: impl Display) {
        if self.prose() {
            println!("{text}");
        }
    }

    /// [main-thread] A blank separating line.
    pub fn blank(self) {
        if self.prose() {
            println!();
        }
    }

    /// [main-thread] A section heading.
    pub fn heading(self, text: impl Display) {
        if self.prose() {
            println!("{text}");
        }
    }

    /// [main-thread] An indented `key   value` line.
    ///
    /// The key column is fixed so that a block of fields lines up whatever it describes.
    pub fn field(self, key: &str, value: impl Display) {
        if self.prose() {
            println!("  {key:<18}{value}");
        }
    }

    /// [main-thread] An indented `key   value` line, printed only when `value` is `Some`.
    pub fn opt_field(self, key: &str, value: Option<impl Display>) {
        if let Some(value) = value {
            self.field(key, value);
        }
    }

    /// [main-thread] A note. Survives `--quiet`, because it is a diagnostic.
    pub fn note(self, text: impl Display) {
        if !self.json {
            eprintln!("note: {text}");
        }
    }

    /// [main-thread] A warning. Survives `--quiet`.
    pub fn warn(self, text: impl Display) {
        if !self.json {
            eprintln!("warning: {text}");
        }
    }

    /// [main-thread] One validation finding, rendered with its severity and stable code.
    ///
    /// The code is what tooling and tests match on, so it is always printed, even in the
    /// prose form.
    pub fn issue(self, issue: &ValidationIssue) {
        if self.json {
            return;
        }
        let marker = issue.severity.as_str();
        let line = format!("  {marker:<8}[{}] {}", issue.code, issue.message);
        // Errors and warnings belong on stderr so that `daux validate x > report.txt`
        // still shows the reader what went wrong; info is part of the report.
        match issue.severity {
            Severity::Info => println!("{line}"),
            _ => eprintln!("{line}"),
        }
    }

    /// [main-thread] Writes the single JSON document a `--json` run produces.
    ///
    /// A no-op in text mode, so a command can build its value unconditionally and let the
    /// channel decide.
    ///
    /// # Errors
    ///
    /// Whatever stdout reports; a closed pipe is the usual one.
    pub fn emit(self, value: &serde_json::Value) -> anyhow::Result<()> {
        if !self.json {
            return Ok(());
        }
        let text = serde_json::to_string_pretty(value)?;
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        lock.write_all(text.as_bytes())?;
        lock.write_all(b"\n")?;
        lock.flush()?;
        Ok(())
    }

    /// [main-thread] Reports a command that could not run.
    ///
    /// In `--json` mode this is still the one document the run produces, so a script never
    /// has to parse stderr to find out what happened.
    pub fn failure(self, error: &anyhow::Error) {
        if self.json {
            let causes: Vec<String> = error.chain().skip(1).map(ToString::to_string).collect();
            let document = serde_json::json!({
                "ok": false,
                "error": error.to_string(),
                "causes": causes,
            });
            // Nothing useful is left to do if even this cannot be written.
            if let Ok(text) = serde_json::to_string_pretty(&document) {
                println!("{text}");
            }
            return;
        }
        eprintln!("daux: error: {error}");
        for cause in error.chain().skip(1) {
            eprintln!("  caused by: {cause}");
        }
    }
}

/// [main-thread] `n item` / `n items`, so that counted output reads like English.
pub fn plural(count: usize, singular: &str) -> String {
    if count == 1 {
        format!("{count} {singular}")
    } else {
        format!("{count} {singular}s")
    }
}

/// How many findings of each severity a list holds. [main-thread]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IssueCounts {
    /// Findings that make the bundle unusable or wrong.
    pub errors: usize,
    /// Findings that will bite later.
    pub warnings: usize,
    /// Findings worth knowing about.
    pub infos: usize,
}

impl IssueCounts {
    /// [main-thread] Counts a list of findings.
    pub fn of(issues: &[ValidationIssue]) -> Self {
        let mut counts = Self::default();
        for issue in issues {
            match issue.severity {
                Severity::Error => counts.errors += 1,
                Severity::Warning => counts.warnings += 1,
                Severity::Info => counts.infos += 1,
            }
        }
        counts
    }

    /// [main-thread] A one-line summary: `2 errors, 1 warning, 0 info`.
    pub fn summary(self) -> String {
        format!(
            "{}, {}, {} info",
            plural(self.errors, "error"),
            plural(self.warnings, "warning"),
            self.infos
        )
    }
}

/// [main-thread] A finding as JSON, for `--json` output.
pub fn issue_json(issue: &ValidationIssue) -> serde_json::Value {
    serde_json::json!({
        "severity": issue.severity.as_str(),
        "code": issue.code,
        "message": issue.message,
    })
}

/// [main-thread] A list of findings as a JSON array.
pub fn issues_json(issues: &[ValidationIssue]) -> serde_json::Value {
    serde_json::Value::Array(issues.iter().map(issue_json).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<ValidationIssue> {
        vec![
            ValidationIssue::error("missing-binary", "no binary for `linux-x86_64`"),
            ValidationIssue::error("no-targets", "nothing declared"),
            ValidationIssue::warning("no-vendor", "no vendor"),
            ValidationIssue::info("not-loadable-here", "not for this machine"),
        ]
    }

    #[test]
    fn findings_are_counted_by_severity() {
        let counts = IssueCounts::of(&sample());
        assert_eq!(counts.errors, 2);
        assert_eq!(counts.warnings, 1);
        assert_eq!(counts.infos, 1);
        assert_eq!(counts.summary(), "2 errors, 1 warning, 1 info");
        assert_eq!(
            IssueCounts::of(&[]).summary(),
            "0 errors, 0 warnings, 0 info"
        );
    }

    /// The stable code is what tooling matches on, so it must survive into the JSON
    /// verbatim — including the severity spelling, which is part of the contract.
    #[test]
    fn the_json_form_carries_the_stable_code_and_severity() {
        let value = issues_json(&sample());
        let array = value.as_array().expect("an array");
        assert_eq!(array.len(), 4);
        assert_eq!(array[0]["code"], "missing-binary");
        assert_eq!(array[0]["severity"], "error");
        assert_eq!(array[2]["severity"], "warning");
        assert_eq!(array[3]["severity"], "info");
        assert_eq!(array[3]["message"], "not for this machine");
    }

    #[test]
    fn counting_words_agree_with_their_numbers() {
        assert_eq!(plural(0, "error"), "0 errors");
        assert_eq!(plural(1, "error"), "1 error");
        assert_eq!(plural(2, "plug-in"), "2 plug-ins");
    }

    /// `--json` and `--quiet` are independent switches, and `--json` implies "no prose"
    /// whether or not `--quiet` was given: a stray line of text would corrupt the document.
    #[test]
    fn json_mode_never_writes_prose() {
        assert!(!Out::new(true, false).prose());
        assert!(!Out::new(true, true).prose());
        assert!(!Out::new(false, true).prose());
        assert!(Out::new(false, false).prose());
    }

    /// `emit` is a no-op in text mode, so a command can build its document unconditionally.
    #[test]
    fn emitting_json_in_text_mode_does_nothing() {
        let out = Out::new(false, false);
        out.emit(&serde_json::json!({ "never": "printed" }))
            .expect("a no-op cannot fail");
    }
}
