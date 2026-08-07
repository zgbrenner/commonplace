# Document Tools

The deterministic document layer: everything that reads, previews,
creates, or modifies a file on the user's behalf, implemented in Rust so
that no model ever improvises a binary format. This document describes
what that layer promises, what ships in the MVP, and what it deliberately
defers.

## Principle

An LLM is good at deciding *what* a document should say and comparatively
bad, and unpredictable, at producing the exact bytes of a `.docx` or
`.pdf` file. Commonspace never asks a model to generate or hand-edit a
binary document format directly. Every document operation is a
deterministic Rust function with a fixed contract, and the model's role
is limited to deciding which operation to call and with what content.

Every operation returns a structured `OperationResult`
(`crates/commonspace-core/src/op_result.rs`):

```rust
pub struct OperationResult {
    pub success: bool,
    pub created: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub backups: Vec<PathBuf>,
    pub warnings: Vec<String>,
    pub validation: ValidationOutcome,
    pub user_summary: String,       // plain-language, for the timeline
    pub diagnostics: Option<String>, // technical detail, developer view
}
```

`ValidationOutcome` is one of `Passed`, `Failed { detail }`, or
`NotApplicable` (for operations with no meaningful validator, such as a
plain directory listing — the UI shows no checkmark for these, distinct
from a passed validation). `OperationResult::ok(summary)` and
`OperationResult::failed(summary, detail)` are the two constructors used
throughout; `success` and `validation` are never allowed to disagree —
a result is only ever `success: true` after validation has actually
passed.

## What ships in the MVP, and what's later

**MVP:**

- Markdown and plain text: reading with encoding detection (BOM sniffing,
  then `chardetng` statistical detection, decoding through `encoding_rs`
  with malformed sequences replaced and reported as a warning, never a
  crash — `crates/commonspace-documents/src/textio.rs`), and editing.
- PDF text extraction and page preview (rendering, not editing).
- DOCX structured extraction and creation (new documents built from
  structured content, not formatting-preserving edits of existing ones).
- Spreadsheet reading (`.xlsx`, `.xlsm`, `.xls`, `.xlsb`, `.ods`, `.csv`,
  `.tsv`) and `.xlsx` creation — again creation, not editing of a
  workbook that already exists. See "Spreadsheets" below.

**Later:** formatting-preserving DOCX edits of existing documents;
editing an existing spreadsheet in place; PPTX; OCR; conversion between
formats. These are deferred, not designed away — ARCHITECTURE.md's "What
the MVP intentionally defers" section and docs/research.md §D describe
the library landscape each of these will draw on when they're built.

## Library choices

Chosen for permissive licensing (see docs/research.md §D and §E for the
full survey and the licensing-constraints table) and, where a category
has more than one viable option, for whether the library is mature enough
to trust with a user's real documents:

- **DOCX:** `docx-rs` (MIT) for structured extraction/creation;
  `ooxmlsdk` (MIT OR Apache-2.0) is being watched for its explicit
  preservation of unknown/extension XML, relevant once
  formatting-preserving edits are built. Typed round-trip libraries
  re-serialize a fresh XML tree and can silently drop unmodeled content —
  formatting-preserving edits, when they ship, will use surgical zip and
  part-level `quick-xml` editing instead, leaving untouched parts of the
  document byte-identical.
- **PDF:** `pdf-extract` (MIT) for text extraction; `lopdf` (MIT) for
  structural operations (merge/split/rotate); `pdfium-render` (MIT/Apache,
  binding the BSD-licensed PDFium engine) for page-preview rendering.
  `mupdf-rs` was considered and rejected — it is AGPL-3.0, and
  `pdfium-render` covers the rendering need without a copyleft dependency.
- **Spreadsheets:** `calamine` (MIT) for reading and `rust_xlsxwriter`
  (MIT/Apache) for creating new files, both in use today. They are
  separate projects sharing no code, which is what makes the validation
  step below mean something. `umya-spreadsheet` (MIT) remains the
  candidate for round-trip editing of an existing workbook, with the
  maintainer's own caveat that drawings/OLE editing is rough — noted for
  whoever picks this up.
- **PPTX (later):** no mature pure-Rust crate exists yet; the realistic
  approach is template-injection over minimal OOXML skeletons, or
  optional LibreOffice conversion, not a general editing library.
- **OCR (later):** `ocrs` (MIT/Apache, pure Rust), preferred over the
  thinner Tesseract binding options.
- **Optional external tools**, detected at runtime, never bundled:
  LibreOffice (`soffice --headless --convert-to`, MPL-2.0, run with
  `-env:UserInstallation` pointed at an isolated profile) and `pandoc`
  (GPL-2.0+, used only as a subprocess, which does not affect
  Commonspace's own MIT license since nothing is linked).
- **General:** `infer` and `mime_guess` for MIME detection (magic bytes
  first, extension fallback — `crates/commonspace-documents/src/inspect.rs`),
  `blake3` for content hashing, `trash` for OS-trash deletion,
  `walkdir` for directory traversal.

## Validation rule

An operation is never reported as successful because an agent, or
Commonspace's own writer code, *claims* it worked. The rule, enforced at
every mutation point in `crates/commonspace-documents/src/fsops.rs`:

1. **Re-parse before reporting success.** Every generated file in a
   structured format is re-read and checked to hold what was asked for.
   How independent that check is varies by format, and the difference is
   worth stating rather than glossing:

   - **Spreadsheets** are validated by a genuinely different library:
     `rust_xlsxwriter` writes, `calamine` reads. Because the two share no
     code, a re-parse is real evidence — it catches a writer bug that
     produces bytes the writer's own reader would forgive.
   - **DOCX** is written and re-read by the same library (`docx-rs`). That
     still catches truncated writes, a malformed package, and content that
     silently failed to land, which is most of what goes wrong. It cannot
     catch a bug shared between that library's writer and its reader.
     Closing that gap needs a second DOCX reader; until there is one, this
     is the honest description of the guarantee.
2. **A required-parts / well-formedness check** on the underlying
   container (the zip structure for OOXML formats, page structure for
   PDF).
3. **On-disk verification is unconditional**, even for plain files: every
   write is followed by re-reading the file and comparing a BLAKE3
   content hash against the bytes that were supposed to be written
   (`create_file`, `overwrite_file` in `fsops.rs`). A hash mismatch is a
   `VerificationFailed` error, not a warning, and `success` is `false`.

Success is never reported before this on-disk verification completes.

## Spreadsheets

Spreadsheets are the clearest case of the principle at the top of this
document. The model never emits workbook bytes; it describes columns,
their formats, and rows, and `crates/commonspace-documents/src/sheets/`
decides what that means in a file. Two MCP tools expose it —
`read_spreadsheet` and `create_spreadsheet`
(`crates/commonspace-runtime/src/tools.rs`) — and `read_document` routes
spreadsheet extensions to the same reader, so an agent that reaches for
the general tool gets an answer rather than an error.

**What is read.** `sheets/read.rs` handles `.xlsx`, `.xlsm`, `.xls`,
`.xlsb` and `.ods` through `calamine`, and parses `.csv` and `.tsv`
directly. A read returns a `Workbook` of `Sheet`s (`sheets/mod.rs`), each
carrying:

- `name`.
- `headers` — the first row when it looks like a header, empty when the
  sheet starts straight into data. Whether a row "looks like a header" is
  a heuristic, and the rule it applies lives in `read::header_looks_real`
  rather than being restated here; it is the kind of thing that gets
  tuned against real files.
- `rows` of `CellValue`, excluding the header row when one was found.
- `total_rows` and `truncated`, so a partial read announces itself
  instead of quietly looking complete.

**Limits.** `ReadLimits` caps how many sheets, rows and columns a single
read returns; the defaults are in `read.rs`. The reason for them is that
a large workbook must not be able to exhaust a model's context or the
app's memory — a truncated read that says it was truncated is the
correct outcome there, not a failure.

**The cell model.** A `CellValue` is `Empty`, `Text`, `Number`, `Bool`,
`Date` or `Formula`. Two choices in that list are worth knowing:

- **Dates are ISO-8601 strings**, not serial numbers. A spreadsheet
  stores a date as a number whose meaning depends on the workbook's
  epoch; a string that already reads correctly is more useful to an agent
  than a number it has to interpret.
- **A formula is reported together with its cached result**, because
  `=SUM(B2:B40)` and `1240` answer different questions about a sheet. The
  result is whatever the application that last saved the file computed;
  Commonspace does not evaluate formulas.

Cell styling, colours, merged-cell geometry, charts, images and comments
are not reported. This layer reads values and structure.

**Creating a workbook.** `sheets/write.rs` builds a new `.xlsx` from a
list of `NewSheet`s: a sheet name, `NewColumn`s each with a header, a
`ColumnFormat` and an optional width, and rows of `CellValue` in column
order. A row shorter than the column list leaves the remaining cells
empty; a longer one is an error rather than a silent truncation, because
losing data quietly is worse than failing.

"Polished" is a fixed list, not an aspiration. Every sheet Commonspace
writes gets:

- A styled header row, visually distinct from the data under it.
- Frozen panes, so the header stays put when the sheet is scrolled.
- An autofilter across the header row.
- Column widths — the caller's value where one is given, fitted to the
  content otherwise.
- Per-column number formats: plain numbers to a chosen number of
  decimals, currency with the symbol the caller supplies, percentages,
  and dates.
- Numbers stored as numbers and dates stored as dates, never as text
  that merely looks right. `ColumnFormat` is presentation only — the
  underlying value stays a number, so the recipient can still sum a
  column, which is the entire reason to produce a spreadsheet instead of
  a table of text.

The result is that two different agents asked for the same data produce
the same-looking file, because none of this is left to the agent.

**Formulas** are written for the recipient's spreadsheet application to
evaluate when it opens the file. Commonspace does not compute the result
itself and does not judge whether the formula is sensible — a reference
to the wrong column yields a wrong number in Excel, not an error here.
The convention a percent column expects — a fraction or a whole number —
is settled in `write.rs` and stated in the tool's own description, which
is where an agent reads it.

**What creation does not do.** No charts, images or pivot tables are
produced. There is no operation that opens an existing workbook and
edits it while preserving what was already in it; creation is creation.
Round-trip editing is on the deferred list above with the rest of the
formatting-preserving work.

**Validation.** A created workbook is written by `rust_xlsxwriter` and
then re-parsed by `calamine` before success is reported. Because those
two libraries share no code, a successful re-parse is evidence from
something that had no hand in producing the bytes — the strongest form
of the rule above, and stronger than DOCX creation, which today re-reads
with the same library that wrote it. A failed re-parse fails the
operation, with `OperationResult::validation` set to `Failed`; the exact
checks made against the re-parsed workbook are in `write.rs`.

## The safe-file-operation contract

Every mutating operation in `commonspace-documents` goes through
`SafeFs` (`crates/commonspace-documents/src/fsops.rs`), which enforces
the same contract regardless of which document format is involved:

- **Scope and protection are re-checked here**, even though the
  orchestrator already evaluated permission policy upstream — this is
  defense-in-depth, not the primary enforcement point. `SafeFs::checked`
  resolves the path through the workspace's `PathGuard` and refuses
  anything outside an authorized root or inside a protected system/
  credential location (`commonspace-permissions::is_protected_location`).
- **Backup before modify or delete.** `overwrite_file` and
  `delete_to_trash` both copy the original into the `BackupStore`
  (`crates/commonspace-documents/src/backup.rs`, timestamped and
  UUID-suffixed, stored outside the workspace so agent operations can
  never touch the backups themselves) before touching the target.
- **Journaled inverse for undo.** Every operation returns a
  `FileOperation` record (`crates/commonspace-documents/src/journal.rs`)
  — kind (`Create`, `Modify`, `RenameMove`, `DeleteToTrash`), source,
  destination, backup path, and before/after content hashes — which
  `SafeFs::undo` replays in reverse.
- **Hash-verified undo that refuses when the file changed since.** Before
  undoing, `verify_unchanged` compares the file's current BLAKE3 hash
  against the hash recorded right after the original operation. If they
  differ — the user or another process touched the file since — undo
  refuses outright with `UndoUnavailable` rather than silently
  overwriting newer content; the backup file remains available as a
  manual fallback.
- **OS trash instead of permanent deletion.** `delete_to_trash` uses the
  `trash` crate (Recycle Bin on Windows, equivalent on macOS/Linux), never
  `std::fs::remove_file`. A backup copy is still taken first, so undo
  after a delete never depends on the platform's own trash-restore API
  working correctly.

## Fixture corpus

Tests for this layer need fixtures that go beyond "one clean file per
format," because real user folders are messy in specific, predictable
ways:

- Valid and **malformed** DOCX, XLSX, PPTX, and PDF files — truncated,
  wrong zip central directory, corrupted XML, a PDF with no `%%EOF`.
- **Unicode filenames** (already exercised in `backup.rs` and
  `inspect.rs` tests, e.g. `données café.txt`, `naïve 报告.md`).
- **Long paths**, exceeding traditional Windows `MAX_PATH`.
- **Symlinks and junctions**, both inside and pointing outside a
  workspace root (see `path_guard.rs` for the containment tests these
  feed).
- **Duplicate files**, byte-identical content under different names, to
  exercise `find_duplicates` (`inspect.rs`).
- **Read-only folders and locked files** — a file open in another
  application, or marked read-only, must fail an operation cleanly, not
  panic (see the Windows `read_only_target_fails_gracefully` test in
  `fsops.rs` as the existing pattern to extend).
- **Large folders**, to exercise `list_dir`'s truncation behavior rather
  than assuming every directory is small.
- **Interrupted operations** — a process killed mid-write — to exercise
  the crash-recovery reconciliation against the journal described in
  ARCHITECTURE.md's Reliability section.
