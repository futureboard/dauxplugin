//! `daux validate` — everything wrong with a bundle, at once.
//!
//! Unlike opening a bundle, validation never stops at the first problem: the point of the
//! command is to turn a packaging mistake into one build failure with a complete list,
//! rather than four builds that each reveal the next thing.
//!
//! With `--probe` it also opens the binary and runs the manifest ↔ binary cross-check of
//! `manifest-v1` §8.1, which is the only way to catch a manifest that has drifted from the
//! code it describes. Without it, nothing in this command executes a line of plug-in code.

use std::path::Path;

use daux_bundle::{Severity, ValidationIssue};
use daux_scan::{ScanEntry, ScanErrorKind, Scanner};

use crate::cli::ValidateArgs;
use crate::cmd::{open_bundle, print_issues};
use crate::exit::Exit;
use crate::out::{IssueCounts, Out, issues_json};

/// [main-thread] Runs `daux validate`.
///
/// # Errors
///
/// When the bundle cannot be opened at all. A bundle that opens and is *wrong* is not an
/// error here: it is the answer, reported as [`Exit::Issues`].
pub fn run(args: &ValidateArgs, out: &Out) -> anyhow::Result<Exit> {
    let bundle = open_bundle(&args.bundle)?;
    let mut issues = bundle.validate();
    let mut probed = false;

    if args.probe {
        match probe(&args.bundle) {
            Ok(entry) => {
                // The scanner's list already contains `Bundle::validate`'s, plus the
                // cross-check, so it replaces rather than extends.
                issues = entry.issues;
                probed = entry.probed;
            }
            Err(issue) => issues.push(issue),
        }
    }
    issues.sort_by(|a, b| a.severity.cmp(&b.severity).then(a.code.cmp(b.code)));

    let counts = IssueCounts::of(&issues);
    let failed = counts.errors > 0 || (args.deny_warnings && counts.warnings > 0);

    if out.is_json() {
        out.emit(&serde_json::json!({
            "ok": !failed,
            "bundle": args.bundle.display().to_string(),
            "id": bundle.metadata().id,
            "name": bundle.metadata().name,
            "version": bundle.metadata().version,
            "probed": probed,
            "issues": issues_json(&issues),
            "counts": {
                "errors": counts.errors,
                "warnings": counts.warnings,
                "info": counts.infos,
            },
        }))?;
        return Ok(Exit::from_issues(failed));
    }

    out.heading(format!(
        "{} — {} {}",
        args.bundle.display(),
        bundle.metadata().name,
        bundle.metadata().version
    ));
    print_issues(out, &issues);
    if issues.is_empty() {
        out.line("  nothing wrong");
    }
    out.blank();
    out.line(counts.summary());
    if failed && counts.errors == 0 {
        out.note("failing on warnings because `--deny-warnings` was given");
    }
    Ok(Exit::from_issues(failed))
}

/// Opens the binary and cross-checks it against the manifest.
///
/// A failure here is turned into a finding rather than raised: "this bundle ships nothing
/// for your machine" is a fact about the bundle, and the rest of the report is still worth
/// printing.
fn probe(path: &Path) -> Result<ScanEntry, ValidationIssue> {
    let mut scanner = Scanner::new();
    // Nothing about validating one bundle should depend on what else is installed.
    scanner.clear_search_paths();
    scanner.set_probe(true);

    scanner.inspect(path).map_err(|error| {
        let (severity, code) = match error.kind() {
            // A cross-platform bundle on the wrong machine is not a defect.
            ScanErrorKind::NoBinaryForTarget => (Severity::Warning, "axt.probe.no-binary-here"),
            ScanErrorKind::Quarantined => (Severity::Warning, "axt.probe.quarantined"),
            _ => (Severity::Error, "axt.probe.failed"),
        };
        let message = format!("the binary could not be cross-checked: {error}");
        match severity {
            Severity::Error => ValidationIssue::error(code, message),
            Severity::Warning => ValidationIssue::warning(code, message),
            Severity::Info => ValidationIssue::info(code, message),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The command's whole contract in one place: errors fail, warnings do not, and
    /// `--deny-warnings` moves the line.
    #[test]
    fn the_exit_code_follows_the_worst_finding() {
        let clean: Vec<ValidationIssue> = Vec::new();
        let warned = vec![ValidationIssue::warning("no-vendor", "no vendor")];
        let broken = vec![ValidationIssue::error("missing-binary", "nothing there")];

        for (issues, deny_warnings, expected) in [
            (&clean, false, Exit::Ok),
            (&clean, true, Exit::Ok),
            (&warned, false, Exit::Ok),
            (&warned, true, Exit::Issues),
            (&broken, false, Exit::Issues),
            (&broken, true, Exit::Issues),
        ] {
            let counts = IssueCounts::of(issues);
            let failed = counts.errors > 0 || (deny_warnings && counts.warnings > 0);
            assert_eq!(
                Exit::from_issues(failed),
                expected,
                "{issues:?} with deny_warnings={deny_warnings}"
            );
        }
    }

    /// Info findings are informational. A bundle that simply does not target this machine
    /// must not fail a build on the machine that cross-compiled it.
    #[test]
    fn an_informational_finding_never_fails_the_command() {
        let issues = vec![ValidationIssue::info(
            "not-loadable-here",
            "another machine",
        )];
        let counts = IssueCounts::of(&issues);
        assert_eq!(counts.errors, 0);
        assert_eq!(counts.warnings, 0);
        assert_eq!(Exit::from_issues(counts.errors > 0), Exit::Ok);
    }
}
