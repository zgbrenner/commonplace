//! Creating an .xlsx workbook that looks like a person made it.
//!
//! The caller describes columns, their formats, and rows. Everything that
//! makes a spreadsheet pleasant to receive — a styled header row, frozen
//! panes, an autofilter, sensible column widths, number formats applied down
//! the column — is decided here, so every workbook Commonspace produces looks
//! the same whichever agent asked for it.
//!
//! # Values keep their type
//!
//! A number is written as a number, a boolean as a boolean, a date as a real
//! Excel date serial, and a formula as a formula Excel will evaluate. Number
//! formats are presentation only. That is the whole reason to emit a
//! spreadsheet instead of a table of text: the recipient can sort, filter and
//! compute without first repairing the file.
//!
//! # Percentages
//!
//! [`ColumnFormat::Percent`] uses Excel's own percent format, which multiplies
//! the *displayed* value by 100. A stored `0.15` displays as `15%`. Values are
//! written exactly as the caller passed them — this layer never scales them —
//! so callers must pass the fraction, not the percentage. The alternative (a
//! literal `%` suffix on an unscaled number) would display `15` as `15%` but
//! produce a cell that is not a percentage to Excel, so `=A1*B1` would be
//! wrong by a factor of 100 and the error would surface far from here.

use super::{CellValue, ColumnFormat, NewSheet};
use crate::office::DocumentError;
use commonspace_core::{OperationResult, ValidationOutcome};
use rust_xlsxwriter::{
    Color, ExcelDateTime, Format, FormatAlign, FormatBorder, Formula, Workbook, Worksheet,
    XlsxError,
};
use std::path::Path;

/// Excel's hard limit on a sheet name.
const MAX_SHEET_NAME_CHARS: usize = 31;

/// Characters Excel refuses in a sheet name.
const ILLEGAL_SHEET_NAME_CHARS: [char; 7] = ['[', ']', ':', '*', '?', '/', '\\'];

/// Used when a sheet name sanitizes down to nothing at all.
const FALLBACK_SHEET_NAME: &str = "Sheet";

/// Excel accepts at most 30 decimal places in a number format.
const MAX_DECIMALS: u8 = 30;

/// Autofit stops here (in pixels). Excel's own autofit will happily make a
/// column 1790px wide for one long string, which is unusable on a laptop; a
/// long header wraps inside this width instead of pushing every other column
/// off screen.
const MAX_AUTOFIT_WIDTH_PIXELS: u32 = 300;

/// ISO order, four-digit year: unambiguous whatever locale opens the file.
const DATE_NUM_FORMAT: &str = "yyyy-mm-dd";

/// Used for values that carry a time, so writing `2026-08-07T14:30:00` does
/// not silently display as midnight.
const DATE_TIME_NUM_FORMAT: &str = "yyyy-mm-dd hh:mm:ss";

/// Excel's grid is 16,384 columns wide.
const MAX_COLUMNS: usize = 16_384;

/// Excel's grid is 1,048,576 rows deep, the first of which is the header.
const MAX_DATA_ROWS: usize = 1_048_575;

/// How many data rows validation checks cell by cell. Re-reading every cell of
/// a 100k-row export would cost more than it proves; the header row, the row
/// count, and this many rows of values are checked in full.
const VALIDATION_SAMPLE_ROWS: usize = 200;

/// Create an .xlsx workbook, then verify it re-opens and holds what was asked
/// for.
///
/// Each sheet gets a bold, filled, wrapped header row with a bottom border;
/// panes frozen below it; an autofilter across it; column widths taken from
/// [`NewColumn::width`](super::NewColumn::width) or fitted to the content; and
/// the column's [`ColumnFormat`] applied to the data below.
///
/// The caller is responsible for having passed the destination through the
/// permission engine; this function only writes and validates.
///
/// # Errors
///
/// - [`DocumentError::Write`] when there are no sheets, when a sheet has no
///   columns, when a row holds more values than the sheet has columns (row
///   indices in the message are 0-based into `NewSheet::rows`), or when the
///   file cannot be built. A row with *fewer* values is fine: the remaining
///   cells are left empty.
/// - [`DocumentError::Validation`] when the file was written but re-reading it
///   with an independent library does not find what was asked for.
pub fn create_xlsx(path: &Path, sheets: &[NewSheet]) -> Result<OperationResult, DocumentError> {
    // Everything the request itself can be wrong about is checked before a
    // file exists, so a rejected request never leaves half a workbook behind.
    check_request(path, sheets)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let names = unique_sheet_names(sheets);
    let mut workbook = Workbook::new();
    for (sheet, name) in sheets.iter().zip(&names) {
        let worksheet = workbook.add_worksheet();
        build_sheet(worksheet, name, sheet).map_err(|e| write_error(path, e))?;
    }
    workbook.save(path).map_err(|e| write_error(path, e))?;

    // Validation: re-read the file with `calamine`, which shares no code with
    // the writer. A workbook that only the writer can parse is not evidence
    // that Excel will open it.
    validate(path, sheets, &names)?;

    let total_rows: usize = sheets.iter().map(|s| s.rows.len()).sum();
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let mut result = OperationResult::ok(format!(
        "Created {name} with {} {} and {} {} of data",
        sheets.len(),
        plural(sheets.len(), "sheet", "sheets"),
        total_rows,
        plural(total_rows, "row", "rows"),
    ));
    result.created.push(path.to_path_buf());
    result.validation = ValidationOutcome::Passed;
    Ok(result)
}

/// Reject requests that cannot produce a usable workbook.
fn check_request(path: &Path, sheets: &[NewSheet]) -> Result<(), DocumentError> {
    let fail = |detail: String| DocumentError::Write {
        path: path.display().to_string(),
        detail,
    };

    if sheets.is_empty() {
        return Err(fail("a workbook needs at least one sheet".to_string()));
    }

    for sheet in sheets {
        if sheet.columns.is_empty() {
            return Err(fail(format!(
                "sheet {:?} has no columns, so there is nothing to write",
                sheet.name
            )));
        }
        // Row and column numbers are narrowed to Excel's u32/u16 grid below,
        // so an oversized request has to be refused here rather than wrap
        // around and write to the wrong cell.
        if sheet.columns.len() > MAX_COLUMNS {
            return Err(fail(format!(
                "sheet {:?} has {} columns but Excel allows {MAX_COLUMNS}",
                sheet.name,
                sheet.columns.len()
            )));
        }
        if sheet.rows.len() > MAX_DATA_ROWS {
            return Err(fail(format!(
                "sheet {:?} has {} rows but Excel allows {MAX_DATA_ROWS} below a header",
                sheet.name,
                sheet.rows.len()
            )));
        }
        for (index, row) in sheet.rows.iter().enumerate() {
            if row.len() > sheet.columns.len() {
                // Truncating here would lose data the caller believes was
                // saved, which is worse than refusing the whole write.
                return Err(fail(format!(
                    "sheet {:?}: row {index} has {} values but the sheet defines {} columns",
                    sheet.name,
                    row.len(),
                    sheet.columns.len()
                )));
            }
        }
    }
    Ok(())
}

/// Write one sheet: header, values, and the presentation around them.
fn build_sheet(worksheet: &mut Worksheet, name: &str, sheet: &NewSheet) -> Result<(), XlsxError> {
    worksheet.set_name(name)?;

    let header_format = header_format();
    let date_format = Format::new().set_num_format(DATE_NUM_FORMAT);
    let date_time_format = Format::new().set_num_format(DATE_TIME_NUM_FORMAT);
    let column_formats: Vec<Option<Format>> = sheet
        .columns
        .iter()
        .map(|column| number_format_string(&column.format).map(|f| Format::new().set_num_format(f)))
        .collect();

    for (index, column) in sheet.columns.iter().enumerate() {
        let col = index as u16;
        worksheet.write_string_with_format(0, col, &column.header, &header_format)?;
        // Setting the format on the column too means a person who types an
        // extra row under the data gets the same formatting, rather than a
        // raw number in a column of currency.
        if let Some(format) = &column_formats[index] {
            worksheet.set_column_format(col, format)?;
        }
    }

    for (row_index, row) in sheet.rows.iter().enumerate() {
        // Row 0 is the header, so data starts at 1.
        let row_num = (row_index + 1) as u32;
        for (col_index, value) in row.iter().enumerate() {
            write_cell(
                worksheet,
                row_num,
                col_index as u16,
                value,
                column_formats[col_index].as_ref(),
                &date_format,
                &date_time_format,
            )?;
        }
    }

    let last_col = (sheet.columns.len() - 1) as u16;
    let last_row = sheet.rows.len() as u32;

    // Freeze below the header so the labels stay put while scrolling, and
    // filter across it so the sheet is usable the moment it opens.
    worksheet.set_freeze_panes(1, 0)?;
    worksheet.autofilter(0, 0, last_row, last_col)?;

    // Autofit measures the strings and numbers that were actually written,
    // using Calibri 11 metrics. It does not account for what a number format
    // adds — a currency symbol, thousands separators, `[Red](…)` around a
    // negative — because that would need the optional `enhanced_autofit`
    // feature, so a heavily formatted number can sit a few pixels wider than
    // its column. Explicit widths are applied afterwards because a width set
    // after `autofit` wins.
    worksheet.set_autofit_max_width(MAX_AUTOFIT_WIDTH_PIXELS);
    worksheet.autofit();
    for (index, column) in sheet.columns.iter().enumerate() {
        if let Some(width) = column.width {
            worksheet.set_column_width(index as u16, width)?;
        }
    }

    // The header row height is deliberately left unset: Excel grows a row
    // with wrapped text to fit, and an explicit height would stop it.
    Ok(())
}

/// Write a single value in its own type.
fn write_cell(
    worksheet: &mut Worksheet,
    row: u32,
    col: u16,
    value: &CellValue,
    column_format: Option<&Format>,
    date_format: &Format,
    date_time_format: &Format,
) -> Result<(), XlsxError> {
    match value {
        // A blank cell still picks up the column format set above.
        CellValue::Empty => {}
        CellValue::Text { value } => match column_format {
            Some(format) => {
                worksheet.write_string_with_format(row, col, value, format)?;
            }
            None => {
                worksheet.write_string(row, col, value)?;
            }
        },
        CellValue::Number { value } => match column_format {
            Some(format) => {
                worksheet.write_number_with_format(row, col, *value, format)?;
            }
            None => {
                worksheet.write_number(row, col, *value)?;
            }
        },
        CellValue::Bool { value } => {
            // Booleans are written without the column's number format: Excel
            // shows TRUE/FALSE regardless, and attaching a currency format to
            // a boolean only confuses anyone who inspects the cell.
            worksheet.write_boolean(row, col, *value)?;
        }
        CellValue::Date { value } => match ExcelDateTime::parse_from_str(value) {
            Ok(datetime) => {
                let format = if has_time_component(value) {
                    date_time_format
                } else {
                    date_format
                };
                worksheet.write_datetime_with_format(row, col, &datetime, format)?;
            }
            // A string that does not parse is still the caller's data. Writing
            // it as text keeps it visible and repairable; dropping it or
            // guessing a date would not.
            Err(_) => {
                worksheet.write_string(row, col, value)?;
            }
        },
        CellValue::Formula { formula, value } => {
            let mut written = Formula::new(formula);
            if !value.is_empty() {
                // The cached result is what a reader sees before Excel
                // recalculates; without it the cell reads as 0.
                written = written.set_result(value);
            }
            match column_format {
                Some(format) => {
                    worksheet.write_formula_with_format(row, col, written, format)?;
                }
                None => {
                    worksheet.write_formula(row, col, written)?;
                }
            }
        }
    }
    Ok(())
}

/// True when an ISO-8601 string carries a time as well as a date.
fn has_time_component(value: &str) -> bool {
    value.contains(':')
}

/// Bold, lightly filled, wrapped, with a rule under it. Enough to read as a
/// header at a glance without looking like a template someone bought.
fn header_format() -> Format {
    Format::new()
        .set_bold()
        .set_background_color(Color::RGB(0xF2F2F2))
        .set_border_bottom(FormatBorder::Thin)
        .set_border_bottom_color(Color::RGB(0x9E9E9E))
        // Wrapping is what keeps a long label inside its column instead of
        // spilling across the next one.
        .set_text_wrap()
        .set_align(FormatAlign::VerticalCenter)
}

/// The Excel number format string for a column, or `None` for General.
fn number_format_string(format: &ColumnFormat) -> Option<String> {
    match format {
        ColumnFormat::Text => None,
        ColumnFormat::Number { decimals } => Some(decimal_pattern("#,##0", *decimals)),
        ColumnFormat::Currency { symbol, decimals } => {
            // The symbol is a literal in the format string, so an embedded
            // quote would end the literal early and corrupt the format.
            let symbol = symbol.replace('"', "");
            let positive = format!("\"{symbol}\"{}", decimal_pattern("#,##0", *decimals));
            // Two sections: positive;negative. Accounting parentheses in red
            // read as negative at a glance, where a minus sign in a column of
            // right-aligned figures is easy to miss.
            Some(format!("{positive};[Red]({positive})"))
        }
        ColumnFormat::Percent { decimals } => Some(format!("{}%", decimal_pattern("0", *decimals))),
        ColumnFormat::Date => Some(DATE_NUM_FORMAT.to_string()),
    }
}

/// `#,##0` plus the requested decimal places, e.g. `#,##0.00`.
fn decimal_pattern(integer_part: &str, decimals: u8) -> String {
    let decimals = usize::from(decimals.min(MAX_DECIMALS));
    if decimals == 0 {
        integer_part.to_string()
    } else {
        format!("{integer_part}.{}", "0".repeat(decimals))
    }
}

/// Sheet names Excel will accept, in the order the sheets were given.
///
/// Sanitization is deterministic, and two names that sanitize to the same
/// thing both survive: the later one gains a ` (2)`, ` (3)`, … suffix. Excel
/// compares sheet names case-insensitively, so collisions are detected that
/// way too.
fn unique_sheet_names(sheets: &[NewSheet]) -> Vec<String> {
    let mut taken: Vec<String> = Vec::with_capacity(sheets.len());
    let mut names = Vec::with_capacity(sheets.len());

    for sheet in sheets {
        let base = sanitize_sheet_name(&sheet.name);
        let mut candidate = base.clone();
        let mut suffix = 2usize;
        while taken.iter().any(|t| t.eq_ignore_ascii_case(&candidate)) {
            let marker = format!(" ({suffix})");
            // The suffix has to fit inside the 31-character limit, so the base
            // gives up whatever room it needs.
            let room = MAX_SHEET_NAME_CHARS.saturating_sub(marker.chars().count());
            candidate = format!("{}{marker}", clip(&base, room));
            suffix += 1;
        }
        taken.push(candidate.clone());
        names.push(candidate);
    }
    names
}

/// Replace what Excel rejects and trim to the length it allows.
fn sanitize_sheet_name(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| {
            if ILLEGAL_SHEET_NAME_CHARS.contains(&c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    let clipped = clip(replaced.trim(), MAX_SHEET_NAME_CHARS);
    if clipped.is_empty() {
        FALLBACK_SHEET_NAME.to_string()
    } else {
        clipped
    }
}

/// Take at most `max` characters — characters, not bytes, so a name in any
/// script survives — and tidy the edges Excel objects to.
fn clip(name: &str, max: usize) -> String {
    name.chars()
        .take(max)
        .collect::<String>()
        .trim()
        // Excel rejects a leading or trailing apostrophe, which truncation can
        // expose even when the original name was fine.
        .trim_matches('\'')
        .trim()
        .to_string()
}

/// Carry a writer failure across into this crate's error type.
fn write_error(path: &Path, error: XlsxError) -> DocumentError {
    DocumentError::Write {
        path: path.display().to_string(),
        detail: error.to_string(),
    }
}

/// Pick the singular or plural word for a count, for the summary the user
/// reads.
fn plural(count: usize, one: &'static str, many: &'static str) -> &'static str {
    if count == 1 {
        one
    } else {
        many
    }
}

/// Re-open the workbook with `calamine` and confirm it holds what was asked
/// for: every sheet, in order, with the expected header text, the expected
/// number of populated rows, and matching values for a sample of the data.
///
/// What this cannot see: `calamine` reads values, not presentation, so it
/// reports nothing about the header's fill or border, the frozen panes, the
/// autofilter, or the column widths. It does expose enough of the number
/// format to distinguish a date cell from a plain number, which is what
/// matters for the values staying usable.
fn validate(path: &Path, sheets: &[NewSheet], names: &[String]) -> Result<(), DocumentError> {
    use calamine::{Data, Reader, Xlsx};

    let fail = DocumentError::Validation;

    let mut workbook: Xlsx<_> =
        calamine::open_workbook(path).map_err(|e| fail(format!("{}: {e}", path.display())))?;

    let observed_names = workbook.sheet_names().to_vec();
    if observed_names != names {
        return Err(fail(format!(
            "expected the sheets {names:?} but the file holds {observed_names:?}"
        )));
    }

    for (sheet, name) in sheets.iter().zip(names) {
        let range = workbook
            .worksheet_range(name)
            .map_err(|e| fail(format!("sheet {name:?} could not be read back: {e}")))?;

        for (index, column) in sheet.columns.iter().enumerate() {
            let observed = range
                .get_value((0, index as u32))
                .map(ToString::to_string)
                .unwrap_or_default();
            if observed != column.header {
                return Err(fail(format!(
                    "sheet {name:?} column {index} should be headed {:?} but reads {observed:?}",
                    column.header
                )));
            }
        }

        // A row of nothing but `Empty` values produces no cells, so the file
        // legitimately ends at the last row that had something in it.
        let last_populated = sheet
            .rows
            .iter()
            .rposition(|row| row.iter().any(|cell| !matches!(cell, CellValue::Empty)));
        let expected_rows = last_populated.map_or(1, |index| index + 2);
        let observed_rows = range.end().map_or(0, |(row, _)| row as usize + 1);
        if observed_rows != expected_rows {
            return Err(fail(format!(
                "sheet {name:?} should hold {expected_rows} rows but holds {observed_rows}"
            )));
        }

        let has_formula = sheet
            .rows
            .iter()
            .flatten()
            .any(|cell| matches!(cell, CellValue::Formula { .. }));
        let formulas = if has_formula {
            Some(
                workbook
                    .worksheet_formula(name)
                    .map_err(|e| fail(format!("sheet {name:?} formulas unreadable: {e}")))?,
            )
        } else {
            None
        };

        for (row_index, row) in sheet.rows.iter().take(VALIDATION_SAMPLE_ROWS).enumerate() {
            let row_num = (row_index + 1) as u32;
            for (col_index, expected) in row.iter().enumerate() {
                let col_num = col_index as u32;
                let observed = range.get_value((row_num, col_num)).unwrap_or(&Data::Empty);
                let complaint = match expected {
                    CellValue::Empty => match observed {
                        Data::Empty => None,
                        other => Some(format!("should be empty but reads {other:?}")),
                    },
                    CellValue::Text { value } => match observed {
                        Data::String(text) if text == value => None,
                        // An empty string is written as a blank cell.
                        Data::Empty if value.is_empty() => None,
                        other => Some(format!("should read {value:?} but reads {other:?}")),
                    },
                    CellValue::Number { value } => match number_of(observed) {
                        Some(observed) if close_enough(observed, *value) => None,
                        _ => Some(format!(
                            "should be the number {value} but reads {observed:?}"
                        )),
                    },
                    CellValue::Bool { value } => match observed {
                        Data::Bool(observed) if observed == value => None,
                        other => Some(format!("should be {value} but reads {other:?}")),
                    },
                    CellValue::Date { value } => match ExcelDateTime::parse_from_str(value) {
                        Ok(datetime) => match observed {
                            Data::DateTime(observed)
                                if close_enough(observed.as_f64(), datetime.to_excel()) =>
                            {
                                None
                            }
                            other => {
                                Some(format!("should be the date {value:?} but reads {other:?}"))
                            }
                        },
                        // Unparseable dates are written as text on purpose.
                        Err(_) => match observed {
                            Data::String(text) if text == value => None,
                            other => Some(format!(
                                "should have kept the text {value:?} but reads {other:?}"
                            )),
                        },
                    },
                    CellValue::Formula { formula, .. } => {
                        let observed = formulas
                            .as_ref()
                            .and_then(|f| f.get_value((row_num, col_num)))
                            .map(String::as_str)
                            .unwrap_or_default();
                        // Excel stores a formula without its leading `=`.
                        let expected = formula.trim_start_matches('=');
                        if observed == expected {
                            None
                        } else {
                            Some(format!(
                                "should hold the formula {expected:?} but holds {observed:?}"
                            ))
                        }
                    }
                };
                if let Some(complaint) = complaint {
                    return Err(fail(format!(
                        "sheet {name:?} row {row_index} column {col_index} {complaint}"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The numeric value of a cell `calamine` may have typed as an integer, a
/// float, or a date serial.
fn number_of(data: &calamine::Data) -> Option<f64> {
    match data {
        calamine::Data::Int(value) => Some(*value as f64),
        calamine::Data::Float(value) => Some(*value),
        calamine::Data::DateTime(value) => Some(value.as_f64()),
        _ => None,
    }
}

/// Compare two f64s that made a round trip through a decimal string in XML.
fn close_enough(left: f64, right: f64) -> bool {
    (left - right).abs() <= 1e-9 * left.abs().max(right.abs()).max(1.0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::super::NewColumn;
    use super::*;
    use calamine::{Data, Reader, Xlsx};

    fn column(header: &str, format: ColumnFormat) -> NewColumn {
        NewColumn {
            header: header.to_string(),
            format,
            width: None,
        }
    }

    fn text(value: &str) -> CellValue {
        CellValue::Text {
            value: value.to_string(),
        }
    }

    fn number(value: f64) -> CellValue {
        CellValue::Number { value }
    }

    fn open(path: &Path) -> Xlsx<std::io::BufReader<std::fs::File>> {
        calamine::open_workbook(path).unwrap()
    }

    #[test]
    fn workbook_round_trips_with_headers_and_values() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("clients.xlsx");
        let sheet = NewSheet {
            name: "Clients".to_string(),
            columns: vec![
                column("Client", ColumnFormat::Text),
                column(
                    "Invoiced",
                    ColumnFormat::Currency {
                        symbol: "€".to_string(),
                        decimals: 2,
                    },
                ),
            ],
            rows: vec![
                vec![text("Acme"), number(1240.5)],
                vec![text("Globex"), number(-980.0)],
            ],
        };

        let result = create_xlsx(&path, &[sheet]).unwrap();
        assert!(result.success);
        assert_eq!(result.validation, ValidationOutcome::Passed);
        assert_eq!(result.created, vec![path.clone()]);

        let mut workbook = open(&path);
        assert_eq!(workbook.sheet_names(), vec!["Clients".to_string()]);
        let range = workbook.worksheet_range("Clients").unwrap();
        assert_eq!(
            range.get_value((0, 0)),
            Some(&Data::String("Client".into()))
        );
        assert_eq!(
            range.get_value((0, 1)),
            Some(&Data::String("Invoiced".into()))
        );
        assert_eq!(range.get_value((1, 0)), Some(&Data::String("Acme".into())));
        assert_eq!(range.end(), Some((2, 1)));
    }

    #[test]
    fn numbers_stay_numeric_and_dates_stay_dates() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("typed.xlsx");
        let sheet = NewSheet {
            name: "Typed".to_string(),
            columns: vec![
                column("Amount", ColumnFormat::Number { decimals: 2 }),
                column("Due", ColumnFormat::Date),
                column("Paid", ColumnFormat::Text),
                column("When", ColumnFormat::Date),
            ],
            rows: vec![vec![
                number(1234.56),
                CellValue::Date {
                    value: "2026-08-07".to_string(),
                },
                CellValue::Bool { value: true },
                CellValue::Date {
                    value: "2026-08-07T14:30:00".to_string(),
                },
            ]],
        };
        create_xlsx(&path, &[sheet]).unwrap();

        let mut workbook = open(&path);
        let range = workbook.worksheet_range("Typed").unwrap();

        // A number is a number, not the string "1234.56".
        match range.get_value((1, 0)) {
            Some(Data::Float(value)) => assert!((value - 1234.56).abs() < 1e-9),
            other => panic!("expected a float, got {other:?}"),
        }
        // calamine reports a date cell as DateTime because it recognises the
        // number format, which is the evidence that the cell is sortable and
        // computable rather than text that looks like a date.
        match range.get_value((1, 1)) {
            Some(Data::DateTime(value)) => {
                assert_eq!(value.to_ymd_hms_milli(), (2026, 8, 7, 0, 0, 0, 0));
            }
            other => panic!("expected a datetime, got {other:?}"),
        }
        assert_eq!(range.get_value((1, 2)), Some(&Data::Bool(true)));
        match range.get_value((1, 3)) {
            Some(Data::DateTime(value)) => {
                assert_eq!(value.to_ymd_hms_milli(), (2026, 8, 7, 14, 30, 0, 0));
            }
            other => panic!("expected a datetime, got {other:?}"),
        }
    }

    #[test]
    fn formulas_are_written_as_formulas() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("totals.xlsx");
        let sheet = NewSheet {
            name: "Totals".to_string(),
            columns: vec![
                column("Value", ColumnFormat::Number { decimals: 0 }),
                column("Doubled", ColumnFormat::Number { decimals: 0 }),
            ],
            rows: vec![
                vec![
                    number(21.0),
                    CellValue::Formula {
                        formula: "=A2*2".to_string(),
                        value: "42".to_string(),
                    },
                ],
                vec![
                    number(10.0),
                    CellValue::Formula {
                        // The leading `=` is optional; both spellings must work.
                        formula: "A3*2".to_string(),
                        value: String::new(),
                    },
                ],
            ],
        };
        create_xlsx(&path, &[sheet]).unwrap();

        let mut workbook = open(&path);
        let formulas = workbook.worksheet_formula("Totals").unwrap();
        assert_eq!(formulas.get_value((1, 1)).map(String::as_str), Some("A2*2"));
        assert_eq!(formulas.get_value((2, 1)).map(String::as_str), Some("A3*2"));

        // The cached result is what a reader sees before Excel recalculates.
        let range = workbook.worksheet_range("Totals").unwrap();
        match range.get_value((1, 1)) {
            Some(Data::Float(value)) => assert!((value - 42.0).abs() < 1e-9),
            Some(Data::Int(value)) => assert_eq!(*value, 42),
            other => panic!("expected the cached result 42, got {other:?}"),
        }
    }

    #[test]
    fn every_column_format_produces_a_readable_file() {
        let formats = [
            ColumnFormat::Text,
            ColumnFormat::Number { decimals: 0 },
            ColumnFormat::Number { decimals: 4 },
            ColumnFormat::Currency {
                symbol: "$".to_string(),
                decimals: 2,
            },
            ColumnFormat::Currency {
                symbol: "£".to_string(),
                decimals: 0,
            },
            ColumnFormat::Percent { decimals: 1 },
            ColumnFormat::Date,
        ];
        let tmp = tempfile::tempdir().unwrap();

        for (index, format) in formats.iter().enumerate() {
            let path = tmp.path().join(format!("format-{index}.xlsx"));
            let sheet = NewSheet {
                name: format!("Format {index}"),
                columns: vec![column("Value", format.clone())],
                rows: vec![vec![number(0.15)], vec![number(-2500.0)]],
            };
            create_xlsx(&path, &[sheet]).unwrap();

            // calamine reads values, not styling, so what can be asserted here
            // is that the file re-opens and the stored numbers are untouched
            // by the format. Whether the cell *displays* as "$-2,500.00" is
            // not observable through the reader; the number format string
            // itself is covered by `number_format_string_is_what_excel_wants`.
            let mut workbook = open(&path);
            let range = workbook
                .worksheet_range(&format!("Format {index}"))
                .unwrap();
            assert_eq!(
                number_of(range.get_value((1, 0)).unwrap()),
                Some(0.15),
                "format {format:?} changed the stored value"
            );
        }
    }

    #[test]
    fn number_format_string_is_what_excel_wants() {
        assert_eq!(number_format_string(&ColumnFormat::Text), None);
        assert_eq!(
            number_format_string(&ColumnFormat::Number { decimals: 0 }).unwrap(),
            "#,##0"
        );
        assert_eq!(
            number_format_string(&ColumnFormat::Number { decimals: 2 }).unwrap(),
            "#,##0.00"
        );
        assert_eq!(
            number_format_string(&ColumnFormat::Currency {
                symbol: "$".to_string(),
                decimals: 2
            })
            .unwrap(),
            "\"$\"#,##0.00;[Red](\"$\"#,##0.00)"
        );
        // A percent format is Excel's own, so a stored 0.15 shows as 15.0%.
        assert_eq!(
            number_format_string(&ColumnFormat::Percent { decimals: 1 }).unwrap(),
            "0.0%"
        );
        assert_eq!(
            number_format_string(&ColumnFormat::Date).unwrap(),
            "yyyy-mm-dd"
        );
        // Excel caps decimals at 30; a wild request is clamped, not rejected.
        assert!(
            number_format_string(&ColumnFormat::Number { decimals: 255 })
                .unwrap()
                .ends_with(&"0".repeat(30))
        );
    }

    #[test]
    fn a_percent_column_stores_the_fraction_it_was_given() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("percent.xlsx");
        let sheet = NewSheet {
            name: "Margin".to_string(),
            columns: vec![column("Margin", ColumnFormat::Percent { decimals: 0 })],
            rows: vec![vec![number(0.15)]],
        };
        create_xlsx(&path, &[sheet]).unwrap();

        let mut workbook = open(&path);
        let range = workbook.worksheet_range("Margin").unwrap();
        // Nothing multiplied the value on the way in: Excel's percent format
        // does the scaling at display time, so 0.15 is still 0.15 on disk.
        assert_eq!(number_of(range.get_value((1, 0)).unwrap()), Some(0.15));
    }

    #[test]
    fn a_row_longer_than_the_columns_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("too-wide.xlsx");
        let sheet = NewSheet {
            name: "Budget".to_string(),
            columns: vec![column("A", ColumnFormat::Text)],
            rows: vec![vec![text("ok")], vec![text("one"), text("two")]],
        };
        let error = create_xlsx(&path, &[sheet]).unwrap_err();
        match error {
            DocumentError::Write { detail, .. } => {
                assert!(detail.contains("Budget"), "{detail}");
                assert!(detail.contains("row 1"), "{detail}");
            }
            other => panic!("expected a write error, got {other}"),
        }
        // The request was rejected before anything was written.
        assert!(!path.exists());
    }

    #[test]
    fn a_short_row_leaves_the_rest_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("short.xlsx");
        let sheet = NewSheet {
            name: "Short".to_string(),
            columns: vec![
                column("A", ColumnFormat::Text),
                column("B", ColumnFormat::Text),
                column("C", ColumnFormat::Text),
            ],
            rows: vec![vec![text("only")]],
        };
        create_xlsx(&path, &[sheet]).unwrap();

        let mut workbook = open(&path);
        let range = workbook.worksheet_range("Short").unwrap();
        assert_eq!(range.get_value((1, 0)), Some(&Data::String("only".into())));
        assert!(matches!(range.get_value((1, 1)), None | Some(Data::Empty)));
    }

    #[test]
    fn sheet_names_are_sanitized_and_collisions_both_survive() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("names.xlsx");
        let make = |name: &str| NewSheet {
            name: name.to_string(),
            columns: vec![column("A", ColumnFormat::Text)],
            rows: vec![vec![text(name)]],
        };
        let sheets = vec![
            make("Report/2026"),
            make("Report:2026"),
            make("Report?2026"),
            make("A very long sheet name that Excel will not accept at all"),
            make("   "),
        ];
        create_xlsx(&path, &sheets).unwrap();

        let mut workbook = open(&path);
        let names = workbook.sheet_names().to_vec();
        assert_eq!(
            names,
            vec![
                "Report-2026".to_string(),
                "Report-2026 (2)".to_string(),
                "Report-2026 (3)".to_string(),
                "A very long sheet name that Exc".to_string(),
                "Sheet".to_string(),
            ]
        );
        // Every sheet still holds its own data; none was dropped in the rename.
        for (name, sheet) in names.iter().zip(&sheets) {
            let range = workbook.worksheet_range(name).unwrap();
            assert_eq!(
                range.get_value((1, 0)),
                Some(&Data::String(sheet.name.clone()))
            );
        }
    }

    #[test]
    fn sanitized_names_stay_within_excels_limit() {
        let long = "x".repeat(40);
        assert_eq!(sanitize_sheet_name(&long).chars().count(), 31);
        // Characters, not bytes: a 40-glyph name in another script keeps 31
        // glyphs rather than being cut mid-character.
        let long_unicode = "行".repeat(40);
        assert_eq!(sanitize_sheet_name(&long_unicode).chars().count(), 31);
        assert_eq!(sanitize_sheet_name("'quoted'"), "quoted");
        assert_eq!(sanitize_sheet_name("[]:*?/\\"), "-------");
    }

    #[test]
    fn unicode_survives_headers_values_and_the_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("données 报告.xlsx");
        let sheet = NewSheet {
            name: "Résumé — 概要".to_string(),
            columns: vec![
                column("Ünité", ColumnFormat::Text),
                column(
                    "収益 (¥)",
                    ColumnFormat::Currency {
                        symbol: "¥".to_string(),
                        decimals: 0,
                    },
                ),
            ],
            rows: vec![vec![text("Ελλάδα 🇬🇷"), number(1_000_000.0)]],
        };
        create_xlsx(&path, &[sheet]).unwrap();

        let mut workbook = open(&path);
        assert_eq!(workbook.sheet_names(), vec!["Résumé — 概要".to_string()]);
        let range = workbook.worksheet_range("Résumé — 概要").unwrap();
        assert_eq!(
            range.get_value((0, 1)),
            Some(&Data::String("収益 (¥)".into()))
        );
        assert_eq!(
            range.get_value((1, 0)),
            Some(&Data::String("Ελλάδα 🇬🇷".into()))
        );
    }

    #[test]
    fn a_multi_sheet_workbook_keeps_each_sheet_separate() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/dir/multi.xlsx");
        let sheets = vec![
            NewSheet {
                name: "Summary".to_string(),
                columns: vec![column("Metric", ColumnFormat::Text)],
                rows: vec![vec![text("Revenue")]],
            },
            NewSheet {
                name: "Detail".to_string(),
                columns: vec![
                    column("Day", ColumnFormat::Date),
                    column("Amount", ColumnFormat::Number { decimals: 2 }),
                ],
                rows: vec![
                    vec![
                        CellValue::Date {
                            value: "2026-01-31".to_string(),
                        },
                        number(12.0),
                    ],
                    vec![
                        CellValue::Date {
                            value: "2026-02-28".to_string(),
                        },
                        number(13.5),
                    ],
                ],
            },
        ];
        let result = create_xlsx(&path, &sheets).unwrap();
        // Parent directories are created, as create_docx does.
        assert!(path.exists());
        assert!(result.user_summary.contains("2 sheets"));
        assert!(result.user_summary.contains("3 rows"));

        let mut workbook = open(&path);
        assert_eq!(
            workbook.sheet_names(),
            vec!["Summary".to_string(), "Detail".to_string()]
        );
        assert_eq!(
            workbook.worksheet_range("Summary").unwrap().end(),
            Some((1, 0))
        );
        assert_eq!(
            workbook.worksheet_range("Detail").unwrap().end(),
            Some((2, 1))
        );
    }

    #[test]
    fn a_workbook_with_no_sheets_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("empty.xlsx");
        let error = create_xlsx(&path, &[]).unwrap_err();
        assert!(matches!(error, DocumentError::Write { .. }));
        assert!(!path.exists());
    }

    #[test]
    fn a_sheet_with_no_columns_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("columnless.xlsx");
        let sheet = NewSheet {
            name: "Nothing".to_string(),
            columns: vec![],
            rows: vec![],
        };
        let error = create_xlsx(&path, &[sheet]).unwrap_err();
        match error {
            DocumentError::Write { detail, .. } => assert!(detail.contains("Nothing"), "{detail}"),
            other => panic!("expected a write error, got {other}"),
        }
        assert!(!path.exists());
    }

    #[test]
    fn a_header_only_sheet_is_valid() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("headers-only.xlsx");
        let sheet = NewSheet {
            name: "Empty".to_string(),
            columns: vec![column("A", ColumnFormat::Text)],
            rows: vec![],
        };
        let result = create_xlsx(&path, &[sheet]).unwrap();
        assert_eq!(result.validation, ValidationOutcome::Passed);
        assert!(result.user_summary.contains("0 rows"));

        let mut workbook = open(&path);
        assert_eq!(
            workbook.worksheet_range("Empty").unwrap().end(),
            Some((0, 0))
        );
    }

    #[test]
    fn an_unparseable_date_is_kept_as_text() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("bad-date.xlsx");
        let sheet = NewSheet {
            name: "Dates".to_string(),
            columns: vec![column("When", ColumnFormat::Date)],
            rows: vec![vec![CellValue::Date {
                value: "sometime next spring".to_string(),
            }]],
        };
        create_xlsx(&path, &[sheet]).unwrap();

        let mut workbook = open(&path);
        let range = workbook.worksheet_range("Dates").unwrap();
        assert_eq!(
            range.get_value((1, 0)),
            Some(&Data::String("sometime next spring".into())),
            "an unparseable date must survive as text rather than be dropped"
        );
    }

    #[test]
    fn explicit_widths_are_honoured_and_others_are_fitted() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("widths.xlsx");
        let sheet = NewSheet {
            name: "Widths".to_string(),
            columns: vec![
                NewColumn {
                    header: "Fixed".to_string(),
                    format: ColumnFormat::Text,
                    width: Some(42.0),
                },
                column("Fitted", ColumnFormat::Text),
            ],
            rows: vec![vec![text("short"), text(&"long value ".repeat(20))]],
        };
        // Widths are not observable through calamine, so what this test can
        // prove is that both paths produce a workbook that re-opens with its
        // values intact — the validation step inside create_xlsx is the
        // assertion.
        let result = create_xlsx(&path, &[sheet]).unwrap();
        assert_eq!(result.validation, ValidationOutcome::Passed);
    }
}
