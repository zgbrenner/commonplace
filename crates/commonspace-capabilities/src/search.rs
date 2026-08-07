//! Finding the right capability, and being able to say why.
//!
//! The contract this module owes the rest of the app is not "good ranking".
//! It is **ranking that can be explained to the person whose files are about
//! to be touched**. Every result carries the [`Reason`]s it scored on, and
//! the score is nothing but the sum of those reasons — there is no hidden
//! term. That is a deliberate constraint, and it is why this is lexical
//! scoring rather than embeddings: a vector search would rank better and
//! would be unable to answer "why did it pick that one?" with anything more
//! honest than a cosine distance.
//!
//! Nothing here stops semantic ranking being added later. It has to arrive
//! *as another [`Reason`] variant* — a term that shows up in the explanation
//! alongside the lexical ones — not as a replacement that swallows them.

use crate::Capability;

/// One capability that matched a query, and why.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Match {
    /// The capability itself, cloned so a result can outlive a borrow of the
    /// registry — these cross the MCP boundary as JSON anyway.
    pub capability: Capability,
    /// The sum of `reasons`' weights. Comparable only within one search.
    pub score: f32,
    /// Every reason this scored, strongest first. Never empty: a capability
    /// with no reasons is not a match and must not be returned.
    pub reasons: Vec<Reason>,
}

/// Why a capability matched. Written to be readable in a UI as-is.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    /// A query term appeared in the capability's name.
    NameMatch { term: String, weight: f32 },
    /// A query term appeared in an explicit keyword.
    KeywordMatch { term: String, weight: f32 },
    /// A query term appeared in the summary.
    SummaryMatch { term: String, weight: f32 },
    /// A query term matched a keyword or name after stemming or a known
    /// synonym — "spreadsheets" finding `read_spreadsheet`, "powerpoint"
    /// finding `pptx`. Separate from an exact match so the explanation can
    /// say the softer thing it actually did.
    RelatedMatch {
        term: String,
        matched: String,
        weight: f32,
    },
}

impl Reason {
    /// The weight this reason contributes to the score.
    pub fn weight(&self) -> f32 {
        match self {
            Reason::NameMatch { weight, .. }
            | Reason::KeywordMatch { weight, .. }
            | Reason::SummaryMatch { weight, .. }
            | Reason::RelatedMatch { weight, .. } => *weight,
        }
    }

    /// One clause, for a UI that wants to print the explanation.
    pub fn describe(&self) -> String {
        match self {
            Reason::NameMatch { term, .. } => format!("“{term}” is in its name"),
            Reason::KeywordMatch { term, .. } => format!("“{term}” is one of its keywords"),
            Reason::SummaryMatch { term, .. } => format!("“{term}” appears in what it does"),
            Reason::RelatedMatch { term, matched, .. } => {
                format!("“{term}” is related to “{matched}”")
            }
        }
    }
}

/// Ranks `capabilities` against `query`, best first, at most `limit` results.
///
/// Implemented in the commit that follows this contract.
pub fn search<'a>(
    capabilities: impl Iterator<Item = &'a Capability>,
    query: &str,
    limit: usize,
) -> Vec<Match> {
    let _ = (capabilities, query, limit);
    Vec::new()
}
