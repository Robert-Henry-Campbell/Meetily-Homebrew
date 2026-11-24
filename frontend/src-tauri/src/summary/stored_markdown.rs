//! A stored summary as Markdown, whatever format it was saved in.
//!
//! Summaries are stored as Markdown, as editor blocks (after the user edited
//! one), or, for older meetings, as a map of titled sections. This is the
//! native counterpart of `frontend/src/lib/summary-markdown.ts`; the export
//! runs without the webview and needs the same result.

use super::service::{SummaryAuthor, SUMMARY_AUTHOR_FIELD};
use serde_json::Value;

/// Keys of a stored summary that are bookkeeping, not sections.
const NON_SECTION_KEYS: [&str; 2] = ["english_cache", "original_markdown"];

fn inline_text(node: &Value) -> String {
    match node {
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(inline_text).collect(),
        Value::Object(map) => match map.get("text") {
            Some(Value::String(text)) => text.clone(),
            _ => map.get("content").map(inline_text).unwrap_or_default(),
        },
        _ => String::new(),
    }
}

fn walk_blocks(blocks: &[Value], depth: usize, out: &mut Vec<String>) {
    for block in blocks {
        let text = block.get("content").map(inline_text).unwrap_or_default().trim().to_string();
        let indent = "  ".repeat(depth);
        let props = block.get("props");
        match block.get("type").and_then(Value::as_str) {
            Some("heading") => {
                let level = props.and_then(|props| props.get("level")).and_then(Value::as_u64).unwrap_or(2).clamp(1, 6);
                out.push(format!("{} {text}", "#".repeat(level as usize)));
            }
            Some("bulletListItem") => out.push(format!("{indent}- {text}")),
            Some("numberedListItem") => out.push(format!("{indent}1. {text}")),
            Some("checkListItem") => {
                let checked = props.and_then(|props| props.get("checked")).and_then(Value::as_bool).unwrap_or(false);
                out.push(format!("{indent}- [{}] {text}", if checked { "x" } else { " " }));
            }
            Some("quote") => {
                if !text.is_empty() {
                    out.push(format!("> {text}"));
                }
            }
            Some("table") => {
                let rows = block.get("content").and_then(|content| content.get("rows")).and_then(Value::as_array);
                if let Some(rows) = rows.filter(|rows| !rows.is_empty()) {
                    let cells: Vec<Vec<String>> = rows
                        .iter()
                        .map(|row| {
                            row.get("cells").and_then(Value::as_array).map(|cells| {
                                cells
                                    .iter()
                                    .map(|cell| {
                                        let content = if cell.is_array() { cell } else { cell.get("content").unwrap_or(&Value::Null) };
                                        inline_text(content).trim().replace('|', "\\|")
                                    })
                                    .collect()
                            }).unwrap_or_default()
                        })
                        .collect();
                    let width = cells.iter().map(Vec::len).max().unwrap_or(0).max(1);
                    let line = |row: &Vec<String>| {
                        format!("| {} |", (0..width).map(|index| row.get(index).cloned().unwrap_or_default()).collect::<Vec<_>>().join(" | "))
                    };
                    out.push(line(&cells[0]));
                    out.push(format!("| {} |", vec!["---"; width].join(" | ")));
                    out.extend(cells.iter().skip(1).map(line));
                }
            }
            _ => {
                if !text.is_empty() {
                    out.push(text);
                }
            }
        }
        if let Some(children) = block.get("children").and_then(Value::as_array).filter(|children| !children.is_empty()) {
            walk_blocks(children, depth + 1, out);
        }
    }
}

fn blocks_to_markdown(blocks: &[Value]) -> String {
    let mut out = Vec::new();
    walk_blocks(blocks, 0, &mut out);
    out.join("\n")
}

/// The stored value as an object. The database column holds JSON, a JSON
/// string of JSON (older rows), or plain Markdown.
fn parse(raw: &str) -> Value {
    let mut value = match serde_json::from_str::<Value>(raw) {
        Ok(value) => value,
        Err(_) => return Value::String(raw.to_string()),
    };
    // Unwrap one level of double encoding.
    if let Value::String(inner) = &value {
        if let Ok(parsed @ Value::Object(_)) = serde_json::from_str::<Value>(inner) {
            value = parsed;
        }
    }
    value
}

/// The complete, user-visible summary as Markdown. Empty when there is none.
pub fn stored_summary_markdown(raw: &str) -> String {
    let value = parse(raw);
    let map = match &value {
        Value::String(text) => return text.clone(),
        Value::Object(map) => map,
        _ => return String::new(),
    };
    if let Some(Value::String(markdown)) = map.get("markdown") {
        return markdown.clone();
    }
    if let Some(Value::Array(blocks)) = map.get("summary_json") {
        return blocks_to_markdown(blocks);
    }
    // Legacy: `{ "_section_order": [...], "<key>": { "title", "blocks": [...] } }`.
    let mut keys: Vec<&str> = map
        .get("_section_order")
        .and_then(Value::as_array)
        .map(|order| order.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    for key in map.keys() {
        if !keys.contains(&key.as_str()) {
            keys.push(key);
        }
    }
    keys.into_iter()
        .filter(|key| !NON_SECTION_KEYS.contains(key))
        .filter_map(|key| {
            let section = map.get(key)?;
            let blocks = section.get("blocks")?.as_array()?;
            let title = section.get("title").and_then(Value::as_str).filter(|title| !title.is_empty()).unwrap_or(key);
            let body = blocks.iter().map(|block| block.get("content").map(inline_text).unwrap_or_default()).collect::<Vec<_>>().join("\n\n");
            Some(format!("## {title}\n\n{body}"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Which model wrote the stored summary, when that was recorded. Absent for
/// summaries the user edited afterwards and for older ones.
pub(crate) fn stored_summary_author(raw: &str) -> Option<SummaryAuthor> {
    serde_json::from_value(parse(raw).get(SUMMARY_AUTHOR_FIELD)?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_is_returned_as_stored_also_when_double_encoded_or_plain() {
        let stored = serde_json::json!({ "markdown": "## Decisions\n- Ship", "english_cache": { "markdown": "x" } }).to_string();
        assert_eq!(stored_summary_markdown(&stored), "## Decisions\n- Ship");
        let double = serde_json::to_string(&stored).unwrap();
        assert_eq!(stored_summary_markdown(&double), "## Decisions\n- Ship");
        assert_eq!(stored_summary_markdown("## Plain\ntext"), "## Plain\ntext");
        assert_eq!(stored_summary_markdown("null"), "");
        assert_eq!(stored_summary_markdown("[]"), "");
    }

    #[test]
    fn markdown_wins_over_editor_blocks_as_on_the_meeting_page() {
        let stored = serde_json::json!({ "markdown": "Edited text", "summary_json": [{ "type": "paragraph", "content": "Old" }] }).to_string();
        assert_eq!(stored_summary_markdown(&stored), "Edited text");
    }

    #[test]
    fn editor_blocks_become_markdown() {
        let stored = serde_json::json!({ "summary_json": [
            { "type": "heading", "props": { "level": 2 }, "content": [{ "type": "text", "text": "Decisions" }] },
            { "type": "bulletListItem", "content": [{ "type": "text", "text": "Ship " }, { "type": "text", "text": "Friday" }],
              "children": [{ "type": "bulletListItem", "content": "after tests" }] },
            { "type": "numberedListItem", "content": "First" },
            { "type": "checkListItem", "props": { "checked": true }, "content": "Done" },
            { "type": "checkListItem", "props": { "checked": false }, "content": "Open" },
            { "type": "quote", "content": "Quoted" },
            { "type": "quote", "content": "" },
            { "type": "paragraph", "content": [{ "type": "link", "content": [{ "type": "text", "text": "a link" }] }] },
            { "type": "paragraph", "content": [] },
            { "type": "heading", "props": { "level": 9 }, "content": "Deep" },
            { "type": "table", "content": { "rows": [
                { "cells": [[{ "type": "text", "text": "Who" }], { "content": [{ "type": "text", "text": "What" }] }] },
                { "cells": [[{ "type": "text", "text": "A|B" }]] }
            ] } }
        ] }).to_string();
        assert_eq!(stored_summary_markdown(&stored), [
            "## Decisions",
            "- Ship Friday",
            "  - after tests",
            "1. First",
            "- [x] Done",
            "- [ ] Open",
            "> Quoted",
            "a link",
            "###### Deep",
            "| Who | What |",
            "| --- | --- |",
            "| A\\|B |  |",
        ].join("\n"));
    }

    #[test]
    fn legacy_sections_keep_their_order_and_skip_bookkeeping() {
        let stored = serde_json::json!({
            "_section_order": ["next", "summary"],
            "summary": { "title": "Summary", "blocks": [{ "content": "One" }, { "content": [{ "text": "Two" }] }] },
            "next": { "title": "", "blocks": [{ "content": "Later" }] },
            "english_cache": { "blocks": [{ "content": "never shown" }] },
            "MeetingName": "ignored, not a section"
        }).to_string();
        assert_eq!(stored_summary_markdown(&stored), "## next\n\nLater\n\n## Summary\n\nOne\n\nTwo");
    }

    #[test]
    fn the_author_is_read_when_recorded() {
        let stored = serde_json::json!({ "markdown": "x", "generated_by": { "provider": "builtin-ai", "model": "qwen3.5:4b", "fallback_reason": "Claude rejected the API key" } }).to_string();
        let author = stored_summary_author(&stored).unwrap();
        assert_eq!((author.provider.as_str(), author.model.as_str()), ("builtin-ai", "qwen3.5:4b"));
        assert_eq!(author.fallback_reason.as_deref(), Some("Claude rejected the API key"));
        assert_eq!(stored_summary_author(r#"{"markdown":"edited"}"#), None);
        assert_eq!(stored_summary_author("plain"), None);
    }
}
