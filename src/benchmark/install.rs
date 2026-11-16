//! `legume benchmark install --on REMOTE`: legume's tools installed on a
//! cluster from here, by `scripts/slurm-install.sh` (built into legume)
//! as one Slurm batch job, with legume-mcp at this legume's version, so
//! the cluster runs the legume a run here expects
//! ([`super::run`] checks it).

use super::remote::{self, Shell};
use crate::command::{canonical_flags, flag_pairs, refuse, required, Flag};
use crate::project::quote;
use anyhow::Result;
use std::io::Write;

/// `legume benchmark install`'s usage.
pub const USAGE: &str = "\
legume benchmark install --on REMOTE [CRATE[@VERSION][:FEATURES] ...]

Installs legume's tools on the cluster REMOTE, from here: one Slurm batch
job builds them on a compute node into the remote's bin folder, which
must be PREFIX/bin. legume-mcp is installed at this legume's version, as
a benchmark run here wants it there. With no CRATE: legume-mcp (with
benchmark), pinto-rs, lupin-rs, senna-rs, chickpea-rs, faba and
data-beans; those already installed at their version are kept.
It does not wait, as building them all can take a few hours: it says
where the job's status and log are, in PREFIX/install/<time>/. While an
install is in Slurm's queue, a new one is not started; this says how
that one goes instead.";

const FLAGS: &[Flag] = &[Flag::once(&["--on"])];

/// The script, as the cluster gets it.
const SCRIPT: &str = include_str!("../../scripts/slurm-install.sh");

/// The line that starts the output when an install is still queued.
const QUEUED: &str = "@@queued";

/// `legume benchmark install` with `args` (those after `install`).
pub fn main(args: &[String], out: &mut dyn Write, _err: &mut dyn Write) -> Result<()> {
    // Flags and their values first, then the crates.
    let mut i = 0;
    while args.get(i).is_some_and(|a| a.starts_with('-')) {
        i += if matches!(args[i].as_str(), "-h" | "--help") {
            1
        } else {
            2
        };
    }
    let (flags, crates) = args.split_at(i.min(args.len()));
    let Some(pairs) = flag_pairs(flags)? else {
        return Ok(writeln!(out, "{USAGE}")?);
    };
    let mut on = None;
    for (_, v) in canonical_flags(FLAGS, pairs)? {
        on = Some(v.to_string());
    }
    let on = required(on, "--on")?;
    let remote = remote::named(&on, out)?;
    let prefix = remote.prefix().ok_or_else(|| {
        refuse(format!(
            "remote {on}'s bin must be PREFIX/bin, the folder an install puts \
             legume's tools in; set it in {}",
            remote::config_file().map_or("remotes.toml".into(), |f| f.display().to_string())
        ))
    })?;
    let shell = Shell::ssh(&remote.ssh);
    let said = shell.check(
        &script(
            prefix,
            remote.partition.as_deref(),
            remote.account.as_deref(),
            crates,
        ),
        "installing on the cluster",
    );
    shell.close();
    let said = said?;
    match said.split_once(QUEUED) {
        Some((_, rest)) => write!(out, "no install was started:{rest}")?,
        None => writeln!(
            out,
            "{said}From here, legume benchmark install --on {on} again says how it goes."
        )?,
    }
    Ok(())
}

/// The cluster's side, in one call, under a lock on `prefix`'s installs
/// where the cluster has flock: one of its last three installs said when
/// it is still queued, or else the script written beside the
/// installs and run, which submits the job.
fn script(
    prefix: &str,
    partition: Option<&str>,
    account: Option<&str>,
    crates: &[String],
) -> String {
    use std::fmt::Write as _;
    let mut run = format!(
        "bash \"$d/slurm-install.sh\" --prefix \"$p\" --legume {}",
        env!("CARGO_PKG_VERSION")
    );
    for (flag, v) in [("--partition", partition), ("--account", account)] {
        if let Some(v) = v {
            let _ = write!(run, " {flag} {}", quote(v));
        }
    }
    for c in crates {
        let _ = write!(run, " {}", quote(c));
    }
    format!(
        "set -e\np={p}\nd=\"$p/install\"\nmkdir -p \"$d\"\n\
         if command -v flock > /dev/null; then\n\
         \x20 exec 9> \"$d/.lock\"\n\
         \x20 flock -n 9 || {{ echo \"{QUEUED} another install is being submitted right now\"; exit 0; }}\n\
         fi\n\
         while IFS= read -r last; do\n\
         \x20 j=$(cat \"$last/job_id\" 2> /dev/null || true)\n\
         \x20 if [ -n \"$j\" ] && squeue -h --jobs=\"$j\" -o %i 2> /dev/null | grep -q .; then\n\
         \x20   echo \"{QUEUED} job $j is still in Slurm's queue, $(cat \"$last/status\" 2> /dev/null)\"\n\
         \x20   echo \"  log: ${{last}}slurm.log\"\n\
         \x20   tail -n 3 \"$last/slurm.log\" 2> /dev/null | sed 's/^/    /'\n\
         \x20   exit 0\n\
         \x20 fi\n\
         done < <(ls -1d \"$d\"/2*/ 2> /dev/null | tail -n 3 | sort -r)\n\
         printf '%s' {script} > \"$d/slurm-install.sh\"\n\
         {run}\n",
        p = quote(prefix),
        script = quote(SCRIPT),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_job_pins_legume_and_quotes_what_it_is_given() {
        let s = script("/p q", Some("PART1"), None, &["pinto-rs".into()]);
        assert!(s.contains("p='/p q'\n"), "{s}");
        assert!(s.ends_with(&format!(
            "--legume {} --partition PART1 pinto-rs\n",
            env!("CARGO_PKG_VERSION")
        )));
    }
}
