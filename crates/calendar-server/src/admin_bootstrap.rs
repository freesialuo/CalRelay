//! Trusted deployment assembly. No remote endpoints or test switches come from requests.
use crate::ConfigError;
use calendar_adapters::admin_auth::{AdminConfig, CloudflareVerifier};
use calendar_application::{CredentialProvider, SecretBytes, SecurityError};
use serde::Deserialize;
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminBootstrapConfig {
    pub team: String,
    pub audience: String,
    pub origin: String,
    pub hostname: String,
    pub action: String,
    pub site_key: String,
    #[serde(deserialize_with = "unique_administrators")]
    pub administrators: BTreeMap<String, String>,
    pub session_seconds: i64,
    pub turnstile_secret_file: PathBuf,
}
impl AdminBootstrapConfig {
    fn verifier_config(&self) -> AdminConfig {
        AdminConfig {
            team: self.team.clone(),
            audience: self.audience.clone(),
            origin: self.origin.clone(),
            hostname: self.hostname.clone(),
            action: self.action.clone(),
            administrators: self.administrators.clone(),
            session_seconds: self.session_seconds,
        }
    }
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        self.verifier_config()
            .validate()
            .map_err(|_| ConfigError("invalid admin deployment configuration"))?;
        crate::admin::validate_site_key(&self.site_key)?;
        if !self.turnstile_secret_file.is_absolute()
            || self.audience.len() > 256
            || self.origin.len() > 2048
            || self.hostname.len() > 253
            || self.action.len() > 32
            || !self
                .action
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            || self.administrators.len() > 64
            || self.administrators.iter().any(|(s, e)| {
                s.len() > 256
                    || e.len() > 320
                    || s.chars().any(char::is_control)
                    || e.chars().any(char::is_control)
            })
        {
            return Err(ConfigError("invalid admin deployment configuration"));
        }
        Ok(())
    }
    pub(crate) async fn load_verifier(&self) -> Result<CloudflareVerifier, ConfigError> {
        self.validate()?;
        let provider = TurnstileFileSecretProvider {
            path: self.turnstile_secret_file.clone(),
        };
        let secret = provider
            .credential("turnstile", "cloudflare_turnstile")
            .await
            .map_err(|_| ConfigError("admin secret unavailable or invalid"))?;
        CloudflareVerifier::new(self.verifier_config(), secret)
            .map_err(|_| ConfigError("invalid admin verifier configuration"))
    }
}
/// Dedicated read-only provider: no DB seed, environment secret, mutation or secret logging.
pub struct TurnstileFileSecretProvider {
    path: PathBuf,
}
impl CredentialProvider for TurnstileFileSecretProvider {
    async fn credential(&self, id: &str, kind: &str) -> Result<SecretBytes, SecurityError> {
        if id != "turnstile" || kind != "cloudflare_turnstile" {
            return Err(SecurityError::Denied);
        }
        let secret = calendar_storage::security::read_secret_file(&self.path, 4096)?;
        if secret.expose().is_empty()
            || secret.expose().len() > 4096
            || !secret.expose().iter().all(u8::is_ascii_graphic)
        {
            return Err(SecurityError::KeyFile);
        }
        Ok(secret)
    }
}

fn unique_administrators<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, String>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique administrator subjects")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate administrator subject"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique)
}

pub(crate) fn configured_admin<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<AdminBootstrapConfig>, D::Error> {
    AdminBootstrapConfig::deserialize(deserializer).map(Some)
}
