//! A tool's clap command line as an agent sets it (feature `clap`): its
//! flags listed for the agent, and a JSON map of flags turned into the words
//! clap reads, then checked as the tool parses them. The tool keeps its own
//! arguments (data files, the output prefix) out of the map and names them
//! in the refusals.

use anyhow::Result;
use clap::error::{ContextKind, ErrorKind};
use clap::{Arg, ArgAction, ArgMatches, Command};
use serde_json::{json, Map, Value};

/// The arguments an agent may set: all but those that print help or the
/// version. Hidden ones are here too; a tool that lists only the visible
/// ones filters on [`Arg::is_hide_set`].
pub fn settable(cmd: &Command) -> impl Iterator<Item = &Arg> {
    cmd.get_arguments().filter(|a| {
        !matches!(
            a.get_action(),
            ArgAction::Help | ArgAction::HelpShort | ArgAction::HelpLong | ArgAction::Version
        )
    })
}

/// An argument as the command line spells it: `--long`, else `-s`, and a
/// positional by its name.
pub fn flag_name(arg: &Arg) -> String {
    match (arg.get_long(), arg.get_short()) {
        (Some(long), _) => format!("--{long}"),
        (None, Some(short)) => format!("-{short}"),
        (None, None) => arg.get_id().as_str().replace('_', "-"),
    }
}

/// Whether `arg` takes no value: set by naming it. True names it (for a
/// `--no-x` switch too); a count takes how many times.
pub fn is_switch(arg: &Arg) -> bool {
    !arg.get_action().takes_values()
}

/// `arg` for the agent: its `flag` and `help`; `switch`, or its `default`
/// and `choices` (hidden ones left out); `positional` and `required` when
/// so.
pub fn describe(arg: &Arg) -> Value {
    let mut flag = json!({
        "flag": flag_name(arg),
        "help": arg.get_help().map(|h| h.to_string()),
    });
    if is_switch(arg) {
        flag["switch"] = true.into();
    } else {
        let defaults: Vec<String> = arg
            .get_default_values()
            .iter()
            .map(|v| v.to_string_lossy().into_owned())
            .collect();
        if !defaults.is_empty() {
            flag["default"] = defaults.join(",").into();
        }
        let choices: Vec<String> = arg
            .get_possible_values()
            .iter()
            .filter(|v| !v.is_hide_set())
            .map(|v| v.get_name().to_string())
            .collect();
        if !choices.is_empty() {
            flag["choices"] = choices.into();
        }
    }
    if arg.is_positional() {
        flag["positional"] = true.into();
    }
    if arg.is_required_set() {
        flag["required"] = true.into();
    }
    flag
}

/// The settable argument `key` names, with or without its dashes: its long
/// name or an alias of it (`_` read as `-`), its id, or its short letter.
pub fn find<'a>(cmd: &'a Command, key: &str) -> Option<&'a Arg> {
    let key = key.trim_start_matches('-');
    let long = key.replace('_', "-");
    let id = key.replace('-', "_");
    settable(cmd).find(|a| {
        a.get_long() == Some(long.as_str())
            || a.get_all_aliases()
                .is_some_and(|aliases| aliases.contains(&long.as_str()))
            || a.get_id().as_str() == id
            || a.get_short().is_some_and(|s| {
                let mut c = key.chars();
                c.next() == Some(s) && c.next().is_none()
            })
    })
}

/// A value as it goes on the command line.
fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => v.to_string(),
    }
}

/// The words for setting `arg` to `value`; null leaves it at its default.
/// A switch is named when true, a count as many times as the number given.
/// A value goes as one `--flag=value` word, so a value starting with `-`
/// stays a value. A list is joined by the argument's delimiter, given once
/// per item to an argument that gathers them, or as words after the flag to
/// one that takes several values (none starting with `-`, unless it allows
/// them); an argument of one value takes no list.
pub fn flag_words(arg: &Arg, value: &Value) -> Result<Vec<String>> {
    let flag = flag_name(arg);
    if is_switch(arg) {
        let times = match (value, arg.get_action()) {
            (Value::Bool(on), _) => usize::from(*on),
            (Value::Null, _) => 0,
            (Value::Number(n), ArgAction::Count) => n
                .as_u64()
                .and_then(|n| usize::try_from(n).ok())
                .ok_or_else(|| anyhow::anyhow!("{flag} counts: a whole number, not {n}"))?,
            (v, ArgAction::Count) => anyhow::bail!("{flag} counts: a number or true, not {v}"),
            (v, _) => anyhow::bail!("{flag} is a switch: true or false, not {v}"),
        };
        return Ok(vec![flag; times]);
    }
    let values: Vec<String> = match value {
        Value::Null => return Ok(vec![]),
        Value::Array(vs) => vs.iter().map(text).collect(),
        v => return Ok(vec![format!("{flag}={}", text(v))]),
    };
    Ok(match arg.get_value_delimiter() {
        Some(delim) => vec![format!("{flag}={}", values.join(&delim.to_string()))],
        None if matches!(arg.get_action(), ArgAction::Append) => {
            values.iter().map(|v| format!("{flag}={v}")).collect()
        }
        None if arg.get_num_args().is_some_and(|r| r.max_values() > 1) => {
            // Separate words: clap takes one value after `=`, and reads a
            // word starting with `-` as a flag unless the argument allows it.
            let hyphens = arg.is_allow_hyphen_values_set() || arg.is_allow_negative_numbers_set();
            if let Some(v) = values.iter().find(|v| v.starts_with('-') && !hyphens) {
                anyhow::bail!(
                    "{flag} cannot take {v}: a value of several starting with `-` reads as a flag"
                );
            }
            std::iter::once(flag).chain(values).collect()
        }
        None => anyhow::bail!("{flag} takes one value, not a list"),
    })
}

/// `flags` (an agent's map of flag to value) as words for `cmd`. `own`
/// pairs the ids of the arguments the tool takes itself (its data files,
/// its output prefix) with the names the tool gives them, so setting one as
/// a flag points to it. Any other positional is refused too; an unknown
/// flag points to `lister`, the tool that lists them.
pub fn to_argv(
    cmd: &Command,
    flags: &Map<String, Value>,
    own: &[(&str, &str)],
    lister: &str,
) -> Result<Vec<String>> {
    let mut argv = Vec::new();
    let mut seen: Vec<(&str, &String)> = Vec::new();
    for (key, value) in flags {
        let arg = find(cmd, key).ok_or_else(|| {
            let shown = key.trim_start_matches('-');
            anyhow::anyhow!(
                "{} has no flag --{shown}: {lister} lists them",
                cmd.get_name()
            )
        })?;
        if let Some((_, name)) = own.iter().find(|(id, _)| arg.get_id() == *id) {
            anyhow::bail!("give {} as `{name}`, not as a flag", flag_name(arg));
        }
        anyhow::ensure!(
            !arg.is_positional(),
            "{} is positional, not a flag: {} takes it as its own argument",
            flag_name(arg),
            cmd.get_name()
        );
        let id = arg.get_id().as_str();
        if let Some((_, first)) = seen.iter().find(|(seen, _)| *seen == id) {
            anyhow::bail!("{first} and {key} name the same flag, {}", flag_name(arg));
        }
        seen.push((id, key));
        argv.extend(flag_words(arg, value)?);
    }
    Ok(argv)
}

/// `argv` (without the program's name) parsed as `cmd` parses it, or clap's
/// error as [`one_line`] gives it.
pub fn check(cmd: &Command, argv: &[String]) -> Result<ArgMatches> {
    cmd.clone()
        .no_binary_name(true)
        .try_get_matches_from(argv)
        .map_err(|e| one_line(e, cmd, argv))
}

/// clap's error on one line under the name of the command it refuses (`app
/// sub` for a subcommand): what is wrong, the choices and clap's tips,
/// without the usage and the pointer to `--help`, which are for a person at
/// a shell. Help or a version asked for comes back whole.
fn one_line(mut e: clap::Error, cmd: &Command, argv: &[String]) -> anyhow::Error {
    if matches!(
        e.kind(),
        ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
    ) {
        return anyhow::anyhow!("{}", e.to_string().trim_end());
    }
    e.remove(ContextKind::Usage);
    let rendered = e.to_string();
    let mut said = rendered.trim_end();
    // clap has no switch for its pointer to --help: it is cut from the end,
    // so that a value of the agent's in the message is never cut.
    if let Some((rest, _)) = said
        .rsplit_once("\n\n")
        .filter(|(_, last)| last.starts_with("For more information"))
    {
        said = rest;
    }
    said = said.strip_prefix("error: ").unwrap_or(said);
    let words: Vec<&str> = said
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    anyhow::anyhow!("{}: {}", command_path(cmd, argv), words.join(" "))
}

/// The command `argv` runs under `cmd`: its name, then each subcommand the
/// leading words name.
fn command_path(cmd: &Command, argv: &[String]) -> String {
    let mut path = vec![cmd.get_name()];
    let mut at = cmd;
    for word in argv {
        let Some(sub) = at.find_subcommand(word) else {
            break;
        };
        path.push(sub.get_name());
        at = sub;
    }
    path.join(" ")
}
