//! Loading Agent Skills from disk.
//!
//! A skill is a directory containing `SKILL.md`: YAML frontmatter naming and
//! describing the skill, then a Markdown body of instructions, then whatever
//! scripts, references and assets the author shipped alongside. Commonspace
//! reads the portable format published at <https://agentskills.io> rather
//! than inventing one, because a format people already write skills in is
//! worth more than a better format nobody has written anything in.
//!
//! ## What Commonspace does and does not honour
//!
//! Honoured: the directory-per-skill layout, the required `name` and
//! `description`, the optional `license`, `compatibility`, `metadata` and
//! `allowed-tools`, and the three-level disclosure discipline — only name and
//! description are indexed, the body arrives when something matches, and
//! bundled files are named but never read until asked for.
//!
//! Not honoured, deliberately: the Claude-Code-specific frontmatter
//! extensions (`when_to_use`, `hooks`, `context: fork`, `paths`, and the
//! rest). They are real and documented, but they are one implementation's
//! extensions rather than the portable spec, and several of them —
//! `hooks` above all — are exactly the injection surface Commonspace closed
//! deliberately when it started passing `--setting-sources user` and
//! `disableAllHooks` to the provider CLI. Reading them back in from a file
//! in the user's project would reopen it. Unknown keys are preserved in the
//! parse but ignored, never fatal.
//!
//! `allowed-tools` is **advisory here, and this is a stronger statement than
//! it is elsewhere**. In Claude Code it is a grant: it pre-approves tools for
//! the invoking turn. In Commonspace it grants nothing at all. It is recorded
//! so the model knows what the author expected and so the user can see what a
//! skill expects to use before trusting it, but what may actually run is
//! decided by the policy engine and the permission broker, exactly as it is
//! for an instruction the user typed by hand. A skill is content, not
//! authority.
//!
//! ## Trust
//!
//! Skills are executable content from wherever the user got them, running
//! with the user's own privileges. Anthropic's own guidance is to treat them
//! like installing software. Commonspace's answer is not to trust them less
//! in the abstract but to make them harmless in the specific: a skill can say
//! anything it likes to the model, and every consequence still has to survive
//! the policy engine, the staging filesystem, and — where the platform allows
//! it — the OS sandbox.

use crate::{Capability, Registry};
use std::path::{Path, PathBuf};

/// Why one skill directory did not load.
///
/// Per-skill rather than per-load: one malformed skill must never stop the
/// others, and the user needs to know which file to go fix.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SkillError {
    #[error("{path}: SKILL.md has no YAML frontmatter (it must start with a line of ---)")]
    MissingFrontmatter { path: PathBuf },
    #[error("{path}: SKILL.md's frontmatter is not valid ({detail})")]
    MalformedFrontmatter { path: PathBuf, detail: String },
    #[error("{path}: SKILL.md is missing the required '{field}' field")]
    MissingField { path: PathBuf, field: String },
    #[error("{path}: '{value}' is not a valid skill name ({detail})")]
    InvalidName {
        path: PathBuf,
        value: String,
        detail: String,
    },
    #[error("{path}: {field} is longer than the {limit} characters the format allows")]
    TooLong {
        path: PathBuf,
        field: String,
        limit: usize,
    },
    #[error("{path}: could not be read ({detail})")]
    Unreadable { path: PathBuf, detail: String },
}

impl SkillError {
    /// The `SKILL.md` this is about, so the UI can offer to open it.
    pub fn path(&self) -> &Path {
        match self {
            SkillError::MissingFrontmatter { path }
            | SkillError::MalformedFrontmatter { path, .. }
            | SkillError::MissingField { path, .. }
            | SkillError::InvalidName { path, .. }
            | SkillError::TooLong { path, .. }
            | SkillError::Unreadable { path, .. } => path,
        }
    }
}

/// What a load run found, including what it could not use.
///
/// Skipped skills are returned rather than logged and forgotten: a skill that
/// silently does not exist is the single most confusing failure this feature
/// can have, and the user is the only one who can fix the file.
#[derive(Debug, Default)]
pub struct LoadReport {
    /// How many skills loaded successfully.
    pub loaded: usize,
    /// Every skill directory that was found but could not be used.
    pub skipped: Vec<SkillError>,
}

/// Loads every skill under `roots` into `registry`.
///
/// Roots are searched in order and later roots win on a name collision, so a
/// caller passes the personal skills directory before the project one and a
/// project can deliberately override a personal skill of the same name.
///
/// Implemented in the commit that follows this contract.
pub fn load_into(registry: &mut Registry, roots: &[PathBuf]) -> LoadReport {
    let _ = (registry, roots);
    LoadReport::default()
}

/// Parses one `SKILL.md`, without touching the filesystem beyond it.
///
/// Separated from [`load_into`] so the whole format can be tested against
/// strings rather than directory trees.
///
/// Implemented in the commit that follows this contract.
pub fn parse_skill(path: &Path, text: &str) -> Result<(Capability, ParsedSkill), SkillError> {
    let _ = (path, text);
    Err(SkillError::MissingFrontmatter {
        path: path.to_path_buf(),
    })
}

/// Everything below level one: the parts of a skill that only reach the model
/// once it has decided the skill is relevant.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParsedSkill {
    /// The Markdown body below the frontmatter, verbatim.
    pub body: String,
    /// Tool names from `allowed-tools`. Advisory — see the module docs.
    pub requires: Vec<String>,
    /// `license`, if declared.
    pub license: Option<String>,
    /// `compatibility`, if declared: free text about what the skill needs.
    pub compatibility: Option<String>,
    /// `metadata`, if declared: author, version, whatever the author chose.
    pub metadata: Vec<(String, String)>,
}
