//! `daux test` — load a bundle into a real host and check that it behaves.
//!
//! Every check here is one a DAW performs on a plug-in it has never seen, and every one of
//! them is a bug that is expensive to find any other way: a block of one frame, a block of
//! the maximum size, an impulse that turns into a `NaN`, a state blob that loads a plug-in
//! into a state it cannot come back from, a corrupt preset that is accepted rather than
//! refused.
//!
//! The host is `daux_host::TestHost`, which drives the plug-in through the C ABI — the same
//! path a DAW takes, `dlopen` and all.

use std::path::Path;

use anyhow::anyhow;
use daux_host::daux_audio::AudioStorage;
use daux_host::daux_runtime::daux_core::ProcessConfig;
use daux_host::{HostErrorKind, InstanceId, TestHost};

use crate::cli::TestArgs;
use crate::exit::Exit;
use crate::out::{Out, plural};

/// What one check concluded. [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    /// The plug-in did the right thing.
    Pass,
    /// The plug-in did the wrong thing, and here is what.
    Fail(String),
    /// The plug-in does not implement what the check is about.
    Skip(String),
}

/// One check and its outcome. [main-thread]
#[derive(Clone, Debug)]
struct Check {
    name: &'static str,
    outcome: Outcome,
}

impl Check {
    fn from_result(name: &'static str, result: Result<(), String>) -> Self {
        Self {
            name,
            outcome: result.map_or_else(Outcome::Fail, |()| Outcome::Pass),
        }
    }
}

/// [main-thread] Runs `daux test`.
///
/// # Errors
///
/// When the bundle cannot be opened or loaded at all. A plug-in that loads and then
/// misbehaves is the *answer*, reported as [`Exit::Issues`].
pub fn run(args: &TestArgs, out: &Out) -> anyhow::Result<Exit> {
    let config = ProcessConfig::new(args.audio.sample_rate, args.audio.block_size);
    let mut host = TestHost::new(config);
    let instance = load(&mut host, &args.bundle, args.plugin.as_deref())?;

    let descriptor = host
        .descriptor(instance)
        .map_err(|error| anyhow!("the instance has no descriptor: {error}"))?
        .clone();

    let channels = args.audio.channels.max(1);
    let block = args.audio.block_size.max(1) as usize;
    let mut checks = Vec::new();

    checks.push(Check {
        name: "identity",
        outcome: if descriptor.id.as_str().is_empty() || descriptor.name.is_empty() {
            Outcome::Fail("the descriptor has no id or no name".to_owned())
        } else {
            Outcome::Pass
        },
    });
    checks.push(Check::from_result(
        "processes silence",
        process_silence(&mut host, instance, channels, block),
    ));
    checks.push(Check::from_result(
        "processes an impulse",
        process_impulse(&mut host, instance, channels, block),
    ));
    checks.push(Check::from_result(
        "a block of one frame",
        process_silence(&mut host, instance, channels, 1),
    ));
    checks.push(Check::from_result(
        "reset",
        host.reset(instance).map_err(|error| error.to_string()),
    ));
    checks.push(state_round_trip(&mut host, instance));
    checks.push(hostile_state(&mut host, instance));
    checks.push(Check::from_result(
        "unload",
        host.unload(instance).map_err(|error| error.to_string()),
    ));

    let failed = checks
        .iter()
        .filter(|check| matches!(check.outcome, Outcome::Fail(_)))
        .count();

    if out.is_json() {
        out.emit(&serde_json::json!({
            "ok": failed == 0,
            "bundle": args.bundle.display().to_string(),
            "id": descriptor.id.as_str(),
            "name": descriptor.name,
            "version": descriptor.version.to_string(),
            "checks": checks
                .iter()
                .map(|check| serde_json::json!({
                    "name": check.name,
                    "outcome": match &check.outcome {
                        Outcome::Pass => "pass",
                        Outcome::Fail(_) => "fail",
                        Outcome::Skip(_) => "skip",
                    },
                    "detail": match &check.outcome {
                        Outcome::Pass => None,
                        Outcome::Fail(detail) | Outcome::Skip(detail) => Some(detail.clone()),
                    },
                }))
                .collect::<Vec<_>>(),
        }))?;
        return Ok(Exit::from_issues(failed > 0));
    }

    out.heading(format!(
        "{} — {} {} {}",
        args.bundle.display(),
        descriptor.id.as_str(),
        descriptor.name,
        descriptor.version
    ));
    for check in &checks {
        match &check.outcome {
            Outcome::Pass => out.line(format!("  ok    {}", check.name)),
            Outcome::Skip(detail) => out.line(format!("  skip  {} — {detail}", check.name)),
            Outcome::Fail(detail) => out.warn(format!("  FAIL  {} — {detail}", check.name)),
        }
    }
    out.blank();
    out.line(format!(
        "{}, {failed} failed",
        plural(checks.len(), "check")
    ));
    Ok(Exit::from_issues(failed > 0))
}

/// Loads the requested plug-in, or the bundle's principal one.
fn load(host: &mut TestHost, path: &Path, plugin: Option<&str>) -> anyhow::Result<InstanceId> {
    match plugin {
        None => host
            .load(path)
            .map_err(|error| anyhow!("`{}` did not load: {error}", path.display())),
        Some(id) => {
            let bundle = crate::cmd::open_bundle(path)?;
            host.load_plugin(&bundle, id)
                .map_err(|error| anyhow!("`{id}` did not load: {error}"))
        }
    }
}

/// Silence in; the output must be finite, whatever the plug-in decided to put there.
fn process_silence(
    host: &mut TestHost,
    instance: InstanceId,
    channels: usize,
    frames: usize,
) -> Result<(), String> {
    let input = AudioStorage::<f32>::new(channels, frames);
    let mut output = AudioStorage::<f32>::new(channels, frames);
    host.process(instance, &input, &mut output)
        .map_err(|error| error.to_string())?;
    finite(&output)
}

/// One sample at full scale. The classic way to make an uninitialised filter state produce
/// a `NaN` that then spreads through a whole mix.
fn process_impulse(
    host: &mut TestHost,
    instance: InstanceId,
    channels: usize,
    frames: usize,
) -> Result<(), String> {
    let mut input = AudioStorage::<f32>::new(channels, frames);
    for channel in 0..channels {
        if let Some(samples) = input.channel_mut(channel)
            && let Some(first) = samples.first_mut()
        {
            *first = 1.0;
        }
    }
    let mut output = AudioStorage::<f32>::new(channels, frames);
    host.process(instance, &input, &mut output)
        .map_err(|error| error.to_string())?;
    finite(&output)
}

/// Every sample has to be a number. `NaN` and infinity are the two values that survive a
/// mix bus and take a whole project down with them.
fn finite(output: &AudioStorage<f32>) -> Result<(), String> {
    for channel in 0..output.channel_count() {
        let Some(samples) = output.channel(channel) else {
            continue;
        };
        for (frame, sample) in samples.iter().enumerate() {
            if !sample.is_finite() {
                return Err(format!(
                    "channel {channel}, frame {frame} is {sample}, which is not a number"
                ));
            }
        }
    }
    Ok(())
}

/// `abi-v1` §12: what a plug-in saves, it must load back into the same state — which shows
/// up as the *second* save producing the same bytes as the first.
fn state_round_trip(host: &mut TestHost, instance: InstanceId) -> Check {
    let saved = match host.save_state(instance) {
        Ok(saved) => saved,
        Err(error) if error.kind() == HostErrorKind::Unsupported => {
            return Check {
                name: "state round-trip",
                outcome: Outcome::Skip("the plug-in publishes no state extension".to_owned()),
            };
        }
        Err(error) => {
            return Check {
                name: "state round-trip",
                outcome: Outcome::Fail(format!("saving failed: {error}")),
            };
        }
    };

    let outcome = match host.load_state(instance, &saved) {
        Err(error) => Outcome::Fail(format!("loading its own state failed: {error}")),
        Ok(()) => match host.save_state(instance) {
            Err(error) => Outcome::Fail(format!("saving after loading failed: {error}")),
            Ok(again) if again == saved => Outcome::Pass,
            Ok(again) => Outcome::Fail(format!(
                "a save/load/save round trip changed the state ({} bytes, then {} bytes)",
                saved.len(),
                again.len()
            )),
        },
    };
    Check {
        name: "state round-trip",
        outcome,
    }
}

/// A preset from another plug-in, a truncated file, a byte flipped on disk. All of them
/// must be refused, and none of them may be accepted quietly.
fn hostile_state(host: &mut TestHost, instance: InstanceId) -> Check {
    let blobs: [&[u8]; 4] = [
        b"",
        b"not a state blob at all",
        &[0xff; 64],
        b"DAUXST\0\0\x01\x00\x00\x00truncated",
    ];
    for blob in blobs {
        match host.load_state(instance, blob) {
            Err(error) if error.kind() == HostErrorKind::Unsupported => {
                return Check {
                    name: "refuses a corrupt state",
                    outcome: Outcome::Skip("the plug-in publishes no state extension".to_owned()),
                };
            }
            Err(_) => {}
            Ok(()) => {
                return Check {
                    name: "refuses a corrupt state",
                    outcome: Outcome::Fail(format!(
                        "a {}-byte blob that is not this plug-in's state was accepted",
                        blob.len()
                    )),
                };
            }
        }
    }
    Check {
        name: "refuses a corrupt state",
        outcome: Outcome::Pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage(values: &[f32]) -> AudioStorage<f32> {
        let mut storage = AudioStorage::<f32>::new(1, values.len());
        if let Some(samples) = storage.channel_mut(0) {
            samples.copy_from_slice(values);
        }
        storage
    }

    /// The check that catches the bug this command exists for: a filter whose state was
    /// never initialised turns one impulse into a buffer of `NaN`.
    #[test]
    fn a_buffer_with_a_nan_in_it_fails_and_says_where() {
        let error = finite(&storage(&[0.0, 0.5, f32::NAN, 0.25])).expect_err("a NaN is a failure");
        assert!(error.contains("frame 2"), "{error}");
        assert!(error.contains("channel 0"), "{error}");
    }

    #[test]
    fn an_infinite_sample_fails_too() {
        assert!(finite(&storage(&[f32::INFINITY])).is_err());
        assert!(finite(&storage(&[f32::NEG_INFINITY])).is_err());
    }

    /// Silence, full scale and denormals are all perfectly good audio and must pass.
    #[test]
    fn ordinary_audio_passes() {
        assert!(finite(&storage(&[0.0, -1.0, 1.0, 1e-40, -0.5])).is_ok());
        assert!(finite(&AudioStorage::<f32>::new(2, 64)).is_ok());
        assert!(finite(&AudioStorage::<f32>::new(0, 0)).is_ok());
    }

    /// A failed check has to carry its reason: "FAIL" on its own tells a developer nothing.
    #[test]
    fn a_failed_check_keeps_the_reason() {
        let check = Check::from_result("something", Err("because of this".to_owned()));
        assert_eq!(check.outcome, Outcome::Fail("because of this".to_owned()));
        assert_eq!(
            Check::from_result("something", Ok(())).outcome,
            Outcome::Pass
        );
    }

    /// A skip is not a failure. A plug-in with no state extension is a legal plug-in, and
    /// counting it as broken would make the command useless for MIDI utilities.
    #[test]
    fn a_skip_does_not_fail_the_command() {
        let checks = [
            Check {
                name: "a",
                outcome: Outcome::Pass,
            },
            Check {
                name: "b",
                outcome: Outcome::Skip("not implemented".to_owned()),
            },
        ];
        let failed = checks
            .iter()
            .filter(|check| matches!(check.outcome, Outcome::Fail(_)))
            .count();
        assert_eq!(failed, 0);
        assert_eq!(Exit::from_issues(failed > 0), Exit::Ok);
    }
}
