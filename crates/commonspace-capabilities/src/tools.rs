//! Turning MCP tool definitions into registry entries.
//!
//! Commonspace's own tools and a third party's tools arrive in exactly the
//! same shape — an MCP `tools/list` entry — so one function handles both.
//! That is not a convenience: it is the reason the built-in descriptors
//! cannot drift from the built-in tools. There is no second list to keep in
//! sync, because there is no second list. The schemas the model calls against
//! are the schemas the registry indexes.

use crate::{Capability, CapabilityId, CapabilityKind, CapabilitySource, LoadedCapability};
use serde_json::Value;

/// Converts MCP `tools/list` entries into registry entries.
///
/// Definitions missing a string `name` are skipped: a tool the model cannot
/// call by name is not a capability, and the alternative — inventing a name —
/// would put something in search results that fails when selected.
pub fn from_tool_definitions(
    kind: CapabilityKind,
    source: &CapabilitySource,
    definitions: &[Value],
) -> Vec<(Capability, LoadedCapability)> {
    definitions
        .iter()
        .filter_map(|definition| {
            let call_name = definition.get("name")?.as_str()?.to_string();
            let description = definition
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let input_schema = definition
                .get("inputSchema")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));

            let capability = Capability {
                id: CapabilityId::new(kind.namespace(), &call_name),
                kind,
                name: human_name(&call_name),
                summary: summarize(description),
                keywords: keywords(&call_name, description),
                source: source.clone(),
            };
            Some((
                capability,
                LoadedCapability::Tool {
                    call_name,
                    input_schema,
                },
            ))
        })
        .collect()
}

/// `read_spreadsheet` becomes "Read spreadsheet" — a label for the user's
/// "what can this do?" screen. The model never sees this; it matches on the
/// summary and calls by `call_name`.
fn human_name(call_name: &str) -> String {
    let spaced = call_name.replace(['_', '-'], " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => spaced,
    }
}

/// The first sentence or two of a tool description, capped.
///
/// Commonspace's own write tools carry long descriptions that spend most of
/// their words telling the model what *not* to conclude after calling them
/// ("do not read the path back expecting the content to be there"). That
/// guidance is essential at call time and noise in a result list, where the
/// only question is "is this the one?". The full description still reaches
/// the model — it is in the schema behind [`LoadedCapability::Tool`].
fn summarize(description: &str) -> String {
    const MAX: usize = 240;
    let collapsed = description.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= MAX {
        return collapsed;
    }
    // Prefer a sentence boundary inside the budget over a hard cut.
    let head = &collapsed[..MAX];
    match head.rfind(". ") {
        Some(end) => collapsed[..=end].trim_end().to_string(),
        None => {
            let cut = head.rfind(' ').unwrap_or(MAX);
            format!("{}…", collapsed[..cut].trim_end())
        }
    }
}

/// Words worth matching that the summary would read badly for: the tool's
/// own name split into parts, and every file extension the description
/// mentions.
///
/// Extensions matter more than they look. A person asks for "a PowerPoint"
/// or "the xlsx"; the tool description says ".pptx" or "spreadsheet". Pulling
/// the extensions out as keywords is what closes that gap without writing
/// marketing copy into a schema the model also reads.
fn keywords(call_name: &str, description: &str) -> Vec<String> {
    let mut out: Vec<String> = call_name
        .split(['_', '-'])
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect();

    for token in description.split(|c: char| !(c.is_alphanumeric() || c == '.')) {
        let token = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '.');
        if let Some(extension) = token.strip_prefix('.') {
            // A description ends sentences and separates lists with the same
            // character extensions start with: ".pdf, .docx and .csv." hands
            // back ".csv." here.
            let extension = extension.trim_end_matches('.');
            if !extension.is_empty()
                && extension.len() <= 5
                && extension.chars().all(|c| c.is_ascii_alphanumeric())
            {
                let extension = extension.to_lowercase();
                if !out.contains(&extension) {
                    out.push(extension);
                }
            }
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition(name: &str, description: &str) -> Value {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}
        })
    }

    #[test]
    fn a_tool_definition_becomes_a_capability_that_can_still_be_called() {
        let entries = from_tool_definitions(
            CapabilityKind::BuiltinTool,
            &CapabilitySource::Builtin,
            &[definition(
                "read_spreadsheet",
                "Read a spreadsheet as structured data.",
            )],
        );
        let (capability, loaded) = entries.first().expect("one entry");
        assert_eq!(capability.id.0, "builtin:read_spreadsheet");
        assert_eq!(capability.name, "Read spreadsheet");
        let LoadedCapability::Tool {
            call_name,
            input_schema,
        } = loaded
        else {
            panic!("a tool definition must load as a tool: {loaded:?}");
        };
        // The name the model calls must survive the round trip untouched —
        // the humanized name is for the UI and must never leak into a call.
        assert_eq!(call_name, "read_spreadsheet");
        assert_eq!(input_schema["properties"]["path"]["type"], "string");
    }

    #[test]
    fn the_same_function_serves_an_external_server() {
        let source = CapabilitySource::Server {
            name: "linear".into(),
        };
        let entries = from_tool_definitions(
            CapabilityKind::McpTool,
            &source,
            &[definition("create_issue", "File an issue.")],
        );
        let (capability, _) = entries.first().expect("one entry");
        assert_eq!(capability.id.0, "mcp:create_issue");
        assert_eq!(capability.kind, CapabilityKind::McpTool);
        assert_eq!(capability.source, source);
    }

    #[test]
    fn a_definition_without_a_name_is_skipped_rather_than_guessed_at() {
        let entries = from_tool_definitions(
            CapabilityKind::McpTool,
            &CapabilitySource::Server {
                name: "broken".into(),
            },
            &[json!({"description": "no name"}), definition("ok", "fine")],
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.id.0, "mcp:ok");
    }

    #[test]
    fn a_definition_without_a_schema_still_loads_as_a_callable_tool() {
        let entries = from_tool_definitions(
            CapabilityKind::McpTool,
            &CapabilitySource::Server { name: "x".into() },
            &[json!({"name": "ping"})],
        );
        let (_, loaded) = entries.first().expect("one entry");
        let LoadedCapability::Tool { input_schema, .. } = loaded else {
            panic!("{loaded:?}");
        };
        assert_eq!(input_schema["type"], "object");
    }

    #[test]
    fn file_extensions_in_the_description_become_keywords() {
        let entries = from_tool_definitions(
            CapabilityKind::BuiltinTool,
            &CapabilitySource::Builtin,
            &[definition(
                "read_document",
                "Extract the contents of a document. Handles .pdf, .docx, .xlsx and .csv.",
            )],
        );
        let keywords = &entries[0].0.keywords;
        for expected in ["read", "document", "pdf", "docx", "xlsx", "csv"] {
            assert!(
                keywords.iter().any(|k| k == expected),
                "missing {expected} in {keywords:?}"
            );
        }
    }

    #[test]
    fn a_long_description_is_summarized_at_a_sentence_boundary() {
        let long = "Propose creating a new file. This stages the content for the person using \
                    Commonspace to review — it does not write to disk, and the file does not exist \
                    until they apply the change. Fails if the file already exists. After calling \
                    this, do not read the path back expecting the content to be there.";
        let entries = from_tool_definitions(
            CapabilityKind::BuiltinTool,
            &CapabilitySource::Builtin,
            &[definition("create_file", long)],
        );
        let summary = &entries[0].0.summary;
        assert!(summary.len() < long.len(), "{summary}");
        assert!(
            summary.starts_with("Propose creating a new file."),
            "{summary}"
        );
        // A summary that ends mid-word reads as a bug to whoever sees it.
        assert!(
            summary.ends_with('.') || summary.ends_with('…'),
            "{summary}"
        );
        // The full text is not lost — it is still in the schema the model
        // gets when it loads the capability.
        assert!(!summary.contains("do not read the path back"), "{summary}");
    }

    #[test]
    fn a_short_description_is_left_exactly_as_written() {
        let entries = from_tool_definitions(
            CapabilityKind::BuiltinTool,
            &CapabilitySource::Builtin,
            &[definition(
                "read_file",
                "Read a text file's contents, detecting its encoding.",
            )],
        );
        assert_eq!(
            entries[0].0.summary,
            "Read a text file's contents, detecting its encoding."
        );
    }
}
