//! What the process returns to whatever started it.

/// The outcome of one `daux` invocation. [main-thread]
///
/// Four outcomes, four numbers. The distinction that matters is between [`Exit::Issues`] —
/// the command ran and the *thing it was pointed at* is wrong — and [`Exit::CannotRun`] —
/// the command never got that far. A CI script wants to fail on the first and to retry or
/// report a broken environment on the second, so they cannot share a code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    /// The command ran and found nothing wrong.
    Ok,
    /// The command ran and found problems: validation errors, a failing check.
    Issues,
    /// The command could not run: no such file, unreadable bundle, `cargo` failed.
    CannotRun,
    /// A bug in `daux` itself. Reserved for a caught panic.
    Internal,
}

impl Exit {
    /// [main-thread] The process exit code.
    ///
    /// `2` is deliberately absent: it belongs to `clap`, which exits with it directly when
    /// the command line is malformed.
    pub const fn code(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Issues => 1,
            Self::CannotRun => 3,
            Self::Internal => 70,
        }
    }

    /// [main-thread] [`Exit::Issues`] when `found` is true, [`Exit::Ok`] otherwise.
    pub const fn from_issues(found: bool) -> Self {
        if found { Self::Issues } else { Self::Ok }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_run_and_a_run_that_found_problems_are_different_codes() {
        assert_eq!(Exit::from_issues(false), Exit::Ok);
        assert_eq!(Exit::from_issues(true), Exit::Issues);
        assert_ne!(Exit::Issues.code(), Exit::CannotRun.code());
        assert_ne!(Exit::Issues.code(), Exit::Ok.code());
    }
}
