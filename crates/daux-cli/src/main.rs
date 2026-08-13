//! The `daux` developer CLI.
//!
//! One binary drives the whole plug-in lifecycle: scaffold a crate, build and package it,
//! check the result, look inside it, find what is installed, and run it in a test host.
//!
//! ```text
//! daux new Reverb --kind effect
//! daux build
//! daux validate target/daux/release/axt/Reverb.axt
//! daux inspect  target/daux/release/axt/Reverb.axt
//! daux scan
//! ```
//!
//! # Exit codes
//!
//! A script has to be able to tell "your bundle is broken" from "I could not even look at
//! it", so the two never share a code:
//!
//! | Code | Meaning |
//! |---:|---|
//! | 0 | the command ran and found nothing wrong |
//! | 1 | the command ran and **found problems** — validation errors, a failed check |
//! | 2 | the command line itself was wrong (clap's own usage exit) |
//! | 3 | the command **could not run** — no such file, unreadable bundle, `cargo` failed |
//! | 70 | an internal error: a bug in `daux` itself |
//!
//! # Output
//!
//! Human-readable text by default, on stdout; diagnostics on stderr. Every command that a
//! script might consume also takes `--json` and then writes exactly one JSON document to
//! stdout and nothing else. `--quiet` suppresses the decorative parts of the text output
//! without hiding a single error.
//!
//! # Threading
//!
//! Everything here is `[main-thread]`. Nothing in this crate is reachable from a plug-in's
//! audio thread; `daux run` and `daux test` drive `daux_host::TestHost`, which calls
//! `process` on the thread it is used from, exactly as a DAW would.

mod cargo_build;
mod cli;
mod cmd;
mod exit;
mod formats;
mod meta;
mod out;
mod pack;
mod template;

use std::panic::AssertUnwindSafe;
use std::process::ExitCode;

use clap::Parser;

use crate::cli::{Cli, Command};
use crate::exit::Exit;
use crate::out::Out;

fn main() -> ExitCode {
    // A CLI that prints a Rust backtrace at somebody who typed a wrong path has failed
    // twice: once at the bug, and once at the person. The hook keeps the message short and
    // actionable, and `catch_unwind` below turns the unwind into a documented exit code.
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::env::var_os("RUST_BACKTRACE").is_some() {
            default_hook(info);
        }
        eprintln!(
            "daux: internal error: {}\n\
             this is a bug in daux, not in your plug-in; \
             re-run with RUST_BACKTRACE=1 and please report it",
            panic_message(info)
        );
    }));

    let cli = Cli::parse();
    let out = Out::new(cli.json, cli.quiet);

    let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| dispatch(&cli, &out)));

    let exit = match outcome {
        Ok(Ok(exit)) => exit,
        Ok(Err(error)) => {
            out.failure(&error);
            Exit::CannotRun
        }
        Err(_) => Exit::Internal,
    };
    ExitCode::from(exit.code())
}

/// The panic payload as text, for the hook above.
fn panic_message(info: &std::panic::PanicHookInfo<'_>) -> String {
    let payload = info.payload();
    let detail = payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown".to_owned());
    match info.location() {
        Some(location) => format!("{detail} (at {location})"),
        None => detail,
    }
}

/// Runs the requested subcommand.
///
/// `Err` means the command could not run at all; `Ok(Exit::Issues)` means it ran and found
/// something wrong with the thing it was pointed at. Keeping those apart is the whole
/// reason the exit codes are what they are.
fn dispatch(cli: &Cli, out: &Out) -> anyhow::Result<Exit> {
    match &cli.command {
        Command::New(args) => cmd::new::run(args, out),
        Command::Build(args) => cmd::build::run(args, out),
        Command::Bundle(args) => cmd::bundle::run(args, out),
        Command::Validate(args) => cmd::validate::run(args, out),
        Command::Inspect(args) => cmd::inspect::run(args, out),
        Command::Scan(args) => cmd::scan::run(args, out),
        Command::Test(args) => cmd::test::run(args, out),
        Command::Run(args) => cmd::run::run(args, out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    /// `clap` can only catch a malformed command definition at runtime, so the check has to
    /// be a test. It catches duplicate short flags, an argument that is both required and
    /// defaulted, and every other way a derive can be self-contradictory.
    #[test]
    fn the_command_definition_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    /// The documented contract of the exit codes, in one place: the four outcomes are four
    /// different numbers, and "found problems" is never confused with "could not run".
    #[test]
    fn the_exit_codes_are_distinct_and_documented() {
        let codes = [
            Exit::Ok.code(),
            Exit::Issues.code(),
            Exit::CannotRun.code(),
            Exit::Internal.code(),
        ];
        assert_eq!(codes, [0, 1, 3, 70]);
        // 2 is clap's usage exit and must not be reused by anything here.
        assert!(!codes.contains(&2));
    }

    /// Every subcommand the contract names must exist and must parse.
    #[test]
    fn every_documented_subcommand_parses() {
        for argv in [
            vec!["daux", "new", "Reverb"],
            vec!["daux", "build"],
            vec!["daux", "bundle", "--binary", "x.dll"],
            vec!["daux", "validate", "Gain.axt"],
            vec!["daux", "inspect", "Gain.axt"],
            vec!["daux", "scan"],
            vec!["daux", "test", "Gain.axt"],
            vec!["daux", "run", "Gain.axt"],
        ] {
            let name = argv[1];
            Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("`daux {name}` must parse: {e}"));
        }
    }

    /// `--json` and `--quiet` are global: a script pipes any command's output.
    #[test]
    fn the_global_flags_reach_every_subcommand() {
        for argv in [
            vec!["daux", "validate", "--json", "Gain.axt"],
            vec!["daux", "--json", "validate", "Gain.axt"],
            vec!["daux", "scan", "--json", "--quiet"],
            vec!["daux", "inspect", "--json", "Gain.axt"],
        ] {
            let cli = Cli::try_parse_from(&argv).expect("parses");
            assert!(cli.json, "{argv:?}");
        }
    }

    /// A wrong command line is a usage error, never a panic and never a silent default.
    #[test]
    fn a_malformed_command_line_is_refused_rather_than_guessed_at() {
        for argv in [
            vec!["daux"],               // no subcommand
            vec!["daux", "validate"],   // no path
            vec!["daux", "inspect"],    // no path
            vec!["daux", "bundle"],     // no binary
            vec!["daux", "frobnicate"], // no such command
            vec!["daux", "validate", "--nonsense", "Gain.axt"],
            vec!["daux", "run", "Gain.axt", "--blocks", "not-a-number"],
        ] {
            let error = Cli::try_parse_from(&argv).expect_err("must be refused");
            assert_ne!(
                error.exit_code(),
                0,
                "`{argv:?}` must not be treated as success"
            );
        }
    }
}
