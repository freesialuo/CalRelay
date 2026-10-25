use calendar_application::{EventV1, FeedScope};
use serde_json::json;
fn event() -> EventV1 {
    serde_json::from_value(json!({"schema_version":"1.0","external_uid":"upstream","title":"base title","description":"base description","location":"base location","url":"https://private.invalid/base","categories":["base category"],"status":"confirmed","start":{"kind":"utc","value":"2026-10-25T10:00:00Z"},"duration":"PT1H","recurrence":{"rrule":"FREQ=DAILY;COUNT=2","exceptions":[{"recurrence_id":{"kind":"utc","value":"2026-10-26T10:00:00Z"},"range":"single","change":{"kind":"override","patch":{"title":"override title","description":"override description","location":"override location","url":"https://private.invalid/override","categories":["override category"]}}}]}})).unwrap()
}
#[test]
fn scope_is_finite_explicit_and_legacy_compatible() {
    assert_eq!(FeedScope::parse("{}"), Ok(FeedScope::Legacy));
    for bad in [
        r#"{"version":2,"calendar_ids":[],"visible_fields":[]}"#,
        r#"{"version":1,"visible_fields":[]}"#,
        r#"{"version":1,"calendar_ids":[]}"#,
        r#"{"version":1,"calendar_ids":null,"visible_fields":[]}"#,
        r#"{"version":1,"calendar_ids":[],"visible_fields":["organizer"]}"#,
        r#"{"version":1,"calendar_ids":[],"visible_fields":[],"filter":"true"}"#,
        r#"{"version":1,"calendar_ids":["a","a"],"visible_fields":[]}"#,
        r#"{"version":1,"calendar_ids":[],"visible_fields":["summary","summary"]}"#,
        r#"{"version":1,"version":1,"calendar_ids":[],"visible_fields":[]}"#,
        r#"{"version":1,"calendar_ids":[""],"visible_fields":[]}"#,
        r#"[]"#,
        r#"null"#,
    ] {
        assert!(FeedScope::parse(bad).is_err(), "accepted {bad}");
    }
    let mut value = event();
    let original = value.clone();
    FeedScope::Legacy.project(&mut value);
    assert_eq!(value, original);
}
#[test]
fn each_field_and_override_is_independently_hidden_without_touching_protocol_semantics() {
    let original = event();
    for visible in [
        vec![],
        vec!["summary"],
        vec!["description"],
        vec!["location"],
        vec!["url"],
        vec!["categories"],
        vec!["summary", "description", "location", "url", "categories"],
    ] {
        let scope = FeedScope::parse(
            &json!({"version":1,"calendar_ids":["c"],"visible_fields":visible}).to_string(),
        )
        .unwrap();
        let mut value = original.clone();
        scope.project(&mut value);
        let doc = serde_json::to_value(&value).unwrap();
        let source = serde_json::to_value(&original).unwrap();
        let patch = &doc["recurrence"]["exceptions"][0]["change"]["patch"];
        for (field, selector) in [
            ("title", "summary"),
            ("description", "description"),
            ("location", "location"),
            ("url", "url"),
            ("categories", "categories"),
        ] {
            if visible.contains(&selector) {
                assert_eq!(doc[field], source[field]);
                assert_eq!(
                    patch[field],
                    source["recurrence"]["exceptions"][0]["change"]["patch"][field]
                );
            } else {
                if field == "title" {
                    assert_eq!(doc[field], "Busy");
                } else {
                    assert!(doc.get(field).is_none());
                }
                assert!(patch.get(field).is_none());
            }
        }
        for key in ["external_uid", "start", "status", "duration"] {
            assert_eq!(doc[key], source[key]);
        }
        assert_eq!(doc["recurrence"]["rrule"], source["recurrence"]["rrule"]);
        assert_eq!(
            doc["recurrence"]["exceptions"][0]["recurrence_id"],
            source["recurrence"]["exceptions"][0]["recurrence_id"]
        );
        assert_eq!(FeedScope::parse(&scope.to_json()).unwrap(), scope);
    }
}
