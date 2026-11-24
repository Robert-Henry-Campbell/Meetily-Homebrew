//! The two Markdown files of an exported meeting: summary and transcript.
//!
//! The layout follows the app's own Markdown export (front matter with title,
//! meeting ID, date and attendees; transcript lines `[mm:ss] **Speaker:**
//! text`). The summary file also carries action items and the user's notes,
//! which the in-app export leaves out. Rendering is a pure function of
//! `MeetingExport`, so the same meeting always gives the same bytes and an
//! unchanged file is recognisable as unchanged.

use super::naming::display_speaker;
use chrono::NaiveDateTime;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TranscriptLine {
    /// Seconds from the start of the recording.
    pub start_seconds: Option<f64>,
    pub speaker: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionItem {
    pub text: String,
    pub owner: Option<String>,
    pub due: Option<String>,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MeetingExport {
    pub meeting_id: String,
    pub title: String,
    /// When the recording started, in the user's local time.
    pub started_local: NaiveDateTime,
    /// The user's display name; empty when not set.
    pub user_name: String,
    /// The other people, as `naming::known_people` lists them.
    pub attendees: Vec<String>,
    pub lines: Vec<TranscriptLine>,
    /// The summary as Markdown; empty when there is none.
    pub summary_markdown: String,
    /// "provider/model" of the summary, when recorded.
    pub summary_model: Option<String>,
    /// Why the local model wrote the summary instead of Claude.
    pub summary_fallback_reason: Option<String>,
    pub action_items: Vec<ActionItem>,
    pub notes_markdown: String,
    /// What the automatic chain skipped (for example: live transcript kept).
    pub processing_note: Option<String>,
}

fn yaml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn front_matter(meeting: &MeetingExport, extra: &[(&str, String)]) -> String {
    let mut lines = vec!["---".to_string()];
    lines.push(format!("title: {}", yaml_string(&meeting.title)));
    lines.push(format!("meeting_id: {}", yaml_string(&meeting.meeting_id)));
    lines.push(format!("date: {}", meeting.started_local.format("%Y-%m-%d")));
    lines.push(format!("time: {}", yaml_string(&meeting.started_local.format("%H:%M").to_string())));
    if !meeting.attendees.is_empty() {
        lines.push("attendees:".to_string());
        lines.extend(meeting.attendees.iter().map(|name| format!("  - {}", yaml_string(name))));
    }
    lines.extend(extra.iter().map(|(key, value)| format!("{key}: {}", yaml_string(value))));
    lines.push("tags:".to_string());
    lines.push("  - meetily".to_string());
    lines.push("---".to_string());
    lines.join("\n")
}

fn heading(meeting: &MeetingExport) -> String {
    format!("# {}\n\n_{}_", meeting.title, meeting.started_local.format("%A, %B %-d, %Y"))
}

/// Moves every heading one level down, so the document's own `#` title stays
/// the only top-level heading. Lines inside fenced code blocks are left alone.
fn demote_headings(markdown: &str) -> String {
    let mut fenced = false;
    markdown
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                fenced = !fenced;
                return line.to_string();
            }
            let hashes = line.chars().take_while(|c| *c == '#').count();
            let is_heading = (1..=5).contains(&hashes) && line[hashes..].starts_with(char::is_whitespace);
            if fenced || !is_heading {
                line.to_string()
            } else {
                format!("#{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn stamp(seconds: Option<f64>) -> String {
    let Some(seconds) = seconds.filter(|seconds| seconds.is_finite()) else { return String::new() };
    let total = seconds.max(0.0).floor() as u64;
    let (hours, minutes, seconds) = (total / 3600, (total % 3600) / 60, total % 60);
    if hours > 0 {
        format!("[{hours}:{minutes:02}:{seconds:02}] ")
    } else {
        format!("[{minutes:02}:{seconds:02}] ")
    }
}

/// The summary file, or `None` when the meeting has no summary, no action
/// items and no notes yet.
pub fn render_summary_file(meeting: &MeetingExport) -> Option<String> {
    let summary = meeting.summary_markdown.trim();
    let notes = meeting.notes_markdown.trim();
    if summary.is_empty() && notes.is_empty() && meeting.action_items.is_empty() {
        return None;
    }
    let mut extra = Vec::new();
    if let Some(model) = meeting.summary_model.as_deref().filter(|_| !summary.is_empty()) {
        extra.push(("summary_model", model.to_string()));
    }
    let mut sections = vec![front_matter(meeting, &extra), heading(meeting)];
    if !summary.is_empty() {
        let mut body = demote_headings(summary);
        if let Some(reason) = &meeting.summary_fallback_reason {
            body = format!("_The local model wrote this summary because {reason}._\n\n{body}");
        }
        sections.push(format!("## Summary\n\n{body}"));
    }
    if !meeting.action_items.is_empty() {
        let items = meeting
            .action_items
            .iter()
            .map(|item| {
                let owner = item.owner.as_deref().map(str::trim).filter(|owner| !owner.is_empty());
                let due = item.due.as_deref().map(str::trim).filter(|due| !due.is_empty());
                let suffix = match (owner, due) {
                    (Some(owner), Some(due)) => format!(" ({owner}, due {due})"),
                    (Some(owner), None) => format!(" ({owner})"),
                    (None, Some(due)) => format!(" (due {due})"),
                    (None, None) => String::new(),
                };
                // One item per line: a line break inside an item would end the list.
                let text = item.text.split_whitespace().collect::<Vec<_>>().join(" ");
                format!("- [{}] {text}{suffix}", if item.done { "x" } else { " " })
            })
            .collect::<Vec<_>>()
            .join("\n");
        sections.push(format!("## Action items\n\n{items}"));
    }
    if !notes.is_empty() {
        sections.push(format!("## Notes\n\n{}", demote_headings(notes)));
    }
    Some(sections.join("\n\n") + "\n")
}

/// The transcript file, or `None` when the meeting has no transcript lines.
pub fn render_transcript_file(meeting: &MeetingExport) -> Option<String> {
    let lines: Vec<String> = meeting
        .lines
        .iter()
        .filter(|line| !line.text.trim().is_empty())
        .map(|line| {
            let speaker = line
                .speaker
                .as_deref()
                .map(str::trim)
                .filter(|speaker| !speaker.is_empty())
                .map(|speaker| format!("**{}:** ", display_speaker(speaker, &meeting.user_name)))
                .unwrap_or_default();
            format!("{}{speaker}{}", stamp(line.start_seconds), line.text.trim())
        })
        .collect();
    if lines.is_empty() {
        return None;
    }
    let mut sections = vec![front_matter(meeting, &[]), heading(meeting)];
    if let Some(note) = meeting.processing_note.as_deref().map(str::trim).filter(|note| !note.is_empty()) {
        sections.push(format!("_{note}_"));
    }
    sections.push(format!("## Transcript\n\n{}", lines.join("\n\n")));
    Some(sections.join("\n\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meeting() -> MeetingExport {
        MeetingExport {
            meeting_id: "meeting-7f3a".into(),
            title: "Budget \"review\"".into(),
            started_local: NaiveDateTime::parse_from_str("2026-11-23 14:30", "%Y-%m-%d %H:%M").unwrap(),
            user_name: "Rob".into(),
            attendees: vec!["Alice Smith".into(), "Bob".into()],
            lines: vec![
                TranscriptLine { start_seconds: Some(1.9), speaker: Some("You".into()), text: " Shall we start? ".into() },
                TranscriptLine { start_seconds: Some(65.0), speaker: Some("Alice Smith".into()), text: "Yes.".into() },
                TranscriptLine { start_seconds: Some(3725.0), speaker: Some("You + Bob".into()), text: "Agreed.".into() },
                TranscriptLine { start_seconds: None, speaker: None, text: "No stamp, no speaker.".into() },
                TranscriptLine { start_seconds: Some(4000.0), speaker: Some("Bob".into()), text: "   ".into() },
            ],
            summary_markdown: "## Decisions\n- Ship on Friday\n\n### Detail\n```\n# not a heading\n```\n#hashtag stays".into(),
            summary_model: Some("claude/claude-opus-5-5".into()),
            summary_fallback_reason: None,
            action_items: vec![
                ActionItem { text: "Send the\nbudget".into(), owner: Some("Alice Smith".into()), due: Some("Friday".into()), done: false },
                ActionItem { text: "Book the room".into(), owner: None, due: None, done: true },
                ActionItem { text: "Call the bank".into(), owner: Some(" ".into()), due: Some("Monday".into()), done: false },
            ],
            notes_markdown: "# My notes\nAsk about Q4.".into(),
            processing_note: None,
        }
    }

    #[test]
    fn the_summary_file_has_front_matter_summary_action_items_and_notes() {
        let expected = r#"---
title: "Budget \"review\""
meeting_id: "meeting-7f3a"
date: 2026-11-23
time: "14:30"
attendees:
  - "Alice Smith"
  - "Bob"
summary_model: "claude/claude-opus-5-5"
tags:
  - meetily
---

# Budget "review"

_Monday, November 23, 2026_

## Summary

### Decisions
- Ship on Friday

#### Detail
```
# not a heading
```
#hashtag stays

## Action items

- [ ] Send the budget (Alice Smith, due Friday)
- [x] Book the room
- [ ] Call the bank (due Monday)

## Notes

## My notes
Ask about Q4.
"#;
        assert_eq!(render_summary_file(&meeting()).unwrap(), expected);
    }

    #[test]
    fn the_transcript_file_lists_every_spoken_line_with_time_and_speaker() {
        let expected = r#"---
title: "Budget \"review\""
meeting_id: "meeting-7f3a"
date: 2026-11-23
time: "14:30"
attendees:
  - "Alice Smith"
  - "Bob"
tags:
  - meetily
---

# Budget "review"

_Monday, November 23, 2026_

## Transcript

[00:01] **Rob (You):** Shall we start?

[01:05] **Alice Smith:** Yes.

[1:02:05] **Rob (You) + Bob:** Agreed.

No stamp, no speaker.
"#;
        assert_eq!(render_transcript_file(&meeting()).unwrap(), expected);
    }

    #[test]
    fn a_takeover_and_a_skipped_step_are_stated_in_the_files() {
        let mut meeting = meeting();
        meeting.summary_model = Some("builtin-ai/qwen3.5:4b".into());
        meeting.summary_fallback_reason = Some("Claude rejected the API key".into());
        meeting.processing_note = Some("The accurate re-transcription did not finish, so the live transcript was kept.".into());
        let summary = render_summary_file(&meeting).unwrap();
        assert!(summary.contains("summary_model: \"builtin-ai/qwen3.5:4b\""));
        assert!(summary.contains("## Summary\n\n_The local model wrote this summary because Claude rejected the API key._\n\n### Decisions"));
        let transcript = render_transcript_file(&meeting).unwrap();
        assert!(transcript.contains("_The accurate re-transcription did not finish, so the live transcript was kept._\n\n## Transcript"));
    }

    #[test]
    fn nothing_to_write_gives_no_file_and_partial_content_gives_partial_files() {
        let mut empty = meeting();
        empty.lines.clear();
        empty.summary_markdown = "  ".into();
        empty.action_items.clear();
        empty.notes_markdown.clear();
        assert_eq!(render_summary_file(&empty), None);
        assert_eq!(render_transcript_file(&empty), None);

        // Notes without a summary still make a file; the model line needs a summary.
        empty.notes_markdown = "Only notes.".into();
        empty.attendees.clear();
        let notes_only = render_summary_file(&empty).unwrap();
        assert!(notes_only.contains("## Notes\n\nOnly notes.\n"));
        assert!(!notes_only.contains("## Summary"));
        assert!(!notes_only.contains("summary_model"));
        assert!(!notes_only.contains("attendees"));
    }

    #[test]
    fn the_same_meeting_always_gives_the_same_bytes() {
        assert_eq!(render_summary_file(&meeting()), render_summary_file(&meeting()));
        assert_eq!(render_transcript_file(&meeting()), render_transcript_file(&meeting()));
    }

    #[test]
    fn control_characters_in_a_title_cannot_break_the_front_matter() {
        let mut meeting = meeting();
        meeting.title = "Line one\nattendees: [evil]\t\u{1}".into();
        let file = render_transcript_file(&meeting).unwrap();
        assert!(file.starts_with("---\ntitle: \"Line one\\nattendees: [evil]\\t\\x01\"\nmeeting_id:"));
    }
}
