//! `legume score`, run as the binary: a method's scores against a truth
//! over a universe, from TSV, gzipped TSV and parquet, and the refusals
//! that keep a metric honest (no universe, truth outside it, a duplicate
//! key, a p value out of range).

use flate2::write::GzEncoder;
use parquet::basic::{Compression, ConvertedType, Repetition, Type as Physical, ZstdLevel};
use parquet::data_type::{ByteArray, ByteArrayType, DoubleType, FloatType, Int32Type, Int64Type};
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::types::Type;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Arc;

/// `legume` with `args`, its output captured.
fn legume(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_legume"))
        .args(args)
        .output()
        .unwrap()
}

/// `legume` with `args`, the argument `piped` replaced by `/dev/stdin` and
/// `input` fed through it: a stream that cannot be opened twice, as
/// `<(...)` is.
fn legume_fed(args: &[String], piped: &Path, input: Vec<u8>) -> Output {
    let piped = piped.to_str().unwrap();
    assert!(args.iter().any(|a| a == piped), "{piped} is no argument");
    let args = args
        .iter()
        .map(|a| if a == piped { "/dev/stdin" } else { a });
    let mut child = Command::new(env!("CARGO_BIN_EXE_legume"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    // A run that stops reading closes the pipe: that write error is the
    // run's answer, not the test's.
    let feed = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let o = child.wait_with_output().unwrap();
    feed.join().unwrap();
    o
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// A TSV with a header, gzipped when the name ends in `.gz`.
fn tsv(path: &Path, header: &str, rows: &[String]) {
    let mut text = format!("{header}\n");
    for r in rows {
        text.push_str(r);
        text.push('\n');
    }
    if path.extension().is_some_and(|e| e == "gz") {
        let mut gz = GzEncoder::new(std::fs::File::create(path).unwrap(), Default::default());
        gz.write_all(text.as_bytes()).unwrap();
        gz.finish().unwrap();
    } else {
        std::fs::write(path, text).unwrap();
    }
}

/// One column of a parquet fixture.
enum Col {
    Str(Vec<String>),
    I32(Vec<i32>),
    I64(Vec<i64>),
    F32(Vec<f32>),
    F64(Vec<f64>),
}

/// A parquet file of REQUIRED columns in one row group, as the tools'
/// writers make them.
fn parquet(path: &Path, columns: &[(&str, Col)], compression: Compression) {
    let fields = columns
        .iter()
        .map(|(name, c)| {
            let ty = match c {
                Col::Str(_) => Physical::BYTE_ARRAY,
                Col::I32(_) => Physical::INT32,
                Col::I64(_) => Physical::INT64,
                Col::F32(_) => Physical::FLOAT,
                Col::F64(_) => Physical::DOUBLE,
            };
            let b = Type::primitive_type_builder(name, ty).with_repetition(Repetition::REQUIRED);
            let b = match c {
                Col::Str(_) => b.with_converted_type(ConvertedType::UTF8),
                _ => b,
            };
            Arc::new(b.build().unwrap())
        })
        .collect();
    let schema = Arc::new(
        Type::group_type_builder("table")
            .with_fields(fields)
            .build()
            .unwrap(),
    );
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
    let pos: Vec<usize> = (0..10).map(|k| k * 100 + 7).collect();
    tsv(
        &truth,
        "gene\tpeak",
        &pos.iter().map(|&i| pair(i)).collect::<Vec<_>>(),
    );
    tsv(
        &scores,
        "gene\tpeak\tscore",
        &[format!("{}\t0.9", pair(7)), format!("{}\t0.8", pair(107))],
    );
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
    let o = legume(&c.args(&c.universe_args()));
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
    let o = legume(&c.args(&[]));
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
    let o = legume(&c.args(&["--universe", "same"]));
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stderr(&o).contains("same"), "{}", stderr(&o));
    assert!(!c.out.exists());
}

#[test]
fn truth_outside_the_universe_exits_3_with_a_count_or_is_dropped_and_counted() {
    let c = few_links();
    // Two more truth pairs, outside the universe.
    let mut pos: Vec<String> = (0..10).map(|k| pair(k * 100 + 7)).collect();
    pos.push("GENE1000\tPEAK10000".into());
    pos.push("GENE1001\tPEAK10010".into());
    tsv(&c.truth, "gene\tpeak", &pos);
    let o = legume(&c.args(&c.universe_args()));
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    assert!(stderr(&o).contains("2 truth"), "{}", stderr(&o));
    assert!(!c.out.exists());

    let mut a = c.args(&c.universe_args());
    a.extend(["--truth-outside", "drop"]);
    let o = legume(&a);
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&c.out);
    let ap = row(&rows, "M1", "auprc");
    assert_eq!(ap.get("outside_pos"), "2");
    assert_eq!(ap.get("n_pos"), "10");
    near(ap.num("value"), 0.208);
}

#[test]
fn scored_rows_outside_the_universe_are_dropped_and_counted() {
    let c = few_links();
    tsv(
        &c.scores,
        "gene\tpeak\tscore",
        &[
            format!("{}\t0.9", pair(7)),
            format!("{}\t0.8", pair(107)),
            "GENE2000\tPEAK1\t5".into(),
            "GENE2001\tPEAK2\t5".into(),
            "GENE2002\tPEAK3\tNaN".into(),
        ],
    );
    let o = legume(&c.args(&c.universe_args()));
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&c.out);
    let ap = row(&rows, "M1", "auprc");
    assert_eq!(ap.get("outside_scored"), "3");
    assert_eq!(ap.get("n_universe"), "1000");
    near(ap.num("value"), 0.208);

    let mut a = c.args(&c.universe_args());
    a.extend(["--scored-outside", "fail"]);
    std::fs::remove_file(&c.out).unwrap();
    let o = legume(&a);
    assert_eq!(o.status.code(), Some(3), "{}", stderr(&o));
    assert!(stderr(&o).contains('3'), "{}", stderr(&o));
    assert!(!c.out.exists());
}

/// Items K1 (true), K2, K3; K1 and K2 each scored twice, K3 once more as
/// NaN.
fn duplicates(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let universe = dir.join("universe.tsv");
    let truth = dir.join("truth.tsv");
    let scores = dir.join("scores.tsv");
    tsv(&universe, "key", &["K1".into(), "K2".into(), "K3".into()]);
    tsv(&truth, "key", &["K1".into()]);
    let rows = [
        "K1\t0.9", "K2\t0.15", "K3\t0.2", "K1\t0.1", "K2\t0.95", "K3\tNaN",
    ];
    tsv(&scores, "key\tv", &rows.map(String::from));
    (universe, truth, scores)
}

// With the max of each key, K1 0.9, K2 0.95, K3 0.2: K1 beats K3 and loses
// to K2, AUROC 1/2. The first rows would give 1, the last or the min 0.
#[test]
fn a_duplicate_score_key_fails_and_dup_max_keeps_the_max() {
    let dir = tempfile::tempdir().unwrap();
    let (universe, truth, scores) = duplicates(dir.path());
    let out = dir.path().join("out.score.tsv");
    let mut a = vec![
        "score",
        "--scores",
        s(&scores),
        "--key",
        "key",
        "--score",
        "M1=v",
        "--truth",
        s(&truth),
        "--truth-key",
        "key",
        "--universe",
        s(&universe),
        "--universe-key",
        "key",
        "-o",
        s(&out),
    ];
    let o = legume(&a);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("duplicate"), "{}", stderr(&o));
    assert!(!out.exists());

    a.extend(["--dup", "max"]);
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
    let universe = dir.path().join("universe.tsv");
    let truth = dir.path().join("truth.tsv");
    let scores = dir.path().join("scores.tsv");
    let out = dir.path().join("out.score.tsv");
    tsv(
        &universe,
        "key",
        &["K1", "K2", "K3", "K4"].map(String::from),
    );
    tsv(
        &truth,
        "key\tw",
        &["K1\t0.5", "K2\t1", "K3\t1.5", "K9\t0.5"].map(String::from),
    );
    tsv(
        &scores,
        "key\tv",
        &["K1\t1", "K2\t2", "K3\t3", "K4\t4"].map(String::from),
    );
    let o = legume(&[
        "score",
        "--scores",
        s(&scores),
        "--key",
        "key",
        "--score",
        "M1=v",
        "--truth",
        s(&truth),
        "--truth-key",
        "key",
        "--label",
        "w",
        "--above",
        "1",
        "--universe",
        s(&universe),
        "--universe-key",
        "key",
        "-o",
        s(&out),
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&out);
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
    let universe = dir.join("universe.tsv");
    let truth = dir.join("truth.tsv");
    let scores = dir.join("scores.tsv");
    let keys: Vec<String> = (1..=p.len()).map(|i| format!("K{i}")).collect();
    tsv(&universe, "key", &keys);
    tsv(&truth, "key", &keys[..1]);
    let rows: Vec<String> = keys
        .iter()
        .zip(p)
        .map(|(k, p)| format!("{k}\t{p}"))
        .collect();
    tsv(&scores, "key\tp", &rows);
}

/// A run scoring `p` of keys K1, K2, ... (K1 true) as `neglog10:p`.
fn neglog10(dir: &Path, p: &[&str]) -> Output {
    neglog10_files(dir, p);
    let universe = dir.join("universe.tsv");
    let truth = dir.join("truth.tsv");
    let scores = dir.join("scores.tsv");
    let out = dir.join("out.score.tsv");
    legume(&[
        "score",
        "--scores",
        s(&scores),
        "--key",
        "key",
        "--score",
        "M1=neglog10:p",
        "--truth",
        s(&truth),
        "--truth-key",
        "key",
        "--universe",
        s(&universe),
        "--universe-key",
        "key",
        "-o",
        s(&out),
    ])
}

#[test]
fn neglog10_of_a_p_outside_0_1_is_an_error() {
    for bad in ["1.5", "-0.1"] {
        let dir = tempfile::tempdir().unwrap();
        let o = neglog10(dir.path(), &["0.01", bad, "0.5", "1"]);
        assert!(!o.status.success(), "p = {bad} was taken");
        assert!(stderr(&o).contains(bad), "{}", stderr(&o));
        assert!(!dir.path().join("out.score.tsv").exists());
    }
}

// p = 0 is +inf: K1 alone at the top, one of four (over 1%), so a warning.
#[test]
fn a_p_of_0_is_inf_and_warned_of() {
    let dir = tempfile::tempdir().unwrap();
    let o = neglog10(dir.path(), &["0", "0.5", "1", "NaN"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stderr(&o).contains("+inf"), "{}", stderr(&o));
    let rows = rows(&dir.path().join("out.score.tsv"));
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
        let rows = rows(&dir.path().join("out.score.tsv"));
        assert_eq!(row(&rows, "M1", "auroc").get("n_inf"), zeros.to_string());
    }
}

/// The arguments of a run over the files in `dir`, keyed `key`, scoring
/// each of `score` (`NAME=COLUMN`).
fn keyed_args<'a>(dir: &'a Path, score: &[&'a str]) -> Vec<String> {
    let at = |f: &str| dir.join(f).to_str().unwrap().to_string();
    let mut a: Vec<String> = ["score", "--scores", &at("scores.tsv"), "--key", "key"]
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
            &at("out.score.tsv"),
        ]
        .map(String::from),
    );
    a
}

// K1 (true) at -5, K2 at -3, K3 at 6. As written K1 is last, AUROC 0;
// negated it is first, 1; by magnitude it falls between K3 and K2, 1/2.
#[test]
fn abs_ranks_by_magnitude() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    tsv(
        &d.join("universe.tsv"),
        "key",
        &["K1", "K2", "K3"].map(String::from),
    );
    tsv(&d.join("truth.tsv"), "key", &["K1".into()]);
    tsv(
        &d.join("scores.tsv"),
        "key\tv",
        &["K1\t-5", "K2\t-3", "K3\t6"].map(String::from),
    );
    let a = keyed_args(d, &["M1=v", "M2=neg:v", "M3=abs:v"]);
    let o = legume(&a.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(o.status.success(), "{}", stderr(&o));
    let rows = rows(&d.join("out.score.tsv"));
    near(row(&rows, "M1", "auroc").num("value"), 0.0);
    near(row(&rows, "M2", "auroc").num("value"), 1.0);
    near(row(&rows, "M3", "auroc").num("value"), 0.5);
}

/// Universe K1, K2, K3; truth K1; scores K1 3, K2 2, K3 1 (AUROC 1). The
/// truth gzipped as well, as `truth.tsv.gz`.
fn ranked_in_order(d: &Path) {
    tsv(
        &d.join("universe.tsv"),
        "key",
        &["K1", "K2", "K3"].map(String::from),
    );
    tsv(&d.join("truth.tsv"), "key", &["K1".into()]);
    tsv(&d.join("truth.tsv.gz"), "key", &["K1".into()]);
    let scores = ["K1\t3", "K2\t2", "K3\t1"].map(String::from);
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
        let rows = rows(&d.join("out.score.tsv"));
        let roc = row(&rows, "M1", "auroc");
        assert_eq!(roc.get("n_pos"), "1", "{fed} piped");
        assert_eq!(roc.get("unscored"), "0", "{fed} piped");
        near(roc.num("value"), 1.0);
    }
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
    assert!(!d.join("out.score.tsv").exists());
}

// pinto `lra-simulate --one`: one parquet holds the pairs, the planted
// label and the scores; a pair not estimable has NaN t and p.
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
        ],
        Compression::SNAPPY,
    );
    let o = legume(&[
        "score",
        "--scores",
        s(&one),
        "--key",
        "ligand,receptor",
        "--score",
        "estimate=t",
        "--score",
        "estimate_p=neglog10:p",
        "--truth",
        s(&one),
        "--truth-key",
        "ligand,receptor",
        "--label",
        "planted",
        "--above",
        "0",
        "--universe",
        "same",
        "-o",
        s(&out),
    ]);
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
        &["GENE1\tchr1\t1000".into(), "GENE2\tchr2\t1000".into()],
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
    let o = legume(&[
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
    let o = legume(&[
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

#[test]
fn score_help_names_every_flag() {
    let o = legume(&["score", "--help"]);
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
