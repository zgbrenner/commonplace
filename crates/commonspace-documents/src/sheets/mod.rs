//! Spreadsheets: reading what someone already has, and building one that
//! looks like a person made it.
//!
//! Two libraries, deliberately: `rust_xlsxwriter` writes and `calamine`
//! reads. Because they share no code, re-parsing a freshly written workbook
//! with the reader is real evidence that the file is sound — it would catch
//! a writer bug that produces bytes the writer's own reader forgives.
//!
//! That is a stronger guarantee than [`crate::office`] gets for DOCX, where
//! `docx-rs` both writes and re-reads, so the check can only catch a
//! truncated write or a malformed package (docs/document-tools.md).
//!
//! The model never emits spreadsheet bytes. It describes columns, their
//! formats, and rows; this layer decides what that means in a file — header
//! styling, frozen panes, column widths, number formats, autofilter — so
//! output is consistent whichever agent produced it.

pub mod read;
pub mod write;

pub use read::{read_spreadsheet, ReadLimits};
pub use write::create_xlsx;

use serde::{Deserialize, Serialize};

/// One cell's value.
///
/// Dates are ISO-8601 strings rather than a date type: spreadsheets store
/// dates as numbers whose meaning depends on the workbook's epoch, and a
/// string that already reads correctly is more useful to an agent than a
/// serial number it has to interpret. [`CellValue::Number`] keeps full
/// precision for anything the reader could not confidently call a date.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CellValue {
    Empty,
    Text {
        value: String,
    },
    Number {
        value: f64,
    },
    Bool {
        value: bool,
    },
    /// ISO-8601: `2026-08-07` or `2026-08-07T14:30:00`.
    Date {
        value: String,
    },
    /// A formula's cached result, with the formula itself alongside. Reading
    /// reports both because "=SUM(B2:B40)" and "1240" answer different
    /// questions about a sheet.
    Formula {
        formula: String,
        value: String,
    },
}

/// One sheet, as read from a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    pub name: String,
    /// The first row, when it looks like a header (see
    /// [`read::header_looks_real`]). Empty when the sheet starts with data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub headers: Vec<String>,
    /// Data rows, excluding the header row when one was detected.
    pub rows: Vec<Vec<CellValue>>,
    /// Rows this sheet actually holds, which is larger than `rows.len()`
    /// when a limit cut the read short.
    pub total_rows: usize,
    pub truncated: bool,
}

/// A whole workbook, as read from a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
}

/// How a column's values should be displayed. This is presentation only —
/// the stored value stays a number, so the recipient can still compute with
/// it, which is the whole point of producing a spreadsheet rather than a
/// table of text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ColumnFormat {
    #[default]
    Text,
    Number {
        #[serde(default)]
        decimals: u8,
    },
    /// `symbol` is prefixed as written, e.g. `"$"`, `"€"`, `"£"`.
    Currency {
        symbol: String,
        #[serde(default)]
        decimals: u8,
    },
    Percent {
        #[serde(default)]
        decimals: u8,
    },
    Date,
}

/// One column of a sheet Commonspace is asked to create.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewColumn {
    pub header: String,
    #[serde(default)]
    pub format: ColumnFormat,
    /// Width in characters. `None` means "fit the content", which is what
    /// almost every caller should want.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<f64>,
}

/// One sheet Commonspace is asked to create.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewSheet {
    pub name: String,
    pub columns: Vec<NewColumn>,
    /// Rows of values, in column order. A row shorter than `columns` leaves
    /// the remaining cells empty; a longer one is an error rather than a
    /// silent truncation, because losing data quietly is worse than failing.
    pub rows: Vec<Vec<CellValue>>,
}
