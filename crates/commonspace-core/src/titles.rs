//! Naming a conversation after what it is about.
//!
//! Nobody titles their own conversations, so Commonspace has to, and a title
//! that is only the first 70 characters of the prompt ("can you please go
//! through the scans folder and rename every") is not a name — it is a
//! truncation. Everything here is deterministic string work: no model call and
//! no network, because a conversation needs a title the instant it exists and
//! must get the same one every time.

/// The title a conversation gets when nothing usable survives.
const FALLBACK: &str = "New task";

/// Longest title generated here, in characters. Sidebar rows are narrow;
/// a title the layout has to clip reads as a bug rather than as a name.
const MAX_CHARS: usize = 52;

/// How far in a sentence boundary still means "the request came first, the
/// detail came after". Past this the text is a paragraph, and its opening
/// sentence is no more the title than any other part of it.
const SENTENCE_SCAN_CHARS: usize = 80;

/// Summaries shorter than this say nothing a title can use ("Done.", "OK").
const MIN_SUMMARY_CHARS: usize = 8;

/// The summary the runtime records when a session ends without ever saying
/// how it went. It is an admission, not an outcome, so it never becomes a
/// title — shared from here so the two crates cannot drift apart on the
/// wording.
pub const NO_RESULT_SUMMARY: &str = "The task ended without a result.";

/// Openings people type before saying what they want. Stripped repeatedly and
/// in this order, so "Hey Commonspace, could you…" loses all three. Longer
/// entries come first: a shorter one must not claim part of a longer one.
const LEADING_FILLER: &[&str] = &[
    "i'd like you to",
    "i need you to",
    "i want you to",
    "commonspace,",
    "could you",
    "would you",
    "can you",
    "help me",
    "hello",
    "please",
    "let's",
    "hey",
    "hi",
];

/// Closings people add out of politeness. They never describe the work.
const TRAILING_FILLER: &[&str] = &["thank you", "thanks", "please"];

/// Openings that mean a summary is reporting a failure rather than an
/// outcome. This is a heuristic and only a heuristic: it covers the phrasings
/// agents actually reach for and will miss anything else. A miss costs a
/// conversation an awkward name, which is why the list can stay this small.
const ERROR_PREFIXES: &[&str] = &["error", "failed", "could not", "couldn't", "unable to"];

/// A short, readable title derived from what the user asked for.
///
/// The conversational wrapping ("hey, could you please…") comes off, the
/// first sentence wins when the request is one, and the result is capped at a
/// width that fits without clipping. Returns `"New task"` when the prompt is
/// empty or turns out to be nothing but filler.
pub fn from_prompt(prompt: &str) -> String {
    condense(prompt).unwrap_or_else(|| FALLBACK.to_owned())
}

/// A better title derived from a finished task's own summary, when the
/// summary actually says something. `None` means "keep what you have".
///
/// Rejected: nothing, the runtime's no-result placeholder, anything too short
/// to be a description, and anything that reads as an error. That last test
/// is a prefix heuristic (see [`ERROR_PREFIXES`]) — "Could not find the scans
/// folder" is a true thing to say and a terrible name for a conversation, but
/// no fixed list catches every way of saying it.
pub fn from_summary(summary: &str) -> Option<String> {
    let normalized = collapse_whitespace(summary);
    if normalized == NO_RESULT_SUMMARY
        || normalized.chars().count() < MIN_SUMMARY_CHARS
        || reads_as_error(&normalized)
    {
        return None;
    }
    condense(&normalized)
}

/// The shared pipeline: strip the filler around the edges, keep the opening
/// sentence when there is one, cap, capitalize. `None` when nothing is left.
fn condense(text: &str) -> Option<String> {
    let collapsed = collapse_whitespace(text);
    let core = strip_trailing_filler(strip_leading_filler(&collapsed));
    let capped = cap(first_sentence(core));
    if capped.is_empty() {
        None
    } else {
        Some(capitalize_first(&capped))
    }
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Punctuation that carries no meaning at the edge of a title.
fn is_edge_punctuation(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '-' | '—' | '…')
}

fn is_trimmable(c: char) -> bool {
    c.is_whitespace() || is_edge_punctuation(c)
}

fn strip_leading_filler(text: &str) -> &str {
    let mut rest = text;
    loop {
        rest = rest.trim_start_matches(is_trimmable);
        let Some(matched) = LEADING_FILLER
            .iter()
            .find_map(|filler| match_prefix(rest, filler))
        else {
            return rest;
        };
        rest = skip_chars(rest, matched);
    }
}

fn strip_trailing_filler(text: &str) -> &str {
    let mut rest = text;
    loop {
        rest = rest.trim_end_matches(is_trimmable);
        let Some(matched) = TRAILING_FILLER
            .iter()
            .find_map(|filler| match_suffix(rest, filler))
        else {
            return rest;
        };
        rest = drop_last_chars(rest, matched);
    }
}

/// Cut at an early sentence end. When someone states the request and then
/// explains it, the request is the title and the explanation is not.
fn first_sentence(text: &str) -> &str {
    let mut chars = text.char_indices().peekable();
    let mut position = 0;
    while let Some((index, c)) = chars.next() {
        if position >= SENTENCE_SCAN_CHARS {
            break;
        }
        // Whitespace is already collapsed, so a real boundary is always
        // punctuation followed by exactly one space.
        if matches!(c, '.' | '?' | '!') && chars.peek().is_some_and(|&(_, next)| next == ' ') {
            return &text[..index];
        }
        position += 1;
    }
    text
}

/// Cap at [`MAX_CHARS`], cutting between words and marking the cut with a
/// visible ellipsis (the convention `Storage::rename_conversation` uses too).
/// A single word longer than the cap offers nowhere to cut, so it is clipped
/// where it falls — better a clipped word than a title that is only an
/// ellipsis.
fn cap(text: &str) -> String {
    if text.chars().count() <= MAX_CHARS {
        return text.to_owned();
    }
    let head: String = text.chars().take(MAX_CHARS).collect();
    let at_boundary = match head.rfind(' ') {
        Some(space) if space > 0 => &head[..space],
        _ => head.as_str(),
    };
    let trimmed = at_boundary.trim_end_matches(is_trimmable);
    let body = if trimmed.is_empty() {
        head.as_str()
    } else {
        trimmed
    };
    format!("{body}…")
}

/// Uppercase the first character and leave every other one alone: file names,
/// product names and acronyms in the middle of a request must survive intact.
fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn reads_as_error(text: &str) -> bool {
    ERROR_PREFIXES
        .iter()
        .any(|prefix| match_prefix(text, prefix).is_some())
}

/// Lowercase for comparison, with the typographic apostrophe folded onto the
/// plain one so "couldn't" matches however the keyboard spelled it.
fn fold(c: char) -> char {
    match c {
        '\u{2019}' => '\'',
        other => other.to_ascii_lowercase(),
    }
}

/// Match `probe` at the start of `text`, case-insensitively, and only when a
/// word boundary follows — otherwise "hi" would eat the start of "hidden
/// files". Returns how many characters matched.
fn match_prefix(text: &str, probe: &str) -> Option<usize> {
    let mut chars = text.chars();
    let mut matched = 0;
    for expected in probe.chars() {
        if fold(chars.next()?) != expected {
            return None;
        }
        matched += 1;
    }
    match chars.next() {
        None => Some(matched),
        Some(next) if !next.is_alphanumeric() => Some(matched),
        Some(_) => None,
    }
}

/// The mirror of [`match_prefix`] for the end of the text.
fn match_suffix(text: &str, probe: &str) -> Option<usize> {
    let mut chars = text.chars().rev();
    let mut matched = 0;
    for expected in probe.chars().rev() {
        if fold(chars.next()?) != expected {
            return None;
        }
        matched += 1;
    }
    match chars.next() {
        None => Some(matched),
        Some(previous) if !previous.is_alphanumeric() => Some(matched),
        Some(_) => None,
    }
}

/// Character-indexed slicing throughout: prompts arrive in every script, and
/// byte offsets into them are a panic waiting to happen.
fn skip_chars(text: &str, count: usize) -> &str {
    match text.char_indices().nth(count) {
        Some((index, _)) => &text[index..],
        None => "",
    }
}

fn drop_last_chars(text: &str, count: usize) -> &str {
    let keep = text.chars().count().saturating_sub(count);
    match text.char_indices().nth(keep) {
        Some((index, _)) => &text[..index],
        None => text,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn the_motivating_prompt_becomes_a_name() {
        assert_eq!(
            from_prompt("can you please go through the scans folder and rename every file to include the date"),
            "Go through the scans folder and rename every file…"
        );
    }

    #[test]
    fn leading_filler_comes_off_in_layers() {
        assert_eq!(
            from_prompt("Hey Commonspace, could you rename my invoices"),
            "Rename my invoices"
        );
        assert_eq!(from_prompt("hi please help me sort photos"), "Sort photos");
        assert_eq!(
            from_prompt("I'd like you to draft a reply"),
            "Draft a reply"
        );
        assert_eq!(
            from_prompt("I\u{2019}d like you to draft a reply"),
            "Draft a reply"
        );
        assert_eq!(from_prompt("Let's plan the trip"), "Plan the trip");
    }

    #[test]
    fn filler_words_inside_real_words_survive() {
        // "hi" must not eat "hidden", nor "hey" "heyday".
        assert_eq!(
            from_prompt("hidden files keep coming back"),
            "Hidden files keep coming back"
        );
        assert_eq!(
            from_prompt("heyday of the newsletter"),
            "Heyday of the newsletter"
        );
        assert_eq!(from_prompt("Please-do-not-split-this"), "Do-not-split-this");
    }

    #[test]
    fn trailing_courtesy_and_punctuation_come_off() {
        assert_eq!(
            from_prompt("Rename the invoices, thanks!"),
            "Rename the invoices"
        );
        assert_eq!(
            from_prompt("Rename the invoices please."),
            "Rename the invoices"
        );
        assert_eq!(
            from_prompt("Rename the invoices. Thank you"),
            "Rename the invoices"
        );
        // The courtesy is on its own line, which is still the end of the text.
        assert_eq!(
            from_prompt("Organize my downloads\nplease"),
            "Organize my downloads"
        );
    }

    #[test]
    fn the_first_sentence_wins_when_it_is_early() {
        assert_eq!(
            from_prompt("Rename the scans. Use the date in the file itself, not today's date."),
            "Rename the scans"
        );
        assert_eq!(
            from_prompt("What changed in the report? I read it yesterday."),
            "What changed in the report"
        );
        // A boundary past the scan window is mid-paragraph, not a heading.
        let late = format!("{} words. And then some more.", "many".repeat(30));
        assert!(!from_prompt(&late).is_empty());
    }

    #[test]
    fn newlines_and_runs_of_spaces_collapse() {
        assert_eq!(
            from_prompt("Organize my downloads\nby file type"),
            "Organize my downloads by file type"
        );
        assert_eq!(
            from_prompt("Organize   my\t\tdownloads"),
            "Organize my downloads"
        );
    }

    #[test]
    fn capping_cuts_between_words_and_marks_the_cut() {
        let title = from_prompt(
            "Summarize every message in the shared inbox and file each one under the right client",
        );
        // The cut lands between words: no half of "file" is left behind.
        assert_eq!(title, "Summarize every message in the shared inbox and…");
        assert!(title.chars().count() <= MAX_CHARS + 1, "{title}");
    }

    #[test]
    fn one_enormous_word_is_clipped_rather_than_lost() {
        let long = "a".repeat(200);
        let title = from_prompt(&long);
        assert_eq!(title.chars().count(), MAX_CHARS + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn casing_beyond_the_first_character_is_left_alone() {
        assert_eq!(
            from_prompt("rename IMG_2024.HEIC to something readable"),
            "Rename IMG_2024.HEIC to something readable"
        );
    }

    #[test]
    fn empty_and_filler_only_prompts_fall_back() {
        assert_eq!(from_prompt(""), "New task");
        assert_eq!(from_prompt("   \n\t "), "New task");
        assert_eq!(from_prompt("hi"), "New task");
        assert_eq!(from_prompt("hello, can you please"), "New task");
        assert_eq!(from_prompt("..."), "New task");
    }

    #[test]
    fn multi_byte_input_never_panics_and_stays_whole() {
        assert_eq!(from_prompt("créer un dossier"), "Créer un dossier");
        assert_eq!(from_prompt("整理我的下载文件夹"), "整理我的下载文件夹");
        // Every char here is multi-byte, so a byte-indexed cap would panic.
        let emoji = "🎉".repeat(80);
        let title = from_prompt(&emoji);
        assert_eq!(title.chars().count(), MAX_CHARS + 1);
        let mixed = format!(
            "Réorganiser {} et les photos anciennes du voyage",
            "é".repeat(40)
        );
        assert!(from_prompt(&mixed).ends_with('…'));
    }

    #[test]
    fn summaries_that_describe_an_outcome_become_titles() {
        assert_eq!(
            from_summary("Renamed 42 scans to include their capture date."),
            Some("Renamed 42 scans to include their capture date".into())
        );
        assert_eq!(
            from_summary("  Filed every invoice under its client folder  "),
            Some("Filed every invoice under its client folder".into())
        );
    }

    #[test]
    fn summaries_that_say_nothing_useful_are_refused() {
        assert_eq!(from_summary(""), None);
        assert_eq!(from_summary("   \n "), None);
        assert_eq!(from_summary(NO_RESULT_SUMMARY), None);
        assert_eq!(from_summary("Done."), None);
        assert_eq!(from_summary("OK"), None);
    }

    #[test]
    fn summaries_that_report_a_failure_are_refused() {
        assert_eq!(from_summary("Error: the scans folder does not exist"), None);
        assert_eq!(from_summary("Failed to rename three of the files"), None);
        assert_eq!(from_summary("Could not find the scans folder"), None);
        assert_eq!(from_summary("Couldn't reach the mail server"), None);
        assert_eq!(from_summary("Couldn\u{2019}t reach the mail server"), None);
        assert_eq!(from_summary("Unable to open the spreadsheet"), None);
        // The heuristic keys on the opening, so a word that merely starts the
        // same way is still an outcome.
        assert!(from_summary("Errors in the ledger are all fixed now").is_some());
    }

    #[test]
    fn long_summaries_are_capped_like_prompts() {
        let summary = from_summary(
            "Renamed every scan in the folder and moved the originals into an archive",
        )
        .expect("a title");
        assert!(summary.chars().count() <= MAX_CHARS + 1);
        assert!(summary.ends_with('…'));
    }
}
