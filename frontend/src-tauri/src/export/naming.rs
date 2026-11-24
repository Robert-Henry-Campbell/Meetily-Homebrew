//! File names, folder choice, and the list of people for an exported meeting.
//! Pure functions: the worker supplies facts from the database, these decide.

use chrono::NaiveDateTime;

/// Long titles are cut so the whole name stays well inside path limits.
const MAX_TITLE_CHARS: usize = 100;
const MAX_NAME_CHARS: usize = 60;

/// Makes text safe as one name in Drive and on Windows, macOS and Linux after
/// a download: no path separators, no reserved characters, no control
/// characters, no leading or trailing dots and spaces.
pub fn safe_name(text: &str, max_chars: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => ' ',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = collapsed.chars().take(max_chars).collect();
    cut.trim_matches(|c: char| c == '.' || c == ' ').to_string()
}

fn is_user_label(label: &str) -> bool {
    let label = label.trim().to_lowercase();
    label == "you" || label.ends_with("(you)")
}

fn is_generated_label(label: &str) -> bool {
    let label = label.trim().to_lowercase();
    if label == "guest" || label == "speaker" {
        return true;
    }
    label.strip_prefix("speaker ").is_some_and(|number| !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()))
}

/// The parts of a stored speaker label ("You + Alice" is two voices at once).
pub fn label_parts(label: &str) -> impl Iterator<Item = &str> {
    label.split(" + ").map(str::trim).filter(|part| !part.is_empty())
}

/// How a stored speaker label is written in a file: "You" becomes the user's
/// name when it is known, as on the meeting page.
pub fn display_speaker(label: &str, user_name: &str) -> String {
    label_parts(label)
        .map(|part| {
            if !is_user_label(part) {
                part.to_string()
            } else if user_name.trim().is_empty() {
                "You".to_string()
            } else {
                format!("{} (You)", user_name.trim())
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// The other people in a meeting, in the order of their sources: names typed
/// at the speaker question, then named voices in the transcript, then calendar
/// invitees. The user and unnamed voices ("Speaker 2", "Guest") are left out.
pub fn known_people(typed: &[String], speaker_labels: &[String], invitees: &[String], user_name: &str) -> Vec<String> {
    let user = user_name.trim().to_lowercase();
    let mut seen = std::collections::HashSet::new();
    let mut people = Vec::new();
    let labels = speaker_labels.iter().flat_map(|label| label_parts(label));
    for name in typed.iter().map(String::as_str).chain(labels).chain(invitees.iter().map(String::as_str)) {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        let key = name.to_lowercase();
        if name.is_empty() || is_user_label(&name) || is_generated_label(&name) || (!user.is_empty() && key == user) {
            continue;
        }
        if seen.insert(key) {
            people.push(name);
        }
    }
    people
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Folder {
    /// "Meetily meetings/"
    Root,
    /// "Meetily meetings/<person>/": exactly one other person is known.
    Person(String),
    /// "Meetily meetings/<group>/": the meeting belongs to a group.
    Group(String),
    /// "Meetily meetings/Group meetings/": several people, no group.
    GroupMeetings,
}

pub const GROUP_MEETINGS_FOLDER: &str = "Group meetings";

/// Where a meeting's files go.
///
/// With one other person the folder carries the name. With several, a folder
/// per combination of people would scatter the files, so the meeting's group
/// is used, or one shared folder with the names in the file title. With
/// `per_person` off, everything stays in the root folder.
pub fn choose_folder(people: &[String], group: Option<&str>, per_person: bool) -> Folder {
    if !per_person {
        return Folder::Root;
    }
    let group = group.map(|name| safe_name(name, MAX_NAME_CHARS)).filter(|name| !name.is_empty());
    match (people, group) {
        ([person], _) => match safe_name(person, MAX_NAME_CHARS) {
            name if name.is_empty() => Folder::Root,
            name => Folder::Person(name),
        },
        (_, Some(group)) => Folder::Group(group),
        ([], None) => Folder::Root,
        (_, None) => Folder::GroupMeetings,
    }
}

/// "2026-11-23 1430 Budget review" or, when the folder does not already say
/// who was there, "2026-11-23 1430 Budget review (Alice, Bob)".
///
/// The date and time lead so that files sort by when the meeting happened.
pub fn file_stem(title: &str, started_local: NaiveDateTime, people: &[String], folder: &Folder) -> String {
    let automatic = crate::database::repositories::transcript::is_default_meeting_title(title);
    let title = if title.trim().is_empty() || automatic {
        // The automatic title repeats the date and time that already lead the name.
        "Meeting".to_string()
    } else {
        safe_name(title, MAX_TITLE_CHARS)
    };
    let title = if title.is_empty() { "Meeting".to_string() } else { title };
    let names_in_title = match folder {
        Folder::Person(_) => false,
        Folder::Root | Folder::Group(_) | Folder::GroupMeetings => !people.is_empty(),
    };
    let mut stem = format!("{} {title}", started_local.format("%Y-%m-%d %H%M"));
    if names_in_title {
        let shown: Vec<String> = people.iter().take(4).map(|name| safe_name(name, 30)).filter(|name| !name.is_empty()).collect();
        let more = people.len().saturating_sub(4);
        if !shown.is_empty() {
            stem.push_str(&format!(" ({}{})", shown.join(", "), if more > 0 { format!(" +{more}") } else { String::new() }));
        }
    }
    stem
}

/// A stable key for a folder's private tag: the same person or group maps to
/// the same folder whatever the capitalisation. Hashed, because Drive limits a
/// property to 124 bytes and names can be long or non-ASCII.
pub fn folder_key(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let normalized = name.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    Sha256::digest(normalized.as_bytes()).iter().take(16).map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap()
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn names_are_safe_on_every_system() {
        assert_eq!(safe_name("  Q3/Q4: plan? \"final\" <v2> | notes*  ", 100), "Q3 Q4 plan final v2 notes");
        assert_eq!(safe_name("..hidden..", 100), "hidden");
        assert_eq!(safe_name("tab\there\nnewline", 100), "tab here newline");
        assert_eq!(safe_name("Zoë — 会議", 100), "Zoë — 会議");
        assert_eq!(safe_name("abcdef", 3), "abc");
        assert_eq!(safe_name("///", 100), "");
    }

    #[test]
    fn people_come_from_typed_names_then_named_voices_then_invitees() {
        let people = known_people(
            &names(&["Alice Smith", "bob"]),
            &names(&["You", "Speaker 1", "You + Carol", "ALICE SMITH", "Guest", "Rob (You)"]),
            &names(&["Dana", "Bob", "Rob"]),
            "Rob",
        );
        assert_eq!(people, ["Alice Smith", "bob", "Carol", "Dana"]);
        // Unnamed voices alone are nobody.
        assert!(known_people(&[], &names(&["You", "Speaker 1", "Speaker 12", "Guest"]), &[], "").is_empty());
        // "Speaker Smith" is a name, not a generated label.
        assert_eq!(known_people(&[], &names(&["Speaker Smith"]), &[], ""), ["Speaker Smith"]);
    }

    #[test]
    fn one_person_gets_a_folder_and_several_share_one() {
        assert_eq!(choose_folder(&names(&["Alice Smith"]), None, true), Folder::Person("Alice Smith".into()));
        // The person wins over the group: the folder is named after who was on the call.
        assert_eq!(choose_folder(&names(&["Alice"]), Some("Weekly"), true), Folder::Person("Alice".into()));
        assert_eq!(choose_folder(&names(&["Alice", "Bob"]), Some("Weekly: sync"), true), Folder::Group("Weekly sync".into()));
        assert_eq!(choose_folder(&names(&["Alice", "Bob"]), None, true), Folder::GroupMeetings);
        assert_eq!(choose_folder(&[], Some("Weekly"), true), Folder::Group("Weekly".into()));
        assert_eq!(choose_folder(&[], None, true), Folder::Root);
        assert_eq!(choose_folder(&[], Some("  "), true), Folder::Root);
        // A name that is nothing but reserved characters cannot name a folder.
        assert_eq!(choose_folder(&names(&["///"]), None, true), Folder::Root);
        // One folder for everything when folders per person are switched off.
        assert_eq!(choose_folder(&names(&["Alice"]), Some("Weekly"), false), Folder::Root);
    }

    #[test]
    fn file_names_lead_with_date_and_time_and_name_people_the_folder_does_not() {
        let when = at("2026-11-23 14:30");
        let alice = names(&["Alice Smith"]);
        assert_eq!(file_stem("Budget review", when, &alice, &Folder::Person("Alice Smith".into())), "2026-11-23 1430 Budget review");
        assert_eq!(file_stem("Budget review", when, &alice, &Folder::Root), "2026-11-23 1430 Budget review (Alice Smith)");
        assert_eq!(
            file_stem("Budget: review?", when, &names(&["Alice", "Bob", "Carol", "Dana", "Eve", "Finn"]), &Folder::GroupMeetings),
            "2026-11-23 1430 Budget review (Alice, Bob, Carol, Dana +2)"
        );
        assert_eq!(file_stem("Budget review", when, &[], &Folder::Root), "2026-11-23 1430 Budget review");
        // The automatic title would repeat the date; an empty one is replaced too.
        assert_eq!(file_stem("Meeting · Mon, Nov 23 · 2:30–3:15 PM", when, &[], &Folder::Root), "2026-11-23 1430 Meeting");
        assert_eq!(file_stem("  ", at("2026-01-05 09:05"), &[], &Folder::Root), "2026-01-05 0905 Meeting");
        assert_eq!(file_stem("???", when, &[], &Folder::Root), "2026-11-23 1430 Meeting");
    }

    #[test]
    fn speakers_are_written_as_on_the_meeting_page() {
        assert_eq!(display_speaker("You", "Rob"), "Rob (You)");
        assert_eq!(display_speaker("You", " "), "You");
        assert_eq!(display_speaker("You + Alice Smith", "Rob"), "Rob (You) + Alice Smith");
        assert_eq!(display_speaker("Speaker 2", "Rob"), "Speaker 2");
    }

    #[test]
    fn folder_keys_ignore_case_and_spacing_and_fit_a_drive_property() {
        assert_eq!(folder_key("Alice  Smith"), folder_key("alice smith"));
        assert_ne!(folder_key("Alice Smith"), folder_key("Alice Smyth"));
        assert_eq!(folder_key(&"长".repeat(500)).len(), 32);
    }
}
