#!/usr/bin/env bash
# Install the legume tools (or any Rust crates) on a Slurm cluster, as one
# batch job that downloads and builds on a compute node.
#
# Run this on the cluster's login node. It writes a job script into
# PREFIX/install/<time>/ and submits it; the job installs each crate with
# `cargo install --locked` into PREFIX/bin. It does not wait: building
# them all can take a few hours. Watch PREFIX/install/<time>/status, which
# goes from "submitted" to "running", then "done", or "failed" with the
# crates that did not install (the others still are), or "failed: ..."
# when the job stopped early. The Slurm job id is in job_id beside it.
#
# Why a job: login nodes are often older than compute nodes, may have no
# internet, and should not carry long builds. A program built on a node
# runs on nodes like it (same or newer glibc); run tools where they were
# built.
#
# The build happens in node-local scratch (--build-tmp, /tmp by default)
# and is removed after, so only the installed binaries land in PREFIX.
# cargo's download cache goes in PREFIX/cargo-home, not the home folder.
#
# Jobs still running a tool keep it: each binary as it was is kept under
# a second name in PREFIX/install/<time>/previous/ (a hard link, no copy),
# as on a shared filesystem such as NFS a binary replaced under running
# jobs on other nodes kills them. The last 3 installs that replaced
# one keep theirs. PREFIX/cargo-home grows with the crates downloaded; it
# gets the home folder's cargo config on each install.
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
#   --no-libclang       never fetch a libclang (see below)
#   --legume VERSION    legume-mcp at VERSION, with benchmark, in the
#                       defaults or when it is named without a version
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
#
# libclang: faba's BAM reader (htslib) makes its C bindings with libclang
# at build time (needs_libclang_crates below names the crates that do).
# When one of them is to be installed and the node has none (no
# LIBCLANG_PATH, LD_LIBRARY_PATH, llvm-config or ldconfig), the job fetches PyPI's libclang
# wheel into PREFIX/libclang once, with python3's standard library, and
# gives bindgen gcc's built-in headers, which the wheel lacks. Nothing
# that runs afterwards uses it. --no-libclang skips this.

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
libclang=1
dry_run=0
legume=""
crates=()
# The crates installed when none are named, and the names of those that
# make C bindings with libclang at build time.
default_crates=(
    "legume-mcp:benchmark" "pinto-rs" "lupin-rs" "senna-rs" "chickpea-rs" "faba" "data-beans"
)
needs_libclang_crates=(faba)

# The value of option $1, which must be given: $2, not empty and not the
# next option.
need() {
    case "${2:-}" in '' | --*) die "$1 needs a value" ;; esac
}

while [ $# -gt 0 ]; do
    case "$1" in
    --prefix) need "$@"; prefix="$2"; shift 2 ;;
    --partition) need "$@"; partition="$2"; shift 2 ;;
    --account) need "$@"; account="$2"; shift 2 ;;
    --cpus) need "$@"; cpus="$2"; shift 2 ;;
    --mem) need "$@"; mem="$2"; shift 2 ;;
    --time) need "$@"; time_limit="$2"; shift 2 ;;
    --build-tmp) need "$@"; build_tmp="$2"; shift 2 ;;
    --no-rust-update) rust_update=0; shift ;;
    --no-libclang) libclang=0; shift ;;
    --legume) need "$@"; legume="$2"; shift 2 ;;
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
for v in "$partition" "$account"; do
    case "$v" in *[!A-Za-z0-9_.,-]*) die "'$v' is not a Slurm partition or account name" ;; esac
done
case "$legume" in *[!A-Za-z0-9.+-]*) die "'$legume' is not a version" ;; esac
[ ${#crates[@]} -gt 0 ] || crates=("${default_crates[@]}")
if [ -n "$legume" ]; then
    for i in "${!crates[@]}"; do
        case "${crates[$i]}" in
        legume-mcp | legume-mcp:*)
            f="${crates[$i]#legume-mcp}"
            f="${f#:}"
            case ",$f," in *,benchmark,*) ;; *) f="${f:+$f,}benchmark" ;; esac
            crates[i]="legume-mcp@$legume:$f"
            ;;
        esac
    done
fi
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

# This install's own folder: its job script, log, status and versions.
run="$prefix/install/$(date +%Y%m%d-%H%M%S)"

job() {
    echo "#!/bin/bash"
    echo "#SBATCH --job-name=legume-install"
    [ -n "$partition" ] && echo "#SBATCH --partition=$partition"
    [ -n "$account" ] && echo "#SBATCH --account=$account"
    echo "#SBATCH --cpus-per-task=$cpus"
    echo "#SBATCH --mem=${mem}G"
    echo "#SBATCH --time=$time_limit"
    echo "set -uo pipefail"
    # Each value quoted so the job reads it back as it is here.
    declare -p prefix build_tmp rust_update libclang run crates needs_libclang_crates
    cat <<'EOF'
say() { echo "[$(date '+%F %T')] $*"; }
status() { echo "$*" > "$run/status.part" && mv "$run/status.part" "$run/status"; }

# A job Slurm stops (time limit, scancel, node failure) says so: TERM
# reaches this shell at once because each build runs in the background
# and is waited for, and the build is stopped with it.
build=""
finish() {
    [ -n "${CARGO_TARGET_DIR:-}" ] && rm -rf "$CARGO_TARGET_DIR"
    [ "$(cat "$run/status" 2> /dev/null)" = running ] \
        && status "failed: the job stopped before it finished (time limit, cancel or node failure)"
}
trap finish EXIT
trap '[ -n "$build" ] && kill "$build" 2> /dev/null; exit 143' TERM INT
status running
node=$(hostname -s)
glibc=$(ldd --version 2>&1 | head -1)
say "node $node, $glibc"

# The Rust toolchain rustup keeps in the prefix. Its proxies find it only
# through RUSTUP_HOME and CARGO_HOME.
use_prefix_rust() {
    export RUSTUP_HOME="$prefix/rust/rustup" CARGO_HOME="$prefix/rust/cargo"
    PATH="$CARGO_HOME/bin:$PATH"
}

# Rust: the cargo on PATH, else ~/.cargo's or the prefix's, else rustup
# into the prefix.
for d in "${CARGO_HOME:-$HOME/.cargo}/bin" "$prefix/rust/cargo/bin"; do
    cargo --version > /dev/null 2>&1 && break
    [ -x "$d/cargo" ] || continue
    if [ "$d" = "$prefix/rust/cargo/bin" ]; then use_prefix_rust; else PATH="$d:$PATH"; fi
done
if ! cargo --version > /dev/null 2>&1; then
    say "no working cargo here: installing rustup into $prefix/rust"
    use_prefix_rust
    curl -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path --profile minimal \
        || { status "failed rustup"; exit 1; }
elif [ "$rust_update" = 1 ] && command -v rustup > /dev/null 2>&1; then
    say "updating the Rust toolchain"
    rustup update stable || say "rustup update failed; going on with $(rustc --version)"
fi
say "$(cargo --version), $(rustc --version)"

# cargo's download cache in the prefix (see above); the toolchain stays.
case "${CARGO_HOME:-}" in
"$prefix"/*) ;;
*)
    old="${CARGO_HOME:-$HOME/.cargo}"
    export CARGO_HOME="$prefix/cargo-home"
    mkdir -p "$CARGO_HOME"
    for f in config config.toml; do
        [ -f "$old/$f" ] && cp "$old/$f" "$CARGO_HOME/"
    done
    ;;
esac

export CARGO_TARGET_DIR="$build_tmp/legume-install.${SLURM_JOB_ID:-$$}"
mkdir -p "$CARGO_TARGET_DIR" "$prefix/bin"

# Each binary as it was (see above): cargo renames each new one over it,
# and on NFS the server frees a file once no name is left on it, while
# jobs on other nodes may still run it.
keep="$run/previous"
mkdir -p "$keep"
for f in "$prefix/bin"/*; do
    [ -f "$f" ] || continue
    ln -f "$f" "$keep/" 2> /dev/null || say "could not keep $f for the jobs running it"
done

# libclang for crates whose C bindings are made at build time.
needs_libclang=0
for spec in "${crates[@]}"; do
    for c in "${needs_libclang_crates[@]}"; do
        [ "${spec%%[@:]*}" = "$c" ] && needs_libclang=1
    done
done
# A libclang the build would find: LIBCLANG_PATH, a folder in
# LD_LIBRARY_PATH or llvm-config's, or the linker's cache (grep reads all
# of ldconfig's listing, so pipefail never sees it cut short).
has_libclang() {
    [ -n "${LIBCLANG_PATH:-}" ] && return 0
    local d ld="${LD_LIBRARY_PATH:-}"
    for d in ${ld//:/ } $(llvm-config --libdir 2> /dev/null); do
        ls "$d"/libclang*.so* > /dev/null 2>&1 && return 0
    done
    PATH="$PATH:/sbin:/usr/sbin" ldconfig -p 2> /dev/null \
        | grep 'libclang[-.0-9]*\.so' > /dev/null
}
# The libclang.so fetched into the prefix on this run or an earlier one,
# or nothing.
fetched_libclang() { find "$prefix/libclang" -name libclang.so 2> /dev/null | head -1; }
# PyPI's libclang wheel, unpacked into the prefix with python3's standard
# library.
fetch_libclang() {
    say "no libclang here, which faba's htslib bindings need: fetching PyPI's libclang wheel into $prefix/libclang"
    python3 - "$prefix/libclang" << 'PY' || say "fetching libclang failed; faba will not build"
import io, json, platform, sys, urllib.request, zipfile
arch = platform.machine()
meta = json.load(urllib.request.urlopen("https://pypi.org/pypi/libclang/json"))
urls = [f["url"] for f in meta["urls"]
        if "manylinux" in f["filename"] and f["filename"].endswith(arch + ".whl")]
if not urls:
    sys.exit("no libclang wheel for " + arch)
url = urls[0]
zipfile.ZipFile(io.BytesIO(urllib.request.urlopen(url).read())).extractall(sys.argv[1])
PY
}
so=""
if [ "$needs_libclang" = 1 ] && [ "$libclang" = 1 ] && ! has_libclang; then
    [ -n "$(fetched_libclang)" ] || fetch_libclang
    so=$(fetched_libclang)
fi
if [ -n "$so" ]; then
    export LIBCLANG_PATH="${so%/*}"
    # The wheel has no clang built-in headers (stddef.h); gcc's serve.
    command -v gcc > /dev/null \
        && export BINDGEN_EXTRA_CLANG_ARGS="-I$(gcc -print-file-name=include)"
    say "libclang: $LIBCLANG_PATH"
fi

installed=""
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
    cargo "${args[@]}" "$name" &
    build=$!
    if wait "$build"; then
        installed="$installed $name"
    else
        failed="$failed $name"
        say "failed: $name"
    fi
    build=""
done

# Only the binaries this install replaced stay kept, so an install that
# replaced none takes none of the 3 places.
for f in "$keep"/*; do
    [ "$f" -ef "$prefix/bin/${f##*/}" ] && rm -f "$f"
done
rmdir "$keep" 2> /dev/null
kept=("$prefix"/install/2*/previous)
for ((i = 0; i < ${#kept[@]} - 3; i++)); do rm -rf "${kept[i]}"; done

{
    echo "node: $node"
    echo "glibc: $glibc"
    # The crates this job installed, each with its version and binaries,
    # from cargo's own record of the prefix.
    cargo install --list --root "$prefix" 2> /dev/null | awk -v names="$installed" '
        BEGIN { n = split(names, a, " "); for (i = 1; i <= n; i++) want[a[i]] = 1 }
        /^[^ ]/ { keep = ($1 in want) }
        keep'
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
mkdir -p "$run"
job > "$run/install.sbatch"
# Before sbatch: the job may start, and write "running", before it returns.
echo submitted > "$run/status"
jid=$(sbatch --parsable --output="$run/slurm.log" "$run/install.sbatch") || {
    echo "failed: sbatch refused the job" > "$run/status"
    die "sbatch refused the job; see above"
}
echo "$jid" > "$run/job_id"
cat <<EOF
Submitted job $jid: installing ${crates[*]} into $prefix
  log:     $run/slurm.log
  status:  $run/status   (submitted, running, then done, or failed ...)
  watch:   squeue -j $jid    or    tail -f $run/slurm.log
When it says done, add to your shell's startup file:
  export PATH="$prefix/bin:\$PATH"
EOF
