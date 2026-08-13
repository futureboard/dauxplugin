//! What `daux new` writes.
//!
//! The scaffold is a working plug-in, not a stub with `todo!()` in it: a template that does
//! not make a sound is a template whose first edit is a debugging session. Each kind
//! produces a crate that builds, packages and passes `daux test`.
//!
//! The `[package.metadata.daux]` table it writes is the single source of truth of
//! `manifest-v1` §2 — the generated crate has no `manifest.json` to edit, by design.

use crate::cli::Kind;

/// One file of a scaffolded crate. [main-thread]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateFile {
    /// Path relative to the crate directory, always with `/` separators.
    pub path: &'static str,
    /// The file's whole contents.
    pub contents: String,
}

/// The identity a scaffolded crate is built around. [main-thread]
#[derive(Clone, Debug)]
pub struct Scaffold {
    /// Display name, e.g. `Reverb`.
    pub name: String,
    /// Cargo package name, e.g. `reverb`.
    pub package: String,
    /// Rust type name, e.g. `Reverb`.
    pub type_name: String,
    /// Permanent reverse-DNS plug-in id.
    pub id: String,
    /// Vendor name.
    pub vendor: String,
    /// Which template to write.
    pub kind: Kind,
}

impl Scaffold {
    /// [main-thread] Derives every name a template needs from the plug-in's display name.
    ///
    /// `id` is only defaulted when the caller did not supply one: an id is permanent
    /// (`abi-v1` §14), and inventing one silently is how a product ends up shipping as
    /// `com.example.something` forever.
    pub fn new(name: &str, id: Option<&str>, vendor: &str, kind: Kind) -> Self {
        let package = package_name(name);
        let type_name = type_name(name);
        let id = id.map_or_else(|| format!("com.example.{package}"), ToOwned::to_owned);
        Self {
            name: name.to_owned(),
            package,
            type_name,
            id,
            vendor: vendor.to_owned(),
            kind,
        }
    }

    /// [main-thread] Every file the scaffold consists of.
    pub fn files(&self) -> Vec<TemplateFile> {
        vec![
            TemplateFile {
                path: "Cargo.toml",
                contents: self.cargo_toml(),
            },
            TemplateFile {
                path: "src/lib.rs",
                contents: self.lib_rs(),
            },
            TemplateFile {
                path: ".gitignore",
                contents: "/target\n".to_owned(),
            },
            TemplateFile {
                path: "README.md",
                contents: self.readme(),
            },
        ]
    }

    /// The category slug and the capability the descriptor declares, for one kind.
    const fn kind_traits(kind: Kind) -> (&'static str, &'static str) {
        match kind {
            Kind::Effect => ("effect", "with_audio_effect"),
            Kind::Instrument => ("instrument", "with_instrument"),
            Kind::MidiEffect => ("midi-effect", "with_midi_effect"),
        }
    }

    fn cargo_toml(&self) -> String {
        let (category, _) = Self::kind_traits(self.kind);
        let extra_caps = match self.kind {
            Kind::Effect => String::new(),
            Kind::Instrument | Kind::MidiEffect => {
                "\n[package.metadata.daux.capabilities]\nmidi-input = true\n".to_owned()
            }
        };
        format!(
            r#"[package]
name = "{package}"
version = "0.1.0"
edition = "2024"
description = "{name}"
publish = false

# A plug-in is a dynamic library. `rlib` as well, so tests and `daux-host` can link it
# directly and drive it without going through the C ABI.
[lib]
crate-type = ["cdylib", "rlib"]

[dependencies]
daux-plugin = {{ version = "0.1", features = ["axt"] }}

# The single source of truth for this plug-in's identity and packaging (manifest-v1 §2).
# `manifest.json` is generated from this table by `daux build`; never write one by hand.
[package.metadata.daux]
id       = "{id}"          # permanent: renaming is free, renumbering breaks saved projects
vendor   = "{vendor}"
name     = "{name}"
category = "{category}"
formats  = ["axt"]
{extra_caps}"#,
            package = self.package,
            name = self.name,
            id = self.id,
            vendor = self.vendor,
        )
    }

    fn readme(&self) -> String {
        format!(
            "# {name}\n\n\
             A DAUx audio plug-in.\n\n\
             ```sh\n\
             daux build                       # compile and package into target/daux/release/axt\n\
             daux validate target/daux/release/axt/{name}.axt\n\
             daux test     target/daux/release/axt/{name}.axt\n\
             ```\n\n\
             The plug-in's identity and packaging live in one place, \
             `[package.metadata.daux]` in `Cargo.toml`. `manifest.json` is a build output.\n",
            name = self.name
        )
    }

    fn lib_rs(&self) -> String {
        let (_, capability) = Self::kind_traits(self.kind);
        let category = match self.kind {
            Kind::Effect => "Effect",
            Kind::Instrument => "Instrument",
            Kind::MidiEffect => "MidiEffect",
        };
        let header = format!(
            "//! {name} — a DAUx plug-in.\n\
             //!\n\
             //! `process` runs on the audio thread. Nothing in it may allocate, lock, log,\n\
             //! block or call the host: everything it needs is prepared in `prepare`.\n\
             \n\
             use daux_plugin::prelude::*;\n\n",
            name = self.name
        );

        let body = match self.kind {
            Kind::Effect => self.effect_body(),
            Kind::Instrument => self.instrument_body(),
            Kind::MidiEffect => self.midi_effect_body(),
        };

        let tail = format!(
            r#"
impl DauxController for {ty} {{
    fn params(&self) -> &dyn Params {{
        &self.params
    }}

    /// `[main-thread]` Every parameter, keyed by its permanent id.
    fn save_state(&self, writer: &mut StateWriter) -> DauxResult<()> {{
        for (id, param) in self.params.param_refs() {{
            writer.put_f64(&id.get().to_string(), param.plain());
        }}
        Ok(())
    }}

    /// `[main-thread]` A key that is not there is a parameter added since the preset was
    /// written, and keeps its default rather than failing the load.
    fn load_state(&mut self, reader: &StateReader) -> DauxResult<()> {{
        for (id, param) in self.params.param_refs() {{
            if let Some(value) = reader.opt_f64(&id.get().to_string()) {{
                param.set_plain(value);
            }}
        }}
        Ok(())
    }}
}}

impl DauxPlugin for {ty} {{
    fn descriptor() -> PluginDescriptor {{
        PluginDescriptor::builder("{id}", "{name}")
            .vendor("{vendor}")
            .version(Version::new(0, 1, 0))
            .category(Category::{category})
            .capabilities(Capabilities::NONE.{capability}())
            .build()
            .expect("this descriptor is a constant of the crate")
    }}

    fn bus_layout(&self) -> BusLayout {{
        {bus_layout}
    }}

    fn event_ports(&self) -> EventPortLayout {{
        {event_ports}
    }}

    fn processor(&mut self) -> &mut dyn DauxProcessor {{
        self
    }}

    fn controller(&mut self) -> &mut dyn DauxController {{
        self
    }}
}}

// Emits the entry point of every format enabled in `Cargo.toml`.
export_plugin!(SingleFactory<{ty}>);
"#,
            ty = self.type_name,
            id = self.id,
            name = self.name,
            vendor = self.vendor,
            bus_layout = match self.kind {
                Kind::Effect => "BusLayout::stereo_effect()",
                Kind::Instrument => "BusLayout::instrument(ChannelLayout::Stereo)",
                Kind::MidiEffect => "BusLayout::default()",
            },
            event_ports = match self.kind {
                Kind::Effect => "EventPortLayout::none()",
                Kind::Instrument => "EventPortLayout::instrument()",
                Kind::MidiEffect => "EventPortLayout::midi_effect()",
            },
        );

        format!("{header}{body}{tail}{}", Self::AUTOMATION)
    }

    /// The one piece of glue every plug-in needs and no adapter can do for it.
    ///
    /// A host changes a parameter by putting an event in the block's input list; the
    /// parameter bank only moves when the plug-in applies it (`abi-v1` §11.2, and the
    /// `daux.params/1` table's `flush`, which covers the *inactive* case only). A template
    /// that left this out would produce a plug-in whose automation lanes silently do
    /// nothing — which is exactly the bug that is hardest to see in a DAW.
    const AUTOMATION: &'static str = r#"
/// Applies this block's parameter automation. `[audio-thread]`
///
/// Nothing here allocates: the lookup is by permanent id and the store is atomic.
fn apply_automation(params: &dyn Params, events: &ProcessEvents<'_>) {
    let input = events.input();
    for index in 0..input.len() {
        if let Some(DauxEvent::ParamValue(event)) = input.get(index)
            && let Some(param) = params.param(ParamId::new(event.param_id))
        {
            param.set_plain(event.value);
        }
    }
}
"#;

    fn effect_body(&self) -> String {
        format!(
            r#"/// The plug-in's parameters. Ids are permanent (`abi-v1` §14).
#[derive(DauxParams)]
struct {ty}Params {{
    #[param(id = 1, name = "Gain", range = 0.0..=4.0, default = 1.0)]
    gain: FloatParam,
}}

/// {name}.
pub struct {ty} {{
    params: {ty}Params,
}}

impl Default for {ty} {{
    fn default() -> Self {{
        Self {{
            params: {ty}Params::new(),
        }}
    }}
}}

impl DauxProcessor for {ty} {{
    /// `[main-thread]` Everything that allocates happens here, once.
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {{
        config.validate()
    }}

    /// `[audio-thread]`
    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {{
        apply_automation(&self.params, events);

        // Read once per block: a parameter is atomic, but reading it per sample would be
        // both slower and less predictable than reading it once.
        let gain = self.params.gain.plain() as f32;

        // Taken before the output is borrowed; an input buffer does not borrow the bus set.
        let input = audio.main_input();
        let Some(mut output) = audio.main_output() else {{
            return ProcessStatus::Continue;
        }};

        for channel in 0..output.channel_count() {{
            let source = input.as_ref().and_then(|bus| bus.get_channel(channel));
            let Some(destination) = output.get_channel_mut(channel) else {{
                continue;
            }};
            match source {{
                Some(source) => {{
                    for (out, sample) in destination.iter_mut().zip(source) {{
                        *out = *sample * gain;
                    }}
                }}
                // A host may hand an effect fewer input channels than output channels.
                None => destination.fill(0.0),
            }}
        }}
        ProcessStatus::Continue
    }}
}}
"#,
            ty = self.type_name,
            name = self.name,
        )
    }

    fn instrument_body(&self) -> String {
        format!(
            r#"/// The plug-in's parameters. Ids are permanent (`abi-v1` §14).
#[derive(DauxParams)]
struct {ty}Params {{
    #[param(id = 1, name = "Level", range = 0.0..=1.0, default = 0.5)]
    level: FloatParam,
}}

/// {name}: one sine voice, so that the scaffold makes a sound on the first build.
pub struct {ty} {{
    params: {ty}Params,
    sample_rate: f32,
    phase: f32,
    frequency: f32,
    amplitude: f32,
}}

impl Default for {ty} {{
    fn default() -> Self {{
        Self {{
            params: {ty}Params::new(),
            sample_rate: 48_000.0,
            phase: 0.0,
            frequency: 440.0,
            amplitude: 0.0,
        }}
    }}
}}

impl DauxProcessor for {ty} {{
    /// `[main-thread]` Everything that allocates happens here, once.
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {{
        config.validate()?;
        self.sample_rate = config.sample_rate as f32;
        Ok(())
    }}

    /// `[audio-thread]` Silence between notes; a sine while one is held.
    fn reset(&mut self) {{
        self.phase = 0.0;
        self.amplitude = 0.0;
    }}

    /// `[audio-thread]`
    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {{
        apply_automation(&self.params, events);

        // A real instrument applies each event at its own sample offset; this one takes the
        // last note of the block, which is the smallest thing that is still audible.
        for index in 0..events.input().len() {{
            match events.input().get(index) {{
                Some(DauxEvent::NoteOn(note)) => {{
                    self.frequency = 440.0 * (f32::from(note.key - 69) / 12.0).exp2();
                    self.amplitude = note.velocity as f32;
                }}
                Some(DauxEvent::NoteOff(_)) => self.amplitude = 0.0,
                _ => {{}}
            }}
        }}

        let level = self.params.level.plain() as f32;
        let Some(mut output) = audio.main_output() else {{
            return ProcessStatus::Continue;
        }};

        let step = self.frequency / self.sample_rate.max(1.0);
        let gain = level * self.amplitude;
        let channels = output.channel_count();
        let frames = output.frames();
        let mut phase = self.phase;

        for frame in 0..frames {{
            let sample = (phase * core::f32::consts::TAU).sin() * gain;
            for channel in 0..channels {{
                if let Some(slot) = output
                    .get_channel_mut(channel)
                    .and_then(|samples| samples.get_mut(frame))
                {{
                    *slot = sample;
                }}
            }}
            phase += step;
            if phase >= 1.0 {{
                phase -= 1.0;
            }}
        }}
        self.phase = phase;

        if gain > 0.0 {{
            ProcessStatus::Continue
        }} else {{
            ProcessStatus::Sleep
        }}
    }}
}}
"#,
            ty = self.type_name,
            name = self.name,
        )
    }

    fn midi_effect_body(&self) -> String {
        format!(
            r#"/// The plug-in's parameters. Ids are permanent (`abi-v1` §14).
#[derive(DauxParams)]
struct {ty}Params {{
    #[param(id = 1, name = "Transpose", range = -24..=24, default = 0, unit = "st")]
    transpose: IntParam,
}}

/// {name}: transposes the notes it is given and touches no audio.
pub struct {ty} {{
    params: {ty}Params,
}}

impl Default for {ty} {{
    fn default() -> Self {{
        Self {{
            params: {ty}Params::new(),
        }}
    }}
}}

impl DauxProcessor for {ty} {{
    /// `[main-thread]` Everything that allocates happens here, once.
    fn prepare(&mut self, config: &ProcessConfig) -> DauxResult<()> {{
        config.validate()
    }}

    /// `[audio-thread]`
    fn process<'a>(
        &mut self,
        _ctx: &ProcessContext<'a>,
        _audio: &mut AudioBuses<'a, f32>,
        events: &mut ProcessEvents<'a>,
    ) -> ProcessStatus {{
        apply_automation(&self.params, events);

        let semitones = self.params.transpose.plain() as i16;

        for index in 0..events.input().len() {{
            // Copied out of the input list before the output list is borrowed. A note event
            // is `Copy`, so this holds no borrow and allocates nothing.
            let transposed = match events.input().get(index) {{
                Some(DauxEvent::NoteOn(mut note)) => {{
                    note.key = (note.key + semitones).clamp(0, 127);
                    Some(DauxEvent::NoteOn(note))
                }}
                Some(DauxEvent::NoteOff(mut note)) => {{
                    note.key = (note.key + semitones).clamp(0, 127);
                    Some(DauxEvent::NoteOff(note))
                }}
                _ => None,
            }};
            if let Some(event) = transposed {{
                // A full output list is a legitimate outcome of a busy block, never a
                // reason to panic on the audio thread.
                let _ = events.output().try_push(&event);
            }}
        }}
        ProcessStatus::Continue
    }}
}}
"#,
            ty = self.type_name,
            name = self.name,
        )
    }
}

/// A cargo package name out of a display name: lower-case, ASCII, `-` between words.
fn package_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut wants_separator = false;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if wants_separator && !out.is_empty() {
                out.push('-');
            }
            wants_separator = false;
            out.push(ch.to_ascii_lowercase());
        } else {
            wants_separator = true;
        }
    }
    // A cargo package name may not be empty and may not begin with a digit.
    if out.is_empty() {
        return "plugin".to_owned();
    }
    if out.starts_with(|ch: char| ch.is_ascii_digit()) {
        return format!("plugin-{out}");
    }
    out
}

/// A Rust type name out of a display name: `UpperCamelCase`, never starting with a digit.
fn type_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut capitalise = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            if capitalise {
                out.push(ch.to_ascii_uppercase());
                capitalise = false;
            } else {
                out.push(ch);
            }
        } else {
            capitalise = true;
        }
    }
    if out.is_empty() {
        return "Plugin".to_owned();
    }
    if out.starts_with(|ch: char| ch.is_ascii_digit()) {
        return format!("Plugin{out}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaffold(name: &str, kind: Kind) -> Scaffold {
        Scaffold::new(name, None, "Example Audio", kind)
    }

    /// A display name is whatever the developer typed; a crate name and a type name are
    /// not. Producing an invalid one would make the scaffold fail to compile, which is the
    /// worst possible first impression.
    #[test]
    fn hostile_display_names_still_produce_legal_rust_and_cargo_names() {
        for (input, package, type_name) in [
            ("Reverb", "reverb", "Reverb"),
            ("Tape Delay", "tape-delay", "TapeDelay"),
            ("EQ-8", "eq-8", "EQ8"),
            ("3Band", "plugin-3band", "Plugin3Band"),
            ("...", "plugin", "Plugin"),
            ("Ünïcödé", "n-c-d", "NCD"),
        ] {
            let scaffold = scaffold(input, Kind::Effect);
            assert_eq!(scaffold.package, package, "package name of `{input}`");
            assert_eq!(scaffold.type_name, type_name, "type name of `{input}`");
            assert!(
                !scaffold.type_name.starts_with(|c: char| c.is_ascii_digit()),
                "`{input}` produced a type name starting with a digit"
            );
            assert!(
                !scaffold.package.starts_with(|c: char| c.is_ascii_digit()),
                "`{input}` produced a package name starting with a digit"
            );
        }
    }

    /// The id is permanent. Defaulting it is fine; overwriting one the caller gave is not.
    #[test]
    fn an_explicit_id_is_never_replaced_by_the_default() {
        let explicit = Scaffold::new("Reverb", Some("studio.acme.reverb"), "Acme", Kind::Effect);
        assert_eq!(explicit.id, "studio.acme.reverb");
        assert_eq!(scaffold("Reverb", Kind::Effect).id, "com.example.reverb");
    }

    /// Everything the scaffold writes has to be there, and the generated `Cargo.toml` has to
    /// be the single source of truth the rest of the CLI reads.
    #[test]
    fn the_generated_manifest_is_one_this_cli_can_read_back() {
        for kind in [Kind::Effect, Kind::Instrument, Kind::MidiEffect] {
            let scaffold = scaffold("Tape Delay", kind);
            let files = scaffold.files();
            let paths: Vec<&str> = files.iter().map(|file| file.path).collect();
            assert_eq!(
                paths,
                ["Cargo.toml", "src/lib.rs", ".gitignore", "README.md"]
            );

            let cargo = &files[0].contents;
            let document: toml::Table = toml::from_str(cargo)
                .unwrap_or_else(|e| panic!("{kind:?} template is not valid TOML: {e}"));
            let meta = crate::meta::read_document(
                &document,
                std::path::Path::new("no-such-crate-directory"),
            )
            .unwrap_or_else(|e| panic!("{kind:?} template is not readable metadata: {e}"));

            assert_eq!(meta.manifest.plugin.id, "com.example.tape-delay");
            assert_eq!(meta.manifest.plugin.name, "Tape Delay");
            assert_eq!(meta.bundle_name, "Tape Delay");
            assert!(meta.is_cdylib(), "a plug-in must be a cdylib");
            assert!(
                meta.warnings.is_empty(),
                "{kind:?} template warns about itself: {:?}",
                meta.warnings
            );
        }
    }

    /// The category and capability the descriptor declares must agree with the manifest's,
    /// or the very first `daux validate` of a scaffolded plug-in reports `DAUX-M104`.
    #[test]
    fn the_template_agrees_with_itself_about_what_kind_of_plug_in_it_is() {
        for (kind, category, capability) in [
            (Kind::Effect, "effect", "with_audio_effect"),
            (Kind::Instrument, "instrument", "with_instrument"),
            (Kind::MidiEffect, "midi-effect", "with_midi_effect"),
        ] {
            let scaffold = scaffold("Reverb", kind);
            let cargo = scaffold.cargo_toml();
            let source = scaffold.lib_rs();
            assert!(
                cargo.contains(&format!("category = \"{category}\"")),
                "{kind:?}: {cargo}"
            );
            assert!(source.contains(capability), "{kind:?}: {source}");
            assert!(
                source.contains("export_plugin!(SingleFactory<Reverb>);"),
                "{kind:?} must export an entry point"
            );
        }
    }

    /// The templates are the CLI's own worked examples of the audio-thread rule, so they
    /// must not contain the calls that rule forbids.
    #[test]
    fn no_template_allocates_or_panics_on_the_audio_thread() {
        for kind in [Kind::Effect, Kind::Instrument, Kind::MidiEffect] {
            let source = scaffold("Reverb", kind).lib_rs();
            let start = source
                .find("fn process<'a>(")
                .expect("every template has a `process`");
            // Up to the close of the `impl DauxProcessor` block, so that the descriptor's
            // main-thread `expect` below it is not mistaken for an audio-thread panic.
            let rest = &source[start..];
            let process = &rest[..rest.find("\n}\n").unwrap_or(rest.len())];
            for forbidden in [
                "unwrap()",
                "expect(",
                "panic!",
                "Vec::",
                "vec![",
                "to_owned()",
                "format!",
                "println!",
                "Box::new",
                ".lock()",
            ] {
                assert!(
                    !process.contains(forbidden),
                    "{kind:?}: `{forbidden}` must not appear in `process`"
                );
            }
        }
    }

    /// A host moves a parameter by putting an event in the block, and the bank only moves
    /// when the plug-in applies it. A template that skipped this produces a plug-in whose
    /// automation lanes do nothing at all, silently — which is the single hardest bug to
    /// see from inside a DAW, and the reason this glue is in the scaffold rather than in a
    /// paragraph of documentation.
    #[test]
    fn every_template_applies_the_hosts_parameter_automation() {
        for kind in [Kind::Effect, Kind::Instrument, Kind::MidiEffect] {
            let source = scaffold("Reverb", kind).lib_rs();
            assert!(
                source.contains("apply_automation(&self.params, events);"),
                "{kind:?} never applies automation:\n{source}"
            );
            assert!(
                source.contains("DauxEvent::ParamValue(event)"),
                "{kind:?} does not read parameter events:\n{source}"
            );
            assert!(
                source.contains("param.set_plain(event.value);"),
                "{kind:?} reads the events but never stores them:\n{source}"
            );
        }
    }

    /// The README tells the developer the three commands that make the crate real; if the
    /// bundle name in it were wrong, every one of them would fail.
    #[test]
    fn the_readme_names_the_bundle_that_the_build_actually_produces() {
        let scaffold = scaffold("Tape Delay", Kind::Effect);
        let readme = scaffold.readme();
        assert!(readme.contains("Tape Delay.axt"), "{readme}");
        assert!(readme.contains("daux build"));
    }
}
