#!/usr/bin/env bash
# Install the legume tools (or any Rust crates) on a Slurm cluster, as one
# batch job that downloads and builds on a compute node.
#
# Run this on the cluster's login node. It writes a job script into
# PREFIX/install/<time>/ and submits it; the job installs each crate with
# `cargo install --locked` into PREFIX/bin. It does not wait: building
# them all can take a few hours. Watch PREFIX/install/<time>/status, which
# goes from "submitted <job id>" to "running", then "done", or "failed"
# with the crates that did not install (the others still are).
#
# Why a job: login nodes are often older than compute nodes, may have no
# internet, and should not carry long builds. A program built on a node
# runs on nodes like it (same or newer glibc); run tools where they were
# built.
#
# The build happens in node-local scratch (--build-tmp, /tmp by default)
# and is removed after, so only the installed binaries land in PREFIX.
#
# Usage:
#   slurm-install.sh --prefix DIR [options] [CRATE[@VERSION][:FEATURES] ...]
#
# Options:
#   --prefix DIR        where to install (required); binaries go to DIR/bin
#   --partition NAME    Slurm partition (default: the cluster's default)
#   --account NAME      Slurm account
#   --cpus N            cores for the build (default 8)
#   --mem GB            memory for the build (default 16)
#   --time HH:MM:SS     time limit (default 8:00:00)
#   --build-tmp DIR     node-local scratch for the build (default /tmp)
#   --no-rust-update    use the Rust toolchain as it is
#   --dry-run           print the job script and do nothing
#
# Crates default to the legume tools: legume-mcp:benchmark (legume),
# pinto-rs (pinto), lupin-rs (lupin), senna-rs (senna), chickpea-rs
# (chickpea), faba and data-beans. FEATURES is a comma-separated list.
# Examples:
#   slurm-install.sh --prefix ~/tools
#   slurm-install.sh --prefix /project/me/tools --partition PART1 --cpus 16 legume-mcp:benchmark pinto-rs
#
# Rust: when `cargo` runs on the node it is used, and `rustup update
# stable` brings it up to date (unless --no-rust-update). Otherwise rustup
# installs a toolchain into PREFIX/rust.

set -euo pipefail

usage() {
    sed -n '2,/^$/s/^# \{0,1\}//p' "$0" >&2
    exit 2
}

die() {
    echo "slurm-install: $*" >&2
    exit 2
}

prefix=""
partition=""
account=""
cpus=8
mem=16
time_limit="8:00:00"
build_tmp="/tmp"
rust_update=1
dry_run=0
crates=()

while [ $# -gt 0 ]; do
    case "$1" in
    --prefix) prefix="${2:-}"; shift 2 ;;
    --partition) partition="${2:-}"; shift 2 ;;
    --account) account="${2:-}"; shift 2 ;;
    --cpus) cpus="${2:-}"; shift 2 ;;
    --mem) mem="${2:-}"; shift 2 ;;
    --time) time_limit="${2:-}"; shift 2 ;;
    --build-tmp) build_tmp="${2:-}"; shift 2 ;;
    --no-rust-update) rust_update=0; shift ;;
    --dry-run) dry_run=1; shift ;;
    -h | --help) usage ;;
    -*) die "no option $1 (see --help)" ;;
    *) crates+=("$1"); shift ;;
    esac
done

[ -n "$prefix" ] || die "--prefix DIR is required: where to install"
case "$cpus" in '' | *[!0-9]* | 0) die "--cpus wants a whole number of cores, not '$cpus'" ;; esac
case "$mem" in '' | *[!0-9]* | 0) die "--mem wants whole GB, not '$mem'" ;; esac
case "$time_limit" in '' | *[!0-9:-]*) die "--time wants HH:MM:SS or D-HH:MM:SS, not '$time_limit'" ;; esac
[ ${#crates[@]} -gt 0 ] || crates=(
    "legume-mcp:benchmark" "pinto-rs" "lupin-rs" "senna-rs" "chickpea-rs" "faba" "data-beans"
)
for c in "${crates[@]}"; do
    case "$c" in
    *[!A-Za-z0-9_.:@,+-]* | :* | @*) die "'$c' is not CRATE[@VERSION][:FEATURES]" ;;
    esac
done

# An absolute prefix, so the job finds it from any folder.
case "$prefix" in
/*) ;;
"~"/*) prefix="$HOME/${prefix#\~/}" ;;
*) prefix="$(pwd -P)/$prefix" ;;
esac

q() { printf '%q' "$1"; }

job() {
    echo "#!/bin/bash"
    echo "#SBATCH --job-name=legume-install"
    [ -n "$partition" ] && echo "#SBATCH --partition=$partition"
    [ -n "$account" ] && echo "#SBATCH --account=$account"
    echo "#SBATCH --cpus-per-task=$cpus"
    echo "#SBATCH --mem=${mem}G"
    echo "#SBATCH --time=$time_limit"
    echo "set -uo pipefail"
    echo "prefix=$(q "$prefix")"
    echo "build_tmp=$(q "$build_tmp")"
    echo "rust_update=$rust_update"
    printf 'crates=('
    for c in "${crates[@]}"; do printf ' %s' "$(q "$c")"; done
    echo ' )'
    cat <<'EOF'
run="${LEGUME_INSTALL_RUN:?}"
say() { echo "[$(date '+%F %T')] $*"; }
status() { echo "$*" > "$run/status.part" && mv "$run/status.part" "$run/status"; }
status running
node=$(hostname -s)
glibc=$(ldd --version 2>&1 | head -1)
say "node $node, $glibc"

# Rust: the cargo on PATH, else ~/.cargo's or the prefix's, else rustup
# into the prefix.
for d in "${CARGO_HOME:-$HOME/.cargo}/bin" "$prefix/rust/cargo/bin"; do
    cargo --version > /dev/null 2>&1 && break
    [ -x "$d/cargo" ] && PATH="$d:$PATH"
done
if ! cargo --version > /dev/null 2>&1; then
    say "no working cargo here: installing rustup into $prefix/rust"
    export RUSTUP_HOME="$prefix/rust/rustup" CARGO_HOME="$prefix/rust/cargo"
    curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal \
        || { status "failed rustup"; exit 1; }
    PATH="$CARGO_HOME/bin:$PATH"
elif [ "$rust_update" = 1 ] && command -v rustup > /dev/null 2>&1; then
    say "updating the Rust toolchain"
    rustup update stable || say "rustup update failed; going on with $(rustc --version)"
fi
say "$(cargo --version), $(rustc --version)"

export CARGO_TARGET_DIR="$build_tmp/legume-install.${SLURM_JOB_ID:-$$}"
trap 'rm -rf "$CARGO_TARGET_DIR"' EXIT
mkdir -p "$CARGO_TARGET_DIR" "$prefix/bin"

failed=""
for spec in "${crates[@]}"; do
    name="${spec%%:*}"
    features=""
    [ "$name" != "$spec" ] && features="${spec#*:}"
    version=""
    case "$name" in *@*) version="${name#*@}"; name="${name%%@*}" ;; esac
    args=(install --locked --root "$prefix" -j "${SLURM_CPUS_PER_TASK:-1}")
    [ -n "$version" ] && args+=(--version "$version")
    [ -n "$features" ] && args+=(--features "$features")
    say "cargo ${args[*]} $name"
    cargo "${args[@]}" "$name" || { failed="$failed $name"; say "failed: $name"; }
done

{
    echo "node: $node"
    echo "glibc: $glibc"
    for b in "$prefix"/bin/*; do
        [ -x "$b" ] && echo "$(basename "$b"): $("$b" --version 2>&1 | head -1)"
    done
} > "$run/versions.txt"
cat "$run/versions.txt"
say "Add to PATH: export PATH=\"$prefix/bin:\$PATH\""
if [ -n "$failed" ]; then
    status "failed$failed"
    say "these did not install (see above):$failed"
    exit 1
fi
status done
say done
EOF
}

if [ "$dry_run" = 1 ]; then
    job
    exit 0
fi

command -v sbatch > /dev/null || die "no sbatch here: run this on the cluster's login node"
run="$prefix/install/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$run"
job > "$run/install.sbatch"
jid=$(sbatch --parsable --export=ALL,LEGUME_INSTALL_RUN="$run" \
    --output="$run/slurm.log" "$run/install.sbatch")
echo "submitted $jid" > "$run/status"
cat <<EOF
Submitted job $jid: installing ${crates[*]} into $prefix
  log:     $run/slurm.log
  status:  $run/status   (submitted, running, then done, or failed <crates>)
  watch:   squeue -j $jid    or    tail -f $run/slurm.log
When it says done, add to your shell's startup file:
  export PATH="$prefix/bin:\$PATH"
EOF
