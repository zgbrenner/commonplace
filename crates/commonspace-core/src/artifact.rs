//! Artifacts: files the agent created or modified, surfaced as cards with
//! previews, backup locations, and undo affordances.

use crate::ids::{ArtifactId, FileOperationId, TaskId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// What kind of artifact this is, driving the preview renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Docx,
    Xlsx,
    Pptx,
    Pdf,
    Markdown,
    Text,
    Image,
    CodeDiff,
    Other,
}

impl ArtifactKind {
    /// Best-effort classification from a file extension (lowercased).
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "docx" => ArtifactKind::Docx,
            "xlsx" => ArtifactKind::Xlsx,
            "pptx" => ArtifactKind::Pptx,
            "pdf" => ArtifactKind::Pdf,
            "md" | "markdown" => ArtifactKind::Markdown,
            "txt" | "csv" | "log" => ArtifactKind::Text,
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => ArtifactKind::Image,
            _ => ArtifactKind::Other,
        }
    }
}

/// A generated or modified file, as shown in the artifact panel.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: ArtifactId,
    pub task_id: TaskId,
    pub kind: ArtifactKind,
    /// Resolved absolute path of the artifact.
    pub path: PathBuf,
    /// Display name (usually the file name).
    pub name: String,
    /// True when this artifact modified a pre-existing file.
    pub modified_existing: bool,
    /// Backup of the original, when one was taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<PathBuf>,
    /// Journal entry enabling undo, when the operation supports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_operation_id: Option<FileOperationId>,
    /// Concise human-readable summary of what changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_summary: Option<String>,
    pub created_at: DateTime<Utc>,
}
