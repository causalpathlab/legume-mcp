//! `legume score`, run as the binary: a method's scores against a truth
//! over a universe, from TSV, gzipped TSV and parquet, and the refusals
//! that keep a metric honest (no universe, truth outside it, a duplicate
//! key, a p value out of range).

mod common;

use common::{legume, legume_fed, tsv};
use parquet::basic::{Compression, ZstdLevel};
use parquet::data_type::{ByteArray, ByteArrayType, DoubleType, FloatType, Int32Type, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::parser::parse_message_type;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The definition levels of an OPTIONAL column: 0 for a null.
fn levels<T>(v: &[Option<T>]) -> Vec<i16> {
    v.iter().map(|x| i16::from(x.is_some())).collect()
}

/// One column of a parquet fixture.
enum Col {
    Str(Vec<String>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
    /// OPTIONAL, `None` a null.
    OptStr(Vec<Option<String>>),
    OptF64(Vec<Option<f64>>),
}

/// A parquet file of the columns `columns` in one row group, REQUIRED as
/// the tools' writers make them unless optional.
fn parquet(path: &Path, columns: &[(&str, Col)], compression: Compression) {
    let fields: Vec<String> = columns
        .iter()
        .map(|(name, c)| {
            let (ty, how) = match c {
                Col::Str(_) => ("BYTE_ARRAY", "REQUIRED"),
                Col::I32(_) => ("INT32", "REQUIRED"),
                Col::I64(_) => ("INT64", "REQUIRED"),
                Col::F32(_) => ("FLOAT", "REQUIRED"),
                Col::F64(_) => ("DOUBLE", "REQUIRED"),
                Col::OptStr(_) => ("BYTE_ARRAY", "OPTIONAL"),
                Col::OptF64(_) => ("DOUBLE", "OPTIONAL"),
            };
            let utf8 = if ty == "BYTE_ARRAY" { " (UTF8)" } else { "" };
            format!("{how} {ty} {name}{utf8};")
        })
        .collect();
    let message = format!("message table {{ {} }}", fields.join(" "));
    let schema = Arc::new(parse_message_type(&message).unwrap());
    let props = Arc::new(
        WriterProperties::builder()
            .set_compression(compression)
            .build(),
    );
    let file = std::fs::File::create(path).unwrap();
    let mut w = SerializedFileWriter::new(file, schema, props).unwrap();
    let mut rg = w.next_row_group().unwrap();
    for (_, c) in columns {
        let mut col = rg.next_column().unwrap().unwrap();
        match c {
            Col::Str(v) => {
                let v: Vec<ByteArray> = v.iter().map(|s| ByteArray::from(s.as_str())).collect();
                col.typed::<ByteArrayType>()
                    .write_batch(&v, None, None)
                    .unwrap();
            }
            Col::I32(v) => {
                col.typed::<Int32Type>().write_batch(v, None, None).unwrap();
            }
            Col::I64(v) => {
                col.typed::<Int64Type>().write_batch(v, None, None).unwrap();
            }
            Col::F32(v) => {
                col.typed::<FloatType>().write_batch(v, None, None).unwrap();
            }
            Col::F64(v) => {
                col.typed::<DoubleType>()
                    .write_batch(v, None, None)
                    .unwrap();
            }
            Col::OptStr(v) => {
                let def = levels(v);
                let v: Vec<ByteArray> = v.iter().flatten().map(|s| s.as_str().into()).collect();
                col.typed::<ByteArrayType>()
                    .write_batch(&v, Some(&def), None)
                    .unwrap();
            }
            Col::OptF64(v) => {
                let def = levels(v);
                let v: Vec<f64> = v.iter().flatten().copied().collect();
                col.typed::<DoubleType>()
                    .write_batch(&v, Some(&def), None)
                    .unwrap();
            }
        }
        col.close().unwrap();
    }
    rg.close().unwrap();
    w.close().unwrap();
}

/// A row of the output: the score, the metric, and each column by name.
struct Row(std::collections::HashMap<String, String>);

impl Row {
    fn get(&self, col: &str) -> &str {
        self.0.get(col).unwrap_or_else(|| panic!("no column {col}"))
    }
    fn num(&self, col: &str) -> f64 {
        self.get(col).parse().unwrap()
    }
}

const HEADER: &str = "score\tmetric\tvalue\tpart\tn_pos\tn_neg\tn_universe\tpos_rate\t\
                      tied_share\tunscored\tunscored_pos\toutside_pos\toutside_scored\tn_inf";

/// The output's rows, its header checked against the contract.
fn rows(path: &Path) -> Vec<Row> {
    let text = std::fs::read_to_string(path).unwrap();
    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap().split('\t').collect();
    assert_eq!(header.join("\t"), HEADER);
    lines
        .map(|l| {
            let cells = l.split('\t').map(str::to_string);
            Row(header.iter().map(|h| h.to_string()).zip(cells).collect())
        })
        .collect()
}

/// The row of `score`'s `metric`.
fn row<'a>(rows: &'a [Row], score: &str, metric: &str) -> &'a Row {
    rows.iter()
        .find(|r| r.get("score") == score && r.get("metric") == metric)
        .unwrap_or_else(|| panic!("no row {score} {metric}"))
}

fn near(got: f64, want: f64) {
    assert!((got - want).abs() <= 1e-12, "got {got}, want {want}");
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}

/// A universe of 1000 pairs (GENE{g}, PEAK{p}) with 10 truth pairs, of
/// which a method scores only two, both true.
struct FewLinks {
    _dir: tempfile::TempDir,
    universe: PathBuf,
    truth: PathBuf,
    scores: PathBuf,
    out: PathBuf,
}

fn pair(i: usize) -> String {
    format!("GENE{}\tPEAK{i}", i / 10)
}

/// The truth of the few-links case: ten pairs, one in each hundred.
fn truth_pairs() -> Vec<String> {
    (0..10).map(|k| pair(k * 100 + 7)).collect()
}

/// The scored rows of the few-links case: two of its true pairs.
fn scored_pairs() -> Vec<String> {
    vec![format!("{}\t0.9", pair(7)), format!("{}\t0.8", pair(107))]
}

fn few_links() -> FewLinks {
    let dir = tempfile::tempdir().unwrap();
    let universe = dir.path().join("universe.tsv.gz");
    let truth = dir.path().join("truth.tsv.gz");
    let scores = dir.path().join("links.tsv");
    let out = dir.path().join("M1.score.tsv");
    tsv(
        &universe,
        "gene\tpeak",
        &(0..1000).map(pair).collect::<Vec<_>>(),
    );
    tsv(&truth, "gene\tpeak", &truth_pairs());
    tsv(&scores, "gene\tpeak\tscore", &scored_pairs());
    FewLinks {
        _dir: dir,
        universe,
        truth,
        scores,
        out,
    }
}

impl FewLinks {
    /// The arguments of a run over this case, `--universe` and its key left
    /// to the caller.
    fn args<'a>(&'a self, more: &[&'a str]) -> Vec<&'a str> {
        let mut a = vec![
            "score",
            "--scores",
            s(&self.scores),
            "--key",
            "gene,peak",
            "--score",
            "M1=score",
            "--truth",
            s(&self.truth),
            "--truth-key",
            "gene,peak",
            "-o",
            s(&self.out),
        ];
        a.extend_from_slice(more);
        a
    }

    fn universe_args(&self) -> [&str; 4] {
        [
            "--universe",
            s(&self.universe),
            "--universe-key",
            "gene,peak",
        ]
    }
}

// Two scored pairs, both true, then the unscored block: 998 items, 8 of
// them true. AP = 0.1 + 0.1 + 0.8 x 10/1000 = 0.208. AUROC: the two beat
// all 990 negatives, the 8 tie with them: (2 + 8/2) / 10 = 0.6.
#[test]
fn few_links_ap_is_0_208_through_the_cli() {
    let c = few_links();
    let o = legume(c.args(&c.universe_args()));
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&c.out);
    assert_eq!(rows.len(), 2);
    let ap = row(&rows, "M1", "auprc");
    near(ap.num("value"), 0.208);
    near(row(&rows, "M1", "auroc").num("value"), 0.6);
    for r in &rows {
        assert_eq!(r.get("part"), "all");
        assert_eq!(r.get("n_pos"), "10");
        assert_eq!(r.get("n_neg"), "990");
        assert_eq!(r.get("n_universe"), "1000");
        near(r.num("pos_rate"), 0.01);
        assert_eq!(r.get("unscored"), "998");
        assert_eq!(r.get("unscored_pos"), "8");
        assert_eq!(r.get("outside_pos"), "0");
        assert_eq!(r.get("outside_scored"), "0");
        assert_eq!(r.get("n_inf"), "0");
        near(r.num("tied_share"), 0.0);
    }
}

#[test]
fn the_universe_flag_is_required() {
    let c = few_links();
    let o = legume(c.args(&[]));
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(
        stderr(&o).contains("--universe is required"),
        "{}",
        stderr(&o)
    );
    assert!(!c.out.exists());
}

#[test]
fn universe_same_needs_the_truth_and_the_scores_in_one_file() {
    let c = few_links();
    let o = legume(c.args(&["--universe", "same"]));
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("same"), "{}", stderr(&o));
    assert!(!c.out.exists());
}

#[test]
fn truth_outside_the_universe_exits_3_with_a_count_or_is_dropped_and_counted() {
    let c = few_links();
    // Two more truth pairs, outside the universe, one listed twice.
    let mut pos = truth_pairs();
    pos.push("GENE1000\tPEAK10000".into());
    pos.push("GENE1001\tPEAK10010".into());
    pos.push("GENE1000\tPEAK10000".into());
    tsv(&c.truth, "gene\tpeak", &pos);
    let o = legume(c.args(&c.universe_args()));
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    assert!(stderr(&o).contains("2 truth"), "{}", stderr(&o));
    assert!(!c.out.exists());

    let mut a = c.args(&c.universe_args());
    a.extend(["--truth-outside", "drop"]);
    let o = legume(&a);
    assert!(o.status.success(), "{}", stderr(&o));
    let said = String::from_utf8_lossy(&o.stdout);
    let truth = "truth: 13 rows, 12 keys, 10 positive in the universe, 2 outside it";
    assert!(said.contains(truth), "{said}");
    let rows = rows(&c.out);
    let ap = row(&rows, "M1", "auprc");
    assert_eq!(ap.get("outside_pos"), "2");
    assert_eq!(ap.get("n_pos"), "10");
    near(ap.num("value"), 0.208);
}

#[test]
fn scored_rows_outside_the_universe_are_dropped_and_counted() {
    let c = few_links();
    let mut scored = scored_pairs();
    scored.extend(
        [
            "GENE2000\tPEAK1\t5",
            "GENE2001\tPEAK2\t5",
            "GENE2002\tPEAK3\tNaN",
            // Outside, so never read as a score.
            "GENE2003\tPEAK4\tnot-a-score",
        ]
        .map(String::from),
    );
    tsv(&c.scores, "gene\tpeak\tscore", &scored);
    let o = legume(c.args(&c.universe_args()));
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&c.out);
    let ap = row(&rows, "M1", "auprc");
    assert_eq!(ap.get("outside_scored"), "4");
    assert_eq!(ap.get("n_universe"), "1000");
    near(ap.num("value"), 0.208);

    let mut a = c.args(&c.universe_args());
    a.extend(["--scored-outside", "fail"]);
    std::fs::remove_file(&c.out).unwrap();
    let o = legume(&a);
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    assert!(stderr(&o).contains("4 scored rows"), "{}", stderr(&o));
    assert!(!c.out.exists());
}

/// The output a run of [`keyed_args`] writes in its folder.
const OUT: &str = "out.score.tsv";

/// The arguments of a `--universe same` run over the one file `one`, keyed
/// `key`, its truth the column `planted` above 0.5, scoring each of
/// `scores` (`NAME=COLUMN`) into `out`.
fn same_args<'a>(one: &'a Path, key: &'a str, scores: &[&'a str], out: &'a Path) -> Vec<&'a str> {
    let mut a = vec!["score", "--scores", s(one), "--key", key];
    for sc in scores {
        a.extend(["--score", sc]);
    }
    a.extend([
        "--truth",
        s(one),
        "--truth-key",
        key,
        "--label",
        "planted",
        "--above",
        "0.5",
        "--universe",
        "same",
        "-o",
        s(out),
    ]);
    a
}

/// The arguments of a run over `scores.tsv`, `truth.tsv` and `universe.tsv`
/// in `dir`, each keyed by its column `key`, scoring each of `score`
/// (`NAME=COLUMN`), into `out.score.tsv`.
fn keyed_args(dir: &Path, score: &[&str]) -> Vec<String> {
    keyed_args_of(dir, "scores.tsv", score)
}

/// [`keyed_args`] with the scores read from the file `scores` in `dir`.
fn keyed_args_of(dir: &Path, scores: &str, score: &[&str]) -> Vec<String> {
    let at = |f: &str| dir.join(f).to_str().unwrap().to_string();
    let mut a: Vec<String> = ["score", "--scores", &at(scores), "--key", "key"]
        .map(String::from)
        .to_vec();
    for sc in score {
        a.extend(["--score".to_string(), sc.to_string()]);
    }
    a.extend(
        [
            "--truth",
            &at("truth.tsv"),
            "--truth-key",
            "key",
            "--universe",
            &at("universe.tsv"),
            "--universe-key",
            "key",
            "-o",
            &at(OUT),
        ]
        .map(String::from),
    );
    a
}

// Items K1 (true), K2, K3; K1 and K2 each scored twice, K3 once more as
// NaN. With the max of each key, K1 0.9, K2 0.95, K3 0.2: K1 beats K3 and
// loses to K2, AUROC 1/2. The first rows would give 1, the last or the min
// 0.
#[test]
fn a_duplicate_score_key_fails_and_dup_max_keeps_the_max() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    tsv(&d.join("universe.tsv"), "key", &["K1", "K2", "K3"]);
    tsv(&d.join("truth.tsv"), "key", &["K1"]);
    let scores = [
        "K1\t0.9", "K2\t0.15", "K3\t0.2", "K1\t0.1", "K2\t0.95", "K3\tNaN",
    ];
    tsv(&d.join("scores.tsv"), "key\tv", &scores);
    let out = d.join(OUT);
    let mut a = keyed_args(d, &["M1=v"]);
    let o = legume(&a);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("duplicate"), "{}", stderr(&o));
    assert!(!out.exists());

    a.extend(["--dup", "max"].map(String::from));
    let o = legume(&a);
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&out);
    let roc = row(&rows, "M1", "auroc");
    near(roc.num("value"), 0.5);
    assert_eq!(roc.get("unscored"), "0");
}

// Weights 0.5, 1 and 1.5 above 1: only the last is a positive. K9, outside
// the universe, is a labelled negative, so no truth is outside it.
#[test]
fn above_is_strict() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    tsv(&d.join("universe.tsv"), "key", &["K1", "K2", "K3", "K4"]);
    tsv(
        &d.join("truth.tsv"),
        "key\tw",
        &["K1\t0.5", "K2\t1", "K3\t1.5", "K9\t0.5"],
    );
    tsv(
        &d.join("scores.tsv"),
        "key\tv",
        &["K1\t1", "K2\t2", "K3\t3", "K4\t4"],
    );
    let mut a = keyed_args(d, &["M1=v"]);
    a.extend(["--label", "w", "--above", "1"].map(String::from));
    let o = legume(&a);
    assert!(o.status.success(), "{}", stderr(&o));
    let said = String::from_utf8_lossy(&o.stdout);
    let truth = "truth: 4 rows, 4 keys, 1 positive in the universe, 0 outside it";
    assert!(said.contains(truth), "{said}");
    let rows = rows(&d.join(OUT));
    let roc = row(&rows, "M1", "auroc");
    assert_eq!(roc.get("n_pos"), "1");
    assert_eq!(roc.get("n_neg"), "3");
    assert_eq!(roc.get("outside_pos"), "0");
    // K3 at 3 beats K1 and K2, loses to K4.
    near(roc.num("value"), 2.0 / 3.0);
}

/// The files of a run scoring `p` of keys K1, K2, ... (K1 true) as
/// `neglog10:p`.
fn neglog10_files(dir: &Path, p: &[&str]) {
    let keys: Vec<String> = (1..=p.len()).map(|i| format!("K{i}")).collect();
    tsv(&dir.join("universe.tsv"), "key", &keys);
    tsv(&dir.join("truth.tsv"), "key", &keys[..1]);
    let rows: Vec<String> = keys
        .iter()
        .zip(p)
        .map(|(k, p)| format!("{k}\t{p}"))
        .collect();
    tsv(&dir.join("scores.tsv"), "key\tp", &rows);
}

/// A run scoring `p` of keys K1, K2, ... (K1 true) as `neglog10:p`.
fn neglog10(dir: &Path, p: &[&str]) -> Output {
    neglog10_files(dir, p);
    legume(keyed_args(dir, &["M1=neglog10:p"]))
}

#[test]
fn neglog10_of_a_p_outside_0_1_is_an_error() {
    for bad in ["1.5", "-0.1"] {
        let dir = tempfile::tempdir().unwrap();
        let o = neglog10(dir.path(), &["0.01", bad, "0.5", "1"]);
        assert!(!o.status.success(), "p = {bad} was taken");
        assert!(stderr(&o).contains(bad), "{}", stderr(&o));
        assert!(!dir.path().join(OUT).exists());
    }
}

// K9, outside the universe, holds a p of 7: it is dropped before it is
// turned into a score, so it cannot fail the run.
#[test]
fn neglog10_never_turns_a_row_outside_the_universe() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    neglog10_files(d, &["0.01", "0.5", "1"]);
    let mut scores = std::fs::read_to_string(d.join("scores.tsv")).unwrap();
    scores.push_str("K9\t7\n");
    std::fs::write(d.join("scores.tsv"), scores).unwrap();
    let o = legume(keyed_args(d, &["M1=neglog10:p"]));
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&d.join(OUT));
    let roc = row(&rows, "M1", "auroc");
    assert_eq!(roc.get("outside_scored"), "1");
    near(roc.num("value"), 1.0);
}

// A label cell that is no number is refused, its row named, not taken as
// a negative.
#[test]
fn a_label_that_is_not_a_number_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    tsv(&d.join("universe.tsv"), "key", &["K1", "K2"]);
    tsv(&d.join("truth.tsv"), "key\tw", &["K1\t2", "K2\tyes"]);
    tsv(&d.join("scores.tsv"), "key\tv", &["K1\t1", "K2\t2"]);
    let mut a = keyed_args(d, &["M1=v"]);
    a.extend(["--label", "w", "--above", "1"].map(String::from));
    let o = legume(&a);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stderr(&o).contains("row 2 of w"), "{}", stderr(&o));
    assert!(stderr(&o).contains("\"yes\""), "{}", stderr(&o));
    assert!(!d.join(OUT).exists());
}

// p = 0 is +inf: K1 alone at the top, one of four (over 1%), so a warning.
#[test]
fn a_p_of_0_is_inf_and_warned_of() {
    let dir = tempfile::tempdir().unwrap();
    let o = neglog10(dir.path(), &["0", "0.5", "1", "NaN"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("+inf"), "{}", stderr(&o));
    let rows = rows(&dir.path().join(OUT));
    let ap = row(&rows, "M1", "auprc");
    assert_eq!(ap.get("n_inf"), "1");
    assert_eq!(ap.get("unscored"), "1");
    near(ap.num("value"), 1.0);
}

// The warning goes to the writer the run is given for it, not to this
// process's stderr: a caller running `score::main` reads it there.
#[test]
fn the_inf_warning_goes_to_the_err_writer() {
    let dir = tempfile::tempdir().unwrap();
    neglog10_files(dir.path(), &["0", "0.5", "1", "NaN"]);
    let a = keyed_args(dir.path(), &["M1=neglog10:p"]);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    legume_mcp::score::main(&a[1..], &mut out, &mut err).unwrap();
    let err = String::from_utf8(err).unwrap();
    assert!(
        err.starts_with("warning: M1 scores 1 of its 3 scored items +inf"),
        "{err}"
    );
    assert!(!String::from_utf8(out).unwrap().contains("warning"));
}

// The warning is for more than 1% of the scored items at +inf: 2 of 200 is
// 1% and quiet, 3 of 200 warns, none is quiet.
#[test]
fn inf_is_warned_of_only_above_1_percent_of_the_scored() {
    for (zeros, warned) in [(0, false), (2, false), (3, true)] {
        let dir = tempfile::tempdir().unwrap();
        let p: Vec<&str> = (0..200)
            .map(|i| if i < zeros { "0" } else { "0.5" })
            .collect();
        let o = neglog10(dir.path(), &p);
        assert!(o.status.success(), "{}", stderr(&o));
        assert_eq!(
            stderr(&o).contains("warning:"),
            warned,
            "{zeros} of 200 at p = 0: {}",
            stderr(&o)
        );
        let rows = rows(&dir.path().join(OUT));
        assert_eq!(row(&rows, "M1", "auroc").get("n_inf"), zeros.to_string());
    }
}

// K1 (true) at -5, K2 at -3, K3 at 6. As written K1 is last, AUROC 0;
// negated it is first, 1; by magnitude it falls between K3 and K2, 1/2.
#[test]
fn abs_ranks_by_magnitude() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    tsv(&d.join("universe.tsv"), "key", &["K1", "K2", "K3"]);
    tsv(&d.join("truth.tsv"), "key", &["K1"]);
    tsv(
        &d.join("scores.tsv"),
        "key\tv",
        &["K1\t-5", "K2\t-3", "K3\t6"],
    );
    let a = keyed_args(d, &["M1=v", "M2=neg:v", "M3=abs:v"]);
    let o = legume(&a);
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&d.join(OUT));
    near(row(&rows, "M1", "auroc").num("value"), 0.0);
    near(row(&rows, "M2", "auroc").num("value"), 1.0);
    near(row(&rows, "M3", "auroc").num("value"), 0.5);
}

/// Universe K1, K2, K3; truth K1; scores K1 3, K2 2, K3 1 (AUROC 1). The
/// truth gzipped as well, as `truth.tsv.gz`.
fn ranked_in_order(d: &Path) {
    tsv(&d.join("universe.tsv"), "key", &["K1", "K2", "K3"]);
    tsv(&d.join("truth.tsv"), "key", &["K1"]);
    tsv(&d.join("truth.tsv.gz"), "key", &["K1"]);
    let scores = ["K1\t3", "K2\t2", "K3\t1"];
    tsv(&d.join("scores.tsv"), "key\tv", &scores);
}

// A truth or scores file given as a pipe (`<(...)`, `/dev/stdin`) is read
// from its first byte: the bytes looked at to tell the format are not lost,
// gzipped or not.
#[test]
fn a_piped_truth_or_scores_file_is_read_whole() {
    for (arg, fed) in [
        ("truth.tsv", "truth.tsv"),
        ("scores.tsv", "scores.tsv"),
        ("truth.tsv", "truth.tsv.gz"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        ranked_in_order(d);
        let a = keyed_args(d, &["M1=v"]);
        let o = legume_fed(&a, &d.join(arg), std::fs::read(d.join(fed)).unwrap());
        assert!(o.status.success(), "{fed} piped: {}", stderr(&o));
        let rows = rows(&d.join(OUT));
        let roc = row(&rows, "M1", "auroc");
        assert_eq!(roc.get("n_pos"), "1", "{fed} piped");
        assert_eq!(roc.get("unscored"), "0", "{fed} piped");
        near(roc.num("value"), 1.0);
    }
}

// One TSV holds the keys, the label and the scores. K1 (true) at 3, K2 at
// 2, K4 at 1, then K3 (true) unscored: K1 beats both negatives, K3 loses
// to both, AUROC 2/4. Given as a pipe it is read once, and whole.
#[test]
fn universe_same_reads_its_one_file_once_so_a_pipe_works() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let one = d.join("one.tsv");
    let out = d.join(OUT);
    tsv(
        &one,
        "key\tplanted\tv",
        &["K1\t1\t3", "K2\t0\t2", "K3\t1\tNA", "K4\t0\t1"],
    );
    let a = same_args(&one, "key", &["M1=v"], &out);
    for piped in [false, true] {
        let o = if piped {
            legume_fed(&a, &one, std::fs::read(&one).unwrap())
        } else {
            legume(&a)
        };
        assert!(o.status.success(), "piped {piped}: {}", stderr(&o));
        let said = String::from_utf8_lossy(&o.stdout);
        assert!(said.contains("truth: 4 rows, 4 keys, 2 positive"), "{said}");
        let rows = rows(&out);
        let roc = row(&rows, "M1", "auroc");
        near(roc.num("value"), 0.5);
        assert_eq!(roc.get("n_pos"), "2", "piped {piped}");
        assert_eq!(roc.get("unscored_pos"), "1", "piped {piped}");
    }
}

// --universe same, K1 listed twice: one item, positive from its second
// row and scored its higher 3, above K2's 2: AUROC 1. Its rows each find
// K1, so the truth has two keys and the scores one duplicate.
#[test]
fn universe_same_finds_a_key_listed_twice_as_one_item() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.tsv");
    let out = dir.path().join(OUT);
    tsv(
        &one,
        "key\tplanted\tv",
        &["K1\t0\t1", "K2\t0\t2", "K1\t1\t3"],
    );
    let mut a = same_args(&one, "key", &["M1=v"], &out);
    a.extend(["--dup", "max"]);
    let o = legume(&a);
    assert!(o.status.success(), "{}", stderr(&o));
    let said = String::from_utf8_lossy(&o.stdout);
    for line in [
        "universe: 2 items (1 duplicate rows)",
        "truth: 3 rows, 2 keys, 1 positive in the universe, 0 outside it",
        "scores: 3 rows, 0 outside the universe, 1 duplicate keys",
    ] {
        assert!(said.contains(line), "{said}");
    }
    let rows = rows(&out);
    let roc = row(&rows, "M1", "auroc");
    near(roc.num("value"), 1.0);
    assert_eq!(roc.get("n_universe"), "2");
}

// Parquet is read by seeking, which a pipe cannot do: refused, and why.
#[test]
fn a_piped_parquet_is_refused_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    ranked_in_order(d);
    let one = d.join("scores.parquet");
    parquet(
        &one,
        &[
            (
                "key",
                Col::Str(["K1", "K2", "K3"].map(String::from).to_vec()),
            ),
            ("v", Col::F64(vec![3.0, 2.0, 1.0])),
        ],
        Compression::SNAPPY,
    );
    let a = keyed_args(d, &["M1=v"]);
    let o = legume_fed(&a, &d.join("scores.tsv"), std::fs::read(&one).unwrap());
    assert!(!o.status.success());
    let e = stderr(&o);
    assert!(
        e.contains("parquet") && e.contains("not a regular file"),
        "{e}"
    );
    assert!(!d.join(OUT).exists());
}

// pinto `lra-simulate --one`: one parquet holds the pairs (ligand and
// receptor, UTF8), the planted label (int32 0/1), estimable (int32) and the
// scores, each a double: t and its p, and the p values of the other tests.
// A pair not estimable (LIG3, REC1, planted) has NaN t and p: it ranks
// last and is counted unscored.
//
// t ranks P 5, P 3, N 1, N 0.5, N -1, then P unscored: AUROC 6/9; AP
// (1 + 1 + 3/6) / 3 = 5/6. -log10 p: P 6, P 3, N 0.52 twice (tied), N
// 0.22, then the same P unscored: the same AUROC and AP, two of five
// scored items tied.
#[test]
fn an_lra_one_parquet_is_scored_over_its_own_rows() {
    let dir = tempfile::tempdir().unwrap();
    let one = dir.path().join("one.parquet");
    let out = dir.path().join("one.score.tsv");
    let ligand = ["LIG1", "LIG1", "LIG2", "LIG2", "LIG3", "LIG3"];
    let receptor = ["REC1", "REC2", "REC1", "REC2", "REC1", "REC2"];
    parquet(
        &one,
        &[
            ("ligand", Col::Str(ligand.map(String::from).to_vec())),
            ("receptor", Col::Str(receptor.map(String::from).to_vec())),
            ("planted", Col::I32(vec![1, 1, 0, 0, 1, 0])),
            ("estimable", Col::I32(vec![1, 1, 1, 1, 0, 1])),
            ("t", Col::F64(vec![5.0, 3.0, 0.5, -1.0, f64::NAN, 1.0])),
            ("p", Col::F64(vec![1e-6, 1e-3, 0.6, 0.3, f64::NAN, 0.3])),
            (
                "marginal_cor_p",
                Col::F64(vec![1e-4, 0.02, 0.5, 0.7, f64::NAN, 0.1]),
            ),
            (
                "unadjusted_diff_p",
                Col::F64(vec![1e-5, 0.01, 0.4, 0.9, f64::NAN, 0.2]),
            ),
            (
                "effect_ipw_p",
                Col::F64(vec![1e-6, 0.03, 0.8, 0.6, f64::NAN, 0.5]),
            ),
        ],
        Compression::SNAPPY,
    );
    let o = legume(same_args(
        &one,
        "ligand,receptor",
        &["estimate=t", "estimate_p=neglog10:p"],
        &out,
    ));
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&out);
    assert_eq!(rows.len(), 4);
    for score in ["estimate", "estimate_p"] {
        let roc = row(&rows, score, "auroc");
        near(roc.num("value"), 6.0 / 9.0);
        near(row(&rows, score, "auprc").num("value"), 5.0 / 6.0);
        assert_eq!(roc.get("n_universe"), "6");
        assert_eq!(roc.get("n_pos"), "3");
        assert_eq!(roc.get("unscored"), "1");
        assert_eq!(roc.get("unscored_pos"), "1");
    }
    near(row(&rows, "estimate", "auroc").num("tied_share"), 0.0);
    near(row(&rows, "estimate_p", "auroc").num("tied_share"), 0.4);
}

// chickpea's links.parquet (ZSTD level 5), pruned: it lists only some of
// the universe's pairs. The universe comes from `legume universe cis`, the
// truth is the simulator's gzipped pair list.
//
// Genes GENE1 (chr1, TSS 1000) and GENE2 (chr2, TSS 1000), window 100:
// GENE1's candidates are peaks 0, 1, 2; GENE2's are peaks 3, 4. Truth:
// (GENE1, peak 0), (GENE2, peak 4). links.parquet lists (GENE1, peak 0),
// (GENE1, peak 1) and (GENE2, peak 3).
//
// score: peak 0 at 0.0, peak 1 at -0.0 (one tie with it), peak 3 at 0.5,
// then peaks 2 and 4 unscored. Ranking: N 0.5; P, N tied at 0; then N, P
// unscored. AUROC: P0 beats N2 (1), ties N1 (1/2), loses to N3; P4 ties N2
// (1/2): 2/6 = 1/3. AP: at 0.5 no recall; at 0, TP 1 of 3 (R 1/2); the
// unscored block, TP 2 of 5 (R 1/2): 1/6 + 1/5 = 11/30.
//
// nearest = neg:distance (distances 50, 20, 30 for peaks 0, 1, 3): N -20,
// N -30, P -50, then the unscored. AUROC: P0 beats N2 (1); P4 ties N2
// (1/2): 1.5/6 = 1/4. AP: TP 1 of 3 at -50 (R 1/2); TP 2 of 5 at the end
// (R 1/2): 1/6 + 1/5 = 11/30.
#[test]
fn a_chickpea_links_parquet_is_scored_over_the_cis_universe() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let genes = d.join("sim.gene_coords.tsv.gz");
    let peaks = d.join("sim.peak_names.txt");
    let universe = d.join("cis.tsv.gz");
    let truth = d.join("sim.ground_truth.tsv.gz");
    let links = d.join("p2g.links.parquet");
    let out = d.join("p2g.score.tsv");
    tsv(
        &genes,
        "gene\tchr\ttss",
        &["GENE1\tchr1\t1000", "GENE2\tchr2\t1000"],
    );
    let names = [
        "chr1:1000-1100", // 1050, d 50
        "chr1:960-1000",  // 980, d 20
        "chr1:1060-1100", // 1080, d 80
        "chr2:1020-1040", // 1030, d 30
        "chr2:900-940",   // 920, d 80
        "chr3:1000-1001",
    ];
    std::fs::write(&peaks, names.join("\n") + "\n").unwrap();
    let o = legume([
        "universe",
        "cis",
        "--gene-coords",
        s(&genes),
        "--peaks",
        s(&peaks),
        "--window",
        "100",
        "-o",
        s(&universe),
    ]);
    assert!(o.status.success(), "{}", stderr(&o));

    tsv(
        &truth,
        "gene\tpeak",
        &[
            format!("GENE1\t{}", names[0]),
            format!("GENE2\t{}", names[4]),
        ],
    );
    let link = |g: &str, p: usize| (g.to_string(), names[p].to_string());
    let listed = [link("GENE1", 0), link("GENE1", 1), link("GENE2", 3)];
    parquet(
        &links,
        &[
            (
                "gene",
                Col::Str(listed.iter().map(|l| l.0.clone()).collect()),
            ),
            (
                "peak",
                Col::Str(listed.iter().map(|l| l.1.clone()).collect()),
            ),
            ("gene_idx", Col::I32(vec![0, 0, 1])),
            ("peak_idx", Col::I32(vec![0, 1, 3])),
            ("distance", Col::I64(vec![50, 20, 30])),
            ("abc", Col::F32(vec![0.4, 0.6, 1.0])),
            ("gate", Col::F32(vec![1.0, 0.0, 1.0])),
            ("corr", Col::F32(vec![0.0, -0.3, 0.5])),
            ("score", Col::F32(vec![0.0, -0.0, 0.5])),
        ],
        Compression::ZSTD(ZstdLevel::try_new(5).unwrap()),
    );
    let o = legume([
        "score",
        "--scores",
        s(&links),
        "--key",
        "gene,peak",
        "--score",
        "M1=score",
        "--score",
        "nearest=neg:distance",
        "--truth",
        s(&truth),
        "--truth-key",
        "gene,peak",
        "--universe",
        s(&universe),
        "--universe-key",
        "gene,peak",
        "-o",
        s(&out),
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&out);
    near(row(&rows, "M1", "auroc").num("value"), 1.0 / 3.0);
    near(row(&rows, "M1", "auprc").num("value"), 11.0 / 30.0);
    near(row(&rows, "nearest", "auroc").num("value"), 0.25);
    near(row(&rows, "nearest", "auprc").num("value"), 11.0 / 30.0);
    let m1 = row(&rows, "M1", "auroc");
    assert_eq!(m1.get("n_universe"), "5");
    assert_eq!(m1.get("unscored"), "2");
    assert_eq!(m1.get("unscored_pos"), "1");
    // 0.0 and -0.0 are one tie: two of the three scored.
    near(m1.num("tied_share"), 2.0 / 3.0);
}

// A null score is no score: K1 (true) ranks last, unscored, below K2 and
// K3, AUROC 0. Read as the next row's score it would rank first. A null
// key names no item: refused.
#[test]
fn a_null_parquet_score_is_unscored_and_a_null_key_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    tsv(&d.join("universe.tsv"), "key", &["K1", "K2", "K3"]);
    tsv(&d.join("truth.tsv"), "key", &["K1"]);
    let scores = d.join("scores.parquet");
    let a = keyed_args_of(d, "scores.parquet", &["M1=v"]);
    let some = |k: &[&str]| k.iter().map(|k| Some(k.to_string())).collect();
    parquet(
        &scores,
        &[
            ("key", Col::OptStr(some(&["K1", "K2", "K3"]))),
            ("v", Col::OptF64(vec![None, Some(0.5), Some(0.2)])),
        ],
        Compression::SNAPPY,
    );
    let o = legume(&a);
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&d.join(OUT));
    let roc = row(&rows, "M1", "auroc");
    near(roc.num("value"), 0.0);
    assert_eq!(roc.get("unscored"), "1");
    assert_eq!(roc.get("unscored_pos"), "1");

    std::fs::remove_file(d.join(OUT)).unwrap();
    let mut keys: Vec<Option<String>> = some(&["K1", "K2", "K3"]);
    keys[1] = None;
    parquet(
        &scores,
        &[
            ("key", Col::OptStr(keys)),
            ("v", Col::OptF64(vec![Some(0.1), Some(0.5), Some(0.2)])),
        ],
        Compression::SNAPPY,
    );
    let o = legume(&a);
    assert!(!o.status.success());
    let e = stderr(&o);
    assert!(e.contains("row 2 of key"), "{e}");
    assert!(!d.join(OUT).exists());
}

#[test]
fn score_help_names_every_flag() {
    let o = legume(["score", "--help"]);
    assert!(o.status.success());
    let help = String::from_utf8_lossy(&o.stdout);
    for flag in [
        "--scores",
        "--key",
        "--score",
        "--truth",
        "--truth-key",
        "--label",
        "--above",
        "--universe",
        "--universe-key",
        "--truth-outside",
        "--scored-outside",
        "--dup",
        "neglog10:",
    ] {
        assert!(help.contains(flag), "{flag} missing from\n{help}");
    }
}
