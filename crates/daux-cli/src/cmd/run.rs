//! `daux run` — instantiate a bundle and push audio through it.
//!
//! `daux test` asks whether a plug-in behaves; this asks what it *does*. It is the command
//! for "the parameter is set, the note is sent, and nothing comes out" — the peak level per
//! channel, the reported latency and tail, and the status the plug-in returned are usually
//! enough to tell which of the three is wrong.

use std::path::Path;
use std::time::Instant;

use anyhow::{anyhow, bail};
use daux_host::daux_audio::AudioStorage;
use daux_host::daux_runtime::daux_core::{Latency, ProcessConfig, ProcessStatus, Tail};
use daux_host::{InstanceId, TestHost};

use crate::cli::RunArgs;
use crate::exit::Exit;
use crate::out::{Out, plural};

/// [main-thread] Runs `daux run`.
///
/// # Errors
///
/// When the bundle cannot be loaded, when a `--param` cannot be parsed, or when the plug-in
/// refuses a block.
pub fn run(args: &RunArgs, out: &Out) -> anyhow::Result<Exit> {
    let config = ProcessConfig::new(args.audio.sample_rate, args.audio.block_size);
    let mut host = TestHost::new(config);
    let instance = load(&mut host, &args.bundle, args.plugin.as_deref())?;

    let descriptor = host
        .descriptor(instance)
        .map_err(|error| anyhow!("the instance has no descriptor: {error}"))?
        .clone();

    let mut requested = Vec::with_capacity(args.params.len());
    for assignment in &args.params {
        let (id, value) = parse_param(assignment)?;
        host.try_set_param(instance, id, value)
            .map_err(|error| anyhow!("`{assignment}`: {error}"))?;
        requested.push((id, value));
    }
    for key in &args.notes {
        host.try_send_note_on(instance, 0, *key, 0.8)
            .map_err(|error| anyhow!("note {key}: {error}"))?;
    }

    let channels = args.audio.channels.max(1);
    let frames = args.audio.block_size.max(1) as usize;
    let mut input = AudioStorage::<f32>::new(channels, frames);
    if args.impulse {
        for channel in 0..channels {
            if let Some(samples) = input.channel_mut(channel)
                && let Some(first) = samples.first_mut()
            {
                *first = 1.0;
            }
        }
    }
    let mut output = AudioStorage::<f32>::new(channels, frames);

    let started = Instant::now();
    let mut peaks = vec![0.0f32; channels];
    let mut status = ProcessStatus::Continue;
    let mut events_out = 0usize;
    for block in 0..args.blocks.max(1) {
        status = host
            .process(instance, &input, &mut output)
            .map_err(|error| anyhow!("block {block}: {error}"))?;
        for (channel, peak) in peaks.iter_mut().enumerate() {
            if let Some(samples) = output.channel(channel) {
                for sample in samples {
                    let magnitude = sample.abs();
                    if magnitude > *peak {
                        *peak = magnitude;
                    }
                }
            }
        }
        events_out += host.output_event_count(instance).unwrap_or(0);
        // Only the first block carries the impulse; a repeated one is a click train.
        if args.impulse && block == 0 {
            input = AudioStorage::<f32>::new(channels, frames);
        }
    }
    let elapsed = started.elapsed();

    // A parameter is changed by an event in the block, and the value only moves when the
    // plug-in applies it. Reading it back afterwards is the difference between "the plug-in
    // ignores automation" and "the plug-in is doing something else with the value" — two
    // very different bugs that look identical from the output alone.
    let drift = drifted(&mut host, instance, &requested);

    let latency = host.latency(instance).unwrap_or(Latency::Zero);
    let tail = host.tail(instance).unwrap_or(Tail::None);
    let silent = peaks.iter().all(|peak| *peak == 0.0);
    let unusable = peaks.iter().any(|peak| !peak.is_finite());

    if out.is_json() {
        out.emit(&serde_json::json!({
            "ok": !unusable,
            "bundle": args.bundle.display().to_string(),
            "id": descriptor.id.as_str(),
            "name": descriptor.name,
            "blocks": args.blocks.max(1),
            "frames": frames,
            "channels": channels,
            "sampleRate": args.audio.sample_rate,
            "status": format!("{status:?}"),
            "latencySamples": match latency {
                Latency::Zero => 0,
                Latency::Samples(samples) => samples,
            },
            "tail": format!("{tail:?}"),
            "peaks": peaks,
            "silent": silent,
            "outputEvents": events_out,
            "elapsedMs": elapsed.as_secs_f64() * 1000.0,
            "parameters": drift
                .iter()
                .map(|(id, wanted, actual)| serde_json::json!({
                    "id": id,
                    "requested": wanted,
                    "reported": actual,
                }))
                .collect::<Vec<_>>(),
        }))?;
        return Ok(Exit::from_issues(unusable));
    }

    out.heading(format!(
        "{} — {} {}",
        args.bundle.display(),
        descriptor.name,
        descriptor.version
    ));
    out.field(
        "processed",
        format!(
            "{} of {} frames at {} Hz, {} channels",
            plural(args.blocks.max(1) as usize, "block"),
            frames,
            args.audio.sample_rate,
            channels
        ),
    );
    out.field("status", format!("{status:?}"));
    out.field(
        "latency",
        match latency {
            Latency::Zero => "0 samples".to_owned(),
            Latency::Samples(samples) => format!("{samples} samples"),
        },
    );
    out.field("tail", format!("{tail:?}"));
    out.field("output events", events_out);
    out.field("elapsed", format!("{elapsed:?}"));
    for (channel, peak) in peaks.iter().enumerate() {
        out.field(
            &format!("peak ch{channel}"),
            format!("{peak:.6} ({})", dbfs(*peak)),
        );
    }
    for (id, wanted, actual) in &drift {
        out.warn(format!(
            "parameter {id} was set to {wanted} but the plug-in reports {actual}; a host \
             changes a parameter with an event in the block, so a plug-in that never \
             applies `ParamValue` from its input list never moves (abi-v1 §11.2)"
        ));
    }
    if silent {
        out.note(
            "every channel is silent; an effect needs `--impulse` and an instrument needs \
             `--note <KEY>` before it has anything to do",
        );
    }
    if unusable {
        out.warn("the output is not a finite number");
    }
    Ok(Exit::from_issues(unusable))
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

/// The parameters whose value the plug-in does not report back.
///
/// A parameter the plug-in has no opinion about — one it refuses to read back at all — is
/// left out rather than reported as drifted: not every plug-in publishes `daux.params/1`.
fn drifted(
    host: &mut TestHost,
    instance: InstanceId,
    requested: &[(u32, f64)],
) -> Vec<(u32, f64, f64)> {
    requested
        .iter()
        .filter_map(|(id, wanted)| {
            let actual = host.param_value(instance, *id).ok()?;
            same_value(actual, *wanted)
                .then_some(())
                .map_or(Some((*id, *wanted, actual)), |()| None)
        })
        .collect()
}

/// Whether two plain parameter values are the same value.
///
/// A parameter round-trips through `f64` and a plug-in may clamp or quantise it, so an
/// exact comparison would report drift on every stepped parameter. The tolerance is
/// relative, because a cutoff in hertz and a gain in dB are not the same scale.
fn same_value(actual: f64, wanted: f64) -> bool {
    if actual == wanted {
        return true;
    }
    let scale = actual.abs().max(wanted.abs()).max(1.0);
    (actual - wanted).abs() <= scale * 1e-6
}

/// `--param 1=0.5` into an id and a plain value.
fn parse_param(assignment: &str) -> anyhow::Result<(u32, f64)> {
    let Some((id, value)) = assignment.split_once('=') else {
        bail!("`{assignment}` is not `id=value`");
    };
    let id: u32 = id
        .trim()
        .parse()
        .map_err(|_| anyhow!("`{id}` is not a parameter id"))?;
    let value: f64 = value
        .trim()
        .parse()
        .map_err(|_| anyhow!("`{value}` is not a number"))?;
    if !value.is_finite() {
        bail!("`{assignment}`: a parameter value must be a finite number");
    }
    Ok((id, value))
}

/// A peak magnitude as decibels relative to full scale, or `-inf` for silence.
fn dbfs(peak: f32) -> String {
    if peak <= 0.0 {
        return "-inf dBFS".to_owned();
    }
    format!("{:.1} dBFS", 20.0 * peak.log10())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parameter_assignment_is_an_id_and_a_number() {
        assert_eq!(parse_param("1=0.5").expect("parses"), (1, 0.5));
        assert_eq!(parse_param(" 42 = -3.25 ").expect("parses"), (42, -3.25));
        assert_eq!(parse_param("7=1e3").expect("parses"), (7, 1000.0));
    }

    /// A mistyped assignment must be refused rather than silently setting something else —
    /// a parameter id is permanent, and `1.0=2` is not id 1.
    #[test]
    fn a_malformed_assignment_is_refused() {
        for hostile in [
            "1",
            "=0.5",
            "1=",
            "gain=0.5",
            "1.0=2",
            "-1=0.5",
            "1=nonsense",
            "1=NaN",
            "1=inf",
            "",
        ] {
            assert!(
                parse_param(hostile).is_err(),
                "`{hostile}` must not parse into a parameter"
            );
        }
    }

    /// The read-back has to tolerate a plug-in that quantises or rounds, and still catch a
    /// plug-in that ignored the change altogether. Reporting drift on every stepped
    /// parameter would train the reader to ignore the warning.
    #[test]
    fn a_value_that_merely_rounded_is_not_reported_as_drift() {
        assert!(same_value(1.0, 1.0));
        assert!(same_value(0.0, 0.0));
        assert!(same_value(-0.0, 0.0));
        assert!(same_value(1.0, 1.000_000_1));
        assert!(same_value(20_000.0, 20_000.001));
        assert!(same_value(f64::MAX, f64::MAX));

        assert!(!same_value(1.0, 2.0), "an ignored change must be visible");
        assert!(!same_value(0.0, 1.0));
        assert!(!same_value(1.0, 1.001));
        assert!(
            !same_value(f64::NAN, 1.0),
            "a NaN is never the value asked for"
        );
    }

    /// The peak reading is the one number a user reads first; getting the silence case
    /// wrong prints `-inf` as `NaN` and looks like a broken plug-in.
    #[test]
    fn a_peak_is_reported_in_decibels_and_silence_is_not_a_nan() {
        assert_eq!(dbfs(0.0), "-inf dBFS");
        assert_eq!(dbfs(-0.0), "-inf dBFS");
        assert_eq!(dbfs(1.0), "0.0 dBFS");
        assert_eq!(dbfs(0.5), "-6.0 dBFS");
        assert!(dbfs(1e-6).starts_with("-120.0"));
    }
}
