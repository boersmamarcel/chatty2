//! The `/conversations` list (HX-19): one numbered line per conversation with
//! where it runs and, for a cloud conversation, what its turn is doing.
//! Pure, like `plan.rs` and `swarm.rs`: rows in, text out. The marks use the
//! desktop sidebar's words (`CloudMark::label`).

use std::collections::HashMap;

use chatty_core::models::CloudMark;
use chatty_core::repositories::ConversationMetadata;
use chatty_core::services::hive_sync::TurnStatus;

/// One conversation as the list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub title: String,
    pub place: Place,
    pub mark: Option<CloudMark>,
}

/// Where a conversation's turns run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Never leaves this machine.
    Local,
    /// Kept on the server, turns run on this device.
    Synced,
    /// Kept on the server, turns run there.
    Cloud,
}

impl Place {
    fn label(self) -> &'static str {
        match self {
            Place::Local => "local",
            Place::Synced => "synced",
            Place::Cloud => "cloud",
        }
    }
}

/// The mark a cloud conversation's last-seen turn status earns.
pub fn mark_of(status: TurnStatus) -> Option<CloudMark> {
    match status {
        TurnStatus::Running => Some(CloudMark::Running),
        TurnStatus::WaitingForYou => Some(CloudMark::WaitingForYou),
        TurnStatus::Interrupted => Some(CloudMark::Interrupted),
        TurnStatus::Idle | TurnStatus::Unknown => None,
    }
}

/// The rows of `metadata`, in its order (newest first), marked from `status`.
pub fn entries(
    metadata: &[ConversationMetadata],
    status: &HashMap<String, TurnStatus>,
) -> Vec<Entry> {
    metadata
        .iter()
        .map(|row| {
            let place = if row.sync.runs_in_cloud() {
                Place::Cloud
            } else if row.sync.synced().is_some() {
                Place::Synced
            } else {
                Place::Local
            };
            let mark = match place {
                Place::Cloud => status.get(&row.id).copied().and_then(mark_of),
                _ => None,
            };
            Entry {
                id: row.id.clone(),
                title: row.title.clone(),
                place,
                mark,
            }
        })
        .collect()
}

/// The list as a system message; `current` is the open conversation's id.
pub fn render(entries: &[Entry], current: Option<&str>) -> String {
    if entries.is_empty() {
        return "No saved conversations yet.".to_string();
    }
    let mut out = String::from("Conversations (/open <number> opens one):\n");
    for (ix, entry) in entries.iter().enumerate() {
        let marker = if Some(entry.id.as_str()) == current {
            '*'
        } else {
            ' '
        };
        let title = if entry.title.is_empty() {
            "Untitled"
        } else {
            entry.title.as_str()
        };
        out.push_str(&format!(
            "{marker}{:>3}. {title}  [{}]",
            ix + 1,
            entry.place.label()
        ));
        if let Some(mark) = entry.mark {
            out.push_str(&format!("  {}", mark.label()));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// The conversation `arg` names: its number in the last list, its id, or a
/// unique start of its id.
pub fn resolve(ids: &[String], arg: &str) -> Result<String, String> {
    let arg = arg.trim();
    if let Ok(number) = arg.parse::<usize>() {
        return number
            .checked_sub(1)
            .and_then(|ix| ids.get(ix))
            .cloned()
            .ok_or_else(|| {
                format!("No conversation numbered {number} (/conversations lists them).")
            });
    }
    if let Some(id) = ids.iter().find(|id| id.as_str() == arg) {
        return Ok(id.clone());
    }
    let mut matches = ids
        .iter()
        .filter(|id| !arg.is_empty() && id.starts_with(arg));
    match (matches.next(), matches.next()) {
        (Some(id), None) => Ok(id.clone()),
        (Some(_), Some(_)) => Err(format!("More than one conversation starts with {arg:?}.")),
        _ => Err(format!(
            "No conversation {arg:?} (/conversations lists them)."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chatty_core::models::conversation::{ConversationSync, Placement, SyncedConversation};

    fn row(id: &str, title: &str, sync: ConversationSync) -> ConversationMetadata {
        ConversationMetadata {
            id: id.into(),
            title: title.into(),
            total_cost: 0.0,
            updated_at: 0,
            sync,
            tool_call_count: 0,
            context_tokens: 0,
        }
    }

    fn synced(placement: Placement) -> ConversationSync {
        ConversationSync::Synced(SyncedConversation {
            server_url: "https://hive.example".into(),
            remote_id: "r".into(),
            version: 1,
            placement,
            last_synced_at: Some(1),
        })
    }

    #[test]
    fn rows_say_where_they_run_and_what_a_cloud_turn_is_doing() {
        let metadata = [
            row("a", "Local chat", ConversationSync::LocalOnly),
            row("b", "Research", synced(Placement::Cloud { model_id: None })),
            row("c", "Waiting", synced(Placement::Cloud { model_id: None })),
            row(
                "d",
                "",
                synced(Placement::Device {
                    device_id: "x".into(),
                }),
            ),
        ];
        let status = HashMap::from([
            ("b".to_string(), TurnStatus::Running),
            ("c".to_string(), TurnStatus::WaitingForYou),
        ]);
        let text = render(&entries(&metadata, &status), Some("a"));
        assert_eq!(
            text,
            "Conversations (/open <number> opens one):\n\
             *  1. Local chat  [local]\n\
             \x20  2. Research  [cloud]  Running in the cloud\n\
             \x20  3. Waiting  [cloud]  Waiting for you\n\
             \x20  4. Untitled  [synced]"
        );
    }

    #[test]
    fn an_empty_list_says_so() {
        assert_eq!(render(&[], None), "No saved conversations yet.");
    }

    #[test]
    fn open_takes_a_number_an_id_or_a_unique_prefix() {
        let ids = vec!["abc-1".to_string(), "abd-2".to_string()];
        assert_eq!(resolve(&ids, "2").unwrap(), "abd-2");
        assert_eq!(resolve(&ids, "abc-1").unwrap(), "abc-1");
        assert_eq!(resolve(&ids, "abc").unwrap(), "abc-1");
        assert!(resolve(&ids, "ab").unwrap_err().contains("More than one"));
        assert!(
            resolve(&ids, "3")
                .unwrap_err()
                .contains("No conversation numbered 3")
        );
        assert!(resolve(&ids, "0").unwrap_err().contains("numbered 0"));
        assert!(
            resolve(&ids, "zzz")
                .unwrap_err()
                .contains("No conversation")
        );
    }
}
