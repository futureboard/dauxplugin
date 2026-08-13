//! Reading `[package.metadata.daux]` — the single source of truth of `manifest-v1` §2.
//!
//! The developer writes exactly one description of the plug-in, in the crate's own
//! `Cargo.toml`. `manifest.json` and `Info.plist` are build outputs; a hand-edited one is a
//! bug in the same way a hand-edited `target/` file is. This module is the reader that makes
//! that true, and everything it produces is a [`daux_bundle::Manifest`] the packaging code
//! writes out verbatim.
//!
//! # What it refuses
//!
//! Errors carry the stable `DAUX-M2xx` code of `manifest-v1` §10.5, so a build log can be
//! matched on:
//!
//! | Code | Refused |
//! |---|---|
//! | `DAUX-M200` | `[package.metadata.daux]` is missing altogether |
//! | `DAUX-M201` | a required key (`id`, `vendor`) is missing |
//! | `DAUX-M202` | one key written in both kebab-case and camelCase |
//! | `DAUX-M204` | the crate does not build a `cdylib` |
//! | `DAUX-M008` | a key of the wrong TOML type |
//! | `DAUX-M015` | an unknown category, framework, renderer or presentation slug |
//! | `DAUX-M055` | `resources` or `library` escaping the crate directory |
//!
//! `DAUX-M203` (a generated metadata file checked into the source tree), `DAUX-M205` (an
//! unknown key — almost always a typo) and `DAUX-M206` (a version suffix dropped when
//! normalising) are warnings, collected on [`CrateMetadata::warnings`] rather than raised.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};
use daux_bundle::{
    Category, GraphicsFramework, GraphicsPresentation, GraphicsRenderer, Manifest, ManifestCaps,
    ManifestGenerator, ManifestGraphics, ManifestResources, TargetId, ValidationIssue,
};

use crate::formats::Format;

/// The keys `[package.metadata.daux]` defines (`manifest-v1` §2.2), in kebab-case.
const KNOWN_KEYS: [&str; 22] = [
    "id",
    "vendor",
    "name",
    "version",
    "version-string",
    "category",
    "features",
    "description",
    "url",
    "support-url",
    "copyright",
    "license",
    "bundle-name",
    "targets",
    "formats",
    "resources",
    "library",
    "dependencies",
    "abi-version-minor",
    "macos-min-version",
    "capabilities",
    "graphics",
];

/// The keys of `[package.metadata.daux.graphics]`, in kebab-case.
const GRAPHICS_KEYS: [&str; 11] = [
    "enabled",
    "framework",
    "renderer",
    "presentation",
    "resizable",
    "width",
    "height",
    "min-width",
    "min-height",
    "max-width",
    "max-height",
];

/// Longest bundle directory name, in bytes (`manifest-v1` §4.3).
const MAX_BUNDLE_NAME_BYTES: usize = 64;

/// One plug-in crate's `[package.metadata.daux]`, resolved against its `[package]`.
/// [main-thread]
#[derive(Clone, Debug)]
pub struct CrateMetadata {
    /// The directory holding the crate's `Cargo.toml`.
    pub crate_dir: PathBuf,
    /// The cargo package name, for `cargo build -p`.
    pub package_name: String,
    /// The manifest this crate describes, ready to be written out.
    ///
    /// `targets` carries what the table declared; a build narrows it to what it actually
    /// produced (`manifest-v1` §5.4).
    pub manifest: Manifest,
    /// The `.axt` directory name, sanitised per `manifest-v1` §4.3.
    pub bundle_name: String,
    /// Which formats to package.
    pub formats: Vec<Format>,
    /// Absolute path of the source resource directory, if one was declared.
    pub resources: Option<PathBuf>,
    /// Absolute path of the source dependency directory, if one was declared.
    pub library: Option<PathBuf>,
    /// The `[lib] crate-type` list, verbatim.
    pub crate_types: Vec<String>,
    /// Findings that do not stop a build.
    pub warnings: Vec<ValidationIssue>,
}

impl CrateMetadata {
    /// [main-thread] Whether the crate builds the dynamic library a plug-in has to be.
    pub fn is_cdylib(&self) -> bool {
        self.crate_types.iter().any(|kind| kind == "cdylib")
    }

    /// [main-thread] The files in the declared dependency directory, if there is one.
    ///
    /// `manifest-v1` §2.3 makes `library` a source *directory*; a bundle carries its
    /// contents as individual files under `Library/{target}/`. A directory that is not
    /// there is not an error: the developer may have removed it since.
    pub fn library_files(&self) -> Vec<PathBuf> {
        let Some(dir) = &self.library else {
            return Vec::new();
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        // Sorted, so two builds of the same tree produce the same bundle.
        files.sort();
        files
    }
}

/// [main-thread] Reads and validates a plug-in crate's `Cargo.toml`.
///
/// # Errors
///
/// Any `DAUX-M2xx` violation from the table above, or an unreadable / unparseable
/// `Cargo.toml`.
pub fn read(manifest_path: &Path) -> anyhow::Result<CrateMetadata> {
    let text = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("cannot read `{}`", manifest_path.display()))?;
    let document: toml::Table = toml::from_str(&text)
        .with_context(|| format!("`{}` is not valid TOML", manifest_path.display()))?;
    let crate_dir = manifest_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);

    read_document(&document, &crate_dir)
}

/// [main-thread] The whole of the reader, over an already-parsed document.
///
/// Split out from [`read`] so the rules can be checked against a string rather than against
/// a temporary directory — which is also how the `daux new` templates prove that what they
/// write is something this reader accepts.
///
/// # Errors
///
/// As [`read`].
pub fn read_document(document: &toml::Table, crate_dir: &Path) -> anyhow::Result<CrateMetadata> {
    let mut warnings = Vec::new();

    let package = document
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| anyhow!("DAUX-M200: `Cargo.toml` has no `[package]` table"))?;
    let package = View::new("package", package);

    let package_name = package
        .string("name")?
        .ok_or_else(|| anyhow!("DAUX-M201: `[package] name` is missing"))?;

    let daux = package
        .table("metadata", "package.metadata")?
        .and_then(|metadata| metadata.table("daux", "package.metadata.daux").transpose())
        .transpose()?
        .ok_or_else(|| {
            anyhow!(
                "DAUX-M200: `{package_name}` has no `[package.metadata.daux]` table; \
                 a plug-in crate describes itself there (manifest-v1 §2)"
            )
        })?;

    for key in daux.unknown_keys(&KNOWN_KEYS) {
        warnings.push(ValidationIssue::warning(
            "DAUX-M205",
            format!("`[package.metadata.daux] {key}` is not a key this SDK knows"),
        ));
    }

    // ---- identity ---------------------------------------------------------------------
    let id = daux
        .string("id")?
        .ok_or_else(|| anyhow!("DAUX-M201: `[package.metadata.daux] id` is required"))?;
    let vendor = daux
        .string("vendor")?
        .ok_or_else(|| anyhow!("DAUX-M201: `[package.metadata.daux] vendor` is required"))?;
    let name = daux.string("name")?.unwrap_or_else(|| package_name.clone());

    let raw_version = match daux.string("version")? {
        Some(version) => version,
        None => package.inherited_string("version")?.ok_or_else(|| {
            anyhow!(
                "DAUX-M201: no version: `[package] version` is inherited from the workspace, \
                 so set `version` in `[package.metadata.daux]`"
            )
        })?,
    };
    let (version, dropped_suffix) = normalise_version(&raw_version)?;
    if dropped_suffix {
        warnings.push(ValidationIssue::warning(
            "DAUX-M206",
            format!(
                "`{raw_version}` carries a pre-release or build suffix, which \
                 `plugin.version` cannot; it became `{version}`. Set `version-string` \
                 deliberately to keep the original as display text"
            ),
        ));
    }

    // `Manifest::new` is where the id and the version meet their normative rules, so the
    // CLI never re-implements them.
    let mut manifest = Manifest::new(&id, &name, &vendor, &version)
        .map_err(|error| anyhow!("DAUX-M010: `[package.metadata.daux]` identity: {error}"))?;

    manifest.plugin.version_string = Some(
        daux.string("version-string")?
            .unwrap_or_else(|| raw_version.clone()),
    );
    manifest.plugin.description = daux
        .string("description")?
        .or(package.inherited_string("description")?)
        .unwrap_or_default();
    manifest.plugin.url = daux
        .string("url")?
        .or(package.inherited_string("homepage")?)
        .unwrap_or_default();
    manifest.plugin.support_url = daux.string("support-url")?.unwrap_or_default();
    manifest.plugin.copyright = daux.string("copyright")?.unwrap_or_default();
    manifest.plugin.license = daux
        .string("license")?
        .or(package.inherited_string("license")?)
        .unwrap_or_default();
    manifest.plugin.features = daux.string_array("features")?.unwrap_or_default();

    let category = match daux.string("category")? {
        Some(slug) => Category::parse(&slug).ok_or_else(|| {
            anyhow!(
                "DAUX-M015: `category = \"{slug}\"` is not one of {}",
                Category::ALL.map(Category::slug).join(", ")
            )
        })?,
        None => Category::Unknown,
    };
    manifest.plugin.category = Some(category);

    // ---- packaging --------------------------------------------------------------------
    manifest.abi_version_minor = u32::try_from(daux.integer("abi-version-minor")?.unwrap_or(0))
        .map_err(|_| anyhow!("DAUX-M016: `abi-version-minor` is outside 0..=4294967295"))?;
    manifest.dependencies = daux.string_array("dependencies")?.unwrap_or_default();
    manifest.resources = Some(ManifestResources::default());
    manifest.generator = Some(ManifestGenerator {
        name: "daux".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        note: "Generated from [package.metadata.daux]; do not edit by hand.".to_owned(),
    });

    manifest.targets = match daux.string_array("targets")? {
        Some(targets) if !targets.is_empty() => targets
            .iter()
            .map(|raw| {
                TargetId::parse(raw)
                    .map_err(|error| anyhow!("DAUX-M013: `targets` entry `{raw}`: {error}"))
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        _ => vec![TargetId::host()],
    };

    let formats = match daux.string_array("formats")? {
        Some(raw) if !raw.is_empty() => raw
            .iter()
            .map(|name| {
                Format::parse(name).ok_or_else(|| {
                    anyhow!("DAUX-M015: `formats` entry `{name}` is not axt, vst3 or clap")
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        _ => vec![Format::Axt],
    };

    let resources = daux
        .string("resources")?
        .map(|relative| inside_crate(crate_dir, &relative, "resources"))
        .transpose()?;
    let library = daux
        .string("library")?
        .map(|relative| inside_crate(crate_dir, &relative, "library"))
        .transpose()?;

    // ---- capabilities and graphics ----------------------------------------------------
    let declared_caps = daux.table("capabilities", "package.metadata.daux.capabilities")?;
    let mut capabilities = ManifestCaps::empty();
    if let Some(table) = &declared_caps {
        for (key, value) in table.entries() {
            let enabled = value.as_bool().ok_or_else(|| {
                anyhow!("DAUX-M008: `capabilities.{key}` must be `true` or `false`")
            })?;
            let camel = to_camel(key);
            if !capabilities.set_named(&camel, enabled) {
                warnings.push(ValidationIssue::warning(
                    "DAUX-M205",
                    format!("`capabilities.{key}` is not a capability this SDK knows"),
                ));
            }
        }
    }

    let graphics = daux
        .table("graphics", "package.metadata.daux.graphics")?
        .map(|table| read_graphics(&table, &mut warnings))
        .transpose()?
        .filter(|graphics| graphics.enabled);

    // `manifest-v1` §5.4: the generator sets these, and never asks the developer to keep
    // them in sync by hand.
    if let Some(graphics) = &graphics {
        capabilities.set(daux_abi::DAUX_CAP_HAS_GUI, true);
        if graphics.presentation == GraphicsPresentation::SharedTexture {
            capabilities.set(daux_abi::DAUX_CAP_SHARED_TEXTURE_GUI, true);
        }
    }
    if declared_caps.is_none() {
        // Only when the developer wrote no capability at all: once one is written, the
        // table is taken as complete.
        let derived = match category {
            Category::Effect => Some(daux_abi::DAUX_CAP_AUDIO_EFFECT),
            Category::Instrument => Some(daux_abi::DAUX_CAP_INSTRUMENT),
            Category::MidiEffect => Some(daux_abi::DAUX_CAP_MIDI_EFFECT),
            Category::Analyzer => Some(daux_abi::DAUX_CAP_ANALYZER),
            _ => None,
        };
        if let Some(bit) = derived {
            capabilities.set(bit, true);
        }
    }
    manifest.capabilities = capabilities;
    manifest.graphics = graphics;

    // ---- bundle name ------------------------------------------------------------------
    let bundle_name = match daux.string("bundle-name")? {
        Some(explicit) => explicit,
        None => sanitise_bundle_name(&name, &id),
    };
    daux_bundle::path_rules::validate_component(&bundle_name).map_err(|error| {
        anyhow!("DAUX-M058: `{bundle_name}` cannot be a bundle directory name: {error}")
    })?;

    // ---- generated files checked into the source tree ---------------------------------
    for stray in ["manifest.json", "Info.plist"] {
        if crate_dir.join(stray).exists() {
            warnings.push(ValidationIssue::warning(
                "DAUX-M203",
                format!(
                    "`{stray}` is in the source tree; it is a build output and the next \
                     build overwrites it (manifest-v1 §2)"
                ),
            ));
        }
    }

    manifest
        .check()
        .map_err(|error| anyhow!("DAUX-M009: `[package.metadata.daux]`: {error}"))?;

    let crate_types = document
        .get("lib")
        .and_then(toml::Value::as_table)
        .and_then(|lib| lib.get("crate-type").or_else(|| lib.get("crate_type")))
        .and_then(toml::Value::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(toml::Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();

    Ok(CrateMetadata {
        crate_dir: crate_dir.to_path_buf(),
        package_name,
        manifest,
        bundle_name,
        formats,
        resources,
        library,
        crate_types,
        warnings,
    })
}

/// Reads `[package.metadata.daux.graphics]`.
fn read_graphics(
    table: &View<'_>,
    warnings: &mut Vec<ValidationIssue>,
) -> anyhow::Result<ManifestGraphics> {
    for key in table.unknown_keys(&GRAPHICS_KEYS) {
        warnings.push(ValidationIssue::warning(
            "DAUX-M205",
            format!("`[package.metadata.daux.graphics] {key}` is not a key this SDK knows"),
        ));
    }

    let mut graphics = ManifestGraphics {
        enabled: table.bool("enabled")?.unwrap_or(true),
        ..ManifestGraphics::default()
    };

    graphics.framework = match table.string("framework")? {
        Some(raw) => Some(match raw.as_str() {
            "egui" => GraphicsFramework::Egui,
            "gpui" => GraphicsFramework::Gpui,
            "custom" => GraphicsFramework::Custom,
            other => {
                bail!("DAUX-M015: `graphics.framework = \"{other}\"` is not egui, gpui or custom")
            }
        }),
        None => None,
    };
    graphics.renderer = match table.string("renderer")? {
        Some(raw) => Some(match raw.as_str() {
            "wgpu" => GraphicsRenderer::Wgpu,
            "opengl" => GraphicsRenderer::OpenGl,
            "software" => GraphicsRenderer::Software,
            other => bail!(
                "DAUX-M015: `graphics.renderer = \"{other}\"` is not wgpu, opengl or software"
            ),
        }),
        None => None,
    };
    if let Some(raw) = table.string("presentation")? {
        graphics.presentation = match raw.as_str() {
            "native-window" => GraphicsPresentation::NativeWindow,
            "embedded-surface" => GraphicsPresentation::EmbeddedSurface,
            "shared-texture" => GraphicsPresentation::SharedTexture,
            "external-window" => GraphicsPresentation::ExternalWindow,
            other => bail!(
                "DAUX-M015: `graphics.presentation = \"{other}\"` is not native-window, \
                 embedded-surface, shared-texture or external-window"
            ),
        };
    }
    graphics.resizable = table.bool("resizable")?.unwrap_or(false);
    if let Some(width) = table.pixels("width")? {
        graphics.width = width;
    }
    if let Some(height) = table.pixels("height")? {
        graphics.height = height;
    }
    graphics.min_width = table.pixels("min-width")?;
    graphics.min_height = table.pixels("min-height")?;
    graphics.max_width = table.pixels("max-width")?;
    graphics.max_height = table.pixels("max-height")?;
    Ok(graphics)
}

/// Resolves a crate-relative directory and refuses one that escapes the crate.
fn inside_crate(crate_dir: &Path, relative: &str, key: &str) -> anyhow::Result<PathBuf> {
    if daux_bundle::path_rules::looks_absolute(relative) {
        bail!("DAUX-M055: `{key} = \"{relative}\"` must be relative to the crate directory");
    }
    let joined = crate_dir.join(relative);
    if daux_bundle::path_rules::has_traversal_component(&joined) {
        bail!("DAUX-M055: `{key} = \"{relative}\"` escapes the crate directory");
    }
    // The textual check above always applies. Canonicalisation catches what text cannot —
    // a symlink pointing out of the tree — and can only run once both paths exist.
    if joined.exists()
        && crate_dir.exists()
        && !daux_bundle::path_rules::is_contained(crate_dir, &joined)
    {
        bail!("DAUX-M055: `{key} = \"{relative}\"` resolves outside the crate directory");
    }
    Ok(joined)
}

/// `manifest-v1` §2.4: `MAJOR.MINOR.PATCH` out of a semver string, and whether a
/// pre-release or build suffix was dropped on the way.
pub fn normalise_version(raw: &str) -> anyhow::Result<(String, bool)> {
    let trimmed = raw.trim();
    let numeric = trimmed
        .split(['-', '+'])
        .next()
        .unwrap_or(trimmed)
        .to_owned();
    daux_bundle::validate_version(&numeric)
        .map_err(|error| anyhow!("DAUX-M011: `{raw}` is not a usable plug-in version: {error}"))?;
    Ok((numeric.clone(), numeric != trimmed))
}

/// `manifest-v1` §4.3: a plug-in name turned into a directory name.
///
/// Keeps `[A-Za-z0-9 ._-]`, collapses runs of spaces, trims leading and trailing spaces and
/// dots, truncates on a character boundary to 64 bytes, and falls back to the last label of
/// the plug-in id when nothing usable is left.
pub fn sanitise_bundle_name(name: &str, id: &str) -> String {
    let mut kept = String::with_capacity(name.len());
    let mut pending_space = false;
    for ch in name.chars() {
        match ch {
            ' ' => pending_space = !kept.is_empty(),
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => {
                if pending_space {
                    kept.push(' ');
                    pending_space = false;
                }
                kept.push(ch);
            }
            _ => {}
        }
    }

    let trimmed = kept.trim_matches(|ch: char| ch == ' ' || ch == '.');
    let mut result = trimmed.to_owned();
    while result.len() > MAX_BUNDLE_NAME_BYTES {
        result.pop();
    }
    let result = result.trim_end_matches([' ', '.']).to_owned();

    if result.is_empty() || daux_bundle::path_rules::is_device_name(&result) {
        return id
            .rsplit('.')
            .next()
            .filter(|label| !label.is_empty())
            .unwrap_or("plugin")
            .to_owned();
    }
    result
}

/// `manifest-v1` §2.1: `sample-accurate-automation` → `sampleAccurateAutomation`.
pub fn to_camel(kebab: &str) -> String {
    let mut out = String::with_capacity(kebab.len());
    let mut capitalise = false;
    for ch in kebab.chars() {
        if ch == '-' {
            capitalise = true;
        } else if capitalise {
            out.extend(ch.to_uppercase());
            capitalise = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// A TOML table that knows its own path, so an error can name the key it is about.
///
/// Every lookup accepts both the kebab-case and the camelCase spelling of one key, and
/// refuses a table that carries both (`manifest-v1` §2.1): picking one silently would make
/// the build depend on which spelling the reader happened to check first.
#[derive(Clone, Copy, Debug)]
struct View<'a> {
    path: &'a str,
    table: &'a toml::Table,
}

impl<'a> View<'a> {
    const fn new(path: &'a str, table: &'a toml::Table) -> Self {
        Self { path, table }
    }

    fn get(&self, key: &str) -> anyhow::Result<Option<&'a toml::Value>> {
        let camel = to_camel(key);
        let kebab_value = self.table.get(key);
        let camel_value = if camel == key {
            None
        } else {
            self.table.get(camel.as_str())
        };
        match (kebab_value, camel_value) {
            (Some(_), Some(_)) => bail!(
                "DAUX-M202: `{}` carries both `{key}` and `{camel}`; \
                 they are the same key and a reader must not pick one",
                self.path
            ),
            (Some(value), None) | (None, Some(value)) => Ok(Some(value)),
            (None, None) => Ok(None),
        }
    }

    fn string(&self, key: &str) -> anyhow::Result<Option<String>> {
        match self.get(key)? {
            None => Ok(None),
            Some(value) => value
                .as_str()
                .map(ToOwned::to_owned)
                .map(Some)
                .ok_or_else(|| anyhow!("DAUX-M008: `{}.{key}` must be a string", self.path)),
        }
    }

    /// A string, treating a value of any other shape as absent.
    ///
    /// Only for `[package]` keys. A workspace-inherited field is written
    /// `version = { workspace = true }` — a table, and one this reader cannot resolve
    /// without parsing the workspace root. Refusing it outright would make every crate in
    /// a workspace unbuildable; treating it as absent lets the value fall through to
    /// `[package.metadata.daux]`, and the diagnostic when nothing is left says so.
    fn inherited_string(&self, key: &str) -> anyhow::Result<Option<String>> {
        Ok(self
            .get(key)?
            .and_then(toml::Value::as_str)
            .map(ToOwned::to_owned))
    }

    fn bool(&self, key: &str) -> anyhow::Result<Option<bool>> {
        match self.get(key)? {
            None => Ok(None),
            Some(value) => value
                .as_bool()
                .map(Some)
                .ok_or_else(|| anyhow!("DAUX-M008: `{}.{key}` must be a boolean", self.path)),
        }
    }

    fn integer(&self, key: &str) -> anyhow::Result<Option<i64>> {
        match self.get(key)? {
            None => Ok(None),
            Some(value) => value
                .as_integer()
                .map(Some)
                .ok_or_else(|| anyhow!("DAUX-M008: `{}.{key}` must be an integer", self.path)),
        }
    }

    /// An integer in the 1..=16384 logical-pixel range of `manifest-v1` §3.9.
    fn pixels(&self, key: &str) -> anyhow::Result<Option<u32>> {
        match self.integer(key)? {
            None => Ok(None),
            Some(value) => u32::try_from(value)
                .ok()
                .filter(|pixels| (1..=16_384).contains(pixels))
                .map(Some)
                .ok_or_else(|| {
                    anyhow!(
                        "DAUX-M016: `{}.{key}` is {value}, outside 1..=16384",
                        self.path
                    )
                }),
        }
    }

    fn string_array(&self, key: &str) -> anyhow::Result<Option<Vec<String>>> {
        let Some(value) = self.get(key)? else {
            return Ok(None);
        };
        let array = value
            .as_array()
            .ok_or_else(|| anyhow!("DAUX-M008: `{}.{key}` must be an array", self.path))?;
        let mut out = Vec::with_capacity(array.len());
        for entry in array {
            out.push(
                entry
                    .as_str()
                    .ok_or_else(|| {
                        anyhow!(
                            "DAUX-M008: every `{}.{key}` entry must be a string",
                            self.path
                        )
                    })?
                    .to_owned(),
            );
        }
        Ok(Some(out))
    }

    /// A nested table.
    ///
    /// `path` is the full dotted path of the nested table, passed in rather than joined so
    /// that a `View` stays a pair of borrows and diagnostics still name the whole key.
    fn table(&self, key: &str, path: &'a str) -> anyhow::Result<Option<View<'a>>> {
        let Some(value) = self.get(key)? else {
            return Ok(None);
        };
        let table = value
            .as_table()
            .ok_or_else(|| anyhow!("DAUX-M008: `{}.{key}` must be a table", self.path))?;
        Ok(Some(View { path, table }))
    }

    fn entries(&self) -> impl Iterator<Item = (&'a str, &'a toml::Value)> {
        self.table.iter().map(|(key, value)| (key.as_str(), value))
    }

    /// The keys of this table that are not in `known`, in either spelling.
    fn unknown_keys(&self, known: &[&str]) -> Vec<String> {
        let mut unknown: Vec<String> = self
            .table
            .keys()
            .filter(|key| {
                !known
                    .iter()
                    .any(|candidate| *candidate == key.as_str() || to_camel(candidate) == **key)
            })
            .cloned()
            .collect();
        unknown.sort();
        unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses a fixture against a directory that deliberately does not exist, so that a
    /// stray file next to the test binary can never change the result.
    fn parse_str(text: &str) -> anyhow::Result<CrateMetadata> {
        let document: toml::Table = toml::from_str(text).expect("the fixture is valid TOML");
        read_document(&document, Path::new("no-such-crate-directory"))
    }

    const MINIMAL: &str = r#"
[package]
name = "gain"
version = "1.2.3"

[lib]
crate-type = ["cdylib"]

[package.metadata.daux]
id = "com.example.gain"
vendor = "Example Audio"
"#;

    #[test]
    fn the_minimal_table_produces_a_complete_manifest() {
        let meta = parse_str(MINIMAL).expect("a minimal table is legal");
        assert_eq!(meta.manifest.plugin.id, "com.example.gain");
        assert_eq!(
            meta.manifest.plugin.name, "gain",
            "defaults to package.name"
        );
        assert_eq!(meta.manifest.plugin.version, "1.2.3");
        assert_eq!(meta.bundle_name, "gain");
        assert_eq!(meta.formats, [Format::Axt]);
        assert_eq!(meta.manifest.targets, [TargetId::host()]);
        assert!(meta.is_cdylib());
        assert!(meta.warnings.is_empty(), "{:?}", meta.warnings);
        // `manifest-v1` §5.4: the generator is stamped, and the resource directories are
        // the constants rather than absent.
        assert_eq!(
            meta.manifest.generator.as_ref().map(|g| g.name.as_str()),
            Some("daux")
        );
        assert_eq!(meta.manifest.resource_dir_name(), "Resources");
        assert_eq!(meta.manifest.library_dir_name(), "Library");
    }

    /// The two errors a developer meets first: no table at all, and a missing required key.
    #[test]
    fn a_missing_table_or_a_missing_required_key_is_refused_with_its_code() {
        let no_table = parse_str("[package]\nname = \"gain\"\nversion = \"1.0.0\"\n")
            .expect_err("there is nothing to build from");
        assert!(no_table.to_string().contains("DAUX-M200"), "{no_table}");

        let no_id = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nvendor = \"Example\"\n",
        )
        .expect_err("an id is permanent and cannot be invented");
        assert!(no_id.to_string().contains("DAUX-M201"), "{no_id}");

        let no_vendor = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\n",
        )
        .expect_err("a vendor is required");
        assert!(no_vendor.to_string().contains("DAUX-M201"), "{no_vendor}");
    }

    /// `manifest-v1` §2.1: both spellings of one key is an error, never a coin toss.
    #[test]
    fn one_key_written_twice_is_refused_rather_than_resolved() {
        let error = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             support-url = \"https://a\"\nsupportUrl = \"https://b\"\n",
        )
        .expect_err("the reader must not pick one");
        assert!(error.to_string().contains("DAUX-M202"), "{error}");
    }

    /// Both spellings are individually accepted, so a developer coming from the JSON side
    /// is not tripped up.
    #[test]
    fn either_spelling_of_a_key_is_accepted_on_its_own() {
        for spelling in ["support-url", "supportUrl"] {
            let meta = parse_str(&format!(
                "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
                 [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
                 {spelling} = \"https://example.test/support\"\n"
            ))
            .unwrap_or_else(|e| panic!("`{spelling}` must be accepted: {e}"));
            assert_eq!(
                meta.manifest.plugin.support_url,
                "https://example.test/support"
            );
        }
    }

    /// A typo must be visible. Silently ignoring `catagory` would ship a plug-in that says
    /// it is `unknown` and nobody would find out until it was in a browser.
    #[test]
    fn an_unknown_key_is_a_warning_that_names_it() {
        let meta = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             catagory = \"effect\"\n",
        )
        .expect("a typo does not stop the build");
        assert_eq!(meta.warnings.len(), 1, "{:?}", meta.warnings);
        assert_eq!(meta.warnings[0].code, "DAUX-M205");
        assert!(meta.warnings[0].message.contains("catagory"));
    }

    /// A typo'd *category* is different: it is a known key with an unknown value, and
    /// `manifest-v1` §3.6 forbids mapping it to `unknown`.
    #[test]
    fn an_unknown_category_is_an_error_and_never_silently_unknown() {
        let error = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             category = \"efect\"\n",
        )
        .expect_err("a typo'd category must be visible at build time");
        assert!(error.to_string().contains("DAUX-M015"), "{error}");
    }

    #[test]
    fn a_key_of_the_wrong_type_is_refused() {
        for (key, value) in [
            ("id", "42"),
            ("vendor", "true"),
            ("name", "1.5"),
            ("features", "\"eq\""),
            ("targets", "\"windows-x86_64\""),
            ("abi-version-minor", "\"one\""),
            ("capabilities", "\"none\""),
            ("graphics", "true"),
        ] {
            let mut fixture = String::from(
                "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n[package.metadata.daux]\n",
            );
            if key != "id" {
                fixture.push_str("id = \"com.example.gain\"\n");
            }
            if key != "vendor" {
                fixture.push_str("vendor = \"E\"\n");
            }
            fixture.push_str(&format!("{key} = {value}\n"));

            let error = parse_str(&fixture).unwrap_err();
            assert!(
                error.to_string().contains("DAUX-M008"),
                "`{key} = {value}` must be a type error: {error}"
            );
        }
    }

    /// `manifest-v1` §2.4: the numeric version and the display string are different things,
    /// and dropping a suffix silently would make two releases compare equal.
    #[test]
    fn a_semver_suffix_is_dropped_from_the_numeric_version_and_reported() {
        let meta = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0-beta.2+ci.7\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n",
        )
        .expect("a pre-release is a normal thing to build");
        assert_eq!(meta.manifest.plugin.version, "1.0.0");
        assert_eq!(
            meta.manifest.plugin.version_string.as_deref(),
            Some("1.0.0-beta.2+ci.7"),
            "the original survives as display text"
        );
        assert!(
            meta.warnings.iter().any(|w| w.code == "DAUX-M206"),
            "{:?}",
            meta.warnings
        );
    }

    #[test]
    fn a_version_that_is_not_a_version_is_refused() {
        for version in [
            "",
            "1",
            "1.0",
            "one.two.three",
            "1.0.0.0.0",
            "99999999999.0.0",
        ] {
            let error = parse_str(&format!(
                "[package]\nname = \"gain\"\nversion = \"{version}\"\n\
                 [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n"
            ))
            .unwrap_err();
            assert!(
                error.to_string().contains("DAUX-M011") || error.to_string().contains("DAUX-M201"),
                "`{version}` must be refused: {error}"
            );
        }
    }

    /// A workspace-inherited version is a table, not a string. Reading it as one would
    /// either panic or produce a nonsense version, so it has to be a clear diagnostic.
    #[test]
    fn a_workspace_inherited_version_asks_for_an_explicit_one() {
        let error = parse_str(
            "[package]\nname = \"gain\"\nversion = { workspace = true }\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n",
        )
        .expect_err("there is no version to read");
        assert!(error.to_string().contains("DAUX-M201"), "{error}");
        assert!(error.to_string().contains("workspace"), "{error}");

        // And setting it in the daux table is the documented way out.
        let meta = parse_str(
            "[package]\nname = \"gain\"\nversion = { workspace = true }\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\nversion = \"2.0.0\"\n",
        )
        .expect("an explicit version resolves it");
        assert_eq!(meta.manifest.plugin.version, "2.0.0");
    }

    /// `manifest-v1` §5.4: `hasGui` is derived from the graphics table, and the
    /// category-derived bits apply only when the developer declared nothing.
    #[test]
    fn capability_bits_are_derived_exactly_as_the_specification_says() {
        let derived = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\ncategory = \"effect\"\n",
        )
        .expect("parses");
        assert!(derived.manifest.capabilities.get("audioEffect") == Some(true));

        // One declared capability makes the table complete: `audioEffect` is no longer
        // inferred from the category.
        let declared = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\ncategory = \"effect\"\n\
             [package.metadata.daux.capabilities]\nsidechain = true\n",
        )
        .expect("parses");
        assert_eq!(declared.manifest.capabilities.get("sidechain"), Some(true));
        assert_eq!(
            declared.manifest.capabilities.get("audioEffect"),
            Some(false),
            "once the developer writes one capability the table is taken as complete"
        );

        // The GUI bits are always derived, declared table or not.
        let gui = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             [package.metadata.daux.capabilities]\nsidechain = true\n\
             [package.metadata.daux.graphics]\nframework = \"gpui\"\nrenderer = \"wgpu\"\n\
             presentation = \"shared-texture\"\n",
        )
        .expect("parses");
        assert_eq!(gui.manifest.capabilities.get("hasGui"), Some(true));
        assert_eq!(
            gui.manifest.capabilities.get("sharedTextureGui"),
            Some(true)
        );
    }

    /// Kebab-case capability names in TOML have to reach the camelCase names of the bitset.
    #[test]
    fn capability_names_are_translated_between_the_two_spellings() {
        let meta = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             [package.metadata.daux.capabilities]\n\
             sample-accurate-automation = true\nmidiInput = true\nnot-a-capability = true\n",
        )
        .expect("parses");
        assert_eq!(
            meta.manifest.capabilities.get("sampleAccurateAutomation"),
            Some(true)
        );
        assert_eq!(meta.manifest.capabilities.get("midiInput"), Some(true));
        assert!(
            meta.warnings
                .iter()
                .any(|w| w.message.contains("not-a-capability")),
            "{:?}",
            meta.warnings
        );
    }

    #[test]
    fn a_disabled_graphics_table_declares_no_editor() {
        let meta = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             [package.metadata.daux.graphics]\nenabled = false\nframework = \"egui\"\nrenderer = \"wgpu\"\n",
        )
        .expect("parses");
        assert!(meta.manifest.graphics.is_none());
        assert_eq!(meta.manifest.capabilities.get("hasGui"), Some(false));
    }

    #[test]
    fn a_graphics_table_with_impossible_bounds_is_refused() {
        for sizes in [
            "width = 0",
            "width = 20000",
            "height = -1",
            "width = 800\nheight = 600\nmin-width = 900",
            "width = 800\nheight = 600\nmax-height = 100",
        ] {
            let error = parse_str(&format!(
                "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
                 [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
                 [package.metadata.daux.graphics]\nframework = \"egui\"\nrenderer = \"wgpu\"\n\
                 {sizes}\n"
            ))
            .unwrap_err();
            let text = error.to_string();
            assert!(
                text.contains("DAUX-M016") || text.contains("DAUX-M009"),
                "`{sizes}` must be refused: {text}"
            );
        }

        // The same table with legal bounds is accepted, so the test above is not simply
        // rejecting every graphics table.
        let ok = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             [package.metadata.daux.graphics]\nframework = \"egui\"\nrenderer = \"wgpu\"\n\
             width = 800\nheight = 600\nmin-width = 640\nmax-height = 2160\n",
        )
        .expect("legal bounds are legal");
        let graphics = ok.manifest.graphics.expect("an editor was declared");
        assert_eq!(graphics.width, 800);
        assert_eq!(graphics.min_width, Some(640));
        assert_eq!(graphics.max_height, Some(2160));
    }

    /// A `resources` or `library` path that leaves the crate is how a build ends up
    /// shipping somebody's home directory.
    #[test]
    fn a_source_directory_that_escapes_the_crate_is_refused() {
        for hostile in [
            "../secrets",
            "/etc",
            "C:\\Windows",
            "assets/../../elsewhere",
        ] {
            let error = parse_str(&format!(
                "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
                 [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
                 resources = \"{}\"\n",
                hostile.replace('\\', "\\\\")
            ))
            .unwrap_err();
            assert!(
                error.to_string().contains("DAUX-M055"),
                "`{hostile}` must be refused: {error}"
            );
        }
    }

    /// `manifest-v1` §4.3, including the fallback that stops a name of pure punctuation
    /// from producing a directory called `""`.
    #[test]
    fn bundle_names_are_sanitised_and_never_empty_or_reserved() {
        assert_eq!(sanitise_bundle_name("EQUZX", "com.example.equzx"), "EQUZX");
        assert_eq!(sanitise_bundle_name("My  Plug-In", "x.y"), "My Plug-In");
        assert_eq!(sanitise_bundle_name("  Spaced  ", "x.y"), "Spaced");
        assert_eq!(sanitise_bundle_name("Ünïcödé", "com.example.uni"), "ncd");
        assert_eq!(sanitise_bundle_name("///", "com.example.slash"), "slash");
        assert_eq!(sanitise_bundle_name("", "com.example.empty"), "empty");
        assert_eq!(sanitise_bundle_name("...", "com.example.dots"), "dots");
        assert_eq!(
            sanitise_bundle_name("CON", "com.example.console"),
            "console",
            "a Windows device name would produce a bundle that cannot be opened"
        );
        assert!(sanitise_bundle_name(&"A".repeat(200), "x.y").len() <= MAX_BUNDLE_NAME_BYTES);
    }

    #[test]
    fn kebab_case_becomes_camel_case_exactly_as_specified() {
        assert_eq!(
            to_camel("sample-accurate-automation"),
            "sampleAccurateAutomation"
        );
        assert_eq!(to_camel("id"), "id");
        assert_eq!(to_camel("support-url"), "supportUrl");
        assert_eq!(to_camel("abi-version-minor"), "abiVersionMinor");
        assert_eq!(to_camel(""), "");
    }

    #[test]
    fn version_normalisation_keeps_a_plain_version_untouched() {
        assert_eq!(
            normalise_version("1.2.3").unwrap(),
            ("1.2.3".to_owned(), false)
        );
        assert_eq!(
            normalise_version("1.2.3.4").unwrap(),
            ("1.2.3.4".to_owned(), false)
        );
        assert_eq!(
            normalise_version("2.0.0-rc.1").unwrap(),
            ("2.0.0".to_owned(), true)
        );
        assert!(normalise_version("not-a-version").is_err());
    }

    /// The declared targets and formats survive, and nonsense in either is refused rather
    /// than dropped — a build that quietly skipped a target would ship half a product.
    #[test]
    fn declared_targets_and_formats_are_read_and_checked() {
        let meta = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             targets = [\"windows-x86_64\", \"linux-x86_64\"]\nformats = [\"axt\", \"vst3\"]\n",
        )
        .expect("parses");
        assert_eq!(meta.manifest.targets.len(), 2);
        assert_eq!(meta.formats, [Format::Axt, Format::Vst3]);

        let bad_target = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             targets = [\"WINDOWS-X86_64\"]\n",
        )
        .unwrap_err();
        assert!(bad_target.to_string().contains("DAUX-M013"), "{bad_target}");

        let bad_format = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n\
             formats = [\"au\"]\n",
        )
        .unwrap_err();
        assert!(bad_format.to_string().contains("DAUX-M015"), "{bad_format}");
    }

    #[test]
    fn a_crate_that_builds_no_cdylib_is_visible_to_the_caller() {
        let meta = parse_str(
            "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
             [lib]\ncrate-type = [\"rlib\"]\n\
             [package.metadata.daux]\nid = \"com.example.gain\"\nvendor = \"E\"\n",
        )
        .expect("the table itself is fine");
        assert!(!meta.is_cdylib());
    }

    /// An id is permanent, so a malformed one must never reach a bundle.
    #[test]
    fn a_malformed_plug_in_id_is_refused() {
        for id in [
            "gain",
            "Com.Example.Gain",
            "com..gain",
            ".com.gain",
            "com.gain.",
        ] {
            let error = parse_str(&format!(
                "[package]\nname = \"gain\"\nversion = \"1.0.0\"\n\
                 [package.metadata.daux]\nid = \"{id}\"\nvendor = \"E\"\n"
            ))
            .unwrap_err();
            assert!(
                error.to_string().contains("DAUX-M010"),
                "`{id}` must be refused: {error}"
            );
        }
    }
}
