//! The tables the tools write, read by column name: TSV with a header,
//! gzipped or not, and parquet. The format is told by the file's first
//! bytes, not its name.

use anyhow::{bail, Context, Result};
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Cursor, Read, Write};
use std::path::Path;

/// One cell, as the file holds it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Int(i64),
    Float(f64),
    /// A parquet null.
    Missing,
}

/// Text cells that mean no value.
const MISSING: &[&str] = &["", "NA", "na", "N/A", "null", "NULL", "None"];

impl Value {
    /// The cell as a key, onto `key`: text as written, an integer in
    /// decimal (as a TSV writes it). A float or a missing key is refused: two
    /// spellings of one float would not join.
    pub fn push_key(&self, key: &mut String) -> Result<()> {
        use std::fmt::Write as _;
        match self {
            Value::Text(s) => key.push_str(s),
            Value::Int(i) => write!(key, "{i}")?,
            Value::Float(_) | Value::Missing => bail!("not a text or integer key: {self:?}"),
        }
        Ok(())
    }

    /// The cell as a number: `None` when missing (an empty cell, `NA`,
    /// `null`, ...). Text that is no number is an error.
    pub fn number(&self) -> Result<Option<f64>> {
        Ok(match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(x) => Some(*x),
            Value::Missing => None,
            Value::Text(s) if MISSING.contains(&s.as_str()) => None,
            Value::Text(s) => Some(
                s.parse()
                    .with_context(|| format!("{s:?} is not a number"))?,
            ),
        })
    }
}

/// The columns `want` of the table at `path`, in that order, each a value
/// per row.
pub fn read(path: &Path, want: &[&str]) -> Result<Vec<Vec<Value>>> {
    let (head, file) = open_peeked(path)?;
    let columns = if head == b"PAR1" {
        read_parquet(file, want)
    } else {
        read_tsv(text(head, file), want)
    };
    columns.with_context(|| format!("reading {}", path.display()))
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

/// A text file being written, gzipped when its name ends in `.gz`.
pub enum TextOut {
    Plain(BufWriter<File>),
    // Boxed: the encoder holds its buffers inline.
    Gz(Box<GzEncoder<BufWriter<File>>>),
}

impl TextOut {
    pub fn create(path: &Path) -> Result<Self> {
        let file = BufWriter::new(
            File::create(path).with_context(|| format!("cannot write {}", path.display()))?,
        );
        Ok(if path.extension().is_some_and(|e| e == "gz") {
            TextOut::Gz(Box::new(GzEncoder::new(file, Default::default())))
        } else {
            TextOut::Plain(file)
        })
    }

    /// Write what is left, the gzip trailer too, and close.
    pub fn finish(self) -> Result<()> {
        match self {
            TextOut::Plain(mut f) => f.flush()?,
            TextOut::Gz(gz) => gz.finish()?.flush()?,
        }
        Ok(())
    }
}

impl Write for TextOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            TextOut::Plain(f) => f.write(buf),
            TextOut::Gz(gz) => gz.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            TextOut::Plain(f) => f.flush(),
            TextOut::Gz(gz) => gz.flush(),
        }
    }
}

/// Where each of `want` is among `columns`.
fn positions(columns: &[String], want: &[&str]) -> Result<Vec<usize>> {
    want.iter()
        .map(|w| {
            columns
                .iter()
                .position(|c| c == w)
                .with_context(|| format!("no column {w:?}; the columns are {}", columns.join(", ")))
        })
        .collect()
}

fn read_tsv(text: Box<dyn BufRead>, want: &[&str]) -> Result<Vec<Vec<Value>>> {
    let mut lines = text.lines();
    let header = lines.next().context("empty, with no header")??;
    let header: Vec<String> = header
        .trim_end_matches('\r')
        .split('\t')
        .map(str::to_string)
        .collect();
    let at = positions(&header, want)?;
    let mut columns = vec![Vec::new(); want.len()];
    for (i, line) in lines.enumerate() {
        let line = line?;
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let cells: Vec<&str> = line.split('\t').collect();
        for (col, &j) in columns.iter_mut().zip(&at) {
            let cell = cells.get(j).with_context(|| {
                format!(
                    "line {} has {} cells, short of {}",
                    i + 2,
                    cells.len(),
                    header[j]
                )
            })?;
            col.push(Value::Text(cell.to_string()));
        }
    }
    Ok(columns)
}

/// The columns `want` of the parquet `file`, read by seeking, so a regular
/// file only.
fn read_parquet(file: File, want: &[&str]) -> Result<Vec<Vec<Value>>> {
    if !file.metadata()?.is_file() {
        bail!(
            "parquet is read by seeking, and this is not a regular file (a pipe?); write it to \
             a file first"
        );
    }
    let reader = SerializedFileReader::new(file)?;
    let schema = reader.metadata().file_metadata().schema_descr_ptr();
    let names: Vec<String> = schema
        .root_schema()
        .get_fields()
        .iter()
        .map(|f| f.name().to_string())
        .collect();
    let at = positions(&names, want)?;
    let mut columns = vec![Vec::new(); want.len()];
    for row in reader.get_row_iter(None)? {
        let row = row?;
        let fields: Vec<&Field> = row.get_column_iter().map(|(_, f)| f).collect();
        for (col, &j) in columns.iter_mut().zip(&at) {
            col.push(value(fields[j]).with_context(|| format!("column {}", names[j]))?);
        }
    }
    Ok(columns)
}

/// A parquet field as a cell.
fn value(f: &Field) -> Result<Value> {
    Ok(match f {
        Field::Null => Value::Missing,
        Field::Bool(b) => Value::Int(i64::from(*b)),
        Field::Byte(x) => Value::Int((*x).into()),
        Field::Short(x) => Value::Int((*x).into()),
        Field::Int(x) => Value::Int((*x).into()),
        Field::Long(x) => Value::Int(*x),
        Field::UByte(x) => Value::Int((*x).into()),
        Field::UShort(x) => Value::Int((*x).into()),
        Field::UInt(x) => Value::Int((*x).into()),
        Field::ULong(x) => Value::Int(i64::try_from(*x)?),
        Field::Float16(x) => Value::Float(x.to_f64()),
        Field::Float(x) => Value::Float((*x).into()),
        Field::Double(x) => Value::Float(*x),
        Field::Str(s) => Value::Text(s.clone()),
        other => bail!("cannot read a {other:?} cell"),
    })
}
