use crate::{model::ServiceEvent, text::normalize_ws};
use serde_json::json;

#[derive(Debug, Clone)]
pub struct ClassifiedServiceEvent {
    pub event_type: String,
    pub actor_name: Option<String>,
    pub target_names: Vec<String>,
    pub display_text: String,
    pub extra_json: serde_json::Value,
}

pub fn classify_service_event(text: &str) -> Option<ClassifiedServiceEvent> {
    let text = normalize_ws(text);

    if let Some((actor, target)) = text.split_once(" invited ") {
        return Some(ClassifiedServiceEvent {
            event_type: "invite_members".to_string(),
            actor_name: Some(actor.to_string()),
            target_names: vec![target.to_string()],
            display_text: text,
            extra_json: json!({ "action": "invite_members" }),
        });
    }

    if let Some((actor, target)) = text.split_once(" removed ") {
        return Some(ClassifiedServiceEvent {
            event_type: "remove_members".to_string(),
            actor_name: Some(actor.to_string()),
            target_names: vec![target.to_string()],
            display_text: text,
            extra_json: json!({ "action": "remove_members" }),
        });
    }

    if let Some(event) =
        classify_json_membership_fallback(&text, "invite_members", "invite members")
    {
        return Some(event);
    }

    if let Some(event) =
        classify_json_membership_fallback(&text, "remove_members", "remove members")
    {
        return Some(event);
    }

    if let Some(event) =
        classify_json_actor_action_fallback(&text, "edit_group_title", "edit group title")
    {
        return Some(event);
    }

    if let Some(event) =
        classify_json_actor_action_fallback(&text, "migrate_from_group", "migrate from group")
    {
        return Some(event);
    }

    if let Some(event) = classify_json_actor_action_fallback(&text, "pin_message", "pin message") {
        return Some(event);
    }

    if let Some(event) = classify_json_actor_action_fallback(&text, "group_call", "group call") {
        return Some(event);
    }

    if let Some((actor, title)) = text.split_once(" changed group title to «")
        && let Some(clean_title) = title
            .strip_suffix('»')
            .map(str::trim)
            .filter(|title| !title.is_empty())
    {
        let clean_title = clean_title.to_string();
        return Some(ClassifiedServiceEvent {
            event_type: "edit_group_title".to_string(),
            actor_name: Some(actor.to_string()),
            target_names: Vec::new(),
            display_text: text,
            extra_json: json!({ "action": "edit_group_title", "title": clean_title }),
        });
    }

    if let Some((actor, title)) = text.split_once(" converted a basic group to this supergroup «")
        && let Some(clean_title) = title
            .strip_suffix('»')
            .map(str::trim)
            .filter(|title| !title.is_empty())
    {
        let clean_title = clean_title.to_string();
        return Some(ClassifiedServiceEvent {
            event_type: "migrate_from_group".to_string(),
            actor_name: Some(actor.to_string()),
            target_names: Vec::new(),
            display_text: text,
            extra_json: json!({ "action": "migrate_from_group", "title": clean_title }),
        });
    }

    if let Some(actor) = text
        .strip_suffix(" pinned this message")
        .filter(|actor| !actor.is_empty())
    {
        let actor = actor.to_string();
        return Some(ClassifiedServiceEvent {
            event_type: "pin_message".to_string(),
            actor_name: (!actor.is_empty()).then_some(actor),
            target_names: Vec::new(),
            display_text: text,
            extra_json: json!({ "action": "pin_message" }),
        });
    }

    if let Some(actor) = text
        .strip_suffix(" started voice chat")
        .filter(|actor| !actor.is_empty())
    {
        return Some(ClassifiedServiceEvent {
            event_type: "group_call".to_string(),
            actor_name: Some(actor.to_string()),
            target_names: Vec::new(),
            display_text: text,
            extra_json: json!({ "action": "group_call" }),
        });
    }

    if text == "Voice chat" {
        return Some(ClassifiedServiceEvent {
            event_type: "group_call".to_string(),
            actor_name: None,
            target_names: Vec::new(),
            display_text: text,
            extra_json: json!({ "action": "group_call" }),
        });
    }

    None
}

fn classify_json_actor_action_fallback(
    text: &str,
    event_type: &str,
    action: &str,
) -> Option<ClassifiedServiceEvent> {
    let actor_suffix = format!(" {action}");
    let extra_json = json!({ "action": event_type });

    if text == action {
        return Some(ClassifiedServiceEvent {
            event_type: event_type.to_string(),
            actor_name: None,
            target_names: Vec::new(),
            display_text: text.to_string(),
            extra_json,
        });
    }

    if let Some(actor) = text
        .strip_suffix(&actor_suffix)
        .map(str::trim)
        .filter(|actor| !actor.is_empty())
    {
        return Some(ClassifiedServiceEvent {
            event_type: event_type.to_string(),
            actor_name: Some(actor.to_string()),
            target_names: Vec::new(),
            display_text: text.to_string(),
            extra_json,
        });
    }

    None
}

fn classify_json_membership_fallback(
    text: &str,
    event_type: &str,
    action: &str,
) -> Option<ClassifiedServiceEvent> {
    let actor_suffix = format!(" {action}");
    let actor_target_separator = format!(" {action}: ");
    let extra_json = json!({ "action": event_type });

    if text == action {
        return Some(ClassifiedServiceEvent {
            event_type: event_type.to_string(),
            actor_name: None,
            target_names: Vec::new(),
            display_text: text.to_string(),
            extra_json,
        });
    }

    if let Some(target) = text
        .strip_prefix(&format!("{action}: "))
        .map(str::trim)
        .filter(|target| !target.is_empty())
    {
        return Some(ClassifiedServiceEvent {
            event_type: event_type.to_string(),
            actor_name: None,
            target_names: vec![target.to_string()],
            display_text: text.to_string(),
            extra_json,
        });
    }

    if let Some((actor, target)) = text.split_once(&actor_target_separator) {
        let actor = actor.trim();
        let target = target.trim();
        if !actor.is_empty() && !target.is_empty() {
            return Some(ClassifiedServiceEvent {
                event_type: event_type.to_string(),
                actor_name: Some(actor.to_string()),
                target_names: vec![target.to_string()],
                display_text: text.to_string(),
                extra_json,
            });
        }
    }

    if let Some(actor) = text
        .strip_suffix(&actor_suffix)
        .map(str::trim)
        .filter(|actor| !actor.is_empty())
    {
        return Some(ClassifiedServiceEvent {
            event_type: event_type.to_string(),
            actor_name: Some(actor.to_string()),
            target_names: Vec::new(),
            display_text: text.to_string(),
            extra_json,
        });
    }

    None
}

impl ClassifiedServiceEvent {
    pub fn into_model(self, timeline_ordinal: i64) -> ServiceEvent {
        ServiceEvent {
            timeline_ordinal,
            event_type: self.event_type,
            actor_name: self.actor_name,
            target_names: self.target_names,
            display_text: self.display_text,
            extra_json: self.extra_json,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_known_telegram_service_text() {
        assert_eq!(
            classify_service_event("Alice invited Bob")
                .unwrap()
                .event_type,
            "invite_members"
        );
        assert_eq!(
            classify_service_event("Bob changed group title to «Family Archive»")
                .unwrap()
                .event_type,
            "edit_group_title"
        );
    }

    #[test]
    fn leaves_unknown_service_text_unclassified() {
        assert!(classify_service_event("Unknown service payload from future Telegram").is_none());
    }

    #[test]
    fn classifies_actorless_voice_chat_without_actor_name() {
        assert_eq!(
            classify_service_event("Voice chat").unwrap().actor_name,
            None
        );
    }

    #[test]
    fn classifies_basic_group_to_supergroup_migration() {
        let event = classify_service_event(
            "Shannon modem shenanigans converted a basic group to this supergroup «Shannon modem shenanigans»",
        )
        .unwrap();

        assert_eq!(event.event_type, "migrate_from_group");
        assert_eq!(
            event.actor_name.as_deref(),
            Some("Shannon modem shenanigans")
        );
        assert_eq!(event.extra_json["action"], "migrate_from_group");
        assert_eq!(event.extra_json["title"], "Shannon modem shenanigans");
    }

    #[test]
    fn rejects_malformed_empty_group_title_change() {
        assert!(classify_service_event("Alice changed group title to «").is_none());
    }

    #[test]
    fn requires_exact_pinned_message_phrase() {
        assert!(classify_service_event("Alice pinned a note about this message").is_none());
    }

    #[test]
    fn rejects_pinned_message_with_trailing_text_or_missing_actor() {
        assert!(classify_service_event("Alice pinned this message again").is_none());
        assert!(classify_service_event("Alice pinned this messageboard").is_none());
        assert!(classify_service_event(" pinned this message").is_none());
    }

    #[test]
    fn rejects_unknown_voice_chat_text() {
        assert!(classify_service_event("Alice started voice chatting").is_none());
        assert!(classify_service_event("Voice chat scheduled").is_none());
    }

    #[test]
    fn rejects_whitespace_only_group_title_change() {
        assert!(classify_service_event("Alice changed group title to «    »").is_none());
    }

    #[test]
    fn preserves_invite_title_and_model_payloads() {
        let invite = classify_service_event("Alice invited Bob").unwrap();
        assert_eq!(invite.event_type, "invite_members");
        assert_eq!(invite.actor_name, Some("Alice".to_string()));
        assert_eq!(invite.target_names, vec!["Bob".to_string()]);
        assert_eq!(invite.display_text, "Alice invited Bob");
        assert_eq!(invite.extra_json, json!({ "action": "invite_members" }));

        let title =
            classify_service_event("Bob changed group title to «  Family Archive  »").unwrap();
        assert_eq!(title.event_type, "edit_group_title");
        assert_eq!(title.actor_name, Some("Bob".to_string()));
        assert_eq!(title.target_names, Vec::<String>::new());
        assert_eq!(
            title.extra_json,
            json!({ "action": "edit_group_title", "title": "Family Archive" })
        );

        let model = title.into_model(42);
        assert_eq!(model.timeline_ordinal, 42);
        assert_eq!(model.event_type, "edit_group_title");
        assert_eq!(model.actor_name, Some("Bob".to_string()));
        assert_eq!(
            model.display_text,
            "Bob changed group title to « Family Archive »"
        );
        assert_eq!(
            model.extra_json,
            json!({ "action": "edit_group_title", "title": "Family Archive" })
        );
    }

    #[test]
    fn classifies_json_fallback_membership_display_text() {
        let actorless_invite = classify_service_event("invite members").unwrap();
        assert_eq!(actorless_invite.event_type, "invite_members");
        assert_eq!(actorless_invite.actor_name, None);
        assert!(actorless_invite.target_names.is_empty());

        let actorless_invite_target = classify_service_event("invite members: Yuhfhrh").unwrap();
        assert_eq!(actorless_invite_target.event_type, "invite_members");
        assert_eq!(actorless_invite_target.actor_name, None);
        assert_eq!(actorless_invite_target.target_names, vec!["Yuhfhrh"]);

        let actor_remove_unknown = classify_service_event("Amelié remove members").unwrap();
        assert_eq!(actor_remove_unknown.event_type, "remove_members");
        assert_eq!(actor_remove_unknown.actor_name.as_deref(), Some("Amelié"));
        assert!(actor_remove_unknown.target_names.is_empty());

        let actorless_remove = classify_service_event("remove members").unwrap();
        assert_eq!(actorless_remove.event_type, "remove_members");
        assert_eq!(actorless_remove.actor_name, None);
        assert!(actorless_remove.target_names.is_empty());
    }

    #[test]
    fn classifies_json_fallback_non_membership_service_display_text() {
        let actor_title = classify_service_event("Alice edit group title").unwrap();
        assert_eq!(actor_title.event_type, "edit_group_title");
        assert_eq!(actor_title.actor_name.as_deref(), Some("Alice"));
        assert!(actor_title.target_names.is_empty());

        let actorless_title = classify_service_event("edit group title").unwrap();
        assert_eq!(actorless_title.event_type, "edit_group_title");
        assert_eq!(actorless_title.actor_name, None);
        assert!(actorless_title.target_names.is_empty());

        let actor_migration = classify_service_event("Family Chat migrate from group").unwrap();
        assert_eq!(actor_migration.event_type, "migrate_from_group");
        assert_eq!(actor_migration.actor_name.as_deref(), Some("Family Chat"));
        assert!(actor_migration.target_names.is_empty());

        let actorless_migration = classify_service_event("migrate from group").unwrap();
        assert_eq!(actorless_migration.event_type, "migrate_from_group");
        assert_eq!(actorless_migration.actor_name, None);
        assert!(actorless_migration.target_names.is_empty());

        let actor_pin = classify_service_event("Alice pin message").unwrap();
        assert_eq!(actor_pin.event_type, "pin_message");
        assert_eq!(actor_pin.actor_name.as_deref(), Some("Alice"));
        assert!(actor_pin.target_names.is_empty());

        let actorless_pin = classify_service_event("pin message").unwrap();
        assert_eq!(actorless_pin.event_type, "pin_message");
        assert_eq!(actorless_pin.actor_name, None);
        assert!(actorless_pin.target_names.is_empty());
    }
}
