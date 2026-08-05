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

**Later:** formatting-preserving DOCX edits of existing documents; XLSX;
PPTX; OCR; conversion between formats. These are deferred, not designed
away — ARCHITECTURE.md's "What the MVP intentionally defers" section and
docs/research.md §D describe the library landscape each of these will
draw on when they're built.

## Library choices

Chosen for permissive licensing (see docs/research.md §D and §E for the
full survey and the licensing-constraints table) and, where a category
has more than one viable option, for whether the library is mature enough
to trust with a user's real documents:

- **DOCX:** `docx-rust` (MIT) for structured extraction/creation;
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
- **XLSX (later):** `calamine` (MIT) for reading, `rust_xlsxwriter`
  (MIT/Apache) for new files, `umya-spreadsheet` (MIT) for round-trip
  editing, with the maintainer's own caveat that drawings/OLE editing is
  rough — noted for whoever picks this up.
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

1. **Re-parse with an independent library.** Where a generated file has a
   structured format, it is validated by re-reading it with a *different*
   library than the one that wrote it — catching a writer bug that
   produces bytes its own reader would forgive.
2. **A required-parts / well-formedness check** on the underlying
   container (the zip structure for OOXML formats, page structure for
   PDF).
3. **On-disk verification is unconditional**, even for plain files: every
   write is followed by re-reading the file and comparing a BLAKE3
   content hash against the bytes that were supposed to be written
   (`create_file`, `overwrite_file` in `fsops.rs`). A hash mismatch is a
   `VerificationFailed` error, not a warning, and `success` is `false`.

Success is never reported before this on-disk verification completes.

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
