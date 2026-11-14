//! `legume universe cis`: every gene's cis candidate peaks, by chickpea's
//! rule, so a metric counts the pairs a pruned links table leaves out.

mod common;

use common::{legume, legume_fed, tsv};
use flate2::read::MultiGzDecoder;
use legume_mcp::universe::{chr_stripped, cis_pairs, parse_peak, Gene, Pair, Peak};
use std::io::Read;
use std::path::Path;

fn gene(name: &str, chr: &str, tss: i64) -> Gene {
    Gene {
        name: name.into(),
        chr: chr.into(),
        tss,
    }
}

fn peaks(names: &[&str]) -> Vec<Option<Peak>> {
    names.iter().map(|n| parse_peak(n)).collect()
}

/// The pairs as (gene, peak, distance).
fn triples(pairs: &[Pair]) -> Vec<(usize, usize, i64)> {
    pairs.iter().map(|p| (p.gene, p.peak, p.distance)).collect()
}

#[test]
fn chr_is_stripped_in_any_case_and_only_once() {
    assert_eq!(chr_stripped("chr1"), "1");
    assert_eq!(chr_stripped("CHRX"), "X");
    assert_eq!(chr_stripped("Chr2"), "2");
    assert_eq!(chr_stripped("chrchr3"), "chr3");
    assert_eq!(chr_stripped("ch"), "ch");
    assert_eq!(chr_stripped("chr"), "");
    assert_eq!(chr_stripped("1"), "1");
}

#[test]
fn a_peak_name_parses_as_chickpea_reads_it() {
    let p = parse_peak("chr1:10-20").unwrap();
    assert_eq!((p.chr.as_str(), p.start, p.end), ("chr1", 10, 20));
    let p = parse_peak("X:010-20").unwrap();
    assert_eq!((p.chr.as_str(), p.start, p.end), ("X", 10, 20));
    for bad in [
        "PEAK1",
        "chr1:10-10",
        "chr1:20-10",
        "chr1:+10-20",
        "chr1:10-+20",
        "chr1:10",
        "chr1:10-20-30",
        "chr:10-20",
        ":10-20",
        "chr 1:10-20",
        "chr1:10_20",
        "chr1-10-20",
    ] {
        assert!(parse_peak(bad).is_none(), "{bad} parsed");
    }
}

// Window 100 around GENE1 (chr1, TSS 1000): midpoints 900 and 1100 are on
// the bounds, and in; 1101 and 899 are out. Peak 1's midpoint is 1100 by
// integer division (1100.5 as a float, which is out). GENE2 sits on "1"
// and matches "CHR1"; GENE3 on chrX matches "X" but not "x". Unparsed
// names keep their index.
#[test]
fn the_cis_universe_matches_chickpeas_rule() {
    let genes = [
        gene("GENE1", "chr1", 1000),
        gene("GENE2", "1", 5000),
        gene("GENE3", "chrX", 1000),
    ];
    let peaks = peaks(&[
        "chr1:899-901",    // 0: 900, d 100
        "chr1:1100-1101",  // 1: 1100, d 100
        "chr1:898-900",    // 2: 899, d 101
        "chr1:1101-1102",  // 3: 1101, d 101
        "PEAK1",           // 4: unparsed
        "chr1:5000-5000",  // 5: unparsed (empty)
        "CHR1:4990-5010",  // 6: 5000, GENE2 at d 0
        "X:1000-1010",     // 7: 1005, GENE3 at d 5
        "x:1000-1010",     // 8: another chromosome
        "chr10:1000-1010", // 9: another chromosome
        "chr1:+990-1010",  // 10: unparsed (a sign)
        "chr1:1000",       // 11: unparsed (a position)
        "chr1:950-1050",   // 12: 1000, d 0
    ]);
    let pairs = cis_pairs(&genes, &peaks, 100, None);
    assert_eq!(
        triples(&pairs),
        [(0, 0, 100), (0, 1, 100), (0, 12, 0), (1, 6, 0), (2, 7, 5)]
    );
}

// Around GENE1 (chr1, TSS 1000): peak 0 at 1050 and peak 1 at 950 are both
// 50 away, peak 2 at 995 is 5 and peak 3 at 1020 is 20. The nearest K by
// (distance, index), then in index order.
#[test]
fn the_nearest_k_break_a_distance_tie_by_peak_index() {
    let genes = [gene("GENE1", "chr1", 1000)];
    let peaks = peaks(&[
        "chr1:1040-1060",
        "chr1:940-960",
        "chr1:990-1000",
        "chr1:1010-1030",
    ]);
    let kept = |k: usize| -> Vec<usize> {
        cis_pairs(&genes, &peaks, 100, Some(k))
            .iter()
            .map(|p| p.peak)
            .collect()
    };
    assert_eq!(kept(1), [2]);
    assert_eq!(kept(2), [2, 3]);
    assert_eq!(kept(3), [0, 2, 3]);
    assert_eq!(kept(4), [0, 1, 2, 3]);
    assert_eq!(kept(9), [0, 1, 2, 3]);
    assert_eq!(cis_pairs(&genes, &peaks, 100, None).len(), 4);
}

fn gunzip(path: &Path) -> String {
    let mut text = String::new();
    MultiGzDecoder::new(std::fs::File::open(path).unwrap())
        .read_to_string(&mut text)
        .unwrap();
    text
}

// The simulator's files: gene coordinates (gene, chr, tss, with a header)
// and the peak names, one a line. A short line is skipped, a gene listed
// twice takes its last position, as chickpea reads them.
#[test]
fn universe_cis_writes_gene_peak_distance_and_says_how_many() {
    let dir = tempfile::tempdir().unwrap();
    let genes = dir.path().join("sim.gene_coords.tsv.gz");
    let peaks = dir.path().join("sim.peak_names.txt");
    let out = dir.path().join("cis.tsv.gz");
    tsv(
        &genes,
        "gene\tchr\ttss",
        &[
            "GENE1\tchr1\t9000",
            "GENE2\tchr2",
            "GENE3\tchr2\t1000",
            "GENE1\tchr1\t1000",
        ],
    );
    let names = [
        "chr1:1040-1060",
        "chr1:940-960",
        "chr1:990-1000",
        "chr2:1000-1010",
        "chr1:9000-9010",
    ];
    std::fs::write(&peaks, names.join("\n") + "\n").unwrap();
    let o = legume([
        "universe",
        "cis",
        "--gene-coords",
        genes.to_str().unwrap(),
        "--peaks",
        peaks.to_str().unwrap(),
        "--window",
        "100",
        "--max-per-gene",
        "2",
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let text = gunzip(&out);
    let want = format!(
        "gene\tpeak\tdistance\n\
         GENE1\t{}\t50\n\
         GENE1\t{}\t5\n\
         GENE3\t{}\t5\n",
        names[0], names[2], names[3]
    );
    assert_eq!(text, want);
    let said = String::from_utf8_lossy(&o.stdout);
    for line in ["rows\t3", "genes\t2", "max_per_gene\t2"] {
        assert!(said.lines().any(|l| l == line), "{line} not in\n{said}");
    }
}

// A file given as a pipe (`<(...)`, `/dev/stdin`) is read from its first
// byte. Losing the bytes looked at to tell gzip would leave the first peak
// "r1:990-1010", still a name that parses, and one row of two.
#[test]
fn a_piped_gene_or_peak_file_is_read_whole() {
    let dir = tempfile::tempdir().unwrap();
    let genes = dir.path().join("sim.gene_coords.tsv.gz");
    let peaks = dir.path().join("sim.peak_names.txt");
    let out = dir.path().join("cis.tsv");
    tsv(&genes, "gene\tchr\ttss", &["GENE1\tchr1\t1000"]);
    std::fs::write(&peaks, "chr1:990-1010\nchr1:995-1005\n").unwrap();
    let args = [
        "universe",
        "cis",
        "--gene-coords",
        genes.to_str().unwrap(),
        "--peaks",
        peaks.to_str().unwrap(),
        "--window",
        "100",
        "-o",
        out.to_str().unwrap(),
    ];
    for piped in [&genes, &peaks] {
        let o = legume_fed(args, piped, std::fs::read(piped).unwrap());
        let e = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "{} piped: {e}", piped.display());
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            "gene\tpeak\tdistance\nGENE1\tchr1:990-1010\t0\nGENE1\tchr1:995-1005\t0\n",
            "{} piped",
            piped.display()
        );
    }
}

#[test]
fn universe_cis_needs_a_window() {
    let dir = tempfile::tempdir().unwrap();
    let genes = dir.path().join("g.tsv");
    let peaks = dir.path().join("p.txt");
    tsv(&genes, "gene\tchr\ttss", &["GENE1\tchr1\t1000"]);
    std::fs::write(&peaks, "chr1:990-1010\n").unwrap();
    let out = dir.path().join("cis.tsv.gz");
    let o = legume([
        "universe",
        "cis",
        "--gene-coords",
        genes.to_str().unwrap(),
        "--peaks",
        peaks.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("--window"));
    assert!(!out.exists());
}

#[test]
fn universe_help_names_every_flag() {
    let o = legume(["universe", "--help"]);
    assert!(o.status.success());
    let help = String::from_utf8_lossy(&o.stdout);
    for flag in [
        "cis",
        "--gene-coords",
        "--peaks",
        "--window",
        "--max-per-gene",
    ] {
        assert!(help.contains(flag), "{flag} missing from\n{help}");
    }
}
