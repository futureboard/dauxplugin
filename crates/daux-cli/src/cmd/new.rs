//! `daux new` — a plug-in crate that builds, packages and makes a sound.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};

use crate::cli::NewArgs;
use crate::exit::Exit;
use crate::out::Out;
use crate::template::Scaffold;

/// [main-thread] Runs `daux new`.
///
/// # Errors
///
/// When the id is not a legal plug-in id, when the destination already holds one of the
/// files the scaffold writes and `--force` was not given, or when the filesystem refuses.
pub fn run(args: &NewArgs, out: &Out) -> anyhow::Result<Exit> {
    if args.name.trim().is_empty() {
        bail!("a plug-in needs a name");
    }
    if let Some(id) = &args.id {
        // An id is permanent (`abi-v1` §14). Refusing a bad one now costs a retype;
        // refusing it later costs a rename that breaks every saved project.
        daux_bundle::validate_plugin_id(id)
            .map_err(|error| anyhow!("`{id}` is not a usable plug-in id: {error}"))?;
    }

    let scaffold = Scaffold::new(
        args.name.trim(),
        args.id.as_deref(),
        args.vendor.trim(),
        args.kind,
    );
    let root = args
        .path
        .clone()
        .unwrap_or_else(|| PathBuf::from(&scaffold.package));
    let files = scaffold.files();

    if !args.force {
        let existing: Vec<&str> = files
            .iter()
            .filter(|file| root.join(file.path).exists())
            .map(|file| file.path)
            .collect();
        if !existing.is_empty() {
            bail!(
                "`{}` already has {}; pass `--force` to overwrite",
                root.display(),
                existing.join(", ")
            );
        }
    }

    for file in &files {
        let path = root.join(file.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create `{}`", parent.display()))?;
        }
        std::fs::write(&path, &file.contents)
            .with_context(|| format!("cannot write `{}`", path.display()))?;
    }

    if out.is_json() {
        out.emit(&serde_json::json!({
            "ok": true,
            "path": root.display().to_string(),
            "id": scaffold.id,
            "name": scaffold.name,
            "package": scaffold.package,
            "kind": args.kind.slug(),
            "files": files
                .iter()
                .map(|file| root.join(file.path).display().to_string())
                .collect::<Vec<_>>(),
        }))?;
        return Ok(Exit::Ok);
    }

    out.heading(format!("created `{}`", root.display()));
    for file in &files {
        out.field("", root.join(file.path).display());
    }
    out.blank();
    out.field("id", &scaffold.id);
    out.field("kind", args.kind.slug());
    out.blank();
    out.line(next_steps(&root, &scaffold.name));
    Ok(Exit::Ok)
}

/// The three commands that turn the scaffold into something a DAW can load.
fn next_steps(root: &Path, name: &str) -> String {
    format!(
        "next:\n  cd {}\n  daux build\n  daux test target/daux/release/axt/{name}.axt",
        root.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Kind;

    /// A scaffolded crate must be readable by this CLI's own metadata reader — otherwise
    /// `daux new` followed by `daux build` fails on the first try.
    #[test]
    fn what_new_writes_is_what_build_reads() {
        let scaffold = Scaffold::new("Tape Delay", None, "Example Audio", Kind::Effect);
        let files = scaffold.files();
        let cargo = files
            .iter()
            .find(|file| file.path == "Cargo.toml")
            .expect("a Cargo.toml");
        let document: toml::Table =
            toml::from_str(&cargo.contents).expect("the template is valid TOML");
        let meta = crate::meta::read_document(&document, Path::new("no-such-crate-directory"))
            .expect("and readable metadata");
        assert_eq!(meta.package_name, "tape-delay");
        assert_eq!(meta.bundle_name, "Tape Delay");
    }

    #[test]
    fn the_next_steps_name_the_bundle_the_build_will_produce() {
        let steps = next_steps(Path::new("tape-delay"), "Tape Delay");
        assert!(steps.contains("cd tape-delay"), "{steps}");
        assert!(steps.contains("Tape Delay.axt"), "{steps}");
    }
}
