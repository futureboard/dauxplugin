//! Where plug-ins live, per operating system.
//!
//! These are conventions, not standards, and getting them wrong is invisible: a host that
//! looks in the wrong directory simply reports that the user owns no plug-ins. Each entry
//! below is the location the platform's other hosts already use, so a plug-in installed for
//! one is found by this one.
//!
//! | | AXT | VST3 | CLAP |
//! | --- | --- | --- | --- |
//! | Windows | `%CommonProgramFiles%\DAUx\Extensions` | `%CommonProgramFiles%\VST3` | `%CommonProgramFiles%\CLAP` |
//! | | `%LocalAppData%\Programs\Common\DAUx\Extensions` | `…\Programs\Common\VST3` | `…\Programs\Common\CLAP` |
//! | macOS | `/Library/Audio/Plug-Ins/DAUx` | `…/VST3` | `…/CLAP` |
//! | | `~/Library/Audio/Plug-Ins/DAUx` | `…/VST3` | `…/CLAP` |
//! | Linux | `~/.axt`, `/usr/lib/axt`, `/usr/local/lib/axt` | `~/.vst3`, `/usr/lib/vst3`, `/usr/local/lib/vst3` | `~/.clap`, `/usr/lib/clap`, `/usr/local/lib/clap` |
//!
//! Three environment variables come first when they are set: `DAUX_PATH`, `VST3_PATH` and
//! `CLAP_PATH`, each a list separated by `;` on Windows and `:` elsewhere. `CLAP_PATH` is
//! the CLAP specification's own variable; the other two follow it. They are prepended, not
//! substituted — an override that hid the system directories would make "it works on my
//! machine" a support case rather than a bug.
//!
//! The list is not filtered against the filesystem. A directory that does not exist costs
//! one failed `read_dir` and may exist by the next scan, whereas a host that cached "this
//! path is absent" would never notice the user installing their first plug-in.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The platform whose conventions apply. [any-thread]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Platform {
    /// Windows, in all its editions.
    Windows,
    /// macOS.
    MacOs,
    /// Linux and other Unixes that follow the same layout.
    Unix,
}

impl Platform {
    /// The platform this binary was built for. [any-thread]
    pub(crate) const fn current() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Unix
        }
    }

    /// The character that separates entries in a `*_PATH` variable. [any-thread]
    const fn list_separator(self) -> char {
        match self {
            // Windows paths contain `:` after the drive letter, so `;` is the only choice.
            Self::Windows => ';',
            Self::MacOs | Self::Unix => ':',
        }
    }
}

/// How the search-path table reads the environment. [any-thread]
///
/// A function rather than [`std::env::var_os`] directly so the table can be tested for all
/// three platforms from one machine — and without touching the process environment, which
/// is a global mutable that no test may safely write.
pub(crate) type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// Reads the real process environment. [main-thread]
pub(crate) fn process_env(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// The directories a scan looks in on `platform`. [main-thread]
///
/// Order is meaningful: overrides first, then per-user locations, then system-wide ones, so
/// a user's own build of a plug-in shadows the installed copy in any consumer that stops at
/// the first hit. Duplicates are removed, keeping the first occurrence.
pub(crate) fn search_paths_for(platform: Platform, env: EnvLookup<'_>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    for variable in ["DAUX_PATH", "VST3_PATH", "CLAP_PATH"] {
        if let Some(value) = env(variable) {
            for entry in split_list(&value, platform.list_separator()) {
                paths.push(entry);
            }
        }
    }

    match platform {
        Platform::Windows => windows_paths(env, &mut paths),
        Platform::MacOs => macos_paths(env, &mut paths),
        Platform::Unix => unix_paths(env, &mut paths),
    }

    dedup_keeping_order(paths)
}

/// `%LocalAppData%\Programs\Common` is where per-user installs go, and
/// `%CommonProgramFiles%` where machine-wide ones do. Both are read from the environment
/// rather than assembled from `C:\Program Files`, because neither is fixed: a machine may
/// have Windows on another volume, and a localised install may not spell it in English.
fn windows_paths(env: EnvLookup<'_>, out: &mut Vec<PathBuf>) {
    if let Some(local) = env("LOCALAPPDATA") {
        let user = PathBuf::from(local).join("Programs").join("Common");
        out.push(user.join("DAUx").join("Extensions"));
        out.push(user.join("VST3"));
        out.push(user.join("CLAP"));
    }
    // `CommonProgramW6432` is the 64-bit directory as seen from a 32-bit process; taking
    // both means a host of either bitness finds the plug-ins meant for it.
    for variable in ["CommonProgramFiles", "CommonProgramW6432"] {
        if let Some(common) = env(variable) {
            let common = PathBuf::from(common);
            out.push(common.join("DAUx").join("Extensions"));
            out.push(common.join("VST3"));
            out.push(common.join("CLAP"));
        }
    }
}

fn macos_paths(env: EnvLookup<'_>, out: &mut Vec<PathBuf>) {
    if let Some(home) = env("HOME") {
        let user = PathBuf::from(home)
            .join("Library")
            .join("Audio")
            .join("Plug-Ins");
        out.push(user.join("DAUx"));
        out.push(user.join("VST3"));
        out.push(user.join("CLAP"));
    }
    let system = Path::new("/Library/Audio/Plug-Ins");
    out.push(system.join("DAUx"));
    out.push(system.join("VST3"));
    out.push(system.join("CLAP"));
}

fn unix_paths(env: EnvLookup<'_>, out: &mut Vec<PathBuf>) {
    if let Some(home) = env("HOME") {
        let home = PathBuf::from(home);
        out.push(home.join(".axt"));
        out.push(home.join(".vst3"));
        out.push(home.join(".clap"));
    }
    // `$XDG_DATA_HOME` is where a Flatpak or a `--user` install puts things; it defaults to
    // `~/.local/share`, which is what the fallback below reconstructs.
    let xdg = env("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env("HOME").map(|home| PathBuf::from(home).join(".local").join("share")));
    if let Some(xdg) = xdg {
        out.push(xdg.join("axt"));
        out.push(xdg.join("vst3"));
        out.push(xdg.join("clap"));
    }
    for prefix in ["/usr/local/lib", "/usr/lib"] {
        let prefix = Path::new(prefix);
        out.push(prefix.join("axt"));
        out.push(prefix.join("vst3"));
        out.push(prefix.join("clap"));
    }
}

/// Splits a `*_PATH` value, dropping empty entries. [any-thread]
///
/// An empty entry is what a trailing separator or a `A;;B` typo produces, and treating it
/// as the current directory — which is what joining an empty string does — would make a
/// scan's results depend on where the host happened to be launched from.
fn split_list(value: &OsString, separator: char) -> Vec<PathBuf> {
    let Some(text) = value.to_str() else {
        // A non-UTF-8 override cannot be split without guessing an encoding. Taking it
        // whole is the honest reading: it is still a usable single path.
        return vec![PathBuf::from(value)];
    };
    text.split(separator)
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// Removes repeats, keeping the first occurrence. [any-thread]
fn dedup_keeping_order(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen: Vec<PathBuf> = Vec::with_capacity(paths.len());
    for path in paths {
        if !seen.contains(&path) {
            seen.push(path);
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an environment from a fixed table, so the same test runs identically on
    /// every machine and never touches the real process environment.
    fn env_from(
        pairs: &'static [(&'static str, &'static str)],
    ) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(*value))
        }
    }

    fn contains_ending(paths: &[PathBuf], suffix: &str) -> bool {
        let suffix = suffix.replace('/', std::path::MAIN_SEPARATOR_STR);
        paths.iter().any(|path| {
            path.to_string_lossy()
                .replace('\\', std::path::MAIN_SEPARATOR_STR)
                .ends_with(&suffix)
        })
    }

    #[test]
    fn windows_looks_where_windows_hosts_look() {
        let env = env_from(&[
            ("LOCALAPPDATA", r"C:\Users\ada\AppData\Local"),
            ("CommonProgramFiles", r"C:\Program Files\Common Files"),
        ]);
        let paths = search_paths_for(Platform::Windows, &env);

        assert!(contains_ending(&paths, r"Common Files/DAUx/Extensions"));
        assert!(contains_ending(&paths, r"Common Files/VST3"));
        assert!(contains_ending(&paths, r"Common Files/CLAP"));
        assert!(contains_ending(
            &paths,
            r"Local/Programs/Common/DAUx/Extensions"
        ));
        assert!(contains_ending(&paths, r"Local/Programs/Common/VST3"));
        assert!(contains_ending(&paths, r"Local/Programs/Common/CLAP"));

        // Per-user before machine-wide: a developer's own build must shadow the installed
        // copy, not the other way round.
        let user = paths
            .iter()
            .position(|p| p.to_string_lossy().contains("AppData"))
            .expect("a per-user path");
        let machine = paths
            .iter()
            .position(|p| p.to_string_lossy().contains("Program Files"))
            .expect("a machine-wide path");
        assert!(user < machine);
    }

    /// Nothing may be invented: a machine where `%CommonProgramFiles%` is unset must
    /// produce no path under it, rather than a guess like `C:\Program Files`.
    #[test]
    fn an_unset_variable_contributes_nothing() {
        let paths = search_paths_for(Platform::Windows, &env_from(&[]));
        assert!(
            paths.is_empty(),
            "with no environment there is nowhere to look on Windows, got {paths:?}"
        );
    }

    #[test]
    fn macos_uses_the_audio_plug_ins_tree_for_both_scopes() {
        let env = env_from(&[("HOME", "/Users/ada")]);
        let paths = search_paths_for(Platform::MacOs, &env);
        assert!(paths.contains(&PathBuf::from("/Users/ada/Library/Audio/Plug-Ins/DAUx")));
        assert!(paths.contains(&PathBuf::from("/Users/ada/Library/Audio/Plug-Ins/VST3")));
        assert!(paths.contains(&PathBuf::from("/Users/ada/Library/Audio/Plug-Ins/CLAP")));
        assert!(paths.contains(&PathBuf::from("/Library/Audio/Plug-Ins/DAUx")));
        assert!(paths.contains(&PathBuf::from("/Library/Audio/Plug-Ins/VST3")));
        assert!(paths.contains(&PathBuf::from("/Library/Audio/Plug-Ins/CLAP")));

        // The system tree is always searched, even for a user with no home directory.
        let homeless = search_paths_for(Platform::MacOs, &env_from(&[]));
        assert!(homeless.contains(&PathBuf::from("/Library/Audio/Plug-Ins/CLAP")));
    }

    #[test]
    fn linux_uses_the_dotfile_xdg_and_system_conventions() {
        let env = env_from(&[("HOME", "/home/ada")]);
        let paths = search_paths_for(Platform::Unix, &env);
        for expected in [
            "/home/ada/.axt",
            "/home/ada/.vst3",
            "/home/ada/.clap",
            "/home/ada/.local/share/axt",
            "/usr/local/lib/vst3",
            "/usr/lib/clap",
        ] {
            assert!(
                paths.contains(&PathBuf::from(expected)),
                "{expected} is missing from {paths:?}"
            );
        }

        // An explicit XDG_DATA_HOME replaces the derived default rather than adding to it.
        let xdg = search_paths_for(
            Platform::Unix,
            &env_from(&[("HOME", "/home/ada"), ("XDG_DATA_HOME", "/home/ada/.data")]),
        );
        assert!(xdg.contains(&PathBuf::from("/home/ada/.data/clap")));
        assert!(!xdg.contains(&PathBuf::from("/home/ada/.local/share/clap")));
    }

    /// The CLAP specification defines `CLAP_PATH`; a host that ignored it would not find
    /// plug-ins the user deliberately pointed it at.
    #[test]
    fn the_environment_overrides_come_first_and_are_added_not_substituted() {
        let paths = search_paths_for(
            Platform::Unix,
            &env_from(&[
                ("HOME", "/home/ada"),
                ("CLAP_PATH", "/opt/clap:/srv/shared/clap"),
                ("DAUX_PATH", "/opt/daux"),
            ]),
        );
        assert_eq!(paths[0], PathBuf::from("/opt/daux"));
        assert!(paths.contains(&PathBuf::from("/opt/clap")));
        assert!(paths.contains(&PathBuf::from("/srv/shared/clap")));
        assert!(
            paths.contains(&PathBuf::from("/usr/lib/clap")),
            "an override must not hide the system directories"
        );
    }

    #[test]
    fn windows_overrides_split_on_semicolons_so_drive_letters_survive() {
        let paths = search_paths_for(
            Platform::Windows,
            &env_from(&[("DAUX_PATH", r"C:\plugins;D:\more plugins")]),
        );
        assert_eq!(
            paths,
            vec![
                PathBuf::from(r"C:\plugins"),
                PathBuf::from(r"D:\more plugins")
            ],
            "splitting on `:` would cut `C` off its own path"
        );
    }

    /// An empty entry joins to the current directory, which would make a scan depend on
    /// where the host was launched from.
    #[test]
    fn empty_and_blank_override_entries_are_dropped() {
        let paths = search_paths_for(
            Platform::Unix,
            &env_from(&[("DAUX_PATH", ":/opt/a: :/opt/b:")]),
        );
        assert_eq!(paths[0], PathBuf::from("/opt/a"));
        assert_eq!(paths[1], PathBuf::from("/opt/b"));
        assert!(!paths.contains(&PathBuf::from("")));
        assert!(!paths.contains(&PathBuf::from(".")));
    }

    #[test]
    fn a_directory_named_twice_is_searched_once() {
        let paths = search_paths_for(
            Platform::Unix,
            &env_from(&[("DAUX_PATH", "/usr/lib/clap:/opt/x:/opt/x")]),
        );
        assert_eq!(paths.iter().filter(|p| p.ends_with("x")).count(), 1);
        assert_eq!(
            paths
                .iter()
                .filter(|p| p.as_path() == Path::new("/usr/lib/clap"))
                .count(),
            1,
            "an override that repeats a system path must not double the work"
        );
    }

    /// The table for the platform this test is running on must at least be non-empty on a
    /// normally configured machine, which is the one thing the fake environment cannot
    /// prove.
    #[test]
    fn the_real_environment_produces_somewhere_to_look() {
        let paths = search_paths_for(Platform::current(), &process_env);
        assert!(
            !paths.is_empty(),
            "no search path at all on {:?}",
            Platform::current()
        );
        assert!(paths.iter().all(|path| !path.as_os_str().is_empty()));
    }
}
