#[path = "support/admin.rs"]
mod support;
use calendar_adapters::admin_auth::CloudflareVerifier;
use calendar_application::SecretBytes;
use serde_json::json;
use std::sync::Arc;
use support::*;

#[tokio::test]
async fn signatures_claims_and_administrator_identity_are_all_required() {
    let fake = Arc::new(Fake::default());
    let verifier = verifier(fake.clone());
    assert!(verifier.access(&sign(claims()), NOW).await.is_ok());
    for (name, value) in [
        ("iss", json!("https://evil.test")),
        ("aud", json!(["wrong"])),
        ("type", json!("org")),
        ("sub", json!("unknown")),
        ("email", json!("forged@example.test")),
        ("exp", json!(NOW)),
        ("nbf", json!(NOW + 1)),
        ("iat", json!(NOW + 1)),
    ] {
        let mut c = claims();
        c[name] = value;
        assert!(
            verifier.access(&sign(c), NOW).await.is_err(),
            "accepted {name}"
        );
    }
    for name in ["iss", "aud", "exp", "nbf", "iat", "sub", "email", "type"] {
        let mut c = claims();
        c.as_object_mut().unwrap().remove(name);
        assert!(
            verifier.access(&sign(c), NOW).await.is_err(),
            "missing {name}"
        );
    }
    for header in [
        json!({"alg":"none","kid":"synthetic"}),
        json!({"alg":"HS256","kid":"synthetic"}),
        json!({"alg":"RS256","kid":"unknown"}),
        json!({"alg":"RS256","kid":"synthetic","jku":"https://evil.test/keys"}),
    ] {
        assert!(verifier
            .access(&sign_header(header, claims()), NOW)
            .await
            .is_err());
    }
    let jwt = sign(claims());
    let mut parts: Vec<_> = jwt.split('.').map(str::to_owned).collect();
    parts[1].push('a');
    assert!(verifier.access(&parts.join("."), NOW).await.is_err());
    for value in ["", "not-a-token", "a.b.c.d"] {
        assert!(verifier.access(value, NOW).await.is_err());
    }
    fake.jwks.lock().unwrap()["keys"][0]["n"] = json!("AQAB");
    assert!(verifier.access(&sign(claims()), NOW).await.is_err());
    *fake.fail.lock().unwrap() = true;
    assert!(verifier.access(&sign(claims()), NOW).await.is_err());
    assert!(fake
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|(u, _)| u == "https://synthetic-team.cloudflareaccess.com/cdn-cgi/access/certs"));
}
#[tokio::test]
async fn siteverify_binds_hostname_action_age_and_fails_closed() {
    let fake = Arc::new(Fake::default());
    let verifier = verifier(fake.clone());
    assert!(verifier.turnstile("synthetic-token", NOW).await.is_ok());
    let original = fake.site.lock().unwrap().clone();
    for (name, value) in [
        ("success", json!(false)),
        ("hostname", json!("evil.test")),
        ("action", json!("other")),
        ("challenge_ts", json!("2023-11-14T22:10:00Z")),
        ("challenge_ts", json!("2023-11-14T22:15:01Z")),
        ("challenge_ts", json!("broken")),
    ] {
        *fake.site.lock().unwrap() = original.clone();
        fake.site.lock().unwrap()[name] = value;
        assert!(
            verifier.turnstile("synthetic-token", NOW).await.is_err(),
            "accepted {name}"
        );
    }
    *fake.site.lock().unwrap() = json!({"success":true});
    assert!(verifier.turnstile("synthetic-token", NOW).await.is_err());
    *fake.fail.lock().unwrap() = true;
    assert!(verifier.turnstile("synthetic-token", NOW).await.is_err());
    assert!(verifier.turnstile("", NOW).await.is_err());
    assert!(verifier.turnstile(&"x".repeat(2049), NOW).await.is_err());
    let requests = fake.requests.lock().unwrap();
    let (url, form) = &requests[0];
    assert_eq!(
        url,
        "https://challenges.cloudflare.com/turnstile/v0/siteverify"
    );
    assert_eq!(
        form.as_ref().unwrap(),
        &vec![
            ("secret".into(), "synthetic-secret".into()),
            ("response".into(), "synthetic-token".into())
        ]
    );
}
#[test]
fn configuration_is_explicit_and_untrusted_urls_cannot_select_keys() {
    for team in ["", "evil.test/", "https://evil", "name@evil", "../evil"] {
        let mut c = config();
        c.team = team.into();
        assert!(CloudflareVerifier::with_transport(
            c,
            SecretBytes::new(b"synthetic".to_vec()),
            Arc::new(Fake::default())
        )
        .is_err());
    }
    for url in [
        "http://admin.example.test",
        "https://evil.test",
        "https://admin.example.test/path",
        "https://admin.example.test/",
    ] {
        let mut c = config();
        c.origin = url.into();
        assert!(CloudflareVerifier::with_transport(
            c,
            SecretBytes::new(b"synthetic".to_vec()),
            Arc::new(Fake::default())
        )
        .is_err());
    }
    let mut c = config();
    c.administrators.clear();
    assert!(CloudflareVerifier::with_transport(
        c,
        SecretBytes::new(b"synthetic".to_vec()),
        Arc::new(Fake::default())
    )
    .is_err());
}
