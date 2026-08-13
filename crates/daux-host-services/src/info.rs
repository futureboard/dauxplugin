//! Identity of the host a plug-in instance is running inside.

use core::fmt;

/// Who is hosting the plug-in. `[main-thread]`
///
/// Mirrors the `name`, `vendor` and `version` fields of `DauxHostApiV1`
/// (`docs/specifications/abi-v1.md` §11.6). Plug-ins mostly ignore it; when they
/// do not, it is because a specific host needs a specific workaround, and that
/// is exactly the case this struct exists for. The fields are plain `String`s
/// because reading them is a main-thread operation — never format or compare
/// them inside `process`.
///
/// ```
/// use daux_host_services::HostInfo;
///
/// let info = HostInfo::new("Ardour", "Ardour Community", "8.6.0");
/// assert_eq!(info.name, "Ardour");
/// assert!(!HostInfo::unknown().is_known());
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct HostInfo {
    /// Product name of the host, e.g. `"Reaper"`. Never empty for a real host.
    pub name: String,
    /// Vendor of the host, e.g. `"Cockos"`. May be empty.
    pub vendor: String,
    /// Host version as the host spells it, e.g. `"7.19"`. May be empty.
    pub version: String,
}

impl HostInfo {
    /// The name reported when the host did not identify itself.
    pub const UNKNOWN_NAME: &'static str = "unknown";

    /// Builds host identity from its three parts. `[main-thread]` — allocates.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        vendor: impl Into<String>,
        version: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            vendor: vendor.into(),
            version: version.into(),
        }
    }

    /// Identity for an unhosted context: offline rendering, unit tests, the
    /// `daux` CLI. `[main-thread]`
    ///
    /// The name is [`HostInfo::UNKNOWN_NAME`] rather than an empty string so
    /// that a log line always says *something*.
    #[must_use]
    pub fn unknown() -> Self {
        Self::new(Self::UNKNOWN_NAME, "", "")
    }

    /// `true` when the host actually identified itself. `[main-thread]`
    ///
    /// A plug-in that keys a workaround off [`name`](HostInfo::name) should
    /// check this first, so that an unhosted run never matches a workaround.
    #[must_use]
    pub fn is_known(&self) -> bool {
        !self.name.is_empty() && self.name != Self::UNKNOWN_NAME
    }
}

impl Default for HostInfo {
    /// Same as [`HostInfo::unknown`].
    fn default() -> Self {
        Self::unknown()
    }
}

impl fmt::Display for HostInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if !self.version.is_empty() {
            write!(f, " {}", self.version)?;
        }
        if !self.vendor.is_empty() {
            write!(f, " ({})", self.vendor)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_parts_are_kept_verbatim() {
        let info = HostInfo::new("Reaper", "Cockos", "7.19");
        assert_eq!(info.name, "Reaper");
        assert_eq!(info.vendor, "Cockos");
        assert_eq!(info.version, "7.19");
        assert!(info.is_known());
        assert_eq!(info.to_string(), "Reaper 7.19 (Cockos)");
    }

    #[test]
    fn an_unknown_host_is_recognisable_as_such() {
        let info = HostInfo::unknown();
        assert_eq!(info, HostInfo::default());
        assert!(!info.is_known());
        assert_eq!(info.to_string(), "unknown");
    }

    #[test]
    fn an_empty_name_does_not_count_as_known() {
        let info = HostInfo::new("", "Someone", "1.0");
        assert!(!info.is_known());
        assert_eq!(info.to_string(), " 1.0 (Someone)");
    }

    #[test]
    fn display_omits_the_parts_the_host_left_out() {
        assert_eq!(HostInfo::new("Bitwig", "", "").to_string(), "Bitwig");
        assert_eq!(HostInfo::new("Bitwig", "", "5.2").to_string(), "Bitwig 5.2");
        assert_eq!(
            HostInfo::new("Bitwig", "Bitwig GmbH", "").to_string(),
            "Bitwig (Bitwig GmbH)"
        );
    }
}
