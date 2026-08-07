//! Deterministic document tooling and safe file operations.
//!
//! Nothing in this crate trusts an agent's claim about what happened on
//! disk: every mutating operation verifies its own result (existence,
//! content hash, independent re-parse for structured formats) before
//! reporting success, and journals an inverse operation for undo.
//!
//! Scope enforcement here is defense-in-depth: the orchestrator evaluates
//! policy *before* calling these tools, and these tools *still* refuse
//! out-of-scope or protected paths.

pub mod backup;
pub mod diff;
pub mod fsops;
pub mod inspect;
pub mod journal;
pub mod office;
pub mod sheets;
pub mod staging;
pub mod textio;

pub use backup::BackupStore;
pub use diff::{
    extracted_text_diff, looks_binary, shape_summary, text_diff, ChangePreview, ChangeShape,
    ComparisonBasis, DiffBudget, DiffLine, Hunk, LineKind, LineSpan, SummaryReason,
};
pub use fsops::{FsToolError, SafeFs};
pub use inspect::{DirEntryInfo, DirListing};
pub use journal::{FileOpKind, FileOperation};
pub use office::{DocBlock, DocumentError, ExtractedDocument};
pub use sheets::{
    create_xlsx, read_spreadsheet, CellValue, ColumnFormat, NewColumn, NewSheet, ReadLimits, Sheet,
    Workbook,
};
pub use staging::{StagedChange, StagedChangeId, StagedKind, StagingError, StagingStore};
