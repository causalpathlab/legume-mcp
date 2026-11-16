//! The machine a benchmark is planned on (`machine.json`): cores, memory,
//! GPUs and their driver, kernel, OS, and this legume's version and
//! features. The first plan writes it; a later one lists what differs. Its
//! memory and cores bound what the spec may book.

use super::spec::Spec;
use crate::command::refuse;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// What is known of this machine; `None` where it cannot say. Memory is in
/// GB (GiB, 2^30 bytes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub host: Option<String>,
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub cores_logical: u32,
    pub cores_physical: Option<u32>,
    pub mem_total_gb: f64,
    /// At plan time; not compared between plans.
    pub mem_available_gb: Option<f64>,
    pub gpus: Vec<String>,
    pub gpu_driver: Option<String>,
    pub legume: String,
    pub features: Vec<String>,
}

const GIB: f64 = (1u64 << 30) as f64;

/// GiB to one decimal.
fn gib(bytes: f64) -> f64 {
    (bytes / GIB * 10.0).round() / 10.0
}

/// The text of `path`, if it can be read.
fn read(path: impl AsRef<std::path::Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// This machine.
pub fn here() -> Machine {
    // SAFETY: sysconf only reads configuration.
    let (cpus, pages, page) = unsafe {
        (
            libc::sysconf(libc::_SC_NPROCESSORS_ONLN),
            libc::sysconf(libc::_SC_PHYS_PAGES),
            libc::sysconf(libc::_SC_PAGESIZE),
        )
    };
    let (host, kernel) = uname();
    let features = [
        ("benchmark", cfg!(feature = "benchmark")),
        ("clap", cfg!(feature = "clap")),
        ("pdf", cfg!(feature = "pdf")),
        ("sessions", cfg!(feature = "sessions")),
        ("tui", cfg!(feature = "tui")),
    ];
    Machine {
        host,
        os: os(),
        kernel,
        cores_logical: u32::try_from(cpus).unwrap_or(1).max(1),
        cores_physical: physical_cores(),
        mem_total_gb: gib(pages.max(0) as f64 * page.max(0) as f64),
        mem_available_gb: mem_available_gb(),
        gpus: gpus(),
        gpu_driver: gpu_driver(),
        legume: env!("CARGO_PKG_VERSION").to_string(),
        features: features
            .iter()
            .filter(|f| f.1)
            .map(|f| f.0.to_string())
            .collect(),
    }
}

/// This machine's host name, as the wrapper says where a task ran.
#[cfg(target_os = "linux")]
pub(crate) fn host() -> Option<String> {
    uname().0
}

/// The host name, and the kernel as `sysname release`.
fn uname() -> (Option<String>, Option<String>) {
    // SAFETY: an all-zero utsname is valid; uname fills it in.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `u` is a valid utsname to write to.
    if unsafe { libc::uname(&mut u) } != 0 {
        return (None, None);
    }
    let field = |f: &[libc::c_char]| {
        let bytes: Vec<u8> = f
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    (
        Some(field(&u.nodename)),
        Some(format!("{} {}", field(&u.sysname), field(&u.release))),
    )
}

/// The OS's PRETTY_NAME in `/etc/os-release`.
fn os() -> Option<String> {
    read("/etc/os-release")?
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
}

/// MemAvailable in `/proc/meminfo`, in GB.
fn mem_available_gb() -> Option<f64> {
    let kb: f64 = read("/proc/meminfo")?
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))?
        .trim()
        .trim_end_matches("kB")
        .trim()
        .parse()
        .ok()?;
    Some(gib(kb * 1024.0))
}

/// The first line of the NVIDIA driver's version file.
fn gpu_driver() -> Option<String> {
    read("/proc/driver/nvidia/version")?
        .lines()
        .next()
        .map(|l| l.trim().to_string())
}

/// Distinct (physical id, core id) pairs in `/proc/cpuinfo`.
fn physical_cores() -> Option<u32> {
    let text = read("/proc/cpuinfo")?;
    let mut cores = std::collections::BTreeSet::new();
    let mut package = None;
    for line in text.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        match k.trim() {
            "physical id" => package = Some(v.trim().to_string()),
            "core id" => {
                cores.insert((package.clone(), v.trim().to_string()));
            }
            _ => {}
        }
    }
    u32::try_from(cores.len()).ok().filter(|&n| n > 0)
}

/// Each NVIDIA GPU's model, by bus id.
fn gpus() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/proc/driver/nvidia/gpus") else {
        return Vec::new();
    };
    let mut found: Vec<(String, String)> = entries
        .flatten()
        .filter_map(|e| {
            let info = read(e.path().join("information"))?;
            let model = info
                .lines()
                .find_map(|l| l.strip_prefix("Model:"))
                .map_or("GPU", str::trim)
                .to_string();
            Some((e.file_name().to_string_lossy().into_owned(), model))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, m)| m).collect()
}

/// What differs from `old` in `new`, each a line `field: old -> new`.
pub fn drift(old: &Machine, new: &Machine) -> Vec<String> {
    let mut out = Vec::new();
    let mut diff = |what: &str, a: String, b: String| {
        if a != b {
            out.push(format!("{what}: {a} -> {b}"));
        }
    };
    let opt = |o: &Option<String>| o.clone().unwrap_or_else(|| "unknown".into());
    diff("host", opt(&old.host), opt(&new.host));
    diff("os", opt(&old.os), opt(&new.os));
    diff("kernel", opt(&old.kernel), opt(&new.kernel));
    diff(
        "cores",
        old.cores_logical.to_string(),
        new.cores_logical.to_string(),
    );
    diff(
        "physical cores",
        old.cores_physical
            .map_or("unknown".into(), |n| n.to_string()),
        new.cores_physical
            .map_or("unknown".into(), |n| n.to_string()),
    );
    diff(
        "memory",
        format!("{} GB", old.mem_total_gb),
        format!("{} GB", new.mem_total_gb),
    );
    diff("gpus", old.gpus.join(", "), new.gpus.join(", "));
    diff("gpu driver", opt(&old.gpu_driver), opt(&new.gpu_driver));
    diff("legume", old.legume.clone(), new.legume.clone());
    diff("features", old.features.join(","), new.features.join(","));
    out
}

/// What the spec books on `machine`: its cores and memory (GB), with what
/// to warn of. Refused: memory above MemTotal, a task's memory above the
/// benchmark's, a task's threads above its cores, a task's GPUs above
/// those listed.
pub fn fit(spec: &Spec, machine: &Machine) -> Result<(u32, f64, Vec<String>)> {
    let mut warnings = Vec::new();
    let cores = spec.cores.unwrap_or(machine.cores_logical);
    if cores > machine.cores_logical {
        warnings.push(format!(
            "cores: {cores} is more than this machine's {} logical CPUs",
            machine.cores_logical
        ));
    }
    if let Some(m) = spec.mem_gb.filter(|&m| m > machine.mem_total_gb) {
        return Err(refuse(format!(
            "mem_gb: {m} GB is more than this machine's MemTotal, {} GB",
            machine.mem_total_gb
        )));
    }
    let (mem, whose) = match spec.mem_gb {
        Some(m) => (m, "the benchmark's mem_gb".to_string()),
        None => {
            let m = machine.mem_available_gb.unwrap_or(machine.mem_total_gb);
            (
                m,
                format!("MemAvailable ({m} GB), the benchmark's mem_gb when it gives none"),
            )
        }
    };
    for (i, id) in spec.gpus.iter().enumerate() {
        if *id as usize >= machine.gpus.len() {
            warnings.push(format!(
                "gpus[{i}]: GPU {id} is not on this machine, which shows {}",
                machine.gpus.len()
            ));
        }
    }
    for method in spec.stages.iter().flat_map(|s| &s.methods) {
        if method.threads > cores {
            return Err(refuse(format!(
                "{}.threads: {} is more than the benchmark's cores, {cores}",
                method.at, method.threads
            )));
        }
        if method.mem_gb > mem {
            return Err(refuse(format!(
                "{}.mem_gb: {} GB is more than {whose}",
                method.at, method.mem_gb
            )));
        }
        if method.gpus as usize > spec.gpus.len() {
            return Err(refuse(format!(
                "{}.gpus: {} GPUs, and the benchmark lists {} (gpus: [ids])",
                method.at,
                method.gpus,
                spec.gpus.len()
            )));
        }
    }
    Ok((cores, mem, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmark::spec::parse;
    use crate::command::code;

    fn machine() -> Machine {
        Machine {
            host: Some("H1".into()),
            os: None,
            kernel: None,
            cores_logical: 8,
            cores_physical: Some(4),
            mem_total_gb: 16.0,
            mem_available_gb: Some(12.0),
            gpus: vec!["GPU1".into()],
            gpu_driver: None,
            legume: "0".into(),
            features: vec![],
        }
    }

    fn spec(top: &str, stage: &str) -> Spec {
        parse(&format!(
            r#"{{"name": "N1", "seeds": {{"n": 1}}, {top}
            "stages": [{{"name": "s", {stage} "run": ["T"], "makes": ["m.score.tsv"]}}]}}"#
        ))
        .unwrap()
        .0
    }

    fn refused(top: &str, stage: &str) -> String {
        let e = fit(&spec(top, stage), &machine()).unwrap_err();
        assert_eq!(code(&e), 3);
        e.to_string()
    }

    // mem_over_total_refused (the rule): memory above MemTotal, and a
    // task's above the benchmark's, are refused; without mem_gb the
    // benchmark books MemAvailable.
    #[test]
    fn mem_over_total_refused() {
        assert_eq!(
            refused(r#""mem_gb": 17,"#, ""),
            "mem_gb: 17 GB is more than this machine's MemTotal, 16 GB"
        );
        assert_eq!(
            refused(r#""mem_gb": 10,"#, r#""mem_gb": 11,"#),
            "stages[0].mem_gb: 11 GB is more than the benchmark's mem_gb"
        );
        assert_eq!(
            refused("", r#""mem_gb": 13,"#),
            "stages[0].mem_gb: 13 GB is more than MemAvailable (12 GB), the benchmark's \
             mem_gb when it gives none"
        );
        let (cores, mem, w) = fit(&spec("", r#""mem_gb": 12,"#), &machine()).unwrap();
        assert_eq!((cores, mem), (8, 12.0));
        assert!(w.is_empty());
        assert_eq!(
            fit(&spec(r#""mem_gb": 16,"#, ""), &machine()).unwrap().1,
            16.0
        );
    }

    #[test]
    fn threads_and_gpus_fit_what_the_benchmark_books() {
        assert_eq!(
            refused(r#""cores": 4,"#, r#""threads": 5,"#),
            "stages[0].threads: 5 is more than the benchmark's cores, 4"
        );
        assert_eq!(
            refused(r#""gpus": [0],"#, r#""gpus": 2,"#),
            "stages[0].gpus: 2 GPUs, and the benchmark lists 1 (gpus: [ids])"
        );
        let (_, _, w) = fit(&spec(r#""gpus": [0, 1], "cores": 9,"#, ""), &machine()).unwrap();
        assert_eq!(
            w,
            [
                "cores: 9 is more than this machine's 8 logical CPUs",
                "gpus[1]: GPU 1 is not on this machine, which shows 1"
            ]
        );
    }

    #[test]
    fn drift_lists_what_changed_but_not_free_memory() {
        let a = machine();
        let mut b = a.clone();
        b.mem_available_gb = Some(1.0);
        assert!(drift(&a, &b).is_empty());
        b.cores_logical = 4;
        b.legume = "1".into();
        assert_eq!(drift(&a, &b), ["cores: 8 -> 4", "legume: 0 -> 1"]);
    }

    #[test]
    fn here_sees_cores_memory_and_this_build() {
        let here = here();
        assert!(
            here.cores_logical >= 1 && here.mem_total_gb > 0.0,
            "{here:?}"
        );
        assert!(here.features.contains(&"benchmark".to_string()));
    }
}
