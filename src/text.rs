use crate::model::{TextEntity, TextEntityKind};
use scraper::{ElementRef, node::Node};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone)]
pub struct RichText {
    pub plain: String,
    pub entities: Vec<TextEntity>,
}

#[derive(Debug, Clone)]
struct EntityMark {
    kind: TextEntityKind,
    extra: Value,
}

pub fn extract_rich_text(element: ElementRef<'_>) -> RichText {
    let plain = text_from_element(element);
    let mut entities = Vec::new();
    let mut active = Vec::new();
    collect_nodes(element, &mut entities, &mut active);

    RichText { plain, entities }
}

pub fn normalize_ws(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn collect_nodes(
    element: ElementRef<'_>,
    entities: &mut Vec<TextEntity>,
    active: &mut Vec<EntityMark>,
) {
    for child in element.children() {
        match child.value() {
            Node::Text(text) => {
                let value: &str = text.as_ref();
                push_text_entities(value, entities, active);
            }
            Node::Element(_) => {
                if let Some(child_element) = ElementRef::wrap(child) {
                    if child_element.value().name() == "br" {
                        push_text_entities("\n", entities, active);
                    } else if let Some(mark) = classify_element(child_element) {
                        active.push(mark);
                        collect_nodes(child_element, entities, active);
                        active.pop();
                    } else {
                        collect_nodes(child_element, entities, active);
                    }
                }
            }
            _ => {}
        }
    }
}

fn push_text_entities(value: &str, entities: &mut Vec<TextEntity>, active: &[EntityMark]) {
    if value.is_empty() {
        return;
    }

    if active.is_empty() {
        entities.push(TextEntity {
            kind: TextEntityKind::Text,
            text: value.to_string(),
            extra: json!({}),
        });
        return;
    }

    for mark in active {
        entities.push(TextEntity {
            kind: mark.kind.clone(),
            text: value.to_string(),
            extra: mark.extra.clone(),
        });
    }
}

fn classify_element(element: ElementRef<'_>) -> Option<EntityMark> {
    let name = element.value().name();
    let classes = element.value().attr("class").unwrap_or("");
    match name {
        "strong" | "b" => Some(entity_mark(TextEntityKind::Bold, json!({}))),
        "em" | "i" => Some(entity_mark(TextEntityKind::Italic, json!({}))),
        "code" => Some(entity_mark(TextEntityKind::Code, json!({}))),
        "pre" => Some(entity_mark(TextEntityKind::Pre, json!({}))),
        "u" => Some(entity_mark(TextEntityKind::Underline, json!({}))),
        "s" => Some(entity_mark(TextEntityKind::Strike, json!({}))),
        "blockquote" => Some(entity_mark(TextEntityKind::Blockquote, json!({}))),
        "a" => {
            let href = element.value().attr("href").unwrap_or("");
            let user_id = element
                .value()
                .attr("data-user-id")
                .map(str::to_string)
                .or_else(|| user_id_from_href(href));
            let extra = anchor_extra(href, user_id.as_deref());
            if href.starts_with("mailto:") {
                Some(entity_mark(TextEntityKind::Email, extra))
            } else if href.starts_with("tel:") {
                Some(entity_mark(TextEntityKind::Phone, extra))
            } else if href.is_empty() || user_id.is_some() {
                Some(entity_mark(TextEntityKind::MentionName, extra))
            } else {
                Some(entity_mark(TextEntityKind::TextUrl, extra))
            }
        }
        "span" if has_class(classes, "spoiler") => {
            Some(entity_mark(TextEntityKind::Spoiler, json!({})))
        }
        _ => None,
    }
}

fn entity_mark(kind: TextEntityKind, extra: Value) -> EntityMark {
    EntityMark { kind, extra }
}

fn anchor_extra(href: &str, user_id: Option<&str>) -> Value {
    let mut extra = Map::new();
    if !href.is_empty() {
        extra.insert("href".to_string(), json!(href));
    }
    if let Some(user_id) = user_id.filter(|user_id| !user_id.is_empty()) {
        extra.insert("user_id".to_string(), json!(user_id));
    }
    Value::Object(extra)
}

fn user_id_from_href(href: &str) -> Option<String> {
    let value = href.strip_prefix("tg://user?id=")?;
    let user_id = value.split('&').next().unwrap_or(value);
    (!user_id.is_empty()).then(|| user_id.to_string())
}

fn has_class(classes: &str, name: &str) -> bool {
    classes.split_whitespace().any(|class| class == name)
}

pub fn text_from_element(element: ElementRef<'_>) -> String {
    let mut text = String::new();
    collect_plain_text(element, &mut text);
    normalize_ws(&text)
}

fn collect_plain_text(element: ElementRef<'_>, text: &mut String) {
    for child in element.children() {
        match child.value() {
            Node::Text(value) => text.push_str(value.as_ref()),
            Node::Element(_) => {
                if let Some(child_element) = ElementRef::wrap(child) {
                    if child_element.value().name() == "br" {
                        text.push('\n');
                    } else {
                        collect_plain_text(child_element, text);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use scraper::{Html, Selector};
    use serde_json::{Value, json};

    use super::*;
    use crate::model::TextEntityKind;

    fn assert_has_entity(rich: &RichText, kind: TextEntityKind, text: &str, extra: Value) {
        assert!(
            rich.entities
                .iter()
                .any(|entity| entity.kind == kind && entity.text == text && entity.extra == extra),
            "missing {kind:?} entity with text {text:?} and extra {extra:?}; entities: {:?}",
            rich.entities
        );
    }

    #[test]
    fn extracts_plain_text_and_entities() {
        let doc = Html::parse_fragment(
            r#"<div class="text">Hello <strong>family</strong> <a href="https://example.com">link</a></div>"#,
        );
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        let rich = extract_rich_text(root);

        assert_eq!(rich.plain, "Hello family link");
        assert_eq!(rich.entities.len(), 4);
        assert_eq!(rich.entities[1].kind, TextEntityKind::Bold);
        assert_eq!(rich.entities[3].kind, TextEntityKind::TextUrl);
        assert_eq!(
            rich.entities[3].extra,
            json!({ "href": "https://example.com" })
        );
    }

    #[test]
    fn text_from_element_does_not_insert_spaces_between_adjacent_nodes() {
        let doc = Html::parse_fragment(r#"<div class="text">foo<strong>bar</strong></div>"#);
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        assert_eq!(text_from_element(root), "foobar");
    }

    #[test]
    fn text_from_element_normalizes_source_whitespace() {
        let doc = Html::parse_fragment(
            r#"<div class="text">foo
                <strong>bar</strong>   baz</div>"#,
        );
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        assert_eq!(text_from_element(root), "foo bar baz");
    }

    #[test]
    fn extract_rich_text_preserves_whitespace_entities() {
        let doc = Html::parse_fragment(
            r#"<div class="text">Hello <strong>family</strong> <a href="https://example.com">link</a></div>"#,
        );
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        let rich = extract_rich_text(root);
        let entity_texts: Vec<&str> = rich
            .entities
            .iter()
            .map(|entity| entity.text.as_str())
            .collect();
        let reconstructed: String = rich
            .entities
            .iter()
            .map(|entity| entity.text.as_str())
            .collect();

        assert_eq!(entity_texts, vec!["Hello ", "family", " ", "link"]);
        assert_eq!(rich.entities[2].kind, TextEntityKind::Text);
        assert_eq!(normalize_ws(&reconstructed), rich.plain);
    }

    #[test]
    fn text_from_element_normalizes_br_to_space() {
        let doc = Html::parse_fragment(r#"<div class="text">foo<br>bar</div>"#);
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        assert_eq!(text_from_element(root), "foo bar");
    }

    #[test]
    fn extract_rich_text_normalizes_plain_but_preserves_br_entity() {
        let doc = Html::parse_fragment(r#"<div class="text">foo<br>bar</div>"#);
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        let rich = extract_rich_text(root);
        let reconstructed: String = rich
            .entities
            .iter()
            .map(|entity| entity.text.as_str())
            .collect();

        assert_eq!(rich.plain, "foo bar");
        assert_eq!(reconstructed, "foo\nbar");
        assert_eq!(rich.entities[1].kind, TextEntityKind::Text);
        assert_eq!(rich.entities[1].text, "\n");
    }

    #[test]
    fn nested_link_then_bold_preserves_href_and_formatting() {
        let doc = Html::parse_fragment(
            r#"<div class="text"><a href="https://example.com"><strong>link</strong></a></div>"#,
        );
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        let rich = extract_rich_text(root);

        assert_eq!(rich.plain, "link");
        assert_has_entity(
            &rich,
            TextEntityKind::TextUrl,
            "link",
            json!({ "href": "https://example.com" }),
        );
        assert_has_entity(&rich, TextEntityKind::Bold, "link", json!({}));
    }

    #[test]
    fn nested_bold_then_link_preserves_href_and_formatting() {
        let doc = Html::parse_fragment(
            r#"<div class="text"><strong><a href="https://example.com">link</a></strong></div>"#,
        );
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        let rich = extract_rich_text(root);

        assert_eq!(rich.plain, "link");
        assert_has_entity(
            &rich,
            TextEntityKind::TextUrl,
            "link",
            json!({ "href": "https://example.com" }),
        );
        assert_has_entity(&rich, TextEntityKind::Bold, "link", json!({}));
    }

    #[test]
    fn mailto_and_tel_entities_preserve_href() {
        let doc = Html::parse_fragment(
            r#"<div class="text"><a href="mailto:a@example.com">email</a> <a href="tel:+123">phone</a></div>"#,
        );
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        let rich = extract_rich_text(root);

        assert_has_entity(
            &rich,
            TextEntityKind::Email,
            "email",
            json!({ "href": "mailto:a@example.com" }),
        );
        assert_has_entity(
            &rich,
            TextEntityKind::Phone,
            "phone",
            json!({ "href": "tel:+123" }),
        );
    }

    #[test]
    fn mention_name_entities_preserve_user_id_metadata() {
        let doc = Html::parse_fragment(
            r#"<div class="text"><a data-user-id="42">Alice</a> <a href="tg://user?id=99">Bob</a></div>"#,
        );
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        let rich = extract_rich_text(root);

        assert_has_entity(
            &rich,
            TextEntityKind::MentionName,
            "Alice",
            json!({ "user_id": "42" }),
        );
        assert_has_entity(
            &rich,
            TextEntityKind::MentionName,
            "Bob",
            json!({ "href": "tg://user?id=99", "user_id": "99" }),
        );
    }

    #[test]
    fn normal_url_with_user_query_is_not_a_mention_name() {
        let doc = Html::parse_fragment(
            r#"<div class="text"><a href="https://example.com/user?id=99">profile</a></div>"#,
        );
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        let rich = extract_rich_text(root);

        assert_has_entity(
            &rich,
            TextEntityKind::TextUrl,
            "profile",
            json!({ "href": "https://example.com/user?id=99" }),
        );
        assert!(
            rich.entities
                .iter()
                .filter(|entity| entity.text == "profile")
                .all(|entity| entity.extra.get("user_id").is_none()),
            "normal URL should not include user_id metadata: {:?}",
            rich.entities
        );
    }

    #[test]
    fn spoiler_detection_uses_class_tokens() {
        let doc = Html::parse_fragment(
            r#"<div class="text"><span class="notspoiler">plain</span> <span class="spoiler hidden">hidden</span></div>"#,
        );
        let root = doc
            .select(&Selector::parse("div.text").unwrap())
            .next()
            .unwrap();

        let rich = extract_rich_text(root);

        assert_has_entity(&rich, TextEntityKind::Text, "plain", json!({}));
        assert_has_entity(&rich, TextEntityKind::Spoiler, "hidden", json!({}));
    }
}
