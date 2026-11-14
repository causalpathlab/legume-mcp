//! `legume universe cis`: the universe of a peak-to-gene benchmark, every
//! gene's cis candidate peaks, by chickpea's rule (its `build_cis_pairs`,
//! with `chr_stripped` and the peak-name parse of legume-genomic-types).
//! A method's links table is pruned, so it is never the universe: the
//! pairs it leaves out still count, unscored, at the bottom.
//!
//! The rule: a peak is a gene's candidate when it is on the gene's
//! chromosome (a `chr` prefix, in any case, dropped from both) and its
//! midpoint `(start + end) / 2`, by integer division, is within `window`
//! of the TSS, bounds included. With `--max-per-gene K`, a gene keeps its
//! K nearest by (distance, peak index), listed by peak index.

use crate::command::{flag_pairs, required, usage};
use crate::table;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// `legume universe`'s usage.
pub const USAGE: &str = "\
legume universe cis --gene-coords F --peaks F --window W [--max-per-gene K] -o F.tsv.gz

Every gene's cis candidate peaks, by chickpea's rule, as the universe of a
peak-to-gene benchmark: a table gene, peak, distance.

  --gene-coords F    gene, chr, tss with a header (the simulator's
                     gene_coords.tsv.gz); a gene listed twice takes its last
  --peaks F          peak names chr:start-end, one a line (the simulator's
                     peak_names.txt); a name that is no interval is no
                     gene's candidate but keeps its place
  --window W         bp: a peak whose midpoint (start + end) / 2 is within W
                     of the TSS, bounds included, on the same chromosome
                     (chr1 and 1 are one)
  --max-per-gene K   keep each gene's K nearest, ties to the earlier peak
                     (chickpea's --max-cis); all of them without it
  -o F               the table, gzipped when F ends in .gz

It says how many rows and genes it wrote, and max_per_gene, the most
candidates any gene has: give chickpea a --max-cis at least that.";

/// A gene's TSS.
#[derive(Clone, Debug, PartialEq)]
pub struct Gene {
    pub name: String,
    pub chr: String,
    pub tss: i64,
}

/// A peak's interval, its chromosome as written.
#[derive(Clone, Debug, PartialEq)]
pub struct Peak {
    pub chr: String,
    pub start: i64,
    pub end: i64,
}

/// A candidate: indices into the genes and the peaks, and the distance
/// `|midpoint - tss|`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pair {
    pub gene: usize,
    pub peak: usize,
    pub distance: i64,
}

/// A chromosome name without its `chr` prefix, in any case; the rest keeps
/// its case.
pub fn chr_stripped(s: &str) -> &str {
    match s.get(..3) {
        Some(p) if p.eq_ignore_ascii_case("chr") => &s[3..],
        _ => s,
    }
}

/// A coordinate: plain digits, no sign.
fn plain_number(s: &str) -> Option<i64> {
    s.parse::<i64>()
        .ok()
        .filter(|_| s.bytes().all(|b| b.is_ascii_digit()))
}

/// A peak name `chr:start-end`, as chickpea reads it: plain numbers, `end >
/// start`, a chromosome with no whitespace and something left after `chr`.
/// `None` for any other name (a single position too).
pub fn parse_peak(name: &str) -> Option<Peak> {
    let (chr, range) = name.split_once(':')?;
    let (start, end) = range.split_once('-')?;
    let (start, end) = (plain_number(start)?, plain_number(end)?);
    let chr_ok = !chr_stripped(chr).is_empty() && !chr.contains(char::is_whitespace);
    (chr_ok && end > start).then(|| Peak {
        chr: chr.into(),
        start,
        end,
    })
}

fn midpoint(p: &Peak) -> i64 {
    (p.start + p.end) / 2
}

/// Every gene's candidates, gene by gene, each gene's by peak index.
pub fn cis_pairs(
    genes: &[Gene],
    peaks: &[Option<Peak>],
    window: i64,
    max_per_gene: Option<usize>,
) -> Vec<Pair> {
    // Peaks by chromosome, sorted by midpoint.
    let mut by_chr: HashMap<&str, Vec<(i64, usize)>> = HashMap::new();
    for (i, p) in peaks.iter().enumerate() {
        if let Some(p) = p {
            by_chr
                .entry(chr_stripped(&p.chr))
                .or_default()
                .push((midpoint(p), i));
        }
    }
    for v in by_chr.values_mut() {
        v.sort_unstable();
    }
    let mut pairs = Vec::new();
    let mut cand: Vec<(i64, usize)> = Vec::new();
    for (g, gene) in genes.iter().enumerate() {
        let Some(sorted) = by_chr.get(chr_stripped(&gene.chr)) else {
            continue;
        };
        let lo = sorted.partition_point(|&(m, _)| m < gene.tss - window);
        let hi = sorted.partition_point(|&(m, _)| m <= gene.tss + window);
        cand.clear();
        cand.extend(
            sorted[lo..hi]
                .iter()
                .map(|&(m, i)| ((m - gene.tss).abs(), i)),
        );
        if let Some(k) = max_per_gene.filter(|&k| cand.len() > k) {
            cand.sort_unstable();
            cand.truncate(k);
        }
        cand.sort_unstable_by_key(|&(_, i)| i);
        pairs.extend(cand.iter().map(|&(distance, peak)| Pair {
            gene: g,
            peak,
            distance,
        }));
    }
    pairs
}

/// Gene positions from a table gene, chr, tss with a header, read as
/// chickpea reads them: by position, the header skipped, a line short of
/// three fields skipped, a gene listed twice at its last position (in the
/// place it was first listed).
fn read_genes(path: &Path) -> Result<Vec<Gene>> {
    let mut genes: Vec<Gene> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for (i, line) in table::open_text(path)?.lines().enumerate().skip(1) {
        let line = line?;
        let fields: Vec<&str> = line.trim_end_matches('\r').split('\t').collect();
        if fields.len() < 3 {
            continue;
        }
        let tss = fields[2].parse().with_context(|| {
            format!("line {} of {}: tss {:?}", i + 1, path.display(), fields[2])
        })?;
        let gene = Gene {
            name: fields[0].into(),
            chr: fields[1].into(),
            tss,
        };
        match at.get(fields[0]) {
            Some(&g) => genes[g] = gene,
            None => {
                at.insert(fields[0].into(), genes.len());
                genes.push(gene);
            }
        }
    }
    Ok(genes)
}

/// Peak names, one a line; blank lines are skipped.
fn read_peak_names(path: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for line in table::open_text(path)?.lines() {
        let line = line?;
        let name = line.trim_end_matches('\r');
        if !name.is_empty() {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

/// A `legume universe cis` command line, checked.
struct Spec {
    genes: PathBuf,
    peaks: PathBuf,
    window: i64,
    max_per_gene: Option<usize>,
    out: PathBuf,
}

fn parse(args: &[String]) -> Result<Option<Spec>> {
    match args.first().map(String::as_str) {
        Some("cis") => {}
        None | Some("-h" | "--help") => return Ok(None),
        Some(other) => return Err(usage(format!("unknown universe {other:?}"))),
    }
    let (mut genes, mut peaks, mut window, mut max_per_gene, mut out) =
        (None, None, None, None, None);
    let Some(pairs) = flag_pairs(&args[1..])? else {
        return Ok(None);
    };
    for (flag, v) in pairs {
        match flag {
            "--gene-coords" => genes = Some(PathBuf::from(v)),
            "--peaks" => peaks = Some(PathBuf::from(v)),
            "--window" => match v.parse::<i64>() {
                Ok(w) if w >= 0 => window = Some(w),
                _ => return Err(usage(format!("--window {v:?} is not a count of bp"))),
            },
            "--max-per-gene" => match v.parse::<usize>() {
                Ok(k) if k > 0 => max_per_gene = Some(k),
                _ => return Err(usage(format!("--max-per-gene {v:?} is not a count"))),
            },
            "-o" | "--out" => out = Some(PathBuf::from(v)),
            _ => return Err(usage(format!("unknown argument {flag}"))),
        }
    }
    Ok(Some(Spec {
        genes: required(genes, "--gene-coords")?,
        peaks: required(peaks, "--peaks")?,
        window: required(window, "--window")?,
        max_per_gene,
        out: required(out, "-o")?,
    }))
}

/// `legume universe` with `args` (those after `universe`), saying what it
/// wrote on `out`. A refusal is a [`crate::command::Exit`] with its exit code.
pub fn main(args: &[String], out: &mut dyn Write) -> Result<()> {
    let Some(spec) = parse(args)? else {
        writeln!(out, "{USAGE}")?;
        return Ok(());
    };
    let genes = read_genes(&spec.genes)?;
    let names = read_peak_names(&spec.peaks)?;
    let peaks: Vec<Option<Peak>> = names.iter().map(|n| parse_peak(n)).collect();
    let pairs = cis_pairs(&genes, &peaks, spec.window, spec.max_per_gene);

    table::write_text(&spec.out, |w| {
        writeln!(w, "gene\tpeak\tdistance")?;
        for p in &pairs {
            writeln!(
                w,
                "{}\t{}\t{}",
                genes[p.gene].name, names[p.peak], p.distance
            )?;
        }
        Ok(())
    })?;

    let mut per_gene = vec![0usize; genes.len()];
    for p in &pairs {
        per_gene[p.gene] += 1;
    }
    let unparsed = peaks.iter().filter(|p| p.is_none()).count();
    writeln!(out, "rows\t{}", pairs.len())?;
    writeln!(
        out,
        "genes\t{}",
        per_gene.iter().filter(|&&n| n > 0).count()
    )?;
    writeln!(out, "max_per_gene\t{}", per_gene.iter().max().unwrap_or(&0))?;
    writeln!(
        out,
        "of {} genes and {} peaks ({unparsed} not intervals); wrote {}",
        genes.len(),
        peaks.len(),
        spec.out.display()
    )?;
    Ok(())
}
