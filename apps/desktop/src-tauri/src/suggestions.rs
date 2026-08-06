//! Task suggestions for the empty state, derived from what is actually in
//! the project's folders.
//!
//! Someone who has just opened a project and has nothing typed needs a
//! starting point, and a generic list ("summarize a document!") is worse
//! than nothing — it describes Commonspace instead of describing their
//! files. So the empty state asks here, and this module answers from a
//! bounded look at the authorized roots: three PDFs on disk is why
//! "Summarize the PDFs" appears, and when nothing on disk crosses a
//! threshold the answer is an empty list. Commonspace does not invent work.
//!
//! Two properties matter more than cleverness:
//!
//! 1. **Cheap.** This runs on every empty-state render. The walk is depth-
//!    and entry-limited, never follows symlinks, and happens on a blocking
//!    thread so the UI is not waiting on a network drive.
//! 2. **Quiet.** A busy, unreadable, or missing folder is skipped. Nobody
//!    should see an error because a suggestion could not be computed.

use serde::Serialize;
use std::path::{Path, PathBuf};
use tauri::State;

use commonspace_core::WorkspaceId;

use crate::commands::CommandError;
use crate::state::AppState;

type Result<T> = std::result::Result<T, CommandError>;

/// One concrete thing worth doing in this project. `label` is the button
/// text; `prompt` is the message sent verbatim if the user picks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TaskSuggestion {
    /// Stable identifier for the rule that produced this suggestion, so the
    /// frontend can key a list and so telemetry-free debugging still has a
    /// name to talk about.
    pub id: String,
    pub label: String,
    pub prompt: String,
}

/// What a bounded survey of the project's folders found. Counts are of
/// files, by category; `loose_files` counts files sitting directly in a
/// root, which is a different question (tidiness) about the same files.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FolderSurvey {
    pub spreadsheets: u32,
    pub documents: u32,
    pub pdfs: u32,
    pub images: u32,
    pub slides: u32,
    pub notes: u32,
    pub loose_files: u32,
}

impl FolderSurvey {
    /// Files the survey recognized, across every category. `loose_files` is
    /// deliberately excluded: it counts the same files again by location.
    /// Used for the log line that explains an empty suggestion list.
    pub fn recognized_files(&self) -> u32 {
        self.spreadsheets
            .saturating_add(self.documents)
            .saturating_add(self.pdfs)
            .saturating_add(self.images)
            .saturating_add(self.slides)
            .saturating_add(self.notes)
    }
}

/* -------------------------------------------------------------- the rules */

/// One suggestion and the evidence it needs. `count` reads the category the
/// rule is about, so the rule table stays a table rather than a match arm.
struct Rule {
    id: &'static str,
    label: &'static str,
    prompt: &'static str,
    /// Fewest files of this kind that make the suggestion worth offering.
    threshold: u32,
    count: fn(&FolderSurvey) -> u32,
}

/// The rules, in declaration order. That order is also the tiebreak: two
/// rules with equal counts are offered in the order they appear here, so
/// the same folder always produces the same list. Rules are listed roughly
/// by how unambiguous the resulting work is.
const RULES: &[Rule] = &[
    Rule {
        id: "pdf-summaries",
        label: "Summarize the PDFs",
        prompt: "Read each PDF in this project and write a short plain-language summary of it \
                 into a single Markdown file.",
        threshold: 3,
        count: |s| s.pdfs,
    },
    Rule {
        id: "spreadsheet-merge",
        label: "Combine the spreadsheets",
        prompt: "Look at the spreadsheets in this project and combine them into one table, \
                 telling me about any columns that don't line up.",
        threshold: 2,
        count: |s| s.spreadsheets,
    },
    Rule {
        id: "document-compare",
        label: "Compare the documents",
        prompt: "Read the documents in this project and tell me where they differ in substance, \
                 not formatting.",
        threshold: 3,
        count: |s| s.documents,
    },
    Rule {
        id: "slide-outline",
        label: "Outline the decks",
        prompt: "Read the slide decks in this project and write one outline covering what each \
                 one covers.",
        threshold: 2,
        count: |s| s.slides,
    },
    Rule {
        id: "scan-rename",
        label: "Rename the scans",
        prompt: "Look at the images in this project and rename each one to describe what it is \
                 and the date on it, then show me the list before renaming anything.",
        threshold: 8,
        count: |s| s.images,
    },
    Rule {
        id: "tidy-folder",
        label: "Tidy this folder",
        prompt: "Look at the loose files in this project, suggest a folder structure for them, \
                 and show me the plan before moving anything.",
        threshold: 25,
        count: |s| s.loose_files,
    },
];

/// How many suggestions the empty state can show without becoming a menu.
const MAX_SUGGESTIONS: usize = 3;

/// Concrete things worth offering, most-supported first. Empty when nothing
/// in the folders crosses a threshold — Commonspace does not invent work.
///
/// "Most-supported" is the raw file count behind each rule: a folder of
/// forty scans leads with the scans. Equal counts fall back to the order of
/// [`RULES`], so the same folder always yields the same order.
pub fn suggestions_from(survey: &FolderSurvey) -> Vec<TaskSuggestion> {
    let mut matched: Vec<(u32, &Rule)> = RULES
        .iter()
        .map(|rule| ((rule.count)(survey), rule))
        .filter(|(count, rule)| *count >= rule.threshold)
        .collect();

    // A stable sort, so ties keep the declaration order established above.
    matched.sort_by_key(|(count, _)| std::cmp::Reverse(*count));
    matched.truncate(MAX_SUGGESTIONS);

    matched
        .into_iter()
        .map(|(_, rule)| TaskSuggestion {
            id: rule.id.to_string(),
            label: rule.label.to_string(),
            prompt: rule.prompt.to_string(),
        })
        .collect()
}

/* ------------------------------------------------------------- the survey */

/// How far below each root to look. Deep enough to see `Invoices/2024/*`,
/// shallow enough that a home directory does not become a full disk scan.
const MAX_DEPTH: u32 = 2;

/// Directory entries examined across all roots before the survey stops.
/// Suggestions are a hint, not an inventory: whatever the first couple of
/// thousand entries show is enough to pick three buttons.
const MAX_ENTRIES: u32 = 2000;

/// Directories that never hold the user's own documents, and that are
/// expensive to walk. `Library` and `AppData` matter because someone may
/// authorize their home directory as a root.
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "venv",
    "__pycache__",
    "Library",
    "AppData",
    "$RECYCLE.BIN",
    "System Volume Information",
];

/// The categories a file extension can land in. Anything else is ignored;
/// no rule is built on "some file exists".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Spreadsheets,
    Documents,
    Pdfs,
    Images,
    Slides,
    Notes,
}

/// Classify by extension, case-insensitively. Extension only — reading
/// magic bytes would mean opening every file in the folder on every render.
fn category_of(path: &Path) -> Option<Category> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "xlsx" | "xls" | "csv" => Category::Spreadsheets,
        "docx" | "doc" | "rtf" | "odt" => Category::Documents,
        "pdf" => Category::Pdfs,
        "png" | "jpg" | "jpeg" | "heic" | "tif" | "tiff" => Category::Images,
        "pptx" | "ppt" => Category::Slides,
        "md" | "txt" => Category::Notes,
        _ => return None,
    })
}

/// Walk the roots and count what is there, within the standard budget.
pub fn survey_roots(roots: &[PathBuf]) -> FolderSurvey {
    survey_roots_within(roots, MAX_ENTRIES)
}

/// The walk, with the entry budget spelled out so tests can exhaust it
/// without creating two thousand files.
fn survey_roots_within(roots: &[PathBuf], budget: u32) -> FolderSurvey {
    let mut survey = FolderSurvey::default();
    let mut remaining = budget;
    for root in roots {
        if remaining == 0 {
            break;
        }
        // Depth 1 is "directly inside a root" — those files are the loose
        // ones. A root that does not exist reads as an empty directory.
        survey_dir(root, 1, &mut remaining, &mut survey);
    }
    survey
}

/// Count one directory's entries, recursing while depth and budget allow.
///
/// Recursion is bounded by `MAX_DEPTH`, so the stack cost is fixed.
fn survey_dir(dir: &Path, depth: u32, remaining: &mut u32, survey: &mut FolderSurvey) {
    // Missing, busy, or permission-denied: skipped silently. A folder the
    // user cannot read is not an error the empty state should report.
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        if *remaining == 0 {
            return;
        }
        *remaining -= 1;

        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Dotfiles and dot-directories are machinery, not documents.
        if name.starts_with('.') {
            continue;
        }

        let path = entry.path();
        // `symlink_metadata` describes the link itself. Following one could
        // lead out of the project entirely — a single symlink to `/` would
        // otherwise turn this into an unbounded walk of the whole disk — so
        // links are counted as neither files nor directories.
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_dir() {
            // Case-insensitive because macOS and Windows filesystems are:
            // `Node_Modules` is the same directory.
            if SKIPPED_DIRS.iter().any(|s| name.eq_ignore_ascii_case(s)) {
                continue;
            }
            if depth < MAX_DEPTH {
                survey_dir(&path, depth + 1, remaining, survey);
            }
            continue;
        }
        if !file_type.is_file() {
            continue; // sockets, fifos, devices
        }

        if depth == 1 {
            survey.loose_files = survey.loose_files.saturating_add(1);
        }
        match category_of(&path) {
            Some(Category::Spreadsheets) => {
                survey.spreadsheets = survey.spreadsheets.saturating_add(1);
            }
            Some(Category::Documents) => survey.documents = survey.documents.saturating_add(1),
            Some(Category::Pdfs) => survey.pdfs = survey.pdfs.saturating_add(1),
            Some(Category::Images) => survey.images = survey.images.saturating_add(1),
            Some(Category::Slides) => survey.slides = survey.slides.saturating_add(1),
            Some(Category::Notes) => survey.notes = survey.notes.saturating_add(1),
            None => {}
        }
    }
}

/* ------------------------------------------------------------- the command */

/// Suggest two or three concrete tasks for this project's folders, or none.
///
/// Never fails because of the filesystem: an unreadable folder, a vanished
/// root, or a walk that could not be scheduled all produce an empty list,
/// which the empty state renders as nothing at all.
#[tauri::command]
pub async fn suggest_tasks(
    state: State<'_, AppState>,
    workspace_id: String,
) -> Result<Vec<TaskSuggestion>> {
    let roots = state
        .storage()
        .workspace_roots(&WorkspaceId(workspace_id))?;
    if roots.is_empty() {
        return Ok(Vec::new());
    }

    // Filesystem I/O inside an async command: off the runtime's worker
    // threads, so a slow network drive cannot stall unrelated tasks.
    let survey = match tauri::async_runtime::spawn_blocking(move || survey_roots(&roots)).await {
        Ok(survey) => survey,
        Err(error) => {
            tracing::warn!(%error, "folder survey did not finish; offering no suggestions");
            FolderSurvey::default()
        }
    };

    let suggestions = suggestions_from(&survey);
    tracing::debug!(
        files = survey.recognized_files(),
        loose = survey.loose_files,
        offered = suggestions.len(),
        "surveyed project folders for suggestions"
    );
    Ok(suggestions)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn ids(suggestions: &[TaskSuggestion]) -> Vec<&str> {
        suggestions.iter().map(|s| s.id.as_str()).collect()
    }

    #[test]
    fn nothing_recognizable_suggests_nothing() {
        assert!(suggestions_from(&FolderSurvey::default()).is_empty());
    }

    #[test]
    fn everything_just_below_its_threshold_suggests_nothing() {
        // The honest empty case: a folder with a little of everything and
        // enough of nothing still gets no guesses.
        let survey = FolderSurvey {
            spreadsheets: 1,
            documents: 2,
            pdfs: 2,
            images: 7,
            slides: 1,
            notes: 99,
            loose_files: 24,
        };
        assert!(suggestions_from(&survey).is_empty());
    }

    #[test]
    fn each_rule_fires_at_its_threshold_and_not_below() {
        // (below, at, above, expected id) for every rule, so a changed
        // threshold has to be changed here too.
        struct Case {
            id: &'static str,
            below: FolderSurvey,
            at: FolderSurvey,
            above: FolderSurvey,
        }
        let cases = [
            Case {
                id: "pdf-summaries",
                below: FolderSurvey {
                    pdfs: 2,
                    ..Default::default()
                },
                at: FolderSurvey {
                    pdfs: 3,
                    ..Default::default()
                },
                above: FolderSurvey {
                    pdfs: 4,
                    ..Default::default()
                },
            },
            Case {
                id: "spreadsheet-merge",
                below: FolderSurvey {
                    spreadsheets: 1,
                    ..Default::default()
                },
                at: FolderSurvey {
                    spreadsheets: 2,
                    ..Default::default()
                },
                above: FolderSurvey {
                    spreadsheets: 3,
                    ..Default::default()
                },
            },
            Case {
                id: "document-compare",
                below: FolderSurvey {
                    documents: 2,
                    ..Default::default()
                },
                at: FolderSurvey {
                    documents: 3,
                    ..Default::default()
                },
                above: FolderSurvey {
                    documents: 4,
                    ..Default::default()
                },
            },
            Case {
                id: "slide-outline",
                below: FolderSurvey {
                    slides: 1,
                    ..Default::default()
                },
                at: FolderSurvey {
                    slides: 2,
                    ..Default::default()
                },
                above: FolderSurvey {
                    slides: 3,
                    ..Default::default()
                },
            },
            Case {
                id: "scan-rename",
                below: FolderSurvey {
                    images: 7,
                    ..Default::default()
                },
                at: FolderSurvey {
                    images: 8,
                    ..Default::default()
                },
                above: FolderSurvey {
                    images: 9,
                    ..Default::default()
                },
            },
            Case {
                id: "tidy-folder",
                below: FolderSurvey {
                    loose_files: 24,
                    ..Default::default()
                },
                at: FolderSurvey {
                    loose_files: 25,
                    ..Default::default()
                },
                above: FolderSurvey {
                    loose_files: 26,
                    ..Default::default()
                },
            },
        ];

        for case in cases {
            assert!(
                suggestions_from(&case.below).is_empty(),
                "{} fired below its threshold",
                case.id
            );
            assert_eq!(ids(&suggestions_from(&case.at)), vec![case.id]);
            assert_eq!(ids(&suggestions_from(&case.above)), vec![case.id]);
        }
    }

    #[test]
    fn suggestions_carry_a_label_and_a_prompt() {
        let survey = FolderSurvey {
            pdfs: 5,
            ..Default::default()
        };
        let suggestions = suggestions_from(&survey);
        let first = suggestions.first().unwrap();
        assert_eq!(first.id, "pdf-summaries");
        assert!(!first.label.is_empty());
        assert!(first.prompt.contains("PDF"));
    }

    #[test]
    fn the_best_supported_suggestions_come_first() {
        let survey = FolderSurvey {
            pdfs: 4,
            spreadsheets: 9,
            documents: 6,
            ..Default::default()
        };
        assert_eq!(
            ids(&suggestions_from(&survey)),
            vec!["spreadsheet-merge", "document-compare", "pdf-summaries"]
        );
    }

    #[test]
    fn ties_fall_back_to_rule_order() {
        // Equal evidence for three rules: the declaration order in RULES
        // decides, so the same folder never reshuffles its buttons.
        let survey = FolderSurvey {
            pdfs: 4,
            spreadsheets: 4,
            documents: 4,
            ..Default::default()
        };
        assert_eq!(
            ids(&suggestions_from(&survey)),
            vec!["pdf-summaries", "spreadsheet-merge", "document-compare"]
        );
    }

    #[test]
    fn at_most_three_are_offered() {
        // Every rule over its threshold; only the three best-supported show.
        let survey = FolderSurvey {
            spreadsheets: 10,
            documents: 4,
            pdfs: 3,
            images: 40,
            slides: 20,
            notes: 100,
            loose_files: 30,
        };
        let suggestions = suggestions_from(&survey);
        assert_eq!(suggestions.len(), MAX_SUGGESTIONS);
        assert_eq!(
            ids(&suggestions),
            vec!["scan-rename", "tidy-folder", "slide-outline"]
        );
    }

    /* ------------------------------------------------------- the walker */

    use std::fs;
    use tempfile::TempDir;

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"x").unwrap();
    }

    #[test]
    fn counts_by_extension_case_insensitively() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("a.PDF"));
        touch(&dir.path().join("b.pdf"));
        touch(&dir.path().join("sheet.XLSX"));
        touch(&dir.path().join("notes.md"));
        touch(&dir.path().join("unknown.zzz"));

        let survey = survey_roots(&[dir.path().to_path_buf()]);
        assert_eq!(survey.pdfs, 2);
        assert_eq!(survey.spreadsheets, 1);
        assert_eq!(survey.notes, 1);
        // Loose files count everything directly in the root, recognized or
        // not — tidiness is about the pile, not the file types.
        assert_eq!(survey.loose_files, 5);
        assert_eq!(survey.recognized_files(), 4);
    }

    #[test]
    fn looks_two_levels_down_and_no_further() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("root.pdf")); // depth 1
        touch(&dir.path().join("one/nested.pdf")); // depth 2
        touch(&dir.path().join("one/two/deep.pdf")); // depth 3 — not counted

        let survey = survey_roots(&[dir.path().to_path_buf()]);
        assert_eq!(survey.pdfs, 2);
        // Only the file at the top of the root is "loose".
        assert_eq!(survey.loose_files, 1);
    }

    #[test]
    fn skips_dotfiles_and_machinery_directories() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join(".hidden.pdf"));
        touch(&dir.path().join(".git/config.pdf"));
        touch(&dir.path().join("node_modules/dep.pdf"));
        touch(&dir.path().join("Node_Modules/other.pdf"));
        touch(&dir.path().join("target/build.pdf"));
        touch(&dir.path().join("__pycache__/cached.pdf"));
        touch(&dir.path().join("real.pdf"));

        let survey = survey_roots(&[dir.path().to_path_buf()]);
        assert_eq!(survey.pdfs, 1);
        assert_eq!(survey.loose_files, 1);
    }

    #[test]
    fn a_missing_root_is_skipped_not_an_error() {
        let dir = TempDir::new().unwrap();
        touch(&dir.path().join("real.pdf"));
        let roots = vec![
            dir.path().join("was-moved-or-unplugged"),
            dir.path().to_path_buf(),
        ];
        assert_eq!(survey_roots(&roots).pdfs, 1);
    }

    #[test]
    fn several_roots_are_counted_together() {
        let first = TempDir::new().unwrap();
        let second = TempDir::new().unwrap();
        touch(&first.path().join("a.pdf"));
        touch(&second.path().join("b.pdf"));
        touch(&second.path().join("c.pdf"));

        let survey = survey_roots(&[first.path().to_path_buf(), second.path().to_path_buf()]);
        assert_eq!(survey.pdfs, 3);
        assert_eq!(survey.loose_files, 3);
    }

    #[test]
    fn the_entry_budget_stops_the_walk() {
        let dir = TempDir::new().unwrap();
        for i in 0..10 {
            touch(&dir.path().join(format!("file{i}.pdf")));
        }
        // Four entries examined, so at most four files counted — the point
        // is that the walk stops, not which four the filesystem listed.
        let survey = survey_roots_within(&[dir.path().to_path_buf()], 4);
        assert_eq!(survey.pdfs, 4);
        assert_eq!(survey_roots_within(&[dir.path().to_path_buf()], 0).pdfs, 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_directory_is_not_followed() {
        // The failure this prevents: a link to a home directory (or `/`)
        // inside the project turning a bounded survey into a disk scan.
        let dir = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        touch(&outside.path().join("elsewhere.pdf"));
        touch(&outside.path().join("also.pdf"));
        touch(&dir.path().join("inside.pdf"));
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        let survey = survey_roots(&[dir.path().to_path_buf()]);
        assert_eq!(survey.pdfs, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_file_is_not_counted_twice() {
        let dir = TempDir::new().unwrap();
        let real = dir.path().join("real.pdf");
        touch(&real);
        std::os::unix::fs::symlink(&real, dir.path().join("alias.pdf")).unwrap();

        let survey = survey_roots(&[dir.path().to_path_buf()]);
        assert_eq!(survey.pdfs, 1);
        assert_eq!(survey.loose_files, 1);
    }

    #[test]
    fn a_real_looking_folder_produces_real_suggestions() {
        // End to end: what a scanned-paperwork folder actually yields.
        let dir = TempDir::new().unwrap();
        for i in 0..4 {
            touch(&dir.path().join(format!("statements/statement{i}.pdf")));
        }
        for i in 0..2 {
            touch(&dir.path().join(format!("budget{i}.xlsx")));
        }
        let survey = survey_roots(&[dir.path().to_path_buf()]);
        assert_eq!(
            ids(&suggestions_from(&survey)),
            vec!["pdf-summaries", "spreadsheet-merge"]
        );
    }
}
