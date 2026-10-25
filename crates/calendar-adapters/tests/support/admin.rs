use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use calendar_adapters::admin_auth::{AdminConfig, AdminError, AdminTransport, CloudflareVerifier};
use calendar_application::SecretBytes;
use ring::{
    rand::SystemRandom,
    signature::{RsaKeyPair, RSA_PKCS1_SHA256},
};
use serde_json::{json, Value};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};
pub const NOW: i64 = 1_700_000_100;
pub fn config() -> AdminConfig {
    AdminConfig {
        team: "synthetic-team".into(),
        audience: "admin-aud".into(),
        origin: "https://admin.example.test".into(),
        hostname: "admin.example.test".into(),
        action: "admin-login".into(),
        administrators: [
            ("alice-sub".into(), "alice@example.test".into()),
            ("bob-sub".into(), "bob@example.test".into()),
        ]
        .into(),
        session_seconds: 120,
    }
}
pub fn fixture() -> Value {
    serde_json::from_str(include_str!("admin-fixture.json")).unwrap()
}
pub fn claims() -> Value {
    json!({"iss":"https://synthetic-team.cloudflareaccess.com","aud":["admin-aud"],"type":"app","sub":"alice-sub","email":"alice@example.test","iat":NOW-10,"nbf":NOW-10,"exp":NOW+600})
}
pub fn sign_header(header: Value, claims: Value) -> String {
    let key =
        RsaKeyPair::from_pkcs8(&B64.decode(fixture()["pkcs8"].as_str().unwrap()).unwrap()).unwrap();
    let input = format!(
        "{}.{}",
        B64.encode(serde_json::to_vec(&header).unwrap()),
        B64.encode(serde_json::to_vec(&claims).unwrap())
    );
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        input.as_bytes(),
        &mut signature,
    )
    .unwrap();
    format!("{input}.{}", B64.encode(signature))
}
pub fn sign(claims: Value) -> String {
    sign_header(json!({"alg":"RS256","kid":"synthetic","typ":"JWT"}), claims)
}
type RecordedRequest = (String, Option<Vec<(String, String)>>);
pub struct Fake {
    pub jwks: Mutex<Value>,
    pub site: Mutex<Value>,
    pub fail: Mutex<bool>,
    pub requests: Mutex<Vec<RecordedRequest>>,
}
impl Default for Fake {
    fn default() -> Self {
        Self {
            jwks: Mutex::new(fixture()["jwks"].clone()),
            site: Mutex::new(
                json!({"success":true,"hostname":"admin.example.test","action":"admin-login","challenge_ts":"2023-11-14T22:15:00Z"}),
            ),
            fail: Mutex::new(false),
            requests: Mutex::new(vec![]),
        }
    }
}
impl AdminTransport for Fake {
    fn request(
        &self,
        url: String,
        form: Option<Vec<(String, String)>>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, AdminError>> + Send + '_>> {
        self.requests.lock().unwrap().push((url.clone(), form));
        Box::pin(async move {
            if *self.fail.lock().unwrap() {
                return Err(AdminError);
            }
            let result = if url.ends_with("/certs") {
                self.jwks.lock().unwrap().clone()
            } else {
                self.site.lock().unwrap().clone()
            };
            Ok(serde_json::to_vec(&result).unwrap())
        })
    }
}
pub fn verifier(fake: Arc<Fake>) -> Arc<CloudflareVerifier> {
    Arc::new(
        CloudflareVerifier::with_transport(
            config(),
            SecretBytes::new(b"synthetic-secret".to_vec()),
            fake,
        )
        .unwrap(),
    )
}
