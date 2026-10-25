//! Synthetic security material, confined to test temporary directories.
pub fn deployment(dir: &std::path::Path, bind: &str) -> serde_json::Value {
    let key = dir.join("runtime-test-master-key");
    let secret = dir.join("runtime-test-turnstile-secret");
    std::fs::write(&key, [73u8; 32]).unwrap();
    std::fs::write(&secret, b"synthetic-turnstile").unwrap();
    serde_json::json!({"bind":bind,"credentials":{"current_key_version":1,"key_files":{"1":key}},
        "admin":{"team":"synthetic-team","audience":"synthetic-aud","origin":"https://admin.example.test",
        "hostname":"admin.example.test","action":"admin-login","site_key":"synthetic-site",
        "administrators":{"synthetic-sub":"admin@example.test"},"session_seconds":120,"turnstile_secret_file":secret}})
}
