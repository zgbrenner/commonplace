//! Deterministic document reading and creation.
//!
//! The model never emits binary formats. It asks for structured content and
//! this layer builds the file, then **validates it by re-parsing with an
//! independent reader** before success is reported (docs/document-tools.md).
//!
//! MVP scope: PDF text extraction, DOCX structured extraction, and DOCX
//! creation. Formatting-preserving DOCX edits, XLSX, and PPTX come later; the
//! result type and validation contract are already shaped for them.

use commonspace_core::{OperationResult, ValidationOutcome};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DocumentError {
    #[error("could not read {path}: {detail}")]
    Read { path: String, detail: String },
    #[error("could not create {path}: {detail}")]
    Write { path: String, detail: String },
    #[error("the file was written but could not be re-opened: {0}")]
    Validation(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Structured text extracted from a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedDocument {
    /// Plain text, in reading order.
    pub text: String,
    /// Paragraph-level blocks, when the format exposes them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paragraphs: Vec<String>,
    /// Page count for paginated formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<usize>,
    /// True when the extraction was cut short by a size limit.
    pub truncated: bool,
}

/// One block of a document Commonspace is asked to create.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocBlock {
    /// A heading; `level` 1–6.
    Heading {
        level: u8,
        text: String,
    },
    Paragraph {
        text: String,
    },
    /// A bulleted list.
    Bullets {
        items: Vec<String>,
    },
}

/// Extract text from a PDF.
pub fn read_pdf(path: &Path, max_chars: usize) -> Result<ExtractedDocument, DocumentError> {
    let pages = lopdf::Document::load(path)
        .ok()
        .map(|d| d.get_pages().len());

    let text = pdf_extract::extract_text(path).map_err(|e| DocumentError::Read {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;

    let truncated = text.chars().count() > max_chars;
    let text: String = if truncated {
        text.chars().take(max_chars).collect()
    } else {
        text
    };
    let paragraphs = text
        .split("\n\n")
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect();

    Ok(ExtractedDocument {
        text,
        paragraphs,
        pages,
        truncated,
    })
}

/// Extract text and structure from a DOCX.
pub fn read_docx(path: &Path, max_chars: usize) -> Result<ExtractedDocument, DocumentError> {
    let bytes = std::fs::read(path)?;
    let doc = docx_rs::read_docx(&bytes).map_err(|e| DocumentError::Read {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;

    let mut paragraphs = Vec::new();
    for child in doc.document.children {
        if let docx_rs::DocumentChild::Paragraph(paragraph) = child {
            let mut line = String::new();
            for run_child in paragraph.children {
                if let docx_rs::ParagraphChild::Run(run) = run_child {
                    for content in run.children {
                        if let docx_rs::RunChild::Text(text) = content {
                            line.push_str(&text.text);
                        }
                    }
                }
            }
            let line = line.trim().to_string();
            if !line.is_empty() {
                paragraphs.push(line);
            }
        }
    }

    let joined = paragraphs.join("\n\n");
    let truncated = joined.chars().count() > max_chars;
    let text: String = if truncated {
        joined.chars().take(max_chars).collect()
    } else {
        joined
    };

    Ok(ExtractedDocument {
        text,
        paragraphs,
        pages: None,
        truncated,
    })
}

/// Create a DOCX from structured blocks, then verify it re-opens.
///
/// The caller is responsible for having passed the destination through the
/// permission engine; this function only writes and validates.
pub fn create_docx(path: &Path, blocks: &[DocBlock]) -> Result<OperationResult, DocumentError> {
    use docx_rs::*;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let mut docx = Docx::new();
    for block in blocks {
        match block {
            DocBlock::Heading { level, text } => {
                // Word's built-in Heading styles are named Heading1..6.
                let style = format!("Heading{}", (*level).clamp(1, 6));
                docx = docx.add_paragraph(
                    Paragraph::new()
                        .add_run(Run::new().add_text(text))
                        .style(&style),
                );
            }
            DocBlock::Paragraph { text } => {
                docx = docx.add_paragraph(Paragraph::new().add_run(Run::new().add_text(text)));
            }
            DocBlock::Bullets { items } => {
                for item in items {
                    docx = docx.add_paragraph(
                        Paragraph::new()
                            .add_run(Run::new().add_text(item))
                            .numbering(NumberingId::new(1), IndentLevel::new(0)),
                    );
                }
            }
        }
    }

    let file = std::fs::File::create(path)?;
    docx.build().pack(file).map_err(|e| DocumentError::Write {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;

    // Validation: re-read the file we just produced. This catches truncated
    // writes and malformed packages before the user is told it worked.
    // `self::` is load-bearing: the glob import above brings
    // `docx_rs::read_docx` into scope, and validation must use *our* reader.
    let round_trip =
        self::read_docx(path, usize::MAX).map_err(|e| DocumentError::Validation(e.to_string()))?;
    let expected_lines: Vec<&str> = blocks
        .iter()
        .flat_map(|b| match b {
            DocBlock::Heading { text, .. } | DocBlock::Paragraph { text } => {
                vec![text.as_str()]
            }
            DocBlock::Bullets { items } => items.iter().map(String::as_str).collect(),
        })
        .collect();
    for expected in &expected_lines {
        if !round_trip.paragraphs.iter().any(|p| p == expected) {
            return Err(DocumentError::Validation(format!(
                "the created document is missing the line {expected:?}"
            )));
        }
    }

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let mut result = OperationResult::ok(format!(
        "Created {name} with {} {}",
        expected_lines.len(),
        if expected_lines.len() == 1 {
            "line"
        } else {
            "lines"
        }
    ));
    result.created.push(path.to_path_buf());
    result.validation = ValidationOutcome::Passed;
    Ok(result)
}

/// Convert Markdown-ish plain text into document blocks. Deliberately simple:
/// headings from leading `#`, bullets from `-`/`*`, everything else a
/// paragraph. Agents that want precise structure pass blocks directly.
pub fn blocks_from_markdown(source: &str) -> Vec<DocBlock> {
    let mut blocks = Vec::new();
    let mut bullets: Vec<String> = Vec::new();

    let flush = |bullets: &mut Vec<String>, blocks: &mut Vec<DocBlock>| {
        if !bullets.is_empty() {
            blocks.push(DocBlock::Bullets {
                items: std::mem::take(bullets),
            });
        }
    };

    for line in source.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush(&mut bullets, &mut blocks);
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix('#') {
            flush(&mut bullets, &mut blocks);
            let level = 1 + rest.chars().take_while(|c| *c == '#').count() as u8;
            let text = rest.trim_start_matches('#').trim().to_string();
            blocks.push(DocBlock::Heading {
                level: level.min(6),
                text,
            });
        } else if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            bullets.push(rest.trim().to_string());
        } else {
            flush(&mut bullets, &mut blocks);
            blocks.push(DocBlock::Paragraph {
                text: trimmed.to_string(),
            });
        }
    }
    flush(&mut bullets, &mut blocks);
    blocks
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn markdown_becomes_blocks() {
        let blocks = blocks_from_markdown("# Title\n\nIntro line.\n\n- one\n- two\n\n## Next");
        assert_eq!(
            blocks,
            vec![
                DocBlock::Heading {
                    level: 1,
                    text: "Title".into()
                },
                DocBlock::Paragraph {
                    text: "Intro line.".into()
                },
                DocBlock::Bullets {
                    items: vec!["one".into(), "two".into()]
                },
                DocBlock::Heading {
                    level: 2,
                    text: "Next".into()
                },
            ]
        );
    }

    #[test]
    fn docx_round_trips_through_an_independent_read() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("report.docx");
        let blocks = blocks_from_markdown(
            "# Quarterly report\n\nRevenue grew.\n\n- Acme: 1,240 EUR\n- Globex: 980 EUR",
        );

        let result = create_docx(&path, &blocks).unwrap();
        assert!(result.success);
        assert_eq!(result.validation, ValidationOutcome::Passed);
        assert_eq!(result.created, vec![path.clone()]);
        assert!(path.metadata().unwrap().len() > 0);

        let read_back = read_docx(&path, 10_000).unwrap();
        assert!(read_back.text.contains("Quarterly report"));
        assert!(read_back.text.contains("Acme: 1,240 EUR"));
        assert_eq!(read_back.paragraphs.len(), 4);
    }

    #[test]
    fn docx_handles_unicode_and_long_content() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("données 报告.docx");
        let long = "x".repeat(5_000);
        let blocks = vec![
            DocBlock::Heading {
                level: 1,
                text: "Résumé — 概要".into(),
            },
            DocBlock::Paragraph { text: long.clone() },
        ];
        create_docx(&path, &blocks).unwrap();
        let read_back = read_docx(&path, usize::MAX).unwrap();
        assert!(read_back.text.contains("Résumé — 概要"));
        assert!(read_back.text.contains(&long));
    }

    #[test]
    fn truncation_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("long.docx");
        create_docx(
            &path,
            &[DocBlock::Paragraph {
                text: "y".repeat(1_000),
            }],
        )
        .unwrap();
        let read_back = read_docx(&path, 100).unwrap();
        assert!(read_back.truncated);
        assert_eq!(read_back.text.chars().count(), 100);
    }

    #[test]
    fn malformed_docx_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("broken.docx");
        std::fs::write(&path, b"this is definitely not a zip archive").unwrap();
        let error = read_docx(&path, 1_000).unwrap_err();
        assert!(matches!(error, DocumentError::Read { .. }));
    }

    #[test]
    fn malformed_pdf_is_an_error_not_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("broken.pdf");
        std::fs::write(&path, b"%PDF-1.7\nnot really a pdf").unwrap();
        // Either a clean error or empty text is acceptable; a panic is not.
        match read_pdf(&path, 1_000) {
            Ok(doc) => assert!(doc.text.trim().is_empty()),
            Err(DocumentError::Read { .. }) => {}
            Err(other) => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn pdf_text_is_extracted() {
        // A minimal one-page PDF with a text object, written by hand so the
        // test needs no binary fixture in the repository.
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("hello.pdf");
        std::fs::write(&path, minimal_pdf("Commonspace test page")).unwrap();

        let doc = read_pdf(&path, 10_000).unwrap();
        assert!(
            doc.text.contains("Commonspace test page"),
            "extracted: {:?}",
            doc.text
        );
        assert_eq!(doc.pages, Some(1));
    }

    /// Build a valid single-page PDF containing `message`.
    fn minimal_pdf(message: &str) -> Vec<u8> {
        let content = format!("BT /F1 24 Tf 72 700 Td ({message}) Tj ET");
        let mut objects: Vec<String> = Vec::new();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        objects.push("<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into());
        objects.push(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"
                .into(),
        );
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());

        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{body}\nendobj\n", index + 1));
        }
        let xref_at = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in &offsets {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n",
            objects.len() + 1
        ));
        pdf.into_bytes()
    }
}
