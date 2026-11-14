//! A `legume` command's refusals and the exit codes they end it with: 2
//! for a command line that cannot run (the `legume` binary says the
//! command's usage after it), 3 and up for what a command refuses to do
//! with what it was given.

use anyhow::Result;

/// A refusal and the exit code it ends `legume` with.
#[derive(Debug)]
pub struct Exit {
    pub code: i32,
    pub message: String,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Exit {}

/// A command line that cannot run, and why: exit code 2.
pub fn usage(message: impl std::fmt::Display) -> anyhow::Error {
    Exit {
        code: 2,
        message: message.to_string(),
    }
    .into()
}

/// `what`, or the refusal that `flag` is required.
pub fn required<T>(what: Option<T>, flag: &str) -> Result<T> {
    what.ok_or_else(|| usage(format!("{flag} is required")))
}

/// The exit code `e` ends `legume` with: an [`Exit`]'s own, 1 for anything
/// else.
pub fn code(e: &anyhow::Error) -> i32 {
    e.downcast_ref::<Exit>().map_or(1, |x| x.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_carries_its_exit_code_and_anything_else_is_1() {
        assert_eq!(code(&usage("M1")), 2);
        assert_eq!(code(&required::<()>(None, "--key").unwrap_err()), 2);
        assert_eq!(required(Some(1), "--key").unwrap(), 1);
        assert_eq!(code(&anyhow::anyhow!("M1")), 1);
        let e = Exit {
            code: 3,
            message: "M1".into(),
        };
        assert_eq!(code(&e.into()), 3);
    }
}
