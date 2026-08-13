//! The command line itself: every flag `daux` accepts, in one place.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

/// The `daux` developer CLI. [main-thread]
#[derive(Debug, Parser)]
#[command(
    name = "daux",
    version,
    about = "Build, package, check and run DAUx audio plug-ins.",
    long_about = "Build, package, check and run DAUx audio plug-ins.\n\n\
                  Exit codes:\n  \
                  0   nothing wrong\n  \
                  1   ran, and found problems (validation errors, a failing check)\n  \
                  2   the command line was wrong\n  \
                  3   could not run (no such file, unreadable bundle, cargo failed)\n  \
                  70  an internal error: a bug in daux itself",
    propagate_version = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Write one JSON document to stdout instead of human-readable text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Print only what went wrong.
    #[arg(long, short, global = true)]
    pub quiet: bool,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands, as fixed by `docs/architecture/crate-contracts.md`. [main-thread]
///
/// `daux bundle` takes far more flags than the others, so the enum is as large as that
/// variant. Boxing it is not available — `clap`'s `Subcommand` derive needs the variant's
/// field to implement `Args`, which `Box<T>` does not — and would buy nothing: exactly one
/// of these is constructed per process, on the stack, and dropped when the command returns.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Scaffold a new plug-in crate.
    New(NewArgs),
    /// Build the plug-in with cargo and package it.
    Build(BuildArgs),
    /// Assemble an `.axt` from an already-built dynamic library.
    Bundle(BundleArgs),
    /// Check a bundle and report everything wrong with it.
    Validate(ValidateArgs),
    /// Print what is inside a bundle.
    Inspect(InspectArgs),
    /// Find the plug-ins installed on this machine.
    Scan(ScanArgs),
    /// Load a bundle into a test host and check that it behaves.
    Test(TestArgs),
    /// Load a bundle into a test host and run audio through it.
    Run(RunArgs),
}

/// Which template `daux new` writes. [main-thread]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Kind {
    /// Processes the audio it is given.
    #[default]
    Effect,
    /// Generates audio from notes.
    Instrument,
    /// Transforms events without producing audio.
    MidiEffect,
}

impl Kind {
    /// [main-thread] The `manifest-v1` §3.6 category slug for this template.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Effect => "effect",
            Self::Instrument => "instrument",
            Self::MidiEffect => "midi-effect",
        }
    }
}

/// `daux new`. [main-thread]
#[derive(Args, Debug)]
pub struct NewArgs {
    /// Display name of the plug-in. Also the default directory name.
    pub name: String,

    /// What kind of plug-in to scaffold.
    #[arg(long, value_enum, default_value_t = Kind::Effect)]
    pub kind: Kind,

    /// Permanent reverse-DNS plug-in id. Defaults to `com.example.<name>`.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,

    /// Vendor name written into the manifest.
    #[arg(long, value_name = "NAME", default_value = "Example Audio")]
    pub vendor: String,

    /// Where to create the crate. Defaults to `./<name>`.
    #[arg(long, value_name = "DIR")]
    pub path: Option<PathBuf>,

    /// Overwrite an existing directory's files.
    #[arg(long)]
    pub force: bool,
}

/// `daux build`. [main-thread]
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// The plug-in crate's `Cargo.toml`. Defaults to `./Cargo.toml`.
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Build the unoptimised `dev` profile instead of `release`.
    #[arg(long)]
    pub debug: bool,

    /// Cross-compile for this Rust target triple.
    #[arg(long, value_name = "TRIPLE")]
    pub target: Option<String>,

    /// Formats to package, overriding `[package.metadata.daux] formats`.
    #[arg(long, value_delimiter = ',', value_name = "FORMAT")]
    pub formats: Vec<String>,

    /// Where to write the bundles. Defaults to `target/daux/{profile}/{format}`.
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,

    /// Do not open the built binary to read its descriptors.
    #[arg(long)]
    pub no_probe: bool,

    /// Treat every warning as a failure.
    #[arg(long)]
    pub strict: bool,
}

/// `daux bundle`. [main-thread]
#[derive(Args, Debug)]
pub struct BundleArgs {
    /// The already-built dynamic library to package.
    #[arg(long, value_name = "PATH")]
    pub binary: PathBuf,

    /// Target id the binary was built for. Defaults to this machine's.
    #[arg(long, value_name = "TARGET")]
    pub target: Option<String>,

    /// Take the identity from this crate's `[package.metadata.daux]`.
    #[arg(long, value_name = "PATH")]
    pub manifest_path: Option<PathBuf>,

    /// Permanent reverse-DNS plug-in id.
    #[arg(long, value_name = "ID")]
    pub id: Option<String>,

    /// Display name; also the bundle directory name.
    #[arg(long, value_name = "NAME")]
    pub name: Option<String>,

    /// Vendor name.
    #[arg(long, value_name = "NAME")]
    pub vendor: Option<String>,

    /// Plug-in version, `MAJOR.MINOR.PATCH[.BUILD]`.
    ///
    /// Spelled `--plugin-version` because `--version` prints the CLI's own version.
    #[arg(long, value_name = "VERSION")]
    pub plugin_version: Option<String>,

    /// One-line description.
    #[arg(long, value_name = "TEXT")]
    pub description: Option<String>,

    /// Category slug: unknown, effect, instrument, midi-effect, analyzer, generator, utility.
    #[arg(long, value_name = "SLUG")]
    pub category: Option<String>,

    /// A capability to declare, by its manifest name, e.g. `audioEffect`. Repeatable.
    #[arg(long = "cap", value_name = "NAME")]
    pub caps: Vec<String>,

    /// Directory copied in as the bundle's `Resources/`.
    #[arg(long, value_name = "DIR")]
    pub resources: Option<PathBuf>,

    /// A bundled dependency to copy into `Library/{target}/`. Repeatable.
    #[arg(long = "library", value_name = "FILE")]
    pub libraries: Vec<PathBuf>,

    /// Where to write the bundle. Defaults to the binary's own directory.
    #[arg(long, value_name = "DIR")]
    pub out_dir: Option<PathBuf>,
}

/// `daux validate`. [main-thread]
#[derive(Args, Debug)]
pub struct ValidateArgs {
    /// The `.axt` bundle to check.
    #[arg(value_name = "BUNDLE")]
    pub bundle: PathBuf,

    /// Also open the binary and cross-check it against the manifest (`manifest-v1` §8.1).
    #[arg(long)]
    pub probe: bool,

    /// Exit non-zero on warnings as well as errors.
    #[arg(long)]
    pub deny_warnings: bool,
}

/// `daux inspect`. [main-thread]
#[derive(Args, Debug)]
pub struct InspectArgs {
    /// The `.axt` bundle to look inside.
    #[arg(value_name = "BUNDLE")]
    pub bundle: PathBuf,

    /// Read the manifest only; do not open the binary.
    #[arg(long)]
    pub no_probe: bool,
}

/// `daux scan`. [main-thread]
#[derive(Args, Debug)]
pub struct ScanArgs {
    /// Search this directory instead of the platform defaults. Repeatable.
    #[arg(long = "path", value_name = "DIR")]
    pub paths: Vec<PathBuf>,

    /// Catalogue from manifests only; never open a plug-in's binary.
    #[arg(long)]
    pub no_probe: bool,

    /// Cache probe results in this file across runs.
    #[arg(long, value_name = "FILE")]
    pub cache: Option<PathBuf>,

    /// Formats to look for: axt, vst3, clap.
    #[arg(long, value_delimiter = ',', value_name = "FORMAT")]
    pub formats: Vec<String>,

    /// How deep below a search path to look.
    #[arg(long, value_name = "N")]
    pub max_depth: Option<usize>,

    /// How long one plug-in may take to load before it is abandoned.
    #[arg(long, value_name = "SECONDS")]
    pub timeout: Option<u64>,

    /// Try the plug-ins that took the scanner down last time.
    #[arg(long)]
    pub clear_quarantine: bool,

    /// Exit non-zero when any artefact could not be catalogued.
    #[arg(long)]
    pub strict: bool,
}

/// The audio configuration `test` and `run` drive a plug-in with. [main-thread]
#[derive(Args, Debug)]
pub struct AudioArgs {
    /// Sample rate in hertz.
    #[arg(long, value_name = "HZ", default_value_t = 48_000.0)]
    pub sample_rate: f64,

    /// Largest block the plug-in is told to expect.
    #[arg(long, value_name = "FRAMES", default_value_t = 512)]
    pub block_size: u32,

    /// How many channels to hand it.
    #[arg(long, value_name = "N", default_value_t = 2)]
    pub channels: usize,
}

/// `daux test`. [main-thread]
#[derive(Args, Debug)]
pub struct TestArgs {
    /// The `.axt` bundle to exercise.
    #[arg(value_name = "BUNDLE")]
    pub bundle: PathBuf,

    /// Which plug-in of a multi-plug-in bundle to load. Defaults to the first.
    #[arg(long, value_name = "ID")]
    pub plugin: Option<String>,

    /// Audio configuration.
    #[command(flatten)]
    pub audio: AudioArgs,
}

/// `daux run`. [main-thread]
#[derive(Args, Debug)]
pub struct RunArgs {
    /// The `.axt` bundle to run.
    #[arg(value_name = "BUNDLE")]
    pub bundle: PathBuf,

    /// Which plug-in of a multi-plug-in bundle to load. Defaults to the first.
    #[arg(long, value_name = "ID")]
    pub plugin: Option<String>,

    /// Audio configuration.
    #[command(flatten)]
    pub audio: AudioArgs,

    /// How many blocks to process.
    #[arg(long, value_name = "N", default_value_t = 8)]
    pub blocks: u32,

    /// Set a parameter before processing, as `id=value`. Repeatable.
    #[arg(long = "param", value_name = "ID=VALUE")]
    pub params: Vec<String>,

    /// Send a note-on at the start of the first block. Repeatable.
    #[arg(long = "note", value_name = "KEY")]
    pub notes: Vec<i16>,

    /// Feed an impulse rather than silence, so an effect has something to work on.
    #[arg(long)]
    pub impulse: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_new_template_kinds_match_the_manifest_category_slugs() {
        assert_eq!(Kind::Effect.slug(), "effect");
        assert_eq!(Kind::Instrument.slug(), "instrument");
        assert_eq!(Kind::MidiEffect.slug(), "midi-effect");
        for kind in [Kind::Effect, Kind::Instrument, Kind::MidiEffect] {
            assert!(
                daux_bundle::Category::parse(kind.slug()).is_some(),
                "`{}` must be a category daux-bundle knows",
                kind.slug()
            );
        }
    }

    /// `--version` is clap's, so the plug-in's version needs another spelling. A regression
    /// here would silently print the CLI's version into a bundle's manifest.
    #[test]
    fn the_plugin_version_flag_does_not_collide_with_the_cli_version_flag() {
        let cli = Cli::try_parse_from([
            "daux",
            "bundle",
            "--binary",
            "x.dll",
            "--plugin-version",
            "2.3.4",
        ])
        .expect("parses");
        let Command::Bundle(args) = cli.command else {
            panic!("expected `bundle`");
        };
        assert_eq!(args.plugin_version.as_deref(), Some("2.3.4"));
    }

    #[test]
    fn repeatable_flags_collect_every_occurrence() {
        let cli = Cli::try_parse_from([
            "daux", "run", "Gain.axt", "--param", "1=0.5", "--param", "2=1", "--note", "60",
            "--note", "64",
        ])
        .expect("parses");
        let Command::Run(args) = cli.command else {
            panic!("expected `run`");
        };
        assert_eq!(args.params, ["1=0.5", "2=1"]);
        assert_eq!(args.notes, [60, 64]);
        assert_eq!(args.blocks, 8, "the default survives other flags");
    }

    #[test]
    fn comma_separated_lists_are_split() {
        let cli = Cli::try_parse_from(["daux", "scan", "--formats", "axt,clap"]).expect("parses");
        let Command::Scan(args) = cli.command else {
            panic!("expected `scan`");
        };
        assert_eq!(args.formats, ["axt", "clap"]);
    }

    #[test]
    fn the_audio_defaults_are_a_usable_configuration() {
        let cli = Cli::try_parse_from(["daux", "test", "Gain.axt"]).expect("parses");
        let Command::Test(args) = cli.command else {
            panic!("expected `test`");
        };
        assert!(args.audio.sample_rate > 0.0);
        assert!(args.audio.block_size > 0);
        assert!(args.audio.channels > 0);
    }
}
