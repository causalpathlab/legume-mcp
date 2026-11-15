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

/// What a command refuses to do with what it was given, and why: exit
/// code 3.
pub fn refuse(message: impl std::fmt::Display) -> anyhow::Error {
    Exit {
        code: 3,
        message: message.to_string(),
    }
    .into()
}

/// `what`, or the refusal that `flag` is required.
pub fn required<T>(what: Option<T>, flag: &str) -> Result<T> {
    what.ok_or_else(|| usage(format!("{flag} is required")))
}

/// `args` as flag and value pairs; `None` for `-h` or `--help` in a flag's
/// place. A flag with no value after it is refused.
pub fn flag_pairs(args: &[String]) -> Result<Option<Vec<(&str, &str)>>> {
    let mut pairs = Vec::with_capacity(args.len() / 2);
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if matches!(flag.as_str(), "-h" | "--help") {
            return Ok(None);
        }
        let Some(value) = it.next() else {
            return Err(usage(format!("{flag} needs a value, or is unknown")));
        };
        pairs.push((flag.as_str(), value.as_str()));
    }
    Ok(Some(pairs))
}

/// The value `v` of `flag` names, among `words`: each word and its value.
pub fn one_of<T: Copy>(flag: &str, v: &str, words: &[(&str, T)]) -> Result<T> {
    match words.iter().find(|(w, _)| *w == v) {
        Some(&(_, value)) => Ok(value),
        None => {
            let names: Vec<&str> = words.iter().map(|(w, _)| *w).collect();
            Err(usage(format!(
                "{flag} is {}, not {v:?}",
                names.join(" or ")
            )))
        }
    }
}

/// The exit code `e` ends `legume` with: an [`Exit`]'s own, 1 for anything
/// else.
pub fn code(e: &anyhow::Error) -> i32 {
    e.downcast_ref::<Exit>().map_or(1, |x| x.code)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn a_refusal_carries_its_exit_code_and_anything_else_is_1() {
        assert_eq!(code(&usage("M1")), 2);
        assert_eq!(code(&required::<()>(None, "--key").unwrap_err()), 2);
        assert_eq!(required(Some(1), "--key").unwrap(), 1);
        assert_eq!(code(&anyhow::anyhow!("M1")), 1);
        assert_eq!(code(&refuse("M1")), 3);
        assert_eq!(refuse("M1").to_string(), "M1");
        let e = Exit {
            code: 3,
            message: "M1".into(),
        };
        assert_eq!(code(&e.into()), 3);
    }

    #[test]
    fn flags_pair_with_their_values_and_help_wins() {
        let a = args("--key K1 --score M1=v");
        assert_eq!(
            flag_pairs(&a).unwrap().unwrap(),
            [("--key", "K1"), ("--score", "M1=v")]
        );
        assert!(flag_pairs(&args("--key K1 --help")).unwrap().is_none());
        assert!(flag_pairs(&args("-h")).unwrap().is_none());
        // A value that looks like help is a value.
        assert_eq!(
            flag_pairs(&args("--label -h")).unwrap().unwrap(),
            [("--label", "-h")]
        );
        let e = flag_pairs(&args("--key K1 --score")).unwrap_err();
        assert_eq!(code(&e), 2);
        assert_eq!(e.to_string(), "--score needs a value, or is unknown");
    }

    #[test]
    fn one_of_names_the_words_it_takes() {
        let words = [("fail", 0), ("drop", 1)];
        assert_eq!(one_of("--dup", "drop", &words).unwrap(), 1);
        let e = one_of("--dup", "keep", &words).unwrap_err();
        assert_eq!(code(&e), 2);
        assert_eq!(e.to_string(), "--dup is fail or drop, not \"keep\"");
    }
}
