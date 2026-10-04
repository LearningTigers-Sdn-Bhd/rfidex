//! The guest's badge fields from EventzFlow's public ticket page.
//!
//! A port of event-printing's `_ticket_payload_from_backend`: the company,
//! title, country and table live in the ticket's free-form answers under a few
//! spellings, and the layout's own custom fields are looked up by key or label.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::layout::{CustomField, Layout, Ticket};

const MAX_TEXT: usize = 200;

/// Plain text from a free-form answer, or `None` when there is nothing to
/// print. A form that stored `{"name": "Acme"}` still prints `Acme`.
fn clean(value: &Value, object_keys: &[&str]) -> Option<String> {
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Object(map) => object_keys
            .iter()
            .find_map(|k| {
                map.get(*k)
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
            })?
            .to_string(),
        _ => return None,
    };
    let text: String = raw
        .replace('\0', "")
        .trim()
        .chars()
        .take(MAX_TEXT)
        .collect();
    (!text.is_empty()).then_some(text)
}

fn field_text(value: &Value) -> Option<String> {
    clean(
        value,
        &["organisation_institution", "name", "company", "label"],
    )
}

fn plain(value: Option<&Value>) -> Option<String> {
    value.and_then(|v| clean(v, &[]))
}

fn custom_values(
    answers: Option<&Map<String, Value>>,
    defs: &BTreeMap<String, CustomField>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(answers) = answers else { return out };
    let lower: BTreeMap<String, &Value> = answers
        .iter()
        .map(|(k, v)| (k.trim().to_lowercase(), v))
        .collect();
    for (id, def) in defs {
        let by_key = (!def.backend_key.is_empty())
            .then(|| answers.get(&def.backend_key))
            .flatten();
        let value = by_key.or_else(|| lower.get(&def.label.trim().to_lowercase()).copied());
        if let Some(text) = value.and_then(|v| clean(v, &["name", "label", "value", "title"])) {
            out.insert(id.clone(), text);
        }
    }
    out
}

/// The badge's fields, or `None` when the page has no ticket id.
pub fn ticket_from_backend(data: &Value, layout: &Layout) -> Option<Ticket> {
    let ticket_id = plain(data.get("public_id"))?;
    let answers = data.get("custom_fields_data").and_then(Value::as_object);
    let pick = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| answers.and_then(|a| a.get(*k)).and_then(field_text))
    };
    Some(Ticket {
        ticket_id,
        name: plain(data.get("attendee_name")),
        company: pick(&[
            "company",
            "organisation_institution",
            "organization",
            "organisation",
        ]),
        title: pick(&["title", "position", "job_title", "designation"]),
        country: pick(&["country"]),
        table_no: pick(&["table_no", "table_number", "table"]),
        ticket_type: plain(data.get("ticket_type")).unwrap_or_else(|| "Visitor".to_string()),
        role: plain(data.get("role")),
        custom: custom_values(answers, &layout.custom_fields),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn layout_with_custom() -> Layout {
        let mut layout = Layout::default();
        layout.custom_fields.insert(
            "custom_aaaaaa".into(),
            CustomField {
                label: "Sponsor".into(),
                backend_key: "sponsor_name".into(),
            },
        );
        layout.custom_fields.insert(
            "custom_bbbbbb".into(),
            CustomField {
                label: "Dietary".into(),
                backend_key: String::new(),
            },
        );
        layout
    }

    #[test]
    fn answers_under_any_known_spelling_reach_the_badge() {
        let data = json!({
            "public_id": "abc-1", "attendee_name": "  Aina  ", "ticket_type": "VIP",
            "role": "Speaker",
            "custom_fields_data": {
                "organisation_institution": {"organisation_institution": "Borneo Expo"},
                "position": "CEO", "country": "Malaysia", "table_number": 12
            }
        });
        let ticket = ticket_from_backend(&data, &Layout::default()).unwrap();
        assert_eq!(ticket.ticket_id, "abc-1");
        assert_eq!(ticket.name.as_deref(), Some("Aina"));
        assert_eq!(ticket.company.as_deref(), Some("Borneo Expo"));
        assert_eq!(ticket.title.as_deref(), Some("CEO"));
        assert_eq!(ticket.country.as_deref(), Some("Malaysia"));
        assert_eq!(ticket.table_no.as_deref(), Some("12"));
        assert_eq!(ticket.ticket_type, "VIP");
        assert_eq!(ticket.role.as_deref(), Some("Speaker"));
    }

    #[test]
    fn custom_fields_match_by_key_then_by_label_ignoring_case() {
        let data = json!({
            "public_id": "abc-1",
            "custom_fields_data": {"sponsor_name": {"name": "Acme"}, " DIETARY ": "Halal"}
        });
        let ticket = ticket_from_backend(&data, &layout_with_custom()).unwrap();
        assert_eq!(
            ticket.custom.get("custom_aaaaaa").map(String::as_str),
            Some("Acme")
        );
        assert_eq!(
            ticket.custom.get("custom_bbbbbb").map(String::as_str),
            Some("Halal")
        );
    }

    #[test]
    fn a_page_without_an_id_or_with_empty_answers_is_handled() {
        assert!(ticket_from_backend(&json!({"attendee_name": "x"}), &Layout::default()).is_none());
        let data = json!({"public_id": "abc-1", "attendee_name": "", "ticket_type": " ",
                          "custom_fields_data": {"company": "  "}});
        let ticket = ticket_from_backend(&data, &Layout::default()).unwrap();
        assert_eq!(ticket.name, None);
        assert_eq!(ticket.company, None);
        assert_eq!(ticket.ticket_type, "Visitor");
        assert!(ticket.custom.is_empty());
    }

    #[test]
    fn long_text_is_cut_and_control_bytes_dropped() {
        let data =
            json!({"public_id": "abc-1", "attendee_name": format!("A\0{}", "b".repeat(500))});
        let ticket = ticket_from_backend(&data, &Layout::default()).unwrap();
        assert_eq!(ticket.name.unwrap().chars().count(), 200);
    }
}
