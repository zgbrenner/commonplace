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
use std::collections::{HashMap, HashSet};

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
    /// A query term matched the name, a keyword, or the summary after
    /// stemming or a known synonym — "spreadsheets" finding
    /// `read_spreadsheet`, "powerpoint" finding `pptx`. Separate from an
    /// exact match so the explanation can say the softer thing it actually
    /// did.
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

/// What a hit in each field is worth.
///
/// A capability's name and its keywords are chosen by whoever wrote it with
/// being found in mind — `keywords` exists for precisely the words the
/// summary would read badly for. The summary is prose aimed at a person
/// deciding "is this the one?", so a word landing there is real evidence but
/// weaker: prose is long, and most of the words in it are incidental.
const NAME_WEIGHT: f32 = 2.0;
const KEYWORD_WEIGHT: f32 = 1.5;
const SUMMARY_WEIGHT: f32 = 1.0;

/// What a softer kind of hit keeps of its field's weight.
///
/// A stem hit is the same word in a different shape, so it keeps most of it.
/// A synonym hit is a *different* word that [`SYNONYMS`] guessed on the
/// user's behalf, so it keeps less. Both are discounted far enough that an
/// exact hit in a field always beats a guess in that same field.
const STEM: f32 = 0.6;
const SYNONYM: f32 = 0.4;

/// Words that say nothing about which capability someone wants.
///
/// Deliberately short. A query is three or four words, and dropping the wrong
/// one leaves nothing to rank on: "how do I make a deck" has to come out of
/// here as "make deck", not as nothing. Anything that could plausibly be part
/// of a capability's own vocabulary — "file", "new", "open", "word" — stays
/// out of this list even where it reads like filler.
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "any", "are", "as", "at", "be", "but", "by", "can", "could", "do", "does",
    "for", "from", "how", "i", "if", "in", "into", "is", "it", "its", "me", "my", "of", "on", "or",
    "please", "so", "some", "that", "the", "their", "them", "then", "there", "these", "they",
    "this", "to", "up", "want", "was", "we", "what", "when", "where", "which", "will", "with",
    "would", "you", "your",
];

/// The gap between what a person says and what a tool is called.
///
/// This is the one place the ranking is allowed to guess, so it is a table
/// rather than logic scattered through the scorer: a reader can audit every
/// guess it makes in one screen. Rows are one hop and one direction —
/// "powerpoint" reaches "pptx", and the reverse is its own row if it is
/// wanted at all. Nothing chains, so the table means exactly what it says.
///
/// The rows earn their place by closing the gap that actually costs a
/// non-technical user a result: the product name they type versus the file
/// extension or the verb the tool is named for. Anything subtler than that
/// belongs in a capability's own `keywords`, where the person who wrote the
/// capability can see it and change it.
const SYNONYMS: &[(&str, &[&str])] = &[
    ("powerpoint", &["pptx", "presentation", "slides", "deck"]),
    ("keynote", &["pptx", "presentation", "slides", "deck"]),
    ("slideshow", &["pptx", "presentation", "slides", "deck"]),
    ("deck", &["pptx", "presentation", "slides"]),
    ("excel", &["xlsx", "spreadsheet", "csv"]),
    ("spreadsheet", &["xlsx", "csv"]),
    ("word", &["docx", "document"]),
    ("doc", &["docx", "document"]),
    ("pdf", &["document"]),
    ("photo", &["image", "png", "jpg"]),
    ("picture", &["image", "png", "jpg"]),
    ("folder", &["directory"]),
    ("directory", &["folder"]),
    ("trash", &["delete", "remove"]),
    ("bin", &["delete", "remove"]),
    ("delete", &["remove", "trash"]),
    ("email", &["mail", "message"]),
    ("make", &["create", "new", "generate"]),
    ("build", &["create", "generate"]),
    // "write it up as a document" is how a person asks for one to be made.
    // Without this row, "write" reaches only the capabilities whose prose
    // happens to contain the word — which includes the *reading* tools,
    // because their descriptions say what they do not write.
    ("write", &["create", "save"]),
    ("open", &["read", "load"]),
    ("edit", &["write", "update"]),
    ("find", &["search", "list"]),
];

/// Ranks `capabilities` against `query`, best first, at most `limit` results.
///
/// Each distinct query term contributes **at most one** [`Reason`]: the
/// strongest honest thing that can be said about it, picked from an exact hit
/// in the name, a keyword or the summary, a stemmed hit, or a [`SYNONYMS`]
/// hit. A capability that scored no reason at all is not a match and is left
/// out. The score is the sum of the reasons it did score, with nothing added
/// on top.
///
/// Ties break on [`crate::CapabilityId`], which is unique and stable across
/// restarts, so two runs over the same registry agree even when the registry
/// was assembled in a different order.
pub fn search<'a>(
    capabilities: impl Iterator<Item = &'a Capability>,
    query: &str,
    limit: usize,
) -> Vec<Match> {
    if limit == 0 {
        return Vec::new();
    }
    let terms = query_terms(query);
    if terms.is_empty() {
        // An empty or all-stopword query has nothing to rank on. Handing
        // back the registry instead would be the token dump this module
        // exists to prevent, delivered at the exact moment the model is
        // least sure what it is looking for.
        return Vec::new();
    }

    // Two passes, so the second one knows how discriminating each word was.
    // The iterator has to be collected for that, which is the price of
    // rarity weighting and is paid once per search over a registry of tens.
    let indexed: Vec<(&Capability, Indexed)> = capabilities
        .map(|capability| (capability, Indexed::of(capability)))
        .collect();
    let rarity = Rarity::over(&terms, &indexed);

    let mut matches: Vec<Match> = indexed
        .iter()
        .filter_map(|(capability, fields)| {
            // One reason per distinct term is how "matched three of your four
            // words" beats "matched one word five times": a repeat buys
            // nothing, because the second occurrence has no term of its own
            // to score against. Breadth is the only thing that accumulates.
            let mut reasons: Vec<Reason> = terms
                .iter()
                .filter_map(|term| evidence(term, fields))
                .map(|reason| rarity.scale(&reason))
                .collect();
            if reasons.is_empty() {
                return None;
            }
            // Strongest first. `sort_by` is stable, so reasons of equal
            // weight keep the order the words were typed in, which is the
            // order a person expects to read them back in.
            reasons.sort_by(|a, b| b.weight().total_cmp(&a.weight()));
            // Summed over `reasons` in the order they are stored, so adding
            // up the printed explanation reproduces the printed score to the
            // bit rather than to some tolerance.
            let score: f32 = reasons.iter().map(Reason::weight).sum();
            Some(Match {
                capability: (*capability).clone(),
                score,
                reasons,
            })
        })
        .collect();

    matches.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.capability.id.cmp(&b.capability.id))
    });
    matches.truncate(limit);
    matches
}

/// A query word, carried with its stem so nothing stems it twice.
struct Term {
    /// As typed, lowercased. This is what the explanation quotes back.
    raw: String,
    stem: String,
}

/// Lowercases, splits on anything that is not a letter or digit, drops
/// stopwords, and keeps one entry per distinct *stem*.
///
/// Deduplicating on the stem rather than the spelling matters: "deck decks"
/// and "create creating" are one idea typed twice, and letting both through
/// would smuggle repetition back in through the side door.
fn query_terms(query: &str) -> Vec<Term> {
    let mut terms: Vec<Term> = Vec::new();
    for token in tokenize(query) {
        if STOPWORDS.contains(&token.as_str()) {
            continue;
        }
        let stem = stem(&token);
        if terms.iter().any(|existing| existing.stem == stem) {
            continue;
        }
        terms.push(Term { raw: token, stem });
    }
    terms
}

fn tokenize(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
}

/// One searchable field of a capability, tokenized.
#[derive(Default)]
struct Field {
    exact: HashSet<String>,
    /// Stem to the first word in this field that produced it. The word is
    /// kept so a [`Reason::RelatedMatch`] can name what the query actually
    /// landed on — "spreadsheets", not the stem it shares.
    stems: HashMap<String, String>,
}

impl Field {
    fn add(&mut self, text: &str) {
        for token in tokenize(text) {
            self.stems
                .entry(stem(&token))
                .or_insert_with(|| token.clone());
            self.exact.insert(token);
        }
    }

    /// The word in this field that `stem` reaches, if any.
    fn by_stem(&self, stem: &str) -> Option<&String> {
        self.stems.get(stem)
    }

    /// The word in this field that one of `term`'s synonyms reaches, if any.
    ///
    /// Synonyms are compared through the stemmer too, so "folder" reaches a
    /// capability whose keywords say "directories".
    fn by_synonym(&self, term: &Term) -> Option<&String> {
        for (spoken, canonical) in SYNONYMS {
            if *spoken != term.raw && stem(spoken) != term.stem {
                continue;
            }
            for word in *canonical {
                if let Some(found) = self.exact.get(*word) {
                    return Some(found);
                }
                if let Some(found) = self.by_stem(&stem(word)) {
                    return Some(found);
                }
            }
        }
        None
    }
}

/// How much each query term is worth, given how many capabilities it reaches.
///
/// Without this, "read the excel file" ranks `read_file` above
/// `read_spreadsheet`: "file" is in almost every tool's name or description
/// and "excel" is in none of them, yet "file" landing in a name scored full
/// marks while "excel" reaching "spreadsheet" through the synonym table
/// scored a fraction of one. The word carrying all the information lost to
/// the word carrying none.
///
/// So a term's weight is scaled by how *few* capabilities it reaches. This is
/// the standard inverse-document-frequency idea and it is applied the
/// standard way rather than as an invented curve, but with one deliberate
/// difference: document frequency here counts the capabilities a term
/// *matched by any route*, not the ones whose text literally contains it.
/// That is forced rather than chosen — "excel" appears nowhere in
/// Commonspace's tool text at all, so its literal frequency is zero and every
/// idf formula is undefined there. Counting matches makes the quantity mean
/// "how many candidates does this word fail to separate", which is the thing
/// actually worth knowing, and it reuses the matcher instead of inventing a
/// second notion of "contains".
///
/// The score stays exactly the sum of the printed reasons, because scaling
/// happens to each reason's own weight before anything is summed.
struct Rarity {
    /// Multiplier per query term, keyed by stem.
    weights: std::collections::HashMap<String, f32>,
}

/// What rarity may do to a weight.
///
/// A term reaching one capability out of many is the strongest signal a query
/// can carry, and doubling it lets that word overturn a rival that matched a
/// commoner word in a stronger field — which is the whole point — without
/// letting one word decide a four-word query on its own.
///
/// The floor is a quarter rather than zero because a word every capability
/// matches is uninformative, not false: "file" in "read the excel file" is
/// still evidence the person wants a file tool, it just cannot say which one.
/// Zero would also print reasons weighing nothing, which reads as a bug to
/// anyone looking at the explanation.
///
/// Together they bound the damage: the widest inversion this permits is a
/// maximally rare hit in the summary beating a maximally common hit in the
/// name, a factor of four. Anything wider and rarity would be deciding
/// results that the fields should decide.
const RARITY_FLOOR: f32 = 0.25;
const RARITY_CEILING: f32 = 2.0;

impl Rarity {
    fn over(terms: &[Term], indexed: &[(&Capability, Indexed)]) -> Self {
        let total = indexed.len() as f32;
        let mut weights = std::collections::HashMap::new();
        for term in terms {
            let hits = indexed
                .iter()
                .filter(|(_, fields)| evidence(term, fields).is_some())
                .count() as f32;
            weights.insert(term.stem.clone(), Self::multiplier(hits, total));
        }
        Self { weights }
    }

    /// Probabilistic idf, smoothed so it can never go negative, then mapped
    /// onto [`RARITY_FLOOR`]..[`RARITY_CEILING`] by its value at the rarest
    /// possible term. Normalizing against `df = 1` rather than a constant
    /// keeps the band meaning the same thing as the registry grows.
    fn multiplier(hits: f32, total: f32) -> f32 {
        if total <= 1.0 || hits <= 0.0 {
            // Nothing to discriminate between, or a term that matched
            // nothing and will not produce a reason anyway.
            return 1.0;
        }
        let idf = |df: f32| ((total - df + 0.5) / (df + 0.5) + 1.0).ln();
        let rarest = idf(1.0);
        let share = if rarest > 0.0 {
            idf(hits) / rarest
        } else {
            1.0
        };
        RARITY_FLOOR + share.clamp(0.0, 1.0) * (RARITY_CEILING - RARITY_FLOOR)
    }

    /// Rescales one reason's weight in place.
    fn scale(&self, reason: &Reason) -> Reason {
        let factor = |term: &str| {
            self.weights
                .get(&stem(term))
                .copied()
                .or_else(|| self.weights.get(term).copied())
                .unwrap_or(1.0)
        };
        match reason {
            Reason::NameMatch { term, weight } => Reason::NameMatch {
                weight: weight * factor(term),
                term: term.clone(),
            },
            Reason::KeywordMatch { term, weight } => Reason::KeywordMatch {
                weight: weight * factor(term),
                term: term.clone(),
            },
            Reason::SummaryMatch { term, weight } => Reason::SummaryMatch {
                weight: weight * factor(term),
                term: term.clone(),
            },
            Reason::RelatedMatch {
                term,
                matched,
                weight,
            } => Reason::RelatedMatch {
                weight: weight * factor(term),
                term: term.clone(),
                matched: matched.clone(),
            },
        }
    }
}

struct Indexed {
    name: Field,
    keywords: Field,
    summary: Field,
}

impl Indexed {
    fn of(capability: &Capability) -> Self {
        let mut name = Field::default();
        name.add(&capability.name);
        let mut keywords = Field::default();
        for keyword in &capability.keywords {
            keywords.add(keyword);
        }
        let mut summary = Field::default();
        summary.add(&capability.summary);
        Self {
            name,
            keywords,
            summary,
        }
    }
}

/// The strongest reason `term` gives for this capability, if any.
///
/// The ladder is written in descending weight order, so the first rung that
/// fires is both the heaviest and the most specific claim that can honestly
/// be made — and a reader can verify that by reading straight down it. Every
/// same-word rung sits above the synonym rung for the same field: a word the
/// user actually typed is better evidence than one [`SYNONYMS`] chose for
/// them.
fn evidence(term: &Term, indexed: &Indexed) -> Option<Reason> {
    let related = |matched: &String, weight: f32| Reason::RelatedMatch {
        term: term.raw.clone(),
        matched: matched.clone(),
        weight,
    };

    if indexed.name.exact.contains(&term.raw) {
        return Some(Reason::NameMatch {
            term: term.raw.clone(),
            weight: NAME_WEIGHT,
        });
    }
    if indexed.keywords.exact.contains(&term.raw) {
        return Some(Reason::KeywordMatch {
            term: term.raw.clone(),
            weight: KEYWORD_WEIGHT,
        });
    }
    if let Some(matched) = indexed.name.by_stem(&term.stem) {
        return Some(related(matched, NAME_WEIGHT * STEM));
    }
    if indexed.summary.exact.contains(&term.raw) {
        return Some(Reason::SummaryMatch {
            term: term.raw.clone(),
            weight: SUMMARY_WEIGHT,
        });
    }
    if let Some(matched) = indexed.keywords.by_stem(&term.stem) {
        return Some(related(matched, KEYWORD_WEIGHT * STEM));
    }
    if let Some(matched) = indexed.name.by_synonym(term) {
        return Some(related(matched, NAME_WEIGHT * SYNONYM));
    }
    if let Some(matched) = indexed.summary.by_stem(&term.stem) {
        return Some(related(matched, SUMMARY_WEIGHT * STEM));
    }
    if let Some(matched) = indexed.keywords.by_synonym(term) {
        return Some(related(matched, KEYWORD_WEIGHT * SYNONYM));
    }
    if let Some(matched) = indexed.summary.by_synonym(term) {
        return Some(related(matched, SUMMARY_WEIGHT * SYNONYM));
    }
    None
}

/// Suffix stripping, not linguistics.
///
/// It exists so "spreadsheets" reaches `read_spreadsheet` and "creating"
/// reaches "create". It handles regular English plurals, `-ing` and `-ed`,
/// and it drops a trailing "e" last so "create"/"creating"/"created" and
/// "slide"/"slides" each land on one stem.
///
/// It does **not** handle irregulars ("people", "wrote", "children"), it is
/// not a Porter stemmer, and it cheerfully collides unrelated short words —
/// "note" and "not" both stem to "not". That is tolerable for two reasons: a
/// stem hit is only ever a discounted [`Reason::RelatedMatch`], so it can
/// promote a capability but never claim to be an exact hit; and both sides of
/// every comparison go through this same function, so being consistent
/// matters far more here than being right.
fn stem(token: &str) -> String {
    let base = plural_or_tense(token).unwrap_or_else(|| token.to_string());
    match base.strip_suffix('e') {
        Some(shorter) if shorter.len() >= MIN_STEM => shorter.to_string(),
        _ => base,
    }
}

/// A stem shorter than this is not a word any more. Every rule below is
/// guarded by it, and the guard is the only thing keeping "read" from losing
/// its "-ed" and stemming to "r", where it would match nothing and be matched
/// by everything.
const MIN_STEM: usize = 3;

/// At most one plural or tense rule, longest suffix first.
fn plural_or_tense(token: &str) -> Option<String> {
    if let Some(base) = token.strip_suffix("sses") {
        if base.len() >= MIN_STEM {
            return Some(format!("{base}ss"));
        }
    }
    if let Some(base) = token.strip_suffix("ies") {
        if base.len() >= MIN_STEM {
            return Some(format!("{base}y"));
        }
    }
    if let Some(base) = token.strip_suffix("ing") {
        if base.len() >= MIN_STEM {
            return Some(base.to_string());
        }
    }
    if let Some(base) = token.strip_suffix("ed") {
        if base.len() >= MIN_STEM {
            return Some(base.to_string());
        }
    }
    if let Some(base) = token.strip_suffix("es") {
        // Only words that need the extra vowel lose it: "boxes" is "box",
        // while "slides" drops just the "s" and lets the trailing-e rule
        // finish the job, so it lands where "slide" does.
        if base.len() >= MIN_STEM && ends_in_sibilant(base) {
            return Some(base.to_string());
        }
    }
    if let Some(base) = token.strip_suffix('s') {
        // "status", "analysis" and "css" are not plurals of anything.
        if base.len() >= MIN_STEM && !base.ends_with(['s', 'u', 'i']) {
            return Some(base.to_string());
        }
    }
    None
}

fn ends_in_sibilant(base: &str) -> bool {
    base.ends_with(['s', 'x', 'z']) || base.ends_with("ch") || base.ends_with("sh")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::{CapabilityId, CapabilityKind, CapabilitySource};

    fn capability(id: &str, name: &str, summary: &str, keywords: &[&str]) -> Capability {
        Capability {
            id: CapabilityId(id.to_string()),
            kind: CapabilityKind::BuiltinTool,
            name: name.to_string(),
            summary: summary.to_string(),
            keywords: keywords.iter().map(|k| (*k).to_string()).collect(),
            source: CapabilitySource::Builtin,
        }
    }

    /// A registry shaped like the real one: a few built-in tools whose names
    /// and summaries are written the way `tools.rs` writes them, plus a skill
    /// that only prose reaches.
    fn registry() -> Vec<Capability> {
        vec![
            capability(
                "builtin:create_presentation",
                "Create presentation",
                "Build a slide deck from an outline and save it as a presentation file.",
                &["create", "presentation", "pptx", "slides", "deck"],
            ),
            capability(
                "builtin:read_spreadsheet",
                "Read spreadsheet",
                "Read a spreadsheet as structured data, one row at a time.",
                &["read", "spreadsheet", "xlsx", "csv"],
            ),
            capability(
                "builtin:delete_file",
                "Delete file",
                "Propose removing a file. The person using Commonspace reviews the change before \
                 anything leaves the disk.",
                &["delete", "file", "remove"],
            ),
            capability(
                "builtin:create_document",
                "Create document",
                "Write a new text document and save it.",
                &["create", "document", "docx", "write"],
            ),
            capability(
                "builtin:list_directory",
                "List directory",
                "List the entries of a directory.",
                &["list", "directory", "folder"],
            ),
            capability(
                "skill:quarterly-deck",
                "Quarterly deck",
                "Assemble the quarterly business review deck from the finance spreadsheet.",
                &["quarterly", "review"],
            ),
        ]
    }

    #[test]
    fn every_result_scores_exactly_the_sum_of_the_reasons_it_prints() {
        let caps = registry();
        let results = search(
            caps.iter(),
            "create a new presentation from the spreadsheet",
            10,
        );
        assert!(
            results.len() >= 3,
            "want a multi-result search: {results:?}"
        );

        for result in &results {
            assert!(!result.reasons.is_empty(), "{result:?}");
            let summed: f32 = result.reasons.iter().map(Reason::weight).sum();
            // Exact, not epsilon. The score is accumulated over `reasons` in
            // the order they are stored, so a person adding up the printed
            // explanation gets the printed score bit for bit. If this ever
            // has to become approximate, the explanation has stopped being
            // the whole story and something got hidden.
            assert_eq!(result.score, summed, "{result:?}");
        }
    }

    #[test]
    fn reasons_are_ordered_strongest_first() {
        let caps = registry();
        let results = search(
            caps.iter(),
            "create a new presentation from the spreadsheet",
            10,
        );
        for result in &results {
            let weights: Vec<f32> = result.reasons.iter().map(Reason::weight).collect();
            assert!(
                weights.windows(2).all(|pair| pair[0] >= pair[1]),
                "{weights:?} in {result:?}"
            );
        }
    }

    #[test]
    fn results_are_ordered_by_score_descending() {
        let caps = registry();
        let results = search(caps.iter(), "create a presentation deck", 10);
        let scores: Vec<f32> = results.iter().map(|m| m.score).collect();
        assert!(
            scores.windows(2).all(|pair| pair[0] >= pair[1]),
            "{scores:?}"
        );
    }

    #[test]
    fn every_reason_variant_is_reachable_from_a_realistic_query() {
        let caps = registry();
        let mut saw_name = false;
        let mut saw_keyword = false;
        let mut saw_summary = false;
        let mut saw_related = false;

        for query in ["delete a file", "pptx", "outline", "make me a powerpoint"] {
            for result in search(caps.iter(), query, 10) {
                for reason in result.reasons {
                    match reason {
                        Reason::NameMatch { .. } => saw_name = true,
                        Reason::KeywordMatch { .. } => saw_keyword = true,
                        Reason::SummaryMatch { .. } => saw_summary = true,
                        Reason::RelatedMatch { .. } => saw_related = true,
                    }
                }
            }
        }
        assert!(saw_name && saw_keyword && saw_summary && saw_related);
    }

    #[test]
    fn a_term_in_the_name_outranks_the_same_term_in_a_summary() {
        let caps = [
            capability("builtin:compress", "Compress", "Archive old files.", &[]),
            capability("builtin:archive", "Archive", "Put things away.", &[]),
        ];
        let results = search(caps.iter(), "archive", 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].capability.id.0, "builtin:archive");
        assert!(
            matches!(&results[0].reasons[0], Reason::NameMatch { term, .. } if term == "archive")
        );
        assert!(
            matches!(&results[1].reasons[0], Reason::SummaryMatch { term, .. } if term == "archive")
        );
        assert!(results[0].score > results[1].score);
    }

    #[test]
    fn matching_more_of_the_query_beats_matching_one_word_over_and_over() {
        let caps = [
            capability(
                "builtin:broad",
                "Report builder",
                "Builds a chart from a table.",
                &[],
            ),
            capability(
                "builtin:narrow",
                "Chart chart chart",
                "Chart chart chart chart chart.",
                &["chart", "chart"],
            ),
        ];
        let results = search(caps.iter(), "chart chart chart table report", 10);
        assert_eq!(results[0].capability.id.0, "builtin:broad");
        assert_eq!(results[0].reasons.len(), 3);
        // Eight occurrences of "chart" are worth exactly one reason, which is
        // the whole mechanism: repetition has nothing new to score against.
        assert_eq!(results[1].reasons.len(), 1);
    }

    #[test]
    fn make_me_a_powerpoint_finds_the_presentation_tool_and_says_why() {
        let caps = registry();
        let results = search(caps.iter(), "make me a powerpoint", 10);
        let best = results.first().unwrap();
        assert_eq!(best.capability.id.0, "builtin:create_presentation");
        assert!(
            best.reasons.iter().any(|reason| matches!(
                reason,
                Reason::RelatedMatch { term, matched, .. }
                    if term == "powerpoint" && matched == "presentation"
            )),
            "{:?}",
            best.reasons
        );
        assert!(
            best.reasons.iter().any(|reason| matches!(
                reason,
                Reason::RelatedMatch { term, matched, .. }
                    if term == "make" && matched == "create"
            )),
            "{:?}",
            best.reasons
        );
    }

    #[test]
    fn read_the_excel_file_finds_the_spreadsheet_tool_and_says_why() {
        let caps = registry();
        let results = search(caps.iter(), "read the excel file", 10);
        let best = results.first().unwrap();
        assert_eq!(best.capability.id.0, "builtin:read_spreadsheet");
        assert!(
            best.reasons.iter().any(|reason| matches!(
                reason,
                Reason::RelatedMatch { term, matched, .. }
                    if term == "excel" && matched == "spreadsheet"
            )),
            "{:?}",
            best.reasons
        );
    }

    #[test]
    fn put_this_in_the_trash_finds_the_delete_tool_and_says_why() {
        let caps = registry();
        let results = search(caps.iter(), "put this in the trash", 10);
        let best = results.first().unwrap();
        assert_eq!(best.capability.id.0, "builtin:delete_file");
        assert!(
            best.reasons.iter().any(|reason| matches!(
                reason,
                Reason::RelatedMatch { term, matched, .. }
                    if term == "trash" && matched == "delete"
            )),
            "{:?}",
            best.reasons
        );
    }

    #[test]
    fn a_singular_query_finds_plural_text_and_a_plural_query_finds_singular_text() {
        let caps = [
            capability(
                "builtin:merge",
                "Merge spreadsheets",
                "Combine several files into one.",
                &[],
            ),
            capability("builtin:read", "Read spreadsheet", "Read one file.", &[]),
        ];

        let singular = search(caps.iter(), "spreadsheet", 10);
        assert_eq!(singular.len(), 2, "{singular:?}");
        let plural_capability = singular
            .iter()
            .find(|m| m.capability.id.0 == "builtin:merge")
            .unwrap();
        assert!(
            matches!(
                &plural_capability.reasons[0],
                Reason::RelatedMatch { term, matched, .. }
                    if term == "spreadsheet" && matched == "spreadsheets"
            ),
            "{:?}",
            plural_capability.reasons
        );

        let plural = search(caps.iter(), "spreadsheets", 10);
        assert_eq!(plural.len(), 2, "{plural:?}");
        let singular_capability = plural
            .iter()
            .find(|m| m.capability.id.0 == "builtin:read")
            .unwrap();
        assert!(
            matches!(
                &singular_capability.reasons[0],
                Reason::RelatedMatch { term, matched, .. }
                    if term == "spreadsheets" && matched == "spreadsheet"
            ),
            "{:?}",
            singular_capability.reasons
        );
    }

    #[test]
    fn creating_finds_a_capability_that_says_create() {
        let caps = registry();
        let results = search(caps.iter(), "creating a document", 10);
        let best = results.first().unwrap();
        assert_eq!(best.capability.id.0, "builtin:create_document");
        assert!(
            best.reasons.iter().any(|reason| matches!(
                reason,
                Reason::RelatedMatch { term, matched, .. }
                    if term == "creating" && matched == "create"
            )),
            "{:?}",
            best.reasons
        );
    }

    #[test]
    fn an_empty_or_all_stopword_query_returns_nothing_rather_than_the_registry() {
        let caps = registry();
        for query in [
            "",
            "   ",
            "!!!",
            "how do I",
            "the",
            "what is it that you want",
        ] {
            assert!(
                search(caps.iter(), query, 10).is_empty(),
                "{query:?} returned results"
            );
        }
    }

    #[test]
    fn a_limit_of_zero_returns_nothing() {
        let caps = registry();
        assert!(search(caps.iter(), "create a presentation", 0).is_empty());
    }

    #[test]
    fn a_limit_keeps_the_best_results_rather_than_the_first_ones() {
        let caps = registry();
        let all = search(caps.iter(), "create a presentation deck", 10);
        assert!(all.len() > 2, "{all:?}");
        let capped = search(caps.iter(), "create a presentation deck", 2);
        assert_eq!(capped.as_slice(), &all[..2]);
        assert!(capped[1].score >= all[2].score);
    }

    #[test]
    fn the_same_search_twice_returns_the_same_order_even_when_scores_tie() {
        // Three capabilities that score identically, listed out of id order,
        // so only the tiebreak can separate them — and it has to separate
        // them the same way every run.
        let caps = [
            capability("builtin:c_convert", "Convert deck", "Convert a deck.", &[]),
            capability("builtin:a_export", "Export deck", "Export a deck.", &[]),
            capability("builtin:b_print", "Print deck", "Print a deck.", &[]),
        ];
        let first = search(caps.iter(), "deck", 10);
        let second = search(caps.iter(), "deck", 10);
        assert_eq!(first, second);

        // Tied, which is the precondition this test needs — not any
        // particular number. The absolute score depends on how rare "deck"
        // is across these three, which is exactly the kind of thing that
        // should be free to change without a determinism test noticing.
        let scores: Vec<f32> = first.iter().map(|m| m.score).collect();
        assert!(
            scores.windows(2).all(|pair| pair[0] == pair[1]),
            "the candidates were meant to tie: {scores:?}"
        );
        let ids: Vec<&str> = first.iter().map(|m| m.capability.id.0.as_str()).collect();
        assert_eq!(
            ids,
            ["builtin:a_export", "builtin:b_print", "builtin:c_convert"]
        );
    }

    #[test]
    fn the_explanation_reads_like_something_a_person_would_accept() {
        let caps = registry();
        let results = search(caps.iter(), "make me a powerpoint", 1);
        let best = results.first().unwrap();
        let sentences: Vec<String> = best.reasons.iter().map(Reason::describe).collect();
        assert_eq!(
            sentences,
            [
                "“make” is related to “create”",
                "“powerpoint” is related to “presentation”",
            ]
        );

        let exact = search(caps.iter(), "delete a file", 1);
        let sentences: Vec<String> = exact
            .first()
            .unwrap()
            .reasons
            .iter()
            .map(Reason::describe)
            .collect();
        assert_eq!(
            sentences,
            ["“delete” is in its name", "“file” is in its name"]
        );
    }

    #[test]
    fn a_capability_that_matched_nothing_is_absent_rather_than_scored_zero() {
        let caps = registry();
        let results = search(caps.iter(), "powerpoint", 10);
        assert!(!results.is_empty());
        for result in &results {
            assert!(!result.reasons.is_empty(), "{result:?}");
            assert!(result.score > 0.0, "{result:?}");
        }
        assert!(
            !results
                .iter()
                .any(|m| m.capability.id.0 == "builtin:read_spreadsheet"),
            "{results:?}"
        );
    }

    #[test]
    fn the_stemmer_leaves_short_words_and_non_plurals_alone() {
        // The rules that would wreck these are guarded, and the guards are
        // the only thing keeping "read" out of the "r" bucket.
        assert_eq!(stem("read"), "read");
        assert_eq!(stem("css"), "css");
        assert_eq!(stem("status"), "status");
        assert_eq!(stem("analysis"), "analysis");
        assert_eq!(stem("pptx"), "pptx");
        // And the rules that should fire, do.
        assert_eq!(stem("spreadsheets"), stem("spreadsheet"));
        assert_eq!(stem("creating"), stem("create"));
        assert_eq!(stem("created"), stem("create"));
        assert_eq!(stem("slides"), stem("slide"));
        assert_eq!(stem("boxes"), stem("box"));
        assert_eq!(stem("directories"), stem("directory"));
        assert_eq!(stem("classes"), stem("class"));
    }
}
