//! Assembling artefacts on disk: the `.axt` bundle, and the two compatibility layouts.
//!
//! [`daux_bundle::BundleBuilder`] owns the `.axt` layout — the staging directory, the
//! atomic move into place, the resource copy. What it does not carry is the *whole*
//! manifest: it has no setter for a category, a licence, a feature list or a generator
//! stamp, and `manifest-v1` §5.4 requires all of them. So the bundle is built with the
//! builder and the manifest is then written over with the complete document. Overwriting
//! rather than merging is exactly what §2 demands of a generator: the file is a build
//! output, and the previous contents are never consulted.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};
use daux_bundle::{Bundle, BundleBuilder, BundleLayout, Manifest, TargetId};

/// The name of the directory binaries are renamed into before they are copied in.
const STAGE_DIR: &str = ".daux-stage";

/// What to assemble. [main-thread]
#[derive(Clone, Debug)]
pub struct PackRequest<'a> {
    /// The complete manifest. `targets` is replaced with the targets actually packaged.
    pub manifest: &'a Manifest,
    /// The `.axt` directory name, without the extension (`manifest-v1` §4.3).
    pub bundle_name: &'a str,
    /// One dynamic library per target.
    pub binaries: &'a [(TargetId, PathBuf)],
    /// Bundled dependencies, per target.
    pub libraries: &'a [(TargetId, PathBuf)],
    /// A directory copied in as the bundle's resources.
    pub resources: Option<&'a Path>,
    /// Where the bundle is written.
    pub out_dir: &'a Path,
}

/// [main-thread] Writes an `.axt` and returns its root directory.
///
/// The binary is renamed to `{BundleName}.{ext}` on the way in (`manifest-v1` §4.3): cargo
/// emits `libgain.so` and `gain.dll`, and a bundle whose binary is named after the crate
/// rather than the product is a bundle a reader has to guess about.
///
/// # Errors
///
/// When a source file is missing, when the filesystem refuses, or when the bundle that was
/// written cannot be opened again — which would mean this function had produced something
/// no host could load.
pub fn write_axt(request: &PackRequest<'_>) -> anyhow::Result<PathBuf> {
    if request.binaries.is_empty() {
        bail!("nothing to package: no binary was given for any target");
    }
    std::fs::create_dir_all(request.out_dir)
        .with_context(|| format!("cannot create `{}`", request.out_dir.display()))?;

    let stage = request.out_dir.join(STAGE_DIR);
    let _ = std::fs::remove_dir_all(&stage);
    let result = assemble(request, &stage);
    // The staging copies are never part of the output, whether or not the build worked.
    let _ = std::fs::remove_dir_all(&stage);
    result
}

/// The body of [`write_axt`], so that the staging directory is cleaned up on every path.
fn assemble(request: &PackRequest<'_>, stage: &Path) -> anyhow::Result<PathBuf> {
    let plugin = &request.manifest.plugin;
    let mut builder = BundleBuilder::new(
        &plugin.id,
        request.bundle_name,
        &plugin.vendor,
        &plugin.version,
    )
    .map_err(|error| anyhow!("the plug-in identity is not usable: {error}"))?
    .description(plugin.description.clone())
    .capabilities(request.manifest.capabilities)
    .abi_version(
        request.manifest.abi_version,
        request.manifest.abi_version_minor,
    );
    if let Some(graphics) = request.manifest.graphics.clone() {
        builder = builder.graphics(graphics);
    }

    let mut targets = Vec::with_capacity(request.binaries.len());
    for (target, source) in request.binaries {
        let staged = stage_binary(stage, target, request.bundle_name, source)?;
        builder = builder.binary(target.clone(), &staged);
        if !targets.contains(target) {
            targets.push(target.clone());
        }
    }
    for (target, source) in request.libraries {
        if !source.exists() {
            bail!("`{}` does not exist", source.display());
        }
        builder = builder.library(target.clone(), source);
    }
    if let Some(resources) = request.resources {
        if !resources.is_dir() {
            bail!("`{}` is not a directory", resources.display());
        }
        builder = builder.resource_dir(resources);
    }

    let layout = builder.effective_layout();
    let root = builder
        .write(request.out_dir)
        .map_err(|error| anyhow!("cannot write the bundle: {error}"))?;

    // `manifest-v1` §5.4: a manifest must never declare a target whose binary is missing,
    // so what is written is what was actually packaged.
    let mut manifest = request.manifest.clone();
    manifest.targets = targets;
    manifest
        .check()
        .map_err(|error| anyhow!("the generated manifest is not valid: {error}"))?;
    write_manifest(&root, layout, &manifest)?;

    // Reading it back is cheap and turns "the tool wrote something wrong" into a build
    // failure instead of a bug report from a user whose host would not load it.
    Bundle::open(&root)
        .map_err(|error| anyhow!("the bundle that was written cannot be opened again: {error}"))?;
    Ok(root)
}

/// Copies one binary into the staging directory under the name the bundle needs.
fn stage_binary(
    stage: &Path,
    target: &TargetId,
    bundle_name: &str,
    source: &Path,
) -> anyhow::Result<PathBuf> {
    if !source.is_file() {
        bail!("`{}` is not a file", source.display());
    }
    let dir = stage.join(target.as_str());
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create `{}`", dir.display()))?;
    let staged = dir.join(format!("{bundle_name}.{}", target.dylib_extension()));
    std::fs::copy(source, &staged)
        .with_context(|| format!("cannot copy `{}`", source.display()))?;
    Ok(staged)
}

/// Writes the complete manifest over the builder's minimal one.
fn write_manifest(root: &Path, layout: BundleLayout, manifest: &Manifest) -> anyhow::Result<()> {
    let path = root.join(layout.manifest_path());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create `{}`", parent.display()))?;
    }
    let json = manifest
        .to_json()
        .map_err(|error| anyhow!("cannot serialise the manifest: {error}"))?;
    std::fs::write(&path, json).with_context(|| format!("cannot write `{}`", path.display()))?;
    Ok(())
}

/// The directory a VST3 bundle keeps this target's binary in.
///
/// Returns `None` for a target VST3 has no directory name for; the export is then skipped
/// rather than written into a path no host looks in.
pub fn vst3_arch_dir(target: &TargetId) -> Option<&'static str> {
    match (target.os(), target.arch()) {
        ("windows", "x86_64") => Some("x86_64-win"),
        ("windows", "aarch64") => Some("arm64-win"),
        ("linux", "x86_64") => Some("x86_64-linux"),
        ("linux", "aarch64") => Some("aarch64-linux"),
        ("macos", _) => Some("MacOS"),
        _ => None,
    }
}

/// The file name a VST3 bundle's binary carries on this target.
///
/// Windows keeps the `.vst3` extension on the library itself, Linux uses `.so`, and Apple
/// uses no extension at all.
pub fn vst3_binary_name(target: &TargetId, bundle_name: &str) -> String {
    match target.os() {
        "windows" => format!("{bundle_name}.vst3"),
        "macos" => bundle_name.to_owned(),
        _ => format!("{bundle_name}.so"),
    }
}

/// [main-thread] Writes a VST3 bundle around an already-built binary.
///
/// The layout is VST3's own — `{Name}.vst3/Contents/{arch}/{binary}` — and nothing else is
/// generated: `moduleinfo.json` is a VST3 3.7.5 optional index this SDK does not write in
/// v1, and a host that does not find one enumerates the module instead.
///
/// # Errors
///
/// When the target has no VST3 directory name, or the filesystem refuses.
pub fn write_vst3(
    out_dir: &Path,
    bundle_name: &str,
    target: &TargetId,
    binary: &Path,
) -> anyhow::Result<PathBuf> {
    let arch = vst3_arch_dir(target)
        .ok_or_else(|| anyhow!("VST3 has no bundle directory for target `{target}`"))?;
    let root = out_dir.join(format!("{bundle_name}.vst3"));
    let contents = root.join("Contents").join(arch);
    std::fs::create_dir_all(&contents)
        .with_context(|| format!("cannot create `{}`", contents.display()))?;
    let destination = contents.join(vst3_binary_name(target, bundle_name));
    std::fs::copy(binary, &destination)
        .with_context(|| format!("cannot copy `{}`", binary.display()))?;
    Ok(root)
}

/// [main-thread] Writes a CLAP artefact around an already-built binary.
///
/// On Windows and Linux a CLAP plug-in is a single file named `{Name}.clap`; on Apple
/// platforms it is a bundle, because codesigning has nothing else to sign.
///
/// # Errors
///
/// When the filesystem refuses.
pub fn write_clap(
    out_dir: &Path,
    bundle_name: &str,
    target: &TargetId,
    binary: &Path,
) -> anyhow::Result<PathBuf> {
    if target.is_apple() {
        let root = out_dir.join(format!("{bundle_name}.clap"));
        let contents = root.join("Contents").join("MacOS");
        std::fs::create_dir_all(&contents)
            .with_context(|| format!("cannot create `{}`", contents.display()))?;
        std::fs::copy(binary, contents.join(bundle_name))
            .with_context(|| format!("cannot copy `{}`", binary.display()))?;
        return Ok(root);
    }
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("cannot create `{}`", out_dir.display()))?;
    let destination = out_dir.join(format!("{bundle_name}.clap"));
    std::fs::copy(binary, &destination)
        .with_context(|| format!("cannot copy `{}`", binary.display()))?;
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daux_bundle::{Category, ManifestCaps};

    /// A temporary directory that removes itself, so a failing test leaves nothing behind.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let unique = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let path = std::env::temp_dir().join(format!("daux-cli-{label}-{unique}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn file(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("a parent directory");
            }
            std::fs::write(&path, contents).expect("a file");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn manifest() -> Manifest {
        let mut manifest = Manifest::new("com.example.gain", "Gain", "Example Audio", "1.2.3")
            .expect("a valid identity");
        manifest.plugin.category = Some(Category::Effect);
        manifest.plugin.license = "MIT OR Apache-2.0".to_owned();
        manifest.plugin.features = vec!["gain".to_owned()];
        manifest.capabilities = ManifestCaps::empty().with(daux_abi::DAUX_CAP_AUDIO_EFFECT);
        manifest.targets = vec![TargetId::host()];
        manifest
    }

    /// The whole point of writing the manifest twice: the keys `BundleBuilder` has no
    /// setter for must survive into the bundle, or `daux build` silently produces a
    /// manifest that `manifest-v1` §5.4 calls incomplete.
    #[test]
    fn the_written_manifest_carries_what_the_builder_cannot_express() {
        let dir = TempDir::new("pack-manifest");
        let binary = dir.file("build/gain.dll", b"not really a library");
        let out = dir.path().join("out");
        let target = TargetId::host();

        let manifest = manifest();
        let root = write_axt(&PackRequest {
            manifest: &manifest,
            bundle_name: "Gain",
            binaries: &[(target.clone(), binary)],
            libraries: &[],
            resources: None,
            out_dir: &out,
        })
        .expect("the bundle writes");

        assert_eq!(root.file_name().and_then(|n| n.to_str()), Some("Gain.axt"));
        let opened = Bundle::open(&root).expect("and opens");
        assert_eq!(opened.metadata().id, "com.example.gain");
        assert_eq!(opened.metadata().version, "1.2.3");

        let written: Manifest = {
            let bytes = std::fs::read(root.join(opened.layout().manifest_path()))
                .expect("the manifest is where the layout says");
            Manifest::from_json_bytes(&bytes).expect("and parses")
        };
        assert_eq!(written.plugin.category, Some(Category::Effect));
        assert_eq!(written.plugin.license, "MIT OR Apache-2.0");
        assert_eq!(written.plugin.features, ["gain"]);
        assert_eq!(written.targets, vec![target]);
    }

    /// `manifest-v1` §4.3: the bundle's binary is named after the product, not after the
    /// crate cargo happened to compile.
    #[test]
    fn the_binary_is_renamed_to_the_bundle_name() {
        let dir = TempDir::new("pack-rename");
        let target = TargetId::host();
        let source = dir.file(
            &format!("build/libdaux_example_gain.{}", target.dylib_extension()),
            b"not really a library",
        );
        let out = dir.path().join("out");

        let root = write_axt(&PackRequest {
            manifest: &manifest(),
            bundle_name: "Gain",
            binaries: &[(target.clone(), source)],
            libraries: &[],
            resources: None,
            out_dir: &out,
        })
        .expect("the bundle writes");

        let binary = Bundle::open(&root)
            .expect("opens")
            .binary_path(&target)
            .expect("the binary is found");
        assert_eq!(
            binary.file_name().and_then(|n| n.to_str()),
            Some(format!("Gain.{}", target.dylib_extension()).as_str())
        );
        // And the staging directory never survives a build.
        assert!(!out.join(STAGE_DIR).exists());
    }

    /// A build that cannot finish must not leave a half-written bundle, or the next scan
    /// caches a broken plug-in.
    #[test]
    fn a_missing_source_file_leaves_nothing_behind() {
        let dir = TempDir::new("pack-missing");
        let out = dir.path().join("out");
        let error = write_axt(&PackRequest {
            manifest: &manifest(),
            bundle_name: "Gain",
            binaries: &[(TargetId::host(), dir.path().join("never-built.dll"))],
            libraries: &[],
            resources: None,
            out_dir: &out,
        })
        .expect_err("there is nothing to copy");
        assert!(error.to_string().contains("never-built"), "{error}");
        assert!(!out.join("Gain.axt").exists());
        assert!(!out.join(STAGE_DIR).exists());
    }

    #[test]
    fn packaging_nothing_is_refused_rather_than_producing_an_empty_bundle() {
        let dir = TempDir::new("pack-empty");
        let error = write_axt(&PackRequest {
            manifest: &manifest(),
            bundle_name: "Gain",
            binaries: &[],
            libraries: &[],
            resources: None,
            out_dir: dir.path(),
        })
        .expect_err("a bundle with no binary can never load");
        assert!(error.to_string().contains("no binary"), "{error}");
    }

    #[test]
    fn resources_are_copied_in_and_readable_through_the_bundle() {
        let dir = TempDir::new("pack-resources");
        let target = TargetId::host();
        let binary = dir.file("build/gain.bin", b"x");
        dir.file("assets/presets/Default.txt", b"hello");
        let out = dir.path().join("out");

        let root = write_axt(&PackRequest {
            manifest: &manifest(),
            bundle_name: "Gain",
            binaries: &[(target, binary)],
            libraries: &[],
            resources: Some(&dir.path().join("assets")),
            out_dir: &out,
        })
        .expect("the bundle writes");

        let bundle = Bundle::open(&root).expect("opens");
        assert_eq!(
            bundle
                .resources()
                .read_to_string("presets/Default.txt")
                .expect("the resource came along"),
            "hello"
        );
    }

    /// Every VST3 host looks in one directory, and it is not the same one on every
    /// platform. Getting it wrong produces a bundle that silently is not found.
    #[test]
    fn the_vst3_layout_matches_the_formats_own_convention() {
        for (target, arch, binary) in [
            ("windows-x86_64", "x86_64-win", "Gain.vst3"),
            ("windows-aarch64", "arm64-win", "Gain.vst3"),
            ("linux-x86_64", "x86_64-linux", "Gain.so"),
            ("linux-aarch64", "aarch64-linux", "Gain.so"),
            ("macos-universal", "MacOS", "Gain"),
        ] {
            let target = TargetId::parse(target).expect("a registered target");
            assert_eq!(vst3_arch_dir(&target), Some(arch));
            assert_eq!(vst3_binary_name(&target, "Gain"), binary);
        }
        // A target VST3 has no name for is skipped rather than written somewhere wrong.
        let unknown = TargetId::parse("aix-power64").expect("syntactically valid");
        assert_eq!(vst3_arch_dir(&unknown), None);
    }

    #[test]
    fn a_vst3_and_a_clap_are_written_where_a_host_would_look() {
        let dir = TempDir::new("pack-compat");
        let binary = dir.file("build/gain.dll", b"not really a library");
        let out = dir.path().join("out");
        let target = TargetId::parse("windows-x86_64").expect("a registered target");

        let vst3 = write_vst3(&out, "Gain", &target, &binary).expect("the vst3 writes");
        assert!(
            vst3.join("Contents/x86_64-win/Gain.vst3").is_file(),
            "{}",
            vst3.display()
        );

        let clap = write_clap(&out, "Gain", &target, &binary).expect("the clap writes");
        assert_eq!(clap, out.join("Gain.clap"));
        assert!(clap.is_file());

        // Apple is the one platform where a CLAP is a directory, so it is written into a
        // directory of its own rather than over the single file above.
        let apple_out = dir.path().join("out-macos");
        let apple = TargetId::parse("macos-universal").expect("a registered target");
        let bundle = write_clap(&apple_out, "Gain", &apple, &binary).expect("the clap writes");
        assert!(bundle.join("Contents/MacOS/Gain").is_file());

        let unsupported = TargetId::parse("aix-power64").expect("syntactically valid");
        assert!(write_vst3(&out, "Gain", &unsupported, &binary).is_err());
    }
}
