//! One neutral registry over everything an agent can reach for.
//!
//! Commonspace gives a provider CLI a small set of typed tools, and it is
//! about to give it more: Agent Skills the user drops into a project, tools
//! from MCP servers they connect, and a browser lane. The obvious way to
//! expose all of that — put every definition in the prompt — stops working
//! well before it stops working at all. A few dozen tool schemas is thousands
//! of tokens spent on every turn, most of them irrelevant to the task, and
//! the model's accuracy at picking the right one falls as the list grows.
//!
//! So capabilities are *retrieved*, not enumerated. The model sees two tools:
//! one to search this registry, one to load a match in full. Everything
//! else — a skill's instructions, an MCP tool's schema — arrives only once
//! something in the conversation actually asked for it.
//!
//! Three rules shape everything here.
//!
//! **One shape for every source.** A built-in Rust tool, a Markdown skill,
//! and a tool on someone's MCP server are different things with different
//! lifetimes, but the model should not have to care. They all become a
//! [`Capability`]; only [`CapabilityKind`] and [`CapabilitySource`] remember
//! where each came from, and those exist for the *user's* benefit — so the
//! app can say "this came from a file in your project" — not the model's.
//!
//! **Activation is explainable.** Every [`Match`] carries the [`Reason`]s it
//! scored on. A person asking "why did it use that skill?" gets an answer
//! from data, not a guess, and a skill that never activates can be debugged
//! by looking at what it did and did not match. Ranking is deterministic
//! lexical scoring for exactly this reason: an embedding model would rank
//! better and explain worse, and the second property is the one that makes
//! this trustworthy. Semantic ranking can be layered on later *behind* the
//! same [`Reason`] contract, never instead of it.
//!
//! **A malformed capability is skipped, never fatal.** Skills are files a
//! user or a third party wrote. One bad frontmatter block must not stop the
//! other skills loading or fail the task — it is reported through
//! [`LoadReport`] and left out of the registry. Same rule as the sandbox
//! module: degrade, but never silently.

#![forbid(unsafe_code)]

use std::path::PathBuf;

pub mod builtin;
pub mod search;
pub mod skills;

pub use search::{Match, Reason};
pub use skills::{LoadReport, SkillError};

/// A capability's stable identifier, namespaced by source so two sources can
/// never collide: `builtin:create_document`, `skill:quarterly-deck`,
/// `mcp:linear/create_issue`.
///
/// Stable across restarts because it is derived from the source, not
/// generated: the model may cite an id in one turn and load it in the next,
/// and a conversation replayed from the timeline has to resolve the same ids
/// it resolved the first time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct CapabilityId(pub String);

impl CapabilityId {
    /// Builds an id from a namespace and a source-local name.
    pub fn new(namespace: &str, name: &str) -> Self {
        Self(format!("{namespace}:{name}"))
    }

    /// The namespace before the first colon, or the whole id if there is none.
    pub fn namespace(&self) -> &str {
        self.0.split_once(':').map_or(&self.0, |(ns, _)| ns)
    }
}

impl std::fmt::Display for CapabilityId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a capability came from. Affects how it is loaded and how much it is
/// trusted — not how it is searched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityKind {
    /// A typed tool implemented in Rust and served by Commonspace's own MCP
    /// server. Always available, always policed by the policy engine.
    BuiltinTool,
    /// An Agent Skill: a `SKILL.md` file whose body is instructions for the
    /// model. Content the user or a third party wrote; carries no privileges
    /// of its own — anything it asks the model to *do* still goes through the
    /// same tools and the same policy engine as an instruction typed by hand.
    Skill,
    /// A tool exposed by an MCP server the user connected.
    McpTool,
    /// A browser action. Reserved: the browser lane lands in a later slice,
    /// and having the variant here keeps the registry from needing a breaking
    /// change when it does.
    Browser,
}

impl CapabilityKind {
    /// The id namespace for this kind.
    pub fn namespace(self) -> &'static str {
        match self {
            CapabilityKind::BuiltinTool => "builtin",
            CapabilityKind::Skill => "skill",
            CapabilityKind::McpTool => "mcp",
            CapabilityKind::Browser => "browser",
        }
    }
}

/// Provenance, for the user-facing side: which file on disk, or which server.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CapabilitySource {
    /// Compiled into Commonspace.
    Builtin,
    /// A file the user can open, edit, and delete.
    File { path: PathBuf },
    /// A connected MCP server, named as the user named it.
    Server { name: String },
}

/// One thing the agent can do, in the single shape the registry speaks.
///
/// The split between [`Self::summary`] and the body behind
/// [`Registry::load`] is the whole point: `summary` is what every search
/// result costs, and it is the only thing the model sees until it decides
/// something is relevant.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Capability {
    pub id: CapabilityId,
    pub kind: CapabilityKind,
    /// Human name, shown in the UI. Not necessarily unique.
    pub name: String,
    /// One or two sentences: what this does and when to reach for it. This is
    /// the text search matches against and the text the model reads in a
    /// result list, so it is written for a reader deciding "is this the one?",
    /// not for someone already using it.
    pub summary: String,
    /// Extra words worth matching that the summary would read badly for —
    /// file extensions, product names, synonyms a user might type.
    pub keywords: Vec<String>,
    pub source: CapabilitySource,
}

/// A capability loaded in full: everything the model needs to actually use
/// it. What "in full" means differs by kind, which is why this is an enum
/// rather than a `String` — a skill's Markdown body and a tool's JSON Schema
/// are not interchangeable and pretending otherwise would push the
/// distinction into the model's prompt.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoadedCapability {
    /// A skill's instructions: the Markdown body below the frontmatter,
    /// verbatim.
    Instructions {
        body: String,
        /// Files the skill ships alongside `SKILL.md`, relative to its
        /// directory. Named but not read: the model asks for one by path if
        /// it needs it, which is the third level of progressive disclosure.
        bundled: Vec<PathBuf>,
        /// Tool names the skill declared it needs. Advisory to the model and
        /// informative to the user — never a grant. Commonspace's policy
        /// engine decides what may actually run, and a skill naming a tool
        /// does not give it access to one.
        requires: Vec<String>,
    },
    /// A callable tool: the name to call and its JSON Schema.
    Tool {
        call_name: String,
        input_schema: serde_json::Value,
    },
}

/// Everything the agent can reach in one session.
#[derive(Debug, Clone, Default)]
pub struct Registry {
    entries: Vec<Entry>,
}

/// A registry row: the searchable descriptor plus how to load it in full.
#[derive(Debug, Clone)]
struct Entry {
    capability: Capability,
    loaded: LoadedCapability,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a capability, replacing any existing entry with the same id.
    ///
    /// Last write wins so a project-level skill can deliberately shadow a
    /// personal one of the same name, which is how every other tool in this
    /// space resolves that collision.
    pub fn insert(&mut self, capability: Capability, loaded: LoadedCapability) {
        self.entries.retain(|e| e.capability.id != capability.id);
        self.entries.push(Entry { capability, loaded });
    }

    /// How many capabilities are registered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the registry holds nothing at all.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every capability, in insertion order. For the UI's "what can this do?"
    /// screen — the model uses [`Self::search`].
    pub fn capabilities(&self) -> impl Iterator<Item = &Capability> {
        self.entries.iter().map(|e| &e.capability)
    }

    /// The descriptor for one id.
    pub fn get(&self, id: &CapabilityId) -> Option<&Capability> {
        self.entries
            .iter()
            .find(|e| &e.capability.id == id)
            .map(|e| &e.capability)
    }

    /// The full contents of one capability. Level two of progressive
    /// disclosure: nothing here reaches the model until it asks by id.
    pub fn load(&self, id: &CapabilityId) -> Option<&LoadedCapability> {
        self.entries
            .iter()
            .find(|e| &e.capability.id == id)
            .map(|e| &e.loaded)
    }

    /// The best `limit` matches for a natural-language query, each carrying
    /// the reasons it scored. See [`search`] for the ranking itself.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Match> {
        search::search(self.entries.iter().map(|e| &e.capability), query, limit)
    }
}
