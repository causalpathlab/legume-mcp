//! Stand-in Slurm and ssh commands for the executor and runner tests,
//! keeping their queue in `$FAKE_SLURM`: `sbatch` runs each array task at
//! once in the background (or holds it with `$FAKE_SLURM_HOLD`, or fails
//! from submit `$FAKE_SLURM_FAIL_AT` on), `squeue` and `scancel` read and
//! change the queue, `srun` and `ssh` run their command here (ssh drops
//! call `$FAKE_SSH_FAIL_AT`).

use std::path::Path;

/// The stand-ins written into `bin`, the folder of the queue made in
/// `state`.
pub fn install(bin: &Path, state: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(bin).unwrap();
    std::fs::create_dir_all(state.join("q")).unwrap();
    for (name, body) in [
        ("sbatch", SBATCH),
        ("squeue", SQUEUE),
        ("scancel", SCANCEL),
        ("ssh", SSH),
        ("srun", SRUN),
    ] {
        let p = bin.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

const SBATCH: &str = r#"#!/bin/bash
# Stand-in sbatch: each array task runs at once, in the background. Each
# queued array task is a folder q/<jid>_<i> holding its name, state and pid.
S=$FAKE_SLURM
echo "$*" >> "$S/sbatch.log"
n=$(( $(cat "$S/submits" 2>/dev/null || echo 0) + 1 )); echo $n > "$S/submits"
if [ -n "$FAKE_SLURM_FAIL_AT" ] && [ "$n" -ge "$FAKE_SLURM_FAIL_AT" ]; then
  echo "sbatch: error: Batch job submission failed" >&2; exit 1
fi
for a in "$@"; do
  case $a in
    --job-name=*) name=${a#*=} ;;
    --array=0-*) last=${a#--array=0-} ;;
    --output=*) out=${a#*=} ;;
    --wrap=*) wrap=${a#*=} ;;
  esac
done
jid=$(( 100 + n ))
# A batch job, not an array: kept in the queue, pending.
if [ -z "$last" ]; then
  mkdir -p "$S/q/$jid"; echo "$name" > "$S/q/$jid/name"; echo PD > "$S/q/$jid/state"
  echo "$jid"; exit 0
fi
for i in $(seq 0 "$last"); do
  q="$S/q/${jid}_$i"
  mkdir -p "$q"; echo "$name" > "$q/name"
  if [ -n "$FAKE_SLURM_HOLD" ]; then echo PD > "$q/state"; continue; fi
  echo R > "$q/state"
  (
    SLURM_ARRAY_TASK_ID=$i bash -c "$wrap" > "${out//%a/$i}" 2>&1 &
    p=$!; echo $p > "$q/pid"; wait $p; rm -rf "$q"
  ) > /dev/null 2>&1 &
done
echo "$jid"
"#;

const SQUEUE: &str = r#"#!/bin/bash
# Stand-in squeue -h --name=NAME -o '%i %t', or -h --jobs=JID -o %i.
S=$FAKE_SLURM
echo "$*" >> "$S/squeue.log"
for a in "$@"; do case $a in --name=*) name=${a#*=} ;; --jobs=*) jobs=${a#*=} ;; esac; done
for q in "$S"/q/*; do
  [ -d "$q" ] || continue
  if [ -n "$jobs" ]; then
    [ "${q##*/}" = "$jobs" ] && echo "$jobs"
  elif [ "$(cat "$q/name" 2>/dev/null)" = "$name" ]; then
    echo "${q##*/} $(cat "$q/state")"
  fi
done
exit 0
"#;

const SCANCEL: &str = r#"#!/bin/bash
# An array job's id cancels each of its tasks.
S=$FAKE_SLURM
echo "$*" >> "$S/scancel.log"
for j in "$@"; do
  case $j in *_*) pat=$j ;; *) pat="${j}_*" ;; esac
  for q in "$S"/q/$pat; do
    [ -d "$q" ] || continue
    if [ -f "$q/pid" ]; then kill -TERM "$(cat "$q/pid")"; else rm -rf "$q"; fi
  done
done
"#;

const SSH: &str = r#"#!/bin/bash
# Stand-in ssh: options and the target are noted and dropped, and the
# command runs here.
echo "$*" >> "$FAKE_SLURM/ssh.log"
# The connection drops on call $FAKE_SSH_FAIL_AT.
n=$(( $(cat "$FAKE_SLURM/ssh.n" 2>/dev/null || echo 0) + 1 )); echo $n > "$FAKE_SLURM/ssh.n"
if [ "$n" = "${FAKE_SSH_FAIL_AT:-}" ]; then echo "Connection closed" >&2; exit 255; fi
while [ $# -gt 0 ]; do
  case $1 in
    -o|-O) shift 2 ;;
    --) shift; shift; break ;;
    *) shift; break ;;
  esac
done
exec "$@"
"#;

const SRUN: &str = r#"#!/bin/bash
# Stand-in srun: its options noted and dropped, the command run here. It
# reads its stdin, as srun does to pass it on.
echo "$*" >> "$FAKE_SLURM/srun.log"
cat > /dev/null
while [ $# -gt 0 ]; do case $1 in --*=*) shift ;; *) break ;; esac; done
exec "$@"
"#;
