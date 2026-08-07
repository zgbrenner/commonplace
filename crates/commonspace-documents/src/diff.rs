//! Comparing two versions of a document, so a change can be shown before it
//! is accepted and read after it is made.
//!
//! The audience compares contracts, reports and letters, not source code, so
//! two things matter more here than they would in a code review tool:
//!
//! - **Word-level detail inside a changed line.** Reflowing a paragraph
//!   rewrites every line in it; a line-level red/green rendering of that is
//!   unreadable. Each [`DiffLine`] therefore carries [`LineSpan`]s marking
//!   exactly which words moved.
//! - **A bound on the work.** This runs on the path to a permission prompt,
//!   with a person waiting. Every entry point either produces a diff inside
//!   [`DiffBudget`] or returns a [`ChangeShape`] summary that says so; a
//!   comparison that hangs is worse than no comparison.
//!
//! Everything here is pure: text in, data out. Reading files, decoding their
//! encoding and extracting text from binary formats belong to the caller
//! ([`crate::textio`], [`crate::office`], [`crate::sheets`]).

use serde::{Deserialize, Serialize};
use similar::{Algorithm, ChangeTag, InlineChangeOptions, TextDiff};
use std::borrow::Cow;
use std::time::{Duration, Instant};

/// Limits every comparison runs under.
///
/// Both halves matter. The size limits keep a pathological file from being
/// tokenized at all; the time limit bounds the algorithms themselves, which
/// are super-linear on inputs with many similar-looking lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffBudget {
    /// Largest side, in bytes, that will be compared line by line.
    pub max_bytes: usize,
    /// Largest side, in lines, that will be compared line by line.
    pub max_lines: usize,
    /// Wall-clock ceiling for the whole comparison, including the word-level
    /// refinement of each changed line.
    pub time_limit: Duration,
    /// Unchanged lines kept either side of a run of changes.
    pub context_lines: usize,
}

impl Default for DiffBudget {
    /// 2 MiB / 50 000 lines per side, one second, three lines of context.
    ///
    /// A document a person would sit and read a diff of is orders of
    /// magnitude under the size limits — they exist to catch the export dump
    /// that got saved with a `.txt` extension, not to ration normal work. The
    /// second is chosen against the permission prompt this feeds: longer than
    /// that and the app feels stuck rather than busy. Three lines of context
    /// is the unified-diff convention, which readers already know how to
    /// scan.
    fn default() -> Self {
        Self {
            max_bytes: 2 * 1024 * 1024,
            max_lines: 50_000,
            time_limit: Duration::from_secs(1),
            context_lines: 3,
        }
    }
}

/// What a line contributes to the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    /// Unchanged, shown only to place the change.
    Context,
    /// Present in the old text, gone from the new.
    Removed,
    /// Present in the new text, absent from the old.
    Added,
}

/// A run of characters within one line, flagged for highlighting or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineSpan {
    pub text: String,
    /// True for the words that actually differ between the paired old and new
    /// line. False everywhere else, including on every span of a line that
    /// has no counterpart to compare against.
    pub emphasized: bool,
}

/// One line of a [`Hunk`], split into spans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    /// 1-based line number in the old text; absent for added lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_line: Option<usize>,
    /// 1-based line number in the new text; absent for removed lines.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_line: Option<usize>,
    /// The line's content. Empty for a blank line. Line terminators are not
    /// included — the renderer decides how lines are separated.
    pub spans: Vec<LineSpan>,
}

impl DiffLine {
    /// The line's full text, spans rejoined.
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// One contiguous run of changes with its surrounding context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// 1-based number of the first old line this hunk covers. When
    /// `old_lines` is 0 the hunk inserts immediately before this line.
    pub old_start: usize,
    /// How many old lines the hunk covers, context included.
    pub old_lines: usize,
    /// 1-based number of the first new line this hunk covers. When
    /// `new_lines` is 0 the hunk deletes immediately before this line.
    pub new_start: usize,
    /// How many new lines the hunk covers, context included.
    pub new_lines: usize,
    pub lines: Vec<DiffLine>,
}

/// What the two compared strings actually were, relative to the files they
/// came from.
///
/// This is the difference between "here is what changed" and "here is what
/// changed in the part of the document we can see", and the caller must be
/// able to tell the user which one they are looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonBasis {
    /// The files' own text, in full.
    FullText,
    /// Text pulled out of a format whose bytes cannot be compared directly —
    /// DOCX, PDF, a spreadsheet. Formatting, layout, images and embedded
    /// objects are not in the comparison at all.
    ExtractedText,
}

/// Why a comparison produced a summary instead of a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryReason {
    /// One side is past [`DiffBudget`]'s size limits.
    TooLarge,
    /// The content is not text (see [`looks_binary`]).
    NotText,
}

/// The coarse shape of a change that could not be rendered line by line.
///
/// Sizes rather than content: enough for the user to see that something
/// substantial happened and decide whether to open the file themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeShape {
    pub reason: SummaryReason,
    pub old_bytes: usize,
    pub new_bytes: usize,
    /// Absent for [`SummaryReason::NotText`], where a newline count would be
    /// a number with no meaning rather than a useful one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_lines: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_lines: Option<usize>,
}

/// What changed between two texts, ready to render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangePreview {
    pub added_lines: usize,
    pub removed_lines: usize,
    pub hunks: Vec<Hunk>,
    /// True when the comparison was cut short by the size or time budget.
    pub truncated: bool,
    /// What was compared, which is not always the whole file.
    pub basis: ComparisonBasis,
    /// Present instead of hunks when no line-by-line answer was possible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<ChangeShape>,
}

impl ChangePreview {
    /// True when the comparison found nothing to show — either the texts
    /// match or the comparison was refused.
    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// A sentence the caller can put next to the diff so the user knows what
    /// they are and are not looking at. `None` when the diff needs no
    /// qualification.
    ///
    /// The [`ComparisonBasis::ExtractedText`] wording is deliberately blunt
    /// about the empty case: a change that only altered formatting produces
    /// an identical extraction, so "nothing changed" would be a false
    /// statement made by omission.
    pub fn caveat(&self) -> Option<&'static str> {
        match (self.basis, self.summary.as_ref().map(|s| s.reason)) {
            (_, Some(SummaryReason::NotText)) => {
                Some("This file's contents cannot be compared as text.")
            }
            (_, Some(SummaryReason::TooLarge)) => {
                Some("This file is too large to compare line by line.")
            }
            (ComparisonBasis::ExtractedText, None) if self.is_empty() => Some(
                "The extracted text is identical. A change limited to formatting, \
                 layout or images would not appear here.",
            ),
            (ComparisonBasis::ExtractedText, None) => {
                Some("Comparing extracted text. Formatting and layout changes are not shown.")
            }
            (ComparisonBasis::FullText, None) => None,
        }
    }
}

/// Compare two texts.
///
/// Never runs unbounded: a side past the size budget returns a
/// [`ChangeShape`] summary, and a comparison that exhausts the time budget
/// returns the approximation the algorithm reached with `truncated` set.
///
/// Line-ending style is normalized first, so a file resaved from CRLF to LF
/// reports no change — which is what the user means by "nothing changed",
/// even though the bytes differ.
pub fn text_diff(old: &str, new: &str, budget: DiffBudget) -> ChangePreview {
    compare(old, new, budget, ComparisonBasis::FullText)
}

/// Compare text extracted from two documents whose bytes cannot be diffed —
/// DOCX, PDF, spreadsheets — using whatever [`crate::office`] or
/// [`crate::sheets`] read out of them.
///
/// The result is identical in shape to [`text_diff`] but carries
/// [`ComparisonBasis::ExtractedText`], so [`ChangePreview::caveat`] can say
/// out loud that formatting is outside the comparison.
pub fn extracted_text_diff(old: &str, new: &str, budget: DiffBudget) -> ChangePreview {
    compare(old, new, budget, ComparisonBasis::ExtractedText)
}

/// Describe a change that cannot be shown line by line.
///
/// Takes bytes rather than text because the two cases that need it are a file
/// too large to hold as one string comfortably and a file that is not text at
/// all.
pub fn shape_summary(
    old: &[u8],
    new: &[u8],
    reason: SummaryReason,
    basis: ComparisonBasis,
) -> ChangePreview {
    let (old_lines, new_lines) = match reason {
        SummaryReason::TooLarge => (Some(count_lines(old)), Some(count_lines(new))),
        SummaryReason::NotText => (None, None),
    };
    ChangePreview {
        added_lines: 0,
        removed_lines: 0,
        hunks: Vec::new(),
        truncated: true,
        basis,
        summary: Some(ChangeShape {
            reason,
            old_bytes: old.len(),
            new_bytes: new.len(),
            old_lines,
            new_lines,
        }),
    }
}

/// Whether these bytes should be refused rather than compared as text.
///
/// DOCX, XLSX and PPTX are ZIP containers; PDF and the legacy Office formats
/// are their own binary encodings. A byte-level diff of any of them is noise
/// no user can act on, so the caller is expected to check this first and go
/// through [`extracted_text_diff`] instead. Files carrying a Unicode byte
/// order mark are text however many zero bytes UTF-16 puts in them.
pub fn looks_binary(bytes: &[u8]) -> bool {
    // A whole file is not needed to answer this, and reading further costs
    // more than the answer is worth.
    let head = &bytes[..bytes.len().min(8 * 1024)];
    if head.is_empty() {
        return false;
    }
    if encoding_rs::Encoding::for_bom(head).is_some() {
        return false;
    }
    const OOXML_ZIP: &[u8] = b"PK\x03\x04";
    const EMPTY_ZIP: &[u8] = b"PK\x05\x06";
    const PDF: &[u8] = b"%PDF-";
    // The OLE compound-file header, shared by .doc/.xls/.ppt.
    const OLE: &[u8] = &[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    if [OOXML_ZIP, EMPTY_ZIP, PDF, OLE]
        .iter()
        .any(|magic| head.starts_with(magic))
    {
        return true;
    }
    // The classic test, and the one git uses: a NUL byte in the leading block
    // means nothing downstream will treat this as lines of text.
    head.contains(&0)
}

/// The shared body of [`text_diff`] and [`extracted_text_diff`].
fn compare(old: &str, new: &str, budget: DiffBudget, basis: ComparisonBasis) -> ChangePreview {
    if over_budget(old, new, &budget) {
        return shape_summary(
            old.as_bytes(),
            new.as_bytes(),
            SummaryReason::TooLarge,
            basis,
        );
    }

    // Owned only when the text actually carries carriage returns, so the
    // common case does not pay for a second copy of the document.
    let old_text = normalize_newlines(old);
    let new_text = normalize_newlines(new);

    let started = Instant::now();
    // One deadline covers both passes. If the line-level pass spends the
    // whole budget the word-level pass gets nothing and silently falls back
    // to unemphasized lines, which is the right way for this to degrade.
    let deadline = started.checked_add(budget.time_limit);

    let mut config = TextDiff::configure();
    // Patience anchors on lines that appear exactly once on each side, which
    // on prose keeps a hunk pinned to the paragraph it belongs to instead of
    // pairing up unrelated blank lines and list markers the way Myers does.
    config.algorithm(Algorithm::Patience);
    if let Some(deadline) = deadline {
        config.deadline(deadline);
    }
    let diff = config.diff_lines(old_text.as_ref(), new_text.as_ref());

    let mut inline_options = InlineChangeOptions::default();
    // Costs a second pass over each replaced region and is worth it here:
    // without it the highlight boundaries land mid-word on reflowed prose.
    inline_options.semantic_cleanup(true);

    let mut hunks = Vec::new();
    let mut added_lines = 0usize;
    let mut removed_lines = 0usize;

    for group in diff.grouped_ops(budget.context_lines) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old_range = first.old_range().start..last.old_range().end;
        let new_range = first.new_range().start..last.new_range().end;

        let mut lines = Vec::new();
        for op in &group {
            for change in
                diff.iter_inline_changes_with_options_deadline(op, inline_options, deadline)
            {
                let kind = match change.tag() {
                    ChangeTag::Equal => LineKind::Context,
                    ChangeTag::Delete => LineKind::Removed,
                    ChangeTag::Insert => LineKind::Added,
                };
                match kind {
                    LineKind::Added => added_lines += 1,
                    LineKind::Removed => removed_lines += 1,
                    LineKind::Context => {}
                }

                let mut spans: Vec<LineSpan> = change
                    .iter_strings_lossy()
                    .map(|(emphasized, text)| LineSpan {
                        text: text.into_owned(),
                        emphasized,
                    })
                    .collect();
                strip_line_terminator(&mut spans);

                lines.push(DiffLine {
                    kind,
                    old_line: change.old_index().map(|i| i + 1),
                    new_line: change.new_index().map(|i| i + 1),
                    spans,
                });
            }
        }

        hunks.push(Hunk {
            old_start: old_range.start + 1,
            old_lines: old_range.len(),
            new_start: new_range.start + 1,
            new_lines: new_range.len(),
            lines,
        });
    }

    ChangePreview {
        added_lines,
        removed_lines,
        hunks,
        // `similar` approximates rather than failing once a deadline passes,
        // so having spent the budget is the only signal that the answer may
        // not be the best available one. Reporting it is the honest move.
        truncated: started.elapsed() >= budget.time_limit,
        basis,
        summary: None,
    }
}

fn over_budget(old: &str, new: &str, budget: &DiffBudget) -> bool {
    if old.len() > budget.max_bytes || new.len() > budget.max_bytes {
        return true;
    }
    count_lines(old.as_bytes()) > budget.max_lines || count_lines(new.as_bytes()) > budget.max_lines
}

/// Lines in `bytes`, counting a final line that has no terminator.
fn count_lines(bytes: &[u8]) -> usize {
    if bytes.is_empty() {
        return 0;
    }
    let terminators = bytes.iter().filter(|b| **b == b'\n').count();
    if bytes.ends_with(b"\n") {
        terminators
    } else {
        terminators + 1
    }
}

/// Collapse CRLF and lone CR to LF.
///
/// A document saved by a Windows editor and the same document saved by a Mac
/// one differ in every single line, and telling the user that is telling them
/// nothing. Line-ending style is not content here.
fn normalize_newlines(text: &str) -> Cow<'_, str> {
    if !text.contains('\r') {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            // Consume the LF of a CRLF pair; a lone CR becomes an LF of its
            // own so old-Mac files still split into lines.
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    Cow::Owned(out)
}

/// Drop the trailing newline the line tokenizer keeps attached, so spans hold
/// only what a renderer should draw.
fn strip_line_terminator(spans: &mut Vec<LineSpan>) {
    let Some(last) = spans.last_mut() else {
        return;
    };
    if last.text.ends_with('\n') {
        last.text.pop();
    }
    if last.text.is_empty() {
        spans.pop();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// The lines of a preview, flattened, as `(kind, text)`.
    fn flat(preview: &ChangePreview) -> Vec<(LineKind, String)> {
        preview
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .map(|l| (l.kind, l.text()))
            .collect()
    }

    fn changed(preview: &ChangePreview, kind: LineKind) -> Vec<String> {
        flat(preview)
            .into_iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, t)| t)
            .collect()
    }

    #[test]
    fn identical_texts_have_nothing_to_show() {
        let text = "Termination\n\nEither party may terminate on 30 days' notice.\n";
        let preview = text_diff(text, text, DiffBudget::default());
        assert!(preview.is_empty());
        assert!(preview.hunks.is_empty());
        assert_eq!(preview.added_lines, 0);
        assert_eq!(preview.removed_lines, 0);
        assert!(!preview.truncated);
        assert_eq!(preview.summary, None);
        assert_eq!(preview.caveat(), None);
    }

    #[test]
    fn pure_insertion() {
        let old = "One\nTwo\n";
        let new = "One\nOne and a half\nTwo\n";
        let preview = text_diff(old, new, DiffBudget::default());
        assert_eq!(preview.added_lines, 1);
        assert_eq!(preview.removed_lines, 0);
        assert_eq!(changed(&preview, LineKind::Added), vec!["One and a half"]);
        assert_eq!(preview.hunks.len(), 1);
        assert!(changed(&preview, LineKind::Removed).is_empty());
    }

    #[test]
    fn pure_deletion() {
        let old = "One\nTwo\nThree\n";
        let new = "One\nThree\n";
        let preview = text_diff(old, new, DiffBudget::default());
        assert_eq!(preview.removed_lines, 1);
        assert_eq!(preview.added_lines, 0);
        assert_eq!(changed(&preview, LineKind::Removed), vec!["Two"]);
    }

    #[test]
    fn a_modified_line_is_highlighted_word_by_word() {
        let old = "The fee is 500 EUR per month.\n";
        let new = "The fee is 750 EUR per month.\n";
        let preview = text_diff(old, new, DiffBudget::default());
        assert_eq!(preview.added_lines, 1);
        assert_eq!(preview.removed_lines, 1);

        let removed = preview
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .find(|l| l.kind == LineKind::Removed)
            .expect("the old line is present");
        let added = preview
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .find(|l| l.kind == LineKind::Added)
            .expect("the new line is present");

        // The point of the whole feature: only the number is marked, not the
        // sentence around it.
        let emphasized = |line: &DiffLine| -> String {
            line.spans
                .iter()
                .filter(|s| s.emphasized)
                .map(|s| s.text.as_str())
                .collect()
        };
        assert_eq!(emphasized(removed), "500");
        assert_eq!(emphasized(added), "750");
        assert_eq!(removed.text(), "The fee is 500 EUR per month.");
        assert_eq!(added.text(), "The fee is 750 EUR per month.");
        assert!(removed.spans.iter().any(|s| !s.emphasized));
    }

    #[test]
    fn separated_changes_become_separate_hunks() {
        let mut old: Vec<String> = (0..40).map(|i| format!("line {i}")).collect();
        let mut new = old.clone();
        new[2] = "line 2 revised".into();
        new[35] = "line 35 revised".into();
        old.push(String::new());
        new.push(String::new());

        let preview = text_diff(&old.join("\n"), &new.join("\n"), DiffBudget::default());
        assert_eq!(preview.hunks.len(), 2, "hunks: {:#?}", preview.hunks);
        assert_eq!(preview.added_lines, 2);
        assert_eq!(preview.removed_lines, 2);
        // Context on both sides of each change, and nothing beyond it.
        for hunk in &preview.hunks {
            assert!(hunk.old_lines <= 7, "hunk too wide: {hunk:?}");
            assert!(hunk.old_start >= 1);
            assert!(hunk.new_start >= 1);
        }
    }

    #[test]
    fn line_numbers_are_one_based_and_side_specific() {
        let preview = text_diff("a\nb\nc\n", "a\nB\nc\n", DiffBudget::default());
        let lines = &preview.hunks[0].lines;
        let first = &lines[0];
        assert_eq!(first.kind, LineKind::Context);
        assert_eq!(first.old_line, Some(1));
        assert_eq!(first.new_line, Some(1));

        let removed = lines
            .iter()
            .find(|l| l.kind == LineKind::Removed)
            .expect("removed line");
        assert_eq!(removed.old_line, Some(2));
        assert_eq!(removed.new_line, None);

        let added = lines
            .iter()
            .find(|l| l.kind == LineKind::Added)
            .expect("added line");
        assert_eq!(added.new_line, Some(2));
        assert_eq!(added.old_line, None);
    }

    #[test]
    fn size_budget_returns_a_summary_instead_of_a_diff() {
        let budget = DiffBudget {
            max_bytes: 64,
            ..DiffBudget::default()
        };
        let old = "a\n".repeat(100);
        let preview = text_diff(&old, "b\n", budget);
        assert!(preview.truncated);
        assert!(preview.hunks.is_empty());
        let shape = preview.summary.as_ref().expect("a summary");
        assert_eq!(shape.reason, SummaryReason::TooLarge);
        assert_eq!(shape.old_bytes, 200);
        assert_eq!(shape.new_bytes, 2);
        assert_eq!(shape.old_lines, Some(100));
        assert_eq!(shape.new_lines, Some(1));
        assert!(preview.caveat().is_some());
    }

    #[test]
    fn line_budget_returns_a_summary_instead_of_a_diff() {
        let budget = DiffBudget {
            max_lines: 10,
            ..DiffBudget::default()
        };
        let preview = text_diff(&"x\n".repeat(50), "x\n", budget);
        assert!(preview.truncated);
        assert_eq!(
            preview.summary.map(|s| s.reason),
            Some(SummaryReason::TooLarge)
        );
    }

    #[test]
    fn a_new_file_is_all_additions() {
        let new = "First line.\nSecond line.\n";
        let preview = text_diff("", new, DiffBudget::default());
        assert_eq!(preview.added_lines, 2);
        assert_eq!(preview.removed_lines, 0);
        assert_eq!(
            changed(&preview, LineKind::Added),
            vec!["First line.", "Second line."]
        );
        assert_eq!(preview.hunks[0].old_lines, 0);
        assert_eq!(preview.hunks[0].new_lines, 2);
    }

    #[test]
    fn emptying_a_file_is_all_removals() {
        let old = "First line.\nSecond line.\n";
        let preview = text_diff(old, "", DiffBudget::default());
        assert_eq!(preview.removed_lines, 2);
        assert_eq!(preview.added_lines, 0);
        assert_eq!(preview.hunks[0].new_lines, 0);
        assert_eq!(preview.hunks[0].old_lines, 2);
    }

    #[test]
    fn two_empty_texts_are_unchanged() {
        let preview = text_diff("", "", DiffBudget::default());
        assert!(preview.is_empty());
        assert!(!preview.truncated);
    }

    #[test]
    fn crlf_against_lf_is_not_a_change() {
        let crlf = "Heading\r\n\r\nBody paragraph.\r\n";
        let lf = "Heading\n\nBody paragraph.\n";
        let preview = text_diff(crlf, lf, DiffBudget::default());
        assert!(preview.is_empty(), "hunks: {:#?}", preview.hunks);
        assert_eq!(preview.added_lines, 0);
        assert_eq!(preview.removed_lines, 0);
    }

    #[test]
    fn crlf_against_lf_still_reports_real_edits() {
        let crlf = "Heading\r\nBody paragraph.\r\n";
        let lf = "Heading\nBody paragraph, revised.\n";
        let preview = text_diff(crlf, lf, DiffBudget::default());
        assert_eq!(preview.added_lines, 1);
        assert_eq!(preview.removed_lines, 1);
        // The carriage return must not survive into what gets drawn.
        assert!(flat(&preview).iter().all(|(_, text)| !text.contains('\r')));
    }

    #[test]
    fn lone_carriage_returns_split_lines() {
        let preview = text_diff("one\rtwo\r", "one\ntwo\n", DiffBudget::default());
        assert!(preview.is_empty());
    }

    #[test]
    fn multibyte_text_is_never_sliced_by_byte() {
        let old = "Résumé — 概要\n価格は 500 円です。\nنص عربي\n";
        let new = "Résumé — 概要\n価格は 750 円です。\nنص عربي\n";
        let preview = text_diff(old, new, DiffBudget::default());
        assert_eq!(preview.added_lines, 1);
        assert_eq!(preview.removed_lines, 1);

        let added = preview
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .find(|l| l.kind == LineKind::Added)
            .expect("added line");
        assert_eq!(added.text(), "価格は 750 円です。");
        // Every span must still be a whole, valid string; a byte slice
        // through a multi-byte character would have failed long before here.
        assert!(added.spans.iter().all(|s| s.text.chars().count() > 0));

        let context: Vec<String> = changed(&preview, LineKind::Context);
        assert!(context.iter().any(|l| l == "Résumé — 概要"));
        assert!(context.iter().any(|l| l == "نص عربي"));
    }

    #[test]
    fn combining_characters_and_emoji_survive_intact() {
        let old = "family 👨‍👩‍👧‍👦 and cafe\u{0301}\n";
        let new = "family 👨‍👩‍👧‍👦 and the\u{0301} cafe\u{0301}\n";
        let preview = text_diff(old, new, DiffBudget::default());
        let added = preview
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .find(|l| l.kind == LineKind::Added)
            .expect("added line");
        assert_eq!(added.text(), new.trim_end_matches('\n'));
    }

    #[test]
    fn a_very_long_single_line_is_compared_without_panicking() {
        // No trailing newline either, which is the case that trips naive
        // line splitting.
        let old = "word ".repeat(60_000);
        let new = format!("{old}tail");
        let preview = text_diff(&old, &new, DiffBudget::default());
        assert!(!preview.hunks.is_empty());
        assert_eq!(preview.added_lines, 1);
        assert_eq!(preview.removed_lines, 1);
    }

    #[test]
    fn a_pathological_input_degrades_instead_of_hanging() {
        // Thousands of near-identical lines is the shape that makes diff
        // algorithms expensive; with a tiny budget the call must still
        // return promptly.
        let old: String = (0..8_000).map(|i| format!("row {}\n", i % 7)).collect();
        let new: String = (0..8_000)
            .map(|i| format!("row {}\n", (i + 1) % 7))
            .collect();
        let budget = DiffBudget {
            time_limit: Duration::from_millis(5),
            ..DiffBudget::default()
        };
        let started = Instant::now();
        let preview = text_diff(&old, &new, budget);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "took {:?}",
            started.elapsed()
        );
        // Spending the budget has to be reported, or a user reading an
        // approximate answer has no way to know that is what it is.
        assert!(preview.truncated);
        // Whatever it found, the counts must agree with the lines emitted.
        assert_eq!(
            preview.added_lines,
            changed(&preview, LineKind::Added).len()
        );
        assert_eq!(
            preview.removed_lines,
            changed(&preview, LineKind::Removed).len()
        );
    }

    #[test]
    fn office_and_pdf_containers_are_refused() {
        assert!(looks_binary(b"PK\x03\x04\x14\x00\x06\x00word/document.xml"));
        assert!(looks_binary(b"PK\x05\x06\x00\x00\x00\x00"));
        assert!(looks_binary(b"%PDF-1.7\n1 0 obj\n"));
        assert!(looks_binary(&[
            0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1, 0x00
        ]));
        assert!(looks_binary(b"plain looking\x00but not"));
    }

    #[test]
    fn text_including_unicode_and_boms_is_not_refused() {
        assert!(!looks_binary(b""));
        assert!(!looks_binary("# Report\n\nRévenu: 1 240 EUR\n".as_bytes()));
        assert!(!looks_binary(b"\xEF\xBB\xBFwith a UTF-8 BOM"));
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in "hi".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(!looks_binary(&utf16));
    }

    #[test]
    fn refusing_binary_still_describes_the_change() {
        let old = b"PK\x03\x04 old docx bytes";
        let new = b"PK\x03\x04 new docx bytes, longer";
        assert!(looks_binary(old) && looks_binary(new));
        let preview = shape_summary(old, new, SummaryReason::NotText, ComparisonBasis::FullText);
        assert!(preview.truncated);
        assert!(preview.hunks.is_empty());
        let shape = preview.summary.as_ref().expect("a summary");
        assert_eq!(shape.reason, SummaryReason::NotText);
        assert_eq!(shape.old_bytes, old.len());
        assert_eq!(shape.new_bytes, new.len());
        // Newline counts over binary bytes would be a meaningless number.
        assert_eq!(shape.old_lines, None);
        assert_eq!(shape.new_lines, None);
    }

    #[test]
    fn extracted_text_says_what_it_left_out() {
        let old = "Quarterly report\n\nRevenue grew.\n";
        let new = "Quarterly report\n\nRevenue grew sharply.\n";
        let preview = extracted_text_diff(old, new, DiffBudget::default());
        assert_eq!(preview.basis, ComparisonBasis::ExtractedText);
        assert_eq!(preview.added_lines, 1);
        let caveat = preview.caveat().expect("a caveat");
        assert!(caveat.contains("extracted text"), "caveat: {caveat}");
    }

    #[test]
    fn identical_extracted_text_does_not_claim_nothing_changed() {
        let text = "Quarterly report\n\nRevenue grew.\n";
        let preview = extracted_text_diff(text, text, DiffBudget::default());
        assert!(preview.is_empty());
        let caveat = preview.caveat().expect("a caveat");
        // A formatting-only edit lands here, and the wording has to admit it.
        assert!(caveat.contains("formatting"), "caveat: {caveat}");
    }

    #[test]
    fn previews_survive_a_json_round_trip() {
        let preview = text_diff(
            "The fee is 500 EUR.\nUnchanged — 概要\n",
            "The fee is 750 EUR.\nUnchanged — 概要\n",
            DiffBudget::default(),
        );
        let json = serde_json::to_string(&preview).unwrap();
        let back: ChangePreview = serde_json::from_str(&json).unwrap();
        assert_eq!(back, preview);
    }

    #[test]
    fn budgets_survive_a_json_round_trip() {
        let budget = DiffBudget::default();
        let json = serde_json::to_string(&budget).unwrap();
        assert_eq!(serde_json::from_str::<DiffBudget>(&json).unwrap(), budget);
    }

    #[test]
    fn blank_lines_are_represented_without_spans() {
        let preview = text_diff("a\nb\n", "a\n\nb\n", DiffBudget::default());
        let added = preview
            .hunks
            .iter()
            .flat_map(|h| h.lines.iter())
            .find(|l| l.kind == LineKind::Added)
            .expect("added line");
        assert!(added.spans.is_empty());
        assert_eq!(added.text(), "");
    }

    #[test]
    fn a_file_without_a_trailing_newline_compares_cleanly() {
        let preview = text_diff("alpha\nbeta", "alpha\ngamma", DiffBudget::default());
        assert_eq!(changed(&preview, LineKind::Removed), vec!["beta"]);
        assert_eq!(changed(&preview, LineKind::Added), vec!["gamma"]);
    }
}
