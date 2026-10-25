use calendar_application::aggregate::*;
use serde_json::json;
fn value() -> serde_json::Value {
    json!({"version":1,"name":"Combined","enabled":false,"sources":[],"visible_fields":[],"stale_policy":"reject","expires_at":null})
}
#[test]
fn explicit_finite_policy_and_global_budgets_fail_closed() {
    let valid: AggregateConfiguration = serde_json::from_value(value()).unwrap();
    valid.validate().unwrap();
    for bad in [
        json!({}),
        json!([]),
        {
            let mut v = value();
            v["version"] = "v1".into();
            v
        },
        {
            let mut v = value();
            v["filter"] = "true".into();
            v
        },
        {
            let mut v = value();
            v["visible_fields"] = json!(["organizer"]);
            v
        },
        {
            let mut v = value();
            v["stale_policy"] = "skip".into();
            v
        },
        {
            let mut v = value();
            v.as_object_mut().unwrap().remove("expires_at");
            v
        },
    ] {
        assert!(serde_json::from_value::<AggregateConfiguration>(bad).is_err());
    }
    for mutate in 0..4 {
        let mut v = value();
        match mutate {
            0 => v["version"] = 2.into(),
            1 => v["visible_fields"] = json!(["summary", "summary"]),
            2 => {
                v["sources"] =
                    json!([{"source_id":"s","calendar_ids":[]},{"source_id":"s","calendar_ids":[]}])
            }
            _ => v["sources"] = json!([{"source_id":"s","calendar_ids":["c","c"]}]),
        };
        assert!(serde_json::from_value::<AggregateConfiguration>(v)
            .unwrap()
            .validate()
            .is_err());
    }
    let mut v = value();
    v["sources"]=json!((0..2).map(|s|json!({"source_id":format!("s{s}"),"calendar_ids":(0..129).map(|i|format!("c{s}_{i}")).collect::<Vec<_>>()})).collect::<Vec<_>>());
    assert!(serde_json::from_value::<AggregateConfiguration>(v)
        .unwrap()
        .validate()
        .is_err());
}
