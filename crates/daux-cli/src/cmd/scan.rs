//! `daux scan` — what is installed on this machine.
//!
//! A scan never fails. It reports what it found, what it could not describe, and what it
//! refused to open, and one broken plug-in costs exactly one line of the report. `--strict`
//! exists for the one caller who wants the opposite — a CI job proving that a freshly
//! installed tree has nothing wrong in it.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::anyhow;
use daux_scan::{PluginFormat, ScanEntry, ScanReport, Scanner};

use crate::cli::ScanArgs;
use crate::exit::Exit;
use crate::out::{Out, plural};

/// [main-thread] Runs `daux scan`.
///
/// # Errors
///
/// Only when an argument names something that cannot be used at all — an unknown format,
/// for instance. A search path that does not exist is normal and is not an error.
pub fn run(args: &ScanArgs, out: &Out) -> anyhow::Result<Exit> {
    let mut scanner = match &args.cache {
        Some(path) => Scanner::with_cache(path.clone()),
        None => Scanner::new(),
    };

    if !args.paths.is_empty() {
        scanner.clear_search_paths();
        for path in &args.paths {
            scanner.add_search_path(path.clone());
        }
    }
    if !args.formats.is_empty() {
        let formats = parse_formats(&args.formats)?;
        scanner.set_formats(&formats);
    }
    scanner.set_probe(!args.no_probe);
    if let Some(depth) = args.max_depth {
        scanner.set_max_depth(depth);
    }
    if let Some(seconds) = args.timeout {
        scanner.set_probe_timeout(Duration::from_secs(seconds));
    }
    if args.clear_quarantine {
        scanner.clear_quarantine();
    }

    // Worth saying before the scan rather than after: the user is about to wait, and one of
    // these is the plug-in that made the last wait end badly.
    let crashed: Vec<PathBuf> = scanner.crashed_last_time().to_vec();
    for path in &crashed {
        out.warn(format!(
            "`{}` did not survive the last scan and is skipped; \
             `--clear-quarantine` tries it again",
            path.display()
        ));
    }

    let report = scanner.scan();
    let failed = args.strict && !report.failures().is_empty();

    if out.is_json() {
        emit_json(out, &report, failed)?;
        return Ok(Exit::from_issues(failed));
    }

    print(out, &report, scanner.search_paths());
    Ok(Exit::from_issues(failed))
}

/// Turns `--formats axt,clap` into the scanner's own enumeration.
fn parse_formats(names: &[String]) -> anyhow::Result<Vec<PluginFormat>> {
    names
        .iter()
        .map(|name| {
            PluginFormat::from_extension(name.trim()).ok_or_else(|| {
                anyhow!(
                    "`{name}` is not a plug-in format; expected one of {}",
                    PluginFormat::ALL.map(PluginFormat::as_str).join(", ")
                )
            })
        })
        .collect()
}

/// The human-readable report.
fn print(out: &Out, report: &ScanReport, search_paths: &[PathBuf]) {
    let stats = report.stats();
    out.line(format!(
        "searched {} in {:?}",
        plural(stats.directories, "directory"),
        stats.duration
    ));
    out.line(format!(
        "{}, {} from cache, {} opened, {} skipped",
        plural(report.len(), "plug-in"),
        stats.from_cache,
        stats.probed,
        stats.failed
    ));
    if search_paths.is_empty() {
        out.note("no search paths: nothing was looked at");
    }

    if !report.entries().is_empty() {
        out.blank();
        for entry in report.entries() {
            print_entry(out, entry);
        }
    }

    if !report.foreign().is_empty() {
        out.blank();
        out.heading("found, not described (a foreign format's identity lives in its binary)");
        for foreign in report.foreign() {
            out.line(format!(
                "  {:<6}{}",
                foreign.format.as_str(),
                foreign.path.display()
            ));
        }
    }

    if !report.failures().is_empty() {
        out.blank();
        out.heading("could not be catalogued");
        for failure in report.failures() {
            let sticky = if failure.is_sticky() {
                "  (skipped again until it changes)"
            } else {
                ""
            };
            out.warn(format!("{failure}{sticky}"));
        }
    }
}

/// One catalogued bundle.
fn print_entry(out: &Out, entry: &ScanEntry) {
    let state = if entry.probed { "probed" } else { "manifest" };
    out.line(format!(
        "{}  {} {}  — {}",
        entry.id(),
        entry.name(),
        entry.metadata.version,
        entry.vendor()
    ));
    out.line(format!(
        "  {}  [{state}, {}]",
        entry.path.display(),
        plural(entry.plugin_count(), "plug-in")
    ));
    if entry.has_errors() {
        for issue in &entry.issues {
            if issue.severity == daux_bundle::Severity::Error {
                out.warn(format!("  [{}] {}", issue.code, issue.message));
            }
        }
    }
}

/// The machine-readable report.
fn emit_json(out: &Out, report: &ScanReport, failed: bool) -> anyhow::Result<()> {
    let stats = report.stats();
    out.emit(&serde_json::json!({
        "ok": !failed,
        "stats": {
            "examined": stats.examined,
            "fromCache": stats.from_cache,
            "probed": stats.probed,
            "failed": stats.failed,
            "quarantined": stats.quarantined,
            "directories": stats.directories,
            "durationMs": stats.duration.as_millis(),
        },
        "plugins": report
            .entries()
            .iter()
            .map(|entry| serde_json::json!({
                "id": entry.id(),
                "name": entry.name(),
                "vendor": entry.vendor(),
                "version": entry.metadata.version,
                "path": entry.path.display().to_string(),
                "format": entry.format.as_str(),
                "probed": entry.probed,
                "pluginCount": entry.plugin_count(),
                "hasErrors": entry.has_errors(),
                "issues": crate::out::issues_json(&entry.issues),
            }))
            .collect::<Vec<_>>(),
        "foreign": report
            .foreign()
            .iter()
            .map(|foreign| serde_json::json!({
                "path": foreign.path.display().to_string(),
                "format": foreign.format.as_str(),
            }))
            .collect::<Vec<_>>(),
        "failures": report
            .failures()
            .iter()
            .map(|failure| serde_json::json!({
                "path": failure.path.display().to_string(),
                "format": failure.format.map(PluginFormat::as_str),
                "kind": failure.kind.as_str(),
                "message": failure.message,
                "sticky": failure.is_sticky(),
            }))
            .collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_name_the_scanner_knows_is_accepted() {
        let parsed = parse_formats(&["axt".to_owned(), "vst3".to_owned(), " clap ".to_owned()])
            .expect("all three are formats");
        assert_eq!(parsed, PluginFormat::ALL.to_vec());
    }

    /// A typo'd format must not silently scan for everything, or for nothing.
    #[test]
    fn an_unknown_format_is_refused_with_the_list_of_real_ones() {
        let error = parse_formats(&["au".to_owned()]).expect_err("there is no `au` here");
        let text = error.to_string();
        assert!(text.contains("au"), "{text}");
        assert!(
            text.contains("axt") && text.contains("vst3") && text.contains("clap"),
            "{text}"
        );
    }

    /// A scan of an empty tree is a successful scan, and `--strict` does not change that:
    /// finding nothing is not the same as failing.
    #[test]
    fn an_empty_report_is_a_success_even_under_strict() {
        let report = ScanReport::new();
        assert!(report.failures().is_empty());
        assert_eq!(Exit::from_issues(!report.failures().is_empty()), Exit::Ok);
    }
}
