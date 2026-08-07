//! Reading spreadsheets someone else made.
//!
//! `calamine` handles the binary and OpenDocument formats behind one entry
//! point. Delimited text (`.csv`, `.tsv`) is parsed here because calamine has
//! no reader for it, and because a CSV carries no types at all — the values
//! arrive as strings and something has to decide which of them are numbers.
//! That guessing is deliberately conservative and documented at
//! [`infer_delimited_cell`].
//!
//! Every read is bounded. A spreadsheet is the one document format where a
//! file that looks small can hold a million rows, and an agent that reads all
//! of them has spent its context on padding.

use super::{CellValue, Sheet, Workbook};
use crate::office::DocumentError;
use calamine::{Data, ExcelDateTime, Range, Reader};
use std::path::Path;

/// Extensions this module can read, in the order a user would recognise them.
/// Used in the error a caller sees when they hand over something else.
const SUPPORTED: &str = ".xlsx, .xlsm, .xls, .xlsb, .ods, .csv and .tsv";

/// Cap on how much delimited text is decoded before the reader gives up on
/// the rest. Large enough for any hand-made CSV; small enough that a runaway
/// database export cannot exhaust memory.
const MAX_DELIMITED_BYTES: usize = 64 * 1024 * 1024;

/// How much of a workbook to read before stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadLimits {
    /// Sheets to read, counted from the first. Later sheets are skipped.
    pub max_sheets: usize,
    /// Data rows to return per sheet, not counting a detected header row.
    pub max_rows_per_sheet: usize,
    /// Columns to return per sheet.
    pub max_cols: usize,
}

impl Default for ReadLimits {
    /// Sized against what an agent can actually use rather than what a
    /// spreadsheet can hold: 16 sheets covers workbooks that separate a month
    /// or a region per tab, 2,000 rows covers most sheets a person maintains
    /// by hand, and 128 columns is wider than nearly any of them. Callers who
    /// know they are summarising a large export should raise these
    /// deliberately.
    fn default() -> Self {
        Self {
            max_sheets: 16,
            max_rows_per_sheet: 2_000,
            max_cols: 128,
        }
    }
}

/// Read a spreadsheet into structured sheets.
///
/// The format is chosen from the extension. Binary and OpenDocument
/// workbooks go through calamine; `.csv` and `.tsv` are parsed here into a
/// single sheet named after the file.
///
/// Each sheet's `total_rows` counts the data rows the sheet holds after
/// trailing blank rows are trimmed and after a header row (if one was
/// detected) is set aside, so it is directly comparable with `rows.len()`
/// and is larger than it exactly when a limit cut the read short.
pub fn read_spreadsheet(path: &Path, limits: ReadLimits) -> Result<Workbook, DocumentError> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match extension.as_str() {
        "csv" => read_delimited(path, ',', limits),
        "tsv" => read_delimited(path, '\t', limits),
        // `xlam` and `xla` are add-in workbooks; calamine opens them as the
        // spreadsheets they are, so there is no reason to refuse them.
        "xlsx" | "xlsm" | "xlam" | "xls" | "xla" | "xlsb" | "ods" => read_workbook(path, limits),
        "" => Err(read_error(
            path,
            format!("the file has no extension, so its format is unknown. Commonspace reads {SUPPORTED}."),
        )),
        other => Err(read_error(
            path,
            format!("Commonspace cannot read .{other} files as spreadsheets. It reads {SUPPORTED} — opening this in a spreadsheet program and saving it as .xlsx or .csv would work."),
        )),
    }
}

/// Whether a sheet's first row reads as a header rather than as data.
///
/// This is a heuristic, and it is wrong sometimes — a spreadsheet does not
/// record which row is a header, so there is nothing better available. The
/// exact rule is:
///
/// 1. every non-empty cell in the first row is text, and
/// 2. the first row has at least one non-empty cell, and
/// 3. there is no second row, or the second row has at least one cell that is
///    neither text nor empty.
///
/// Rule 3 is what carries it: numbers or dates sitting directly under words
/// is the shape of labelled data. When the second row is also all text the
/// answer is `false`, because a sheet of names and addresses looks exactly
/// like a header followed by nothing, and treating real data as labels loses
/// a row.
pub fn header_looks_real(first: &[CellValue], second: Option<&[CellValue]>) -> bool {
    let mut saw_text = false;
    for cell in first {
        match cell {
            CellValue::Empty => {}
            CellValue::Text { .. } => saw_text = true,
            _ => return false,
        }
    }
    if !saw_text {
        return false;
    }

    match second {
        None => true,
        Some(row) => row
            .iter()
            .any(|cell| !matches!(cell, CellValue::Text { .. } | CellValue::Empty)),
    }
}

/// Open a workbook through calamine and turn each sheet into a [`Sheet`].
fn read_workbook(path: &Path, limits: ReadLimits) -> Result<Workbook, DocumentError> {
    let mut book = calamine::open_workbook_auto(path).map_err(|e| open_failure(path, &e))?;

    let names = book.sheet_names();
    let mut sheets = Vec::new();
    for name in names.into_iter().take(limits.max_sheets) {
        let values = book
            .worksheet_range(&name)
            .map_err(|e| read_error(path, format!("the sheet {name:?} could not be read: {e}")))?;
        // Formulas are an extra, not the point. A workbook whose formula
        // records are damaged still has perfectly good values in it, so a
        // failure here costs the formula text and nothing else.
        let formulas = book.worksheet_formula(&name).ok();
        sheets.push(sheet_from_range(&name, &values, formulas.as_ref(), limits));
    }

    Ok(Workbook { sheets })
}

/// Convert one calamine range (plus its formulas, when they were readable)
/// into a [`Sheet`], trimming trailing blanks and applying the row and column
/// limits.
fn sheet_from_range(
    name: &str,
    values: &Range<Data>,
    formulas: Option<&Range<String>>,
    limits: ReadLimits,
) -> Sheet {
    let Some((row0, col0)) = values.start() else {
        return empty_sheet(name);
    };

    let formula_at = |row: usize, col: usize| -> Option<&str> {
        formulas
            .and_then(|f| f.get_value((row0 + row as u32, col0 + col as u32)))
            .map(String::as_str)
            .filter(|f| !f.is_empty())
    };

    // Spreadsheets routinely report a used range far larger than the real
    // data: a cell that was formatted once and then cleared still occupies a
    // record in the file. Find the last row and column that hold something
    // before converting anything, so `total_rows` describes the data rather
    // than the file's bookkeeping.
    let mut used_rows = 0usize;
    let mut used_cols = 0usize;
    for (row, cells) in values.rows().enumerate() {
        for (col, cell) in cells.iter().enumerate() {
            if has_content(cell) || formula_at(row, col).is_some() {
                used_rows = row + 1;
                used_cols = used_cols.max(col + 1);
            }
        }
    }
    if used_rows == 0 {
        return empty_sheet(name);
    }

    let cols = used_cols.min(limits.max_cols);
    // One row more than the limit allows: the header decision needs to see a
    // second row, and if there turns out to be no header that extra row is
    // dropped again below.
    let row_budget = limits.max_rows_per_sheet.saturating_add(1);

    let mut converted: Vec<Vec<CellValue>> = Vec::new();
    for (row, cells) in values.rows().enumerate().take(used_rows) {
        if converted.len() == row_budget {
            break;
        }
        converted.push(
            cells
                .iter()
                .take(cols)
                .enumerate()
                .map(|(col, cell)| cell_value(cell, formula_at(row, col)))
                .collect(),
        );
    }

    finish_sheet(name, converted, used_rows, used_cols, limits)
}

/// Assemble a sheet from converted rows: split off a header if the first row
/// looks like one, apply the row limit, and record what was cut.
fn finish_sheet(
    name: &str,
    mut converted: Vec<Vec<CellValue>>,
    used_rows: usize,
    used_cols: usize,
    limits: ReadLimits,
) -> Sheet {
    let has_header = match converted.split_first() {
        Some((first, rest)) => header_looks_real(first, rest.first().map(Vec::as_slice)),
        None => false,
    };

    let headers = if has_header {
        converted
            .remove(0)
            .into_iter()
            .map(|cell| match cell {
                CellValue::Text { value } => value,
                // `header_looks_real` only says yes when every non-empty cell
                // is text, so the rest are blanks in an unlabelled column.
                _ => String::new(),
            })
            .collect()
    } else {
        Vec::new()
    };

    let total_rows = used_rows - usize::from(has_header);
    converted.truncate(limits.max_rows_per_sheet);
    let truncated = converted.len() < total_rows || used_cols > limits.max_cols;

    Sheet {
        name: name.to_string(),
        headers,
        rows: converted,
        total_rows,
        truncated,
    }
}

fn empty_sheet(name: &str) -> Sheet {
    Sheet {
        name: name.to_string(),
        headers: Vec::new(),
        rows: Vec::new(),
        total_rows: 0,
        truncated: false,
    }
}

/// Whether a raw cell holds anything worth keeping a row or column for.
///
/// `Data::Error` counts as nothing. A cell showing `#REF!` or `#DIV/0!` has
/// no value — the formula that should have produced one failed — so a
/// trailing block of them is padding, not data.
fn has_content(cell: &Data) -> bool {
    !matches!(cell, Data::Empty | Data::Error(_))
}

/// Map one calamine cell onto a [`CellValue`], pairing it with its formula
/// text when the cell has one.
fn cell_value(cell: &Data, formula: Option<&str>) -> CellValue {
    let value = plain_value(cell);
    match formula {
        Some(formula) => CellValue::Formula {
            // calamine reports the formula as it is stored, without the
            // leading `=` a person types. Putting it back makes the string
            // paste straight into a spreadsheet.
            formula: if formula.starts_with('=') {
                formula.to_string()
            } else {
                format!("={formula}")
            },
            value: render(&value),
        },
        None => value,
    }
}

/// Map one calamine cell onto a [`CellValue`], ignoring formulas.
fn plain_value(cell: &Data) -> CellValue {
    match cell {
        Data::Empty => CellValue::Empty,
        Data::String(value) => CellValue::Text {
            value: value.clone(),
        },
        Data::Bool(value) => CellValue::Bool { value: *value },
        Data::Int(value) => CellValue::Number {
            value: *value as f64,
        },
        Data::Float(value) => CellValue::Number { value: *value },
        // A date is a number plus a display format, and calamine has already
        // done the reading of both. When its conversion does not land on a
        // real date — an out-of-range serial, or the 1900-02-29 that Excel
        // believes in and the calendar does not — the number is reported as a
        // number. Inventing a plausible date would be worse than saying less.
        Data::DateTime(value) => match iso_from_excel(value) {
            Some(iso) => CellValue::Date { value: iso },
            None => CellValue::Number {
                value: value.as_f64(),
            },
        },
        // Already ISO-8601 in the file; nothing to convert.
        Data::DateTimeIso(value) | Data::DurationIso(value) => CellValue::Date {
            value: value.clone(),
        },
        Data::Error(_) => CellValue::Empty,
    }
}

/// Render a cell as the string a person would see in the sheet. Used for the
/// cached side of a formula, which the API carries as text.
fn render(value: &CellValue) -> String {
    match value {
        CellValue::Empty => String::new(),
        CellValue::Text { value } => value.clone(),
        CellValue::Number { value } => value.to_string(),
        // Spreadsheets display booleans in capitals, and a formula's cached
        // result is what the sheet displays.
        CellValue::Bool { value } => if *value { "TRUE" } else { "FALSE" }.to_string(),
        CellValue::Date { value } => value.clone(),
        CellValue::Formula { value, .. } => value.clone(),
    }
}

/// Convert an Excel date/time into ISO-8601, or `None` when the value is not
/// a date the calendar recognises.
fn iso_from_excel(value: &ExcelDateTime) -> Option<String> {
    if value.is_duration() {
        return iso_duration(value.as_f64());
    }

    // calamine owns the epoch rules — 1900 versus 1904, and Excel's phantom
    // leap day — so the components come from it rather than from arithmetic
    // here. It converts unconditionally though, including to dates that do
    // not exist, which is what the validation below is for.
    let (year, month, day, hour, minute, second, milli) = value.to_ymd_hms_milli();
    chrono::NaiveDate::from_ymd_opt(i32::from(year), u32::from(month), u32::from(day))?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    if hour == 0 && minute == 0 && second == 0 && milli == 0 {
        Some(format!("{year:04}-{month:02}-{day:02}"))
    } else if milli == 0 {
        Some(format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}"
        ))
    } else {
        Some(format!(
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{milli:03}"
        ))
    }
}

/// Render an Excel duration — a count of days, usually fractional — as an
/// ISO-8601 duration such as `PT1H30M`. A duration is a span rather than a
/// point in time, so it cannot honestly be written as a date.
fn iso_duration(days: f64) -> Option<String> {
    if !days.is_finite() {
        return None;
    }
    let total_ms = (days * 86_400_000.0).round();
    // Beyond this a "duration" is a corrupt number, not a length of time
    // anyone recorded. `u64` would still hold it; the reader should not
    // pretend it means something.
    if total_ms.abs() >= 1e15 {
        return None;
    }

    let mut remaining = total_ms.abs() as u64;
    let hours = remaining / 3_600_000;
    remaining %= 3_600_000;
    let minutes = remaining / 60_000;
    remaining %= 60_000;
    let seconds = remaining / 1_000;
    let milli = remaining % 1_000;

    let mut out = String::from(if total_ms < 0.0 { "-PT" } else { "PT" });
    if hours > 0 {
        out.push_str(&format!("{hours}H"));
    }
    if minutes > 0 {
        out.push_str(&format!("{minutes}M"));
    }
    // A zero-length duration still needs a component to be valid ISO-8601.
    if seconds > 0 || milli > 0 || (hours == 0 && minutes == 0) {
        if milli == 0 {
            out.push_str(&format!("{seconds}S"));
        } else {
            out.push_str(&format!("{seconds}.{milli:03}S"));
        }
    }
    Some(out)
}

/// Read a `.csv` or `.tsv` into a single sheet named after the file.
fn read_delimited(
    path: &Path,
    delimiter: char,
    limits: ReadLimits,
) -> Result<Workbook, DocumentError> {
    // Text files off a desktop are not reliably UTF-8; `textio` sniffs the
    // BOM first and falls back to statistical detection, and its decode
    // strips the BOM rather than leaving it glued to the first header.
    let decoded = crate::textio::read_text(path, MAX_DELIMITED_BYTES).map_err(|e| {
        read_error(
            path,
            format!("the file could not be opened for reading: {e}"),
        )
    })?;

    let mut records = parse_delimited(&decoded.content, delimiter);
    if decoded.truncated {
        // The read stopped mid-file, so the last record is probably a
        // fragment. Dropping it is better than reporting half a row.
        records.pop();
    }

    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Sheet1".to_string());

    let used_cols = records.iter().map(Vec::len).max().unwrap_or(0);
    let cols = used_cols.min(limits.max_cols);
    let used_rows = records.len();
    let row_budget = limits.max_rows_per_sheet.saturating_add(1);

    let converted: Vec<Vec<CellValue>> = records
        .into_iter()
        .take(row_budget)
        .map(|record| {
            record
                .into_iter()
                .take(cols)
                .map(|field| infer_delimited_cell(&field))
                .collect()
        })
        .collect();

    let mut sheet = finish_sheet(&name, converted, used_rows, used_cols, limits);
    sheet.truncated = sheet.truncated || decoded.truncated;
    Ok(Workbook {
        sheets: vec![sheet],
    })
}

/// Split delimited text into records, following RFC 4180: a field may be
/// wrapped in double quotes, in which case it can contain the delimiter,
/// newlines, and doubled quotes standing for a literal one.
///
/// Trailing blank rows are not produced — a file ending in a newline yields
/// no final empty record — and `\n`, `\r\n` and a lone `\r` all end a record,
/// because files arrive from all three families of machine.
fn parse_delimited(text: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }

        match c {
            // A quote only opens a field at the field's start; anywhere else
            // it is a character someone actually typed.
            '"' if field.is_empty() => quoted = true,
            _ if c == delimiter => record.push(std::mem::take(&mut field)),
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
            }
            _ => field.push(c),
        }
    }

    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        records.push(record);
    }
    records
}

/// Guess what a delimited field means. Delimited text has no types, so this
/// is a guess, and it errs towards text:
///
/// - `TRUE`/`FALSE` in any casing is a boolean;
/// - a plain ISO date or date-time is a date;
/// - anything Rust parses as a finite number is a number, *unless* it has a
///   leading zero, because `01730` is a postcode or an account number far
///   more often than it is the quantity 1730;
/// - everything else, including blank, stays as it came.
fn infer_delimited_cell(field: &str) -> CellValue {
    let trimmed = field.trim();
    if trimmed.is_empty() {
        return CellValue::Empty;
    }

    if trimmed.eq_ignore_ascii_case("true") {
        return CellValue::Bool { value: true };
    }
    if trimmed.eq_ignore_ascii_case("false") {
        return CellValue::Bool { value: false };
    }

    if chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d").is_ok()
        || chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%dT%H:%M:%S").is_ok()
        || chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S").is_ok()
    {
        return CellValue::Date {
            value: trimmed.to_string(),
        };
    }

    let digits = trimmed.trim_start_matches(['-', '+']);
    let leading_zero = digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.");
    if !leading_zero {
        if let Ok(value) = trimmed.parse::<f64>() {
            if value.is_finite() {
                return CellValue::Number { value };
            }
        }
    }

    CellValue::Text {
        value: field.to_string(),
    }
}

/// Turn a calamine open failure into something a person can act on.
fn open_failure(path: &Path, error: &calamine::Error) -> DocumentError {
    let detail = match error {
        calamine::Error::Io(e) => format!("the file could not be opened for reading: {e}"),
        other => {
            let text = other.to_string();
            if text.to_lowercase().contains("password") {
                "the file is protected with a password. Open it in a spreadsheet program, save an unprotected copy, and try that instead.".to_string()
            } else {
                format!("the file is not a spreadsheet Commonspace can open, or it is damaged ({text}). Commonspace reads {SUPPORTED}.")
            }
        }
    };
    read_error(path, detail)
}

fn read_error(path: &Path, detail: impl Into<String>) -> DocumentError {
    DocumentError::Read {
        path: path.display().to_string(),
        detail: detail.into(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// One cell of a test fixture.
    enum Cell {
        Str(&'static str),
        Num(f64),
        Bool(bool),
        /// Year, month, day.
        Date(u16, u8, u8),
        /// Formula text and the cached result stored beside it.
        Formula(&'static str, &'static str),
        Blank,
    }

    /// Write an XLSX fixture with `rust_xlsxwriter`.
    ///
    /// This is a convenience for building a real workbook at test time so no
    /// binary file has to live in the repository. It is not the round-trip
    /// validation contract — that belongs to the writer's own tests, which
    /// check that what `create_xlsx` produces re-opens correctly. Here the
    /// writer is only a fixture generator.
    fn write_fixture(path: &Path, sheets: &[(&str, Vec<Vec<Cell>>)]) {
        use rust_xlsxwriter::{ExcelDateTime, Format, Formula, Workbook};

        let mut workbook = Workbook::new();
        let date_format = Format::new().set_num_format("yyyy-mm-dd");

        for (name, rows) in sheets {
            let sheet = workbook.add_worksheet();
            sheet.set_name(*name).unwrap();
            for (row_index, row) in rows.iter().enumerate() {
                for (col_index, cell) in row.iter().enumerate() {
                    let r = row_index as u32;
                    let c = col_index as u16;
                    match cell {
                        Cell::Str(value) => {
                            sheet.write_string(r, c, *value).unwrap();
                        }
                        Cell::Num(value) => {
                            sheet.write_number(r, c, *value).unwrap();
                        }
                        Cell::Bool(value) => {
                            sheet.write_boolean(r, c, *value).unwrap();
                        }
                        Cell::Date(year, month, day) => {
                            let date = ExcelDateTime::from_ymd(*year, *month, *day).unwrap();
                            sheet
                                .write_datetime_with_format(r, c, &date, &date_format)
                                .unwrap();
                        }
                        Cell::Formula(formula, cached) => {
                            sheet
                                .write_formula(r, c, Formula::new(*formula).set_result(*cached))
                                .unwrap();
                        }
                        Cell::Blank => {}
                    }
                }
            }
        }

        workbook.save(path).unwrap();
    }

    fn text(value: &str) -> CellValue {
        CellValue::Text {
            value: value.to_string(),
        }
    }

    fn number(value: f64) -> CellValue {
        CellValue::Number { value }
    }

    fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn simple_sheet_has_a_header_and_typed_rows() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sales.xlsx");
        write_fixture(
            &path,
            &[(
                "Sales",
                vec![
                    vec![
                        Cell::Str("Customer"),
                        Cell::Str("Amount"),
                        Cell::Str("Paid"),
                    ],
                    vec![Cell::Str("Acme"), Cell::Num(1240.0), Cell::Bool(true)],
                    vec![Cell::Str("Globex"), Cell::Num(980.5), Cell::Bool(false)],
                ],
            )],
        );

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        assert_eq!(book.sheets.len(), 1);
        let sheet = &book.sheets[0];
        assert_eq!(sheet.name, "Sales");
        assert_eq!(sheet.headers, vec!["Customer", "Amount", "Paid"]);
        assert_eq!(sheet.total_rows, 2);
        assert!(!sheet.truncated);
        assert_eq!(
            sheet.rows[0],
            vec![
                text("Acme"),
                number(1240.0),
                CellValue::Bool { value: true }
            ]
        );
        assert_eq!(
            sheet.rows[1],
            vec![
                text("Globex"),
                number(980.5),
                CellValue::Bool { value: false }
            ]
        );
    }

    #[test]
    fn all_text_sheet_is_not_treated_as_headed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("contacts.xlsx");
        write_fixture(
            &path,
            &[(
                "Contacts",
                vec![
                    vec![Cell::Str("Ada Lovelace"), Cell::Str("London")],
                    vec![Cell::Str("Grace Hopper"), Cell::Str("Arlington")],
                ],
            )],
        );

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        let sheet = &book.sheets[0];
        assert!(sheet.headers.is_empty());
        assert_eq!(sheet.total_rows, 2);
        assert_eq!(sheet.rows.len(), 2);
        assert_eq!(sheet.rows[0][0], text("Ada Lovelace"));
    }

    #[test]
    fn header_heuristic_rules() {
        // Text over numbers: labels.
        assert!(header_looks_real(
            &[text("Name"), text("Total")],
            Some(&[text("Acme"), number(12.0)])
        ));
        // Text over text: two rows of data.
        assert!(!header_looks_real(
            &[text("Ada"), text("London")],
            Some(&[text("Grace"), text("Arlington")])
        ));
        // A lone text row has nothing below to contradict it.
        assert!(header_looks_real(&[text("Name")], None));
        // A number in the first row is not a label.
        assert!(!header_looks_real(
            &[text("Name"), number(1.0)],
            Some(&[text("Acme"), number(12.0)])
        ));
        // An entirely blank row labels nothing.
        assert!(!header_looks_real(
            &[CellValue::Empty, CellValue::Empty],
            Some(&[number(1.0)])
        ));
        // Blanks in the second row do not count as "not text".
        assert!(!header_looks_real(
            &[text("Name"), text("City")],
            Some(&[text("Ada"), CellValue::Empty])
        ));
    }

    #[test]
    fn dates_become_iso_strings() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("dates.xlsx");
        write_fixture(
            &path,
            &[(
                "Dates",
                vec![
                    vec![Cell::Str("When"), Cell::Str("Count")],
                    vec![Cell::Date(2026, 8, 7), Cell::Num(3.0)],
                    vec![Cell::Date(1999, 12, 31), Cell::Num(4.0)],
                ],
            )],
        );

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(
            sheet.rows[0][0],
            CellValue::Date {
                value: "2026-08-07".to_string()
            }
        );
        assert_eq!(
            sheet.rows[1][0],
            CellValue::Date {
                value: "1999-12-31".to_string()
            }
        );
    }

    #[test]
    fn formulas_carry_their_cached_result() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("totals.xlsx");
        write_fixture(
            &path,
            &[(
                "Totals",
                vec![
                    vec![Cell::Str("Item"), Cell::Str("Amount")],
                    vec![Cell::Str("Acme"), Cell::Num(10.0)],
                    vec![Cell::Str("Globex"), Cell::Num(20.0)],
                    vec![Cell::Str("Total"), Cell::Formula("SUM(B2:B3)", "30")],
                ],
            )],
        );

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(
            sheet.rows[2][1],
            CellValue::Formula {
                formula: "=SUM(B2:B3)".to_string(),
                value: "30".to_string(),
            }
        );
    }

    #[test]
    fn multiple_sheets_are_read_in_order_and_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("book.xlsx");
        write_fixture(
            &path,
            &[
                ("January", vec![vec![Cell::Str("Jan"), Cell::Num(1.0)]]),
                ("February", vec![vec![Cell::Str("Feb"), Cell::Num(2.0)]]),
                ("March", vec![vec![Cell::Str("Mar"), Cell::Num(3.0)]]),
            ],
        );

        let all = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        let names: Vec<&str> = all.sheets.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["January", "February", "March"]);

        let capped = read_spreadsheet(
            &path,
            ReadLimits {
                max_sheets: 2,
                ..ReadLimits::default()
            },
        )
        .unwrap();
        assert_eq!(capped.sheets.len(), 2);
        assert_eq!(capped.sheets[1].name, "February");
    }

    #[test]
    fn row_limit_truncates_but_total_rows_stays_true() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("long.xlsx");
        let mut rows = vec![vec![Cell::Str("Index"), Cell::Str("Value")]];
        for i in 0..50 {
            rows.push(vec![Cell::Num(f64::from(i)), Cell::Num(f64::from(i) * 2.0)]);
        }
        write_fixture(&path, &[("Long", rows)]);

        let book = read_spreadsheet(
            &path,
            ReadLimits {
                max_rows_per_sheet: 10,
                ..ReadLimits::default()
            },
        )
        .unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(sheet.headers, vec!["Index", "Value"]);
        assert_eq!(sheet.rows.len(), 10);
        assert_eq!(sheet.total_rows, 50);
        assert!(sheet.truncated);
        assert_eq!(sheet.rows[9][0], number(9.0));
    }

    #[test]
    fn column_limit_truncates() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("wide.xlsx");
        write_fixture(
            &path,
            &[(
                "Wide",
                vec![
                    vec![Cell::Str("a"), Cell::Str("b"), Cell::Str("c")],
                    vec![Cell::Num(1.0), Cell::Num(2.0), Cell::Num(3.0)],
                ],
            )],
        );

        let book = read_spreadsheet(
            &path,
            ReadLimits {
                max_cols: 2,
                ..ReadLimits::default()
            },
        )
        .unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(sheet.headers, vec!["a", "b"]);
        assert_eq!(sheet.rows[0], vec![number(1.0), number(2.0)]);
        assert!(sheet.truncated);
    }

    #[test]
    fn trailing_empty_rows_and_columns_are_trimmed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("padded.xlsx");
        write_fixture(
            &path,
            &[(
                "Padded",
                vec![
                    vec![Cell::Str("Name"), Cell::Str("Total"), Cell::Blank],
                    vec![Cell::Str("Acme"), Cell::Num(5.0), Cell::Blank],
                    // Written as blanks so the used range extends past the
                    // data, the way a cleared-but-formatted block does.
                    vec![Cell::Blank, Cell::Blank, Cell::Blank],
                    vec![Cell::Blank, Cell::Blank, Cell::Blank],
                ],
            )],
        );

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(sheet.total_rows, 1);
        assert_eq!(sheet.rows.len(), 1);
        assert_eq!(sheet.headers, vec!["Name", "Total"]);
        assert_eq!(sheet.rows[0].len(), 2);
        assert!(!sheet.truncated);
    }

    #[test]
    fn empty_sheet_reads_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("blank.xlsx");
        write_fixture(&path, &[("Blank", vec![])]);

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(sheet.name, "Blank");
        assert!(sheet.headers.is_empty());
        assert!(sheet.rows.is_empty());
        assert_eq!(sheet.total_rows, 0);
        assert!(!sheet.truncated);
    }

    #[test]
    fn csv_handles_quoted_commas_and_newlines() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_file(
            tmp.path(),
            "notes.csv",
            b"Customer,Note,Amount\n\
              \"Acme, Inc.\",\"line one\nline two\",1240\n\
              Globex,\"he said \"\"yes\"\"\",980\n",
        );

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(sheet.name, "notes");
        assert_eq!(sheet.headers, vec!["Customer", "Note", "Amount"]);
        assert_eq!(sheet.total_rows, 2);
        assert_eq!(sheet.rows[0][0], text("Acme, Inc."));
        assert_eq!(sheet.rows[0][1], text("line one\nline two"));
        assert_eq!(sheet.rows[0][2], number(1240.0));
        assert_eq!(sheet.rows[1][1], text("he said \"yes\""));
    }

    #[test]
    fn csv_with_a_utf8_bom_does_not_poison_the_first_header() {
        let tmp = tempfile::tempdir().unwrap();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("Name,Total\nAcme,5\n".as_bytes());
        let path = write_file(tmp.path(), "bom.csv", &bytes);

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(sheet.headers, vec!["Name", "Total"]);
        assert_eq!(sheet.rows[0], vec![text("Acme"), number(5.0)]);
    }

    #[test]
    fn tsv_uses_tabs_and_infers_types() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_file(
            tmp.path(),
            "rows.tsv",
            b"Name\tWhen\tActive\tCode\nAcme\t2026-08-07\tTRUE\t01730\n",
        );

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        let sheet = &book.sheets[0];
        assert_eq!(sheet.headers, vec!["Name", "When", "Active", "Code"]);
        assert_eq!(
            sheet.rows[0],
            vec![
                text("Acme"),
                CellValue::Date {
                    value: "2026-08-07".to_string()
                },
                CellValue::Bool { value: true },
                // A leading zero means an identifier, not a quantity.
                text("01730"),
            ]
        );
    }

    #[test]
    fn empty_csv_is_an_empty_sheet() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_file(tmp.path(), "nothing.csv", b"");

        let book = read_spreadsheet(&path, ReadLimits::default()).unwrap();
        assert_eq!(book.sheets.len(), 1);
        assert!(book.sheets[0].rows.is_empty());
        assert_eq!(book.sheets[0].total_rows, 0);
    }

    #[test]
    fn malformed_workbook_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_file(
            tmp.path(),
            "broken.xlsx",
            b"this is definitely not a zip archive",
        );

        let error = read_spreadsheet(&path, ReadLimits::default()).unwrap_err();
        match error {
            DocumentError::Read { detail, .. } => {
                assert!(detail.contains("not a spreadsheet"), "detail: {detail}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn empty_file_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_file(tmp.path(), "empty.xlsx", b"");
        assert!(matches!(
            read_spreadsheet(&path, ReadLimits::default()),
            Err(DocumentError::Read { .. })
        ));
    }

    #[test]
    fn unsupported_extension_names_what_is_supported() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_file(tmp.path(), "notes.pdf", b"%PDF-1.7");

        let error = read_spreadsheet(&path, ReadLimits::default()).unwrap_err();
        match error {
            DocumentError::Read { detail, .. } => {
                assert!(detail.contains(".xlsx"), "detail: {detail}");
                assert!(detail.contains(".csv"), "detail: {detail}");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn missing_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(read_spreadsheet(&tmp.path().join("gone.xlsx"), ReadLimits::default()).is_err());
        assert!(read_spreadsheet(&tmp.path().join("gone.csv"), ReadLimits::default()).is_err());
    }

    #[test]
    fn delimited_parsing_edge_cases() {
        assert_eq!(
            parse_delimited("a,b\r\nc,d\rE,f\n", ','),
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string(), "d".to_string()],
                vec!["E".to_string(), "f".to_string()],
            ]
        );
        // An empty quoted field is an empty field, not a quote character.
        assert_eq!(
            parse_delimited("\"\",x", ','),
            vec![vec![String::new(), "x".to_string()]]
        );
        // No trailing record from the final newline.
        assert_eq!(
            parse_delimited("only\n", ','),
            vec![vec!["only".to_string()]]
        );
        assert!(parse_delimited("", ',').is_empty());
    }

    #[test]
    fn durations_render_as_iso_durations() {
        assert_eq!(iso_duration(1.5 / 24.0).as_deref(), Some("PT1H30M"));
        assert_eq!(iso_duration(0.0).as_deref(), Some("PT0S"));
        assert_eq!(iso_duration(-1.0 / 24.0).as_deref(), Some("-PT1H"));
        assert_eq!(iso_duration(f64::NAN), None);
    }

    #[test]
    fn impossible_serial_dates_fall_back_to_numbers() {
        use calamine::ExcelDateTimeType;

        // Serial 60 is Excel's phantom 1900-02-29, a day that never happened.
        let phantom = ExcelDateTime::new(60.0, ExcelDateTimeType::DateTime, false);
        assert_eq!(
            plain_value(&Data::DateTime(phantom)),
            CellValue::Number { value: 60.0 }
        );

        // A real serial still converts.
        let real = ExcelDateTime::new(46246.0, ExcelDateTimeType::DateTime, false);
        assert!(matches!(
            plain_value(&Data::DateTime(real)),
            CellValue::Date { .. }
        ));
    }

    #[test]
    fn error_cells_read_as_empty() {
        assert_eq!(
            plain_value(&Data::Error(calamine::CellErrorType::Ref)),
            CellValue::Empty
        );
    }
}
