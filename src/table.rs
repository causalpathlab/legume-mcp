//! The tables the tools write, read by column name: TSV with a header,
//! gzipped or not, and parquet. The format is told by the file's first
//! bytes, not its name. A table is read for its keys and number columns
//! ([`read_keyed`]); the tables `legume score` and `legume universe` write
//! are text, written whole ([`write_text`]).

use anyhow::{bail, Context, Result};
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use parquet::basic::{ConvertedType, LogicalType};
use parquet::column::reader::{ColumnReader, ColumnReaderImpl};
use parquet::data_type::{ByteArray, DataType};
use parquet::file::reader::{FileReader, RowGroupReader, SerializedFileReader};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Cursor, Read, Write};
use std::ops::Range;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Text cells that mean no value.
const MISSING: &[&str] = &["", "NA", "na", "N/A", "null", "NULL", "None"];

/// A table read for its keys: each row's key, and the number columns asked
/// for beside it.
#[derive(Debug)]
pub struct Keyed {
    /// Each row's key: its key columns' cells, joined by tabs. Text as
    /// written, an integer in decimal (as a TSV writes it).
    pub keys: Vec<String>,
    /// The number columns, in the order asked.
    pub numbers: Vec<Numbers>,
}

/// A column read as numbers, one a row.
#[derive(Debug, Default, PartialEq)]
pub struct Numbers {
    /// Each row's number; NaN where it has none (an empty cell, `NA`,
    /// `null`, ..., a parquet null) or holds text that is no number.
    pub values: Vec<f64>,
    /// The cells of text that is no number, by row (from 0), as written: an
    /// error only for a row that is read.
    pub bad: Vec<(usize, String)>,
}

impl Numbers {
    /// The text `cell` as the next row's number.
    fn push_text(&mut self, cell: &str) {
        let x = if MISSING.contains(&cell) {
            f64::NAN
        } else {
            cell.parse().unwrap_or_else(|_| {
                self.bad.push((self.values.len(), cell.to_string()));
                f64::NAN
            })
        };
        self.values.push(x);
    }

    /// The first cell of text that is no number in a row `read` says is
    /// read: its row (from 0) and the text.
    pub fn first_bad(&self, read: impl Fn(usize) -> bool) -> Option<(usize, &str)> {
        self.bad
            .iter()
            .find(|(row, _)| read(*row))
            .map(|(row, cell)| (*row, cell.as_str()))
    }
}

/// The table at `path`, read for its keys by the columns `key` and the
/// columns `numbers` as numbers.
pub fn read_keyed(path: &Path, key: &[String], numbers: &[&str]) -> Result<Keyed> {
    let (head, file) = open_peeked(path)?;
    let keyed = if head == b"PAR1" {
        read_parquet(file, key, numbers)
    } else {
        read_tsv(text(head, file), key, numbers)
    };
    keyed.with_context(|| format!("reading {}", path.display()))
}

/// A text file's lines, gunzipped when it is gzip.
pub fn open_text(path: &Path) -> Result<Box<dyn BufRead>> {
    let (head, file) = open_peeked(path)?;
    Ok(text(head, file))
}

/// The file at `path`, opened once, and its first four bytes (fewer when
/// it is shorter), read to tell the format. Once only: a pipe, as `<(...)`
/// gives, opened again would go on from where the last read stopped.
fn open_peeked(path: &Path) -> Result<(Vec<u8>, File)> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut head = Vec::with_capacity(4);
    (&mut file)
        .take(4)
        .read_to_end(&mut head)
        .with_context(|| format!("cannot read {}", path.display()))?;
    Ok((head, file))
}

/// The lines of `head` then the rest of `file`, gunzipped when it is gzip.
fn text(head: Vec<u8>, file: File) -> Box<dyn BufRead> {
    let gzip = head.starts_with(&[0x1f, 0x8b]);
    let whole = Cursor::new(head).chain(file);
    if gzip {
        Box::new(BufReader::new(MultiGzDecoder::new(whole)))
    } else {
        Box::new(BufReader::new(whole))
    }
}

/// The text `body` writes, as the file `path`, gzipped when its name ends
/// in `.gz`. Written beside it first and renamed over it once whole, so a
/// run that fails or is stopped never leaves `path` cut short: it is the
/// whole new text or what it was before.
pub fn write_text(path: &Path, body: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> Result<()> {
    // Numbered too, so two threads of this process never share one.
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .with_context(|| format!("{} names no file", path.display()))?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut tmp = OsString::from(".");
    tmp.push(name);
    tmp.push(format!(".{}.{n}", std::process::id()));
    let tmp = path.with_file_name(tmp);
    let gz = path.extension().is_some_and(|e| e == "gz");
    let written = File::create(&tmp)
        .and_then(|file| {
            if gz {
                through(GzEncoder::new(file, Default::default()), body)?.finish()?;
            } else {
                through(file, body)?;
            }
            Ok(())
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if written.is_err() {
        // Not left behind half written, or written and not put in place.
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("cannot write {}", path.display()))
}

/// `body` written to `w` through a buffer, so its small writes reach `w`
/// (an encoder, say) in large ones; `w` back once they all have.
fn through<W: Write>(w: W, body: impl FnOnce(&mut dyn Write) -> io::Result<()>) -> io::Result<W> {
    let mut buffered = BufWriter::with_capacity(64 << 10, w);
    body(&mut buffered)?;
    buffered
        .into_inner()
        .map_err(io::IntoInnerError::into_error)
}

/// Where each of `want` is among `columns`.
fn positions(columns: &[String], want: &[impl AsRef<str>]) -> Result<Vec<usize>> {
    want.iter()
        .map(|w| {
            let w = w.as_ref();
            columns
                .iter()
                .position(|c| c == w)
                .with_context(|| format!("no column {w:?}; the columns are {}", columns.join(", ")))
        })
        .collect()
}

/// A line without its end: `\n`, then any `\r`.
fn trimmed(line: &str) -> &str {
    line.strip_suffix('\n')
        .unwrap_or(line)
        .trim_end_matches('\r')
}

/// The TSV `text` read for its keys, by one line buffer: each row's key
/// built from its cells, its numbers parsed as they are read.
fn read_tsv(mut text: Box<dyn BufRead>, key: &[String], numbers: &[&str]) -> Result<Keyed> {
    let mut line = String::new();
    if text.read_line(&mut line)? == 0 {
        bail!("empty, with no header");
    }
    let header: Vec<String> = trimmed(&line).split('\t').map(str::to_string).collect();
    let (key_at, number_at) = (positions(&header, key)?, positions(&header, numbers)?);
    let wanted: Vec<usize> = key_at.iter().chain(&number_at).copied().collect();
    let last = wanted.iter().copied().max().unwrap_or(0);
    let mut keyed = Keyed {
        keys: Vec::new(),
        numbers: numbers.iter().map(|_| Numbers::default()).collect(),
    };
    // Each cell's place in the line, as far as the last one wanted.
    let mut cells: Vec<Range<usize>> = Vec::new();
    for number in 2.. {
        line.clear();
        if text.read_line(&mut line)? == 0 {
            break;
        }
        let row = trimmed(&line);
        if row.is_empty() {
            continue;
        }
        cells.clear();
        let mut start = 0;
        for cell in row.split('\t') {
            cells.push(start..start + cell.len());
            start += cell.len() + 1;
            if cells.len() > last {
                break;
            }
        }
        if let Some(&j) = wanted.iter().find(|&&j| j >= cells.len()) {
            bail!(
                "line {number} has {} cells, short of {}",
                cells.len(),
                header[j]
            );
        }
        let cell = |j: usize| &row[cells[j].clone()];
        let mut k = String::with_capacity(key_at.iter().map(|&j| cells[j].len() + 1).sum());
        for (n, &j) in key_at.iter().enumerate() {
            if n > 0 {
                k.push('\t');
            }
            k.push_str(cell(j));
        }
        keyed.keys.push(k);
        for (col, &j) in keyed.numbers.iter_mut().zip(&number_at) {
            col.push_text(cell(j));
        }
    }
    Ok(keyed)
}

/// The parquet `file` read for its keys, by seeking, so a regular file
/// only: row group by row group, each column wanted read whole by its typed
/// reader.
fn read_parquet(file: File, key: &[String], numbers: &[&str]) -> Result<Keyed> {
    if !file.metadata()?.is_file() {
        bail!(
            "parquet is read by seeking, and this is not a regular file (a pipe?); write it to \
             a file first"
        );
    }
    let reader = SerializedFileReader::new(file)?;
    let schema = reader.metadata().file_metadata().schema_descr_ptr();
    let fields = schema.root_schema().get_fields();
    let names: Vec<String> = fields.iter().map(|f| f.name().to_string()).collect();
    // The leaf of each field `at`, a column of single cells.
    let leaves = |at: Vec<usize>| -> Result<Vec<usize>> {
        at.into_iter()
            .map(|j| {
                let leaf = (0..schema.num_columns()).find(|&i| schema.get_column_root_idx(i) == j);
                match leaf {
                    Some(i)
                        if fields[j].is_primitive() && schema.column(i).max_rep_level() == 0 =>
                    {
                        Ok(i)
                    }
                    _ => bail!("cannot read {}: not a column of single cells", names[j]),
                }
            })
            .collect()
    };
    let key_at = leaves(positions(&names, key)?)?;
    let number_at = leaves(positions(&names, numbers)?)?;
    let mut keyed = Keyed {
        keys: Vec::new(),
        numbers: numbers.iter().map(|_| Numbers::default()).collect(),
    };
    let mut def = Vec::new();
    let mut parts = Vec::with_capacity(key.len());
    for g in 0..reader.num_row_groups() {
        let group = reader.get_row_group(g)?;
        let rows = usize::try_from(group.metadata().num_rows())?;
        let first = keyed.keys.len();
        parts.clear();
        for &i in &key_at {
            let column = schema.column(i);
            let name = column.name();
            let cells = cells(&*group, i, rows, &mut def)?;
            if let Some(r) = def.iter().position(|&d| d == 0) {
                bail!("row {} of {name}: a null, which is no key", first + r + 1);
            }
            if let Cells::Float(_) = cells {
                bail!(
                    "{name}: a float, which is no key (two spellings of one float would not join)"
                );
            }
            parts.push(cells);
        }
        keyed.keys.reserve(rows);
        for r in 0..rows {
            let mut k = String::with_capacity(parts.iter().map(|c| c.width(r) + 1).sum());
            for (n, (cells, &i)) in parts.iter().zip(&key_at).enumerate() {
                if n > 0 {
                    k.push('\t');
                }
                cells.push_key(r, &mut k).with_context(|| {
                    format!("row {} of {}", first + r + 1, schema.column(i).name())
                })?;
            }
            keyed.keys.push(k);
        }
        for (col, &i) in keyed.numbers.iter_mut().zip(&number_at) {
            cells(&*group, i, rows, &mut def)?
                .push_numbers(&def, col)
                .with_context(|| format!("column {}", schema.column(i).name()))?;
        }
    }
    Ok(keyed)
}

/// A row group's cells of one parquet column, nulls left out.
enum Cells {
    Int(Vec<i64>),
    Float(Vec<f64>),
    Text(Vec<ByteArray>),
}

impl Cells {
    /// At most the length of the key cell `r`.
    fn width(&self, r: usize) -> usize {
        match self {
            Cells::Int(_) => 20,
            Cells::Float(_) => 0,
            Cells::Text(v) => v[r].len(),
        }
    }

    /// The cell `r` of a column with no nulls, as a key, onto `key`.
    fn push_key(&self, r: usize, key: &mut String) -> Result<()> {
        use std::fmt::Write as _;
        match self {
            Cells::Int(v) => write!(key, "{}", v[r])?,
            Cells::Text(v) => key.push_str(v[r].as_utf8()?),
            Cells::Float(_) => unreachable!("a float key is refused"),
        }
        Ok(())
    }

    /// Each row's number onto `col`: NaN for a null, where `def` (the
    /// column's definition levels, none when it cannot be null) is 0.
    fn push_numbers(&self, def: &[i16], col: &mut Numbers) -> Result<()> {
        let rows = if def.is_empty() {
            self.len()
        } else {
            def.len()
        };
        let mut next = 0;
        for r in 0..rows {
            if def.get(r) == Some(&0) {
                col.values.push(f64::NAN);
                continue;
            }
            match self {
                Cells::Int(v) => col.values.push(v[next] as f64),
                Cells::Float(v) => col.values.push(v[next]),
                Cells::Text(v) => col.push_text(v[next].as_utf8()?),
            }
            next += 1;
        }
        Ok(())
    }

    fn len(&self) -> usize {
        match self {
            Cells::Int(v) => v.len(),
            Cells::Float(v) => v.len(),
            Cells::Text(v) => v.len(),
        }
    }
}

/// The cells of the leaf column `i` of `group`, which holds `rows`, nulls
/// left out; `def` gets its definition levels (0 for a null), or none when
/// it cannot be null. Integers, floats and text, as parquet's rows give
/// them; a type they give otherwise (a date, a decimal, bytes, ...) is
/// refused.
fn cells(group: &dyn RowGroupReader, i: usize, rows: usize, def: &mut Vec<i16>) -> Result<Cells> {
    use ColumnReader as R;
    use ConvertedType as C;
    let column = group.metadata().column(i).column_descr();
    def.clear();
    let def = (column.max_def_level() > 0).then_some(def);
    let float16 = column.logical_type_ref() == Some(&LogicalType::Float16);
    Ok(
        match (group.get_column_reader(i)?, column.converted_type()) {
            (R::BoolColumnReader(r), _) => Cells::Int(ints(all(r, rows, def)?)),
            (R::Int32ColumnReader(r), C::NONE | C::INT_8 | C::INT_16 | C::INT_32) => {
                Cells::Int(ints(all(r, rows, def)?))
            }
            (R::Int32ColumnReader(r), C::UINT_8 | C::UINT_16 | C::UINT_32) => Cells::Int(
                all(r, rows, def)?
                    .into_iter()
                    .map(|x| (x as u32).into())
                    .collect(),
            ),
            (R::Int64ColumnReader(r), C::NONE | C::INT_64) => Cells::Int(all(r, rows, def)?),
            (R::Int64ColumnReader(r), C::UINT_64) => Cells::Int(
                all(r, rows, def)?
                    .into_iter()
                    .map(|x| i64::try_from(x as u64))
                    .collect::<Result<_, _>>()?,
            ),
            (R::FloatColumnReader(r), _) => {
                Cells::Float(all(r, rows, def)?.into_iter().map(f64::from).collect())
            }
            (R::DoubleColumnReader(r), _) => Cells::Float(all(r, rows, def)?),
            (R::ByteArrayColumnReader(r), C::UTF8 | C::ENUM | C::JSON) => {
                Cells::Text(all(r, rows, def)?)
            }
            (R::FixedLenByteArrayColumnReader(r), C::NONE) if float16 => Cells::Float(
                all(r, rows, def)?
                    .iter()
                    .map(|b| match *b.data() {
                        [lo, hi] => Ok(half::f16::from_le_bytes([lo, hi]).to_f64()),
                        _ => bail!("a FLOAT16 of {} bytes", b.len()),
                    })
                    .collect::<Result<_>>()?,
            ),
            (_, converted) => bail!(
                "cannot read {}, {} {converted}",
                column.name(),
                column.physical_type()
            ),
        },
    )
}

fn ints<T: Into<i64>>(v: Vec<T>) -> Vec<i64> {
    v.into_iter().map(Into::into).collect()
}

/// Every value `reader` reads of a row group of `rows`, nulls left out,
/// and onto `def` its definition levels.
fn all<T: DataType>(
    mut reader: ColumnReaderImpl<T>,
    rows: usize,
    mut def: Option<&mut Vec<i16>>,
) -> Result<Vec<T::T>> {
    let mut values = Vec::with_capacity(rows);
    while reader
        .read_records(rows, def.as_deref_mut(), None, &mut values)?
        .0
        > 0
    {}
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_back(path: &Path) -> String {
        let mut text = String::new();
        open_text(path).unwrap().read_to_string(&mut text).unwrap();
        text
    }

    /// The names in `dir`.
    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn text_is_gzipped_when_the_name_ends_in_gz() {
        let dir = tempfile::tempdir().unwrap();
        for (name, gzipped) in [("t.tsv.gz", true), ("t.tsv", false)] {
            let path = dir.path().join(name);
            write_text(&path, |w| {
                for k in 1..=3 {
                    writeln!(w, "K{k}")?;
                }
                Ok(())
            })
            .unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(bytes.starts_with(&[0x1f, 0x8b]), gzipped, "{name}");
            assert_eq!(read_back(&path), "K1\nK2\nK3\n", "{name}");
        }
        assert_eq!(names(dir.path()), ["t.tsv", "t.tsv.gz"]);
    }

    // A body that fails part way leaves the file as it was, and nothing
    // beside it; one that ends replaces it whole.
    #[test]
    fn a_file_is_replaced_whole_or_left_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["t.tsv", "t.tsv.gz"] {
            let path = dir.path().join(name);
            write_text(&path, |w| w.write_all(b"M1\n")).unwrap();
            let e = write_text(&path, |w| {
                // More than the buffer holds, so some of it reaches the file.
                w.write_all(&vec![b'x'; 1 << 20])?;
                Err(io::Error::other("stopped"))
            })
            .unwrap_err();
            assert!(format!("{e:#}").contains("stopped"), "{e:#}");
            let text = read_back(&path);
            assert!(text == "M1\n", "{name}: {} bytes", text.len());
            write_text(&path, |w| w.write_all(b"M2\n")).unwrap();
            assert_eq!(read_back(&path), "M2\n", "{name}");
        }
        assert_eq!(names(dir.path()), ["t.tsv", "t.tsv.gz"]);
    }

    /// A parquet column of a fixture, one row group's cells.
    enum Fixture {
        Text(Vec<&'static str>),
        Int(Vec<i32>),
        Bool(Vec<bool>),
        Float(Vec<Option<f32>>),
        MaybeText(Vec<Option<&'static str>>),
        Half(Vec<[u8; 2]>),
        Double(Vec<f64>),
    }

    /// A parquet file of `schema`, a row group for each of `groups`.
    fn parquet_of(path: &Path, schema: &str, groups: Vec<Vec<Fixture>>) {
        use parquet::data_type::{
            BoolType, ByteArrayType, DoubleType, FixedLenByteArrayType, FloatType, Int32Type,
        };
        use parquet::file::writer::SerializedFileWriter;
        let schema =
            std::sync::Arc::new(parquet::schema::parser::parse_message_type(schema).unwrap());
        let file = File::create(path).unwrap();
        let mut w = SerializedFileWriter::new(file, schema, Default::default()).unwrap();
        let text = |v: &[&str]| v.iter().map(|&s| ByteArray::from(s)).collect::<Vec<_>>();
        let def = |n: Vec<bool>| n.into_iter().map(i16::from).collect::<Vec<_>>();
        for group in groups {
            let mut g = w.next_row_group().unwrap();
            for cells in group {
                let mut c = g.next_column().unwrap().unwrap();
                match cells {
                    Fixture::Text(v) => {
                        c.typed::<ByteArrayType>()
                            .write_batch(&text(&v), None, None)
                    }
                    Fixture::Int(v) => c.typed::<Int32Type>().write_batch(&v, None, None),
                    Fixture::Bool(v) => c.typed::<BoolType>().write_batch(&v, None, None),
                    Fixture::Float(v) => {
                        let some: Vec<f32> = v.iter().flatten().copied().collect();
                        let def = def(v.iter().map(Option::is_some).collect());
                        c.typed::<FloatType>().write_batch(&some, Some(&def), None)
                    }
                    Fixture::MaybeText(v) => {
                        let some: Vec<&str> = v.iter().flatten().copied().collect();
                        let def = def(v.iter().map(Option::is_some).collect());
                        c.typed::<ByteArrayType>()
                            .write_batch(&text(&some), Some(&def), None)
                    }
                    Fixture::Half(v) => {
                        let v: Vec<_> = v
                            .iter()
                            .map(|b| ByteArray::from(b.to_vec()).into())
                            .collect();
                        c.typed::<FixedLenByteArrayType>()
                            .write_batch(&v, None, None)
                    }
                    Fixture::Double(v) => c.typed::<DoubleType>().write_batch(&v, None, None),
                }
                .unwrap();
                c.close().unwrap();
            }
            g.close().unwrap();
        }
        w.close().unwrap();
    }

    fn shown(v: &[f64]) -> Vec<String> {
        v.iter().map(f64::to_string).collect()
    }

    // Each type as parquet's rows read it, over two row groups, so a row
    // is counted from the file's first: an integer key in decimal (UINT_32
    // unsigned, a bool 0 or 1), a null no number, text read as a number
    // and kept as written when it is none, a FLOAT16 as its value.
    #[test]
    fn parquet_cells_are_read_by_their_type_and_nulls_are_no_number() {
        use Fixture::*;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.parquet");
        let schema = "message table {
            REQUIRED BYTE_ARRAY s (UTF8);
            REQUIRED INT32 i;
            REQUIRED INT32 u (UINT_32);
            REQUIRED BOOLEAN b;
            OPTIONAL FLOAT f;
            OPTIONAL BYTE_ARRAY t (UTF8);
            REQUIRED BYTE_ARRAY w (UTF8);
            REQUIRED FIXED_LEN_BYTE_ARRAY (2) h (FLOAT16);
            REQUIRED DOUBLE d;
            REQUIRED INT32 day (DATE);
        }";
        parquet_of(
            &path,
            schema,
            vec![
                vec![
                    Text(vec!["K1", "K2"]),
                    Int(vec![-3, 0]),
                    Int(vec![-1, 1]),
                    Bool(vec![true, false]),
                    Float(vec![Some(0.5), Some(1.5)]),
                    MaybeText(vec![Some("1.5"), Some("NA")]),
                    Text(vec!["0.5", "M1"]),
                    Half(vec![[0x00, 0x3c], [0x00, 0xc0]]),
                    Double(vec![0.25, -1.0]),
                    Int(vec![1, 2]),
                ],
                vec![
                    Text(vec!["K3"]),
                    Int(vec![7]),
                    Int(vec![2]),
                    Bool(vec![true]),
                    Float(vec![None]),
                    MaybeText(vec![None]),
                    Text(vec!["M2"]),
                    Half(vec![[0x00, 0x38]]),
                    Double(vec![4.0]),
                    Int(vec![3]),
                ],
            ],
        );
        let key = |k: &[&str]| k.iter().map(|k| k.to_string()).collect::<Vec<_>>();
        let t = read_keyed(
            &path,
            &key(&["s", "i", "u", "b"]),
            &["i", "b", "f", "t", "w", "h", "d"],
        )
        .unwrap();
        assert_eq!(
            t.keys,
            ["K1\t-3\t4294967295\t1", "K2\t0\t1\t0", "K3\t7\t2\t1"]
        );
        let values: Vec<Vec<String>> = t.numbers.iter().map(|n| shown(&n.values)).collect();
        let want = [
            vec![-3.0, 0.0, 7.0],
            vec![1.0, 0.0, 1.0],
            vec![0.5, 1.5, f64::NAN],
            vec![1.5, f64::NAN, f64::NAN],
            vec![0.5, f64::NAN, f64::NAN],
            vec![1.0, -2.0, 0.5],
            vec![0.25, -1.0, 4.0],
        ];
        assert_eq!(values, want.map(|v| shown(&v)));
        let bad: Vec<_> = t.numbers.iter().map(|n| n.bad.clone()).collect();
        assert_eq!(bad[4], [(1, "M1".to_string()), (2, "M2".to_string())]);
        assert!(bad.iter().enumerate().all(|(j, b)| j == 4 || b.is_empty()));
        assert_eq!(t.numbers[4].first_bad(|row| row > 1), Some((2, "M2")));

        let refused =
            |k: &[&str], n: &[&str]| format!("{:#}", read_keyed(&path, &key(k), n).unwrap_err());
        let e = refused(&["t"], &[]);
        assert!(e.contains("row 3 of t: a null, which is no key"), "{e}");
        for float in ["f", "d", "h"] {
            let e = refused(&[float], &[]);
            assert!(e.contains("a float, which is no key"), "{e}");
        }
        let e = refused(&["s"], &["day"]);
        assert!(e.contains("cannot read day, INT32 DATE"), "{e}");
    }
}
