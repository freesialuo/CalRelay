//! Explicit local Bark provisioning and an owned read-only runtime port.
use crate::{ConfigError, DeploymentConfig, Services};
use calendar_application::{CredentialProvider, SecretBytes, SecurityError};
use calendar_storage::{
    security::{read_secret_file, KeyRing, SecurityStore},
    Storage,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CredentialKeysConfig {
    pub current_key_version: i64,
    #[serde(deserialize_with = "unique_key_files")]
    pub key_files: BTreeMap<i64, PathBuf>,
}
impl CredentialKeysConfig {
    pub(crate) fn validate(&self) -> Result<(), ConfigError> {
        if self.current_key_version <= 0
            || self.key_files.is_empty()
            || self.key_files.len() > 16
            || !self.key_files.contains_key(&self.current_key_version)
            || self
                .key_files
                .iter()
                .any(|(v, p)| *v <= 0 || !p.is_absolute())
        {
            return Err(ConfigError("invalid credential key configuration"));
        }
        Ok(())
    }
    pub fn load(&self) -> Result<KeyRing, ConfigError> {
        self.validate()?;
        KeyRing::from_files(self.current_key_version, &self.key_files)
            .map_err(|_| ConfigError("credential keys unavailable or invalid"))
    }
}
pub(crate) fn configured_credentials<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<CredentialKeysConfig>, D::Error> {
    CredentialKeysConfig::deserialize(d).map(Some)
}
fn unique_key_files<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<BTreeMap<i64, PathBuf>, D::Error> {
    struct Unique;
    impl<'de> serde::de::Visitor<'de> for Unique {
        type Value = BTreeMap<i64, PathBuf>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique canonical positive key versions")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((text, path)) = map.next_entry::<String, PathBuf>()? {
                let version: i64 = text
                    .parse()
                    .map_err(|_| serde::de::Error::custom("invalid key version"))?;
                if version <= 0
                    || version.to_string() != text
                    || result.insert(version, path).is_some()
                {
                    return Err(serde::de::Error::custom("invalid or duplicate key version"));
                }
            }
            Ok(result)
        }
    }
    d.deserialize_map(Unique)
}

#[derive(Clone)]
pub struct RuntimeCredentialProvider {
    pub(crate) services: Arc<Services>,
}
impl CredentialProvider for RuntimeCredentialProvider {
    async fn credential(
        &self,
        id: &str,
        expected_kind: &str,
    ) -> Result<SecretBytes, SecurityError> {
        let keys = self
            .services
            .credential_keys
            .get()
            .ok_or(SecurityError::MissingKey)?;
        SecurityStore {
            storage: &self.services.storage,
            keys,
        }
        .credential(id, expected_kind)
        .await
    }
}

#[derive(Serialize)]
pub struct CredentialReceipt {
    pub credential_id: String,
    pub revision: i64,
}
const CLI_ERROR: ConfigError = ConfigError("invalid credential command");
/// Explicit local mode only. No listener, admin verifier, HTTP, or external account action.
pub async fn run_credential_cli(
    args: impl IntoIterator<Item = String>,
) -> Result<CredentialReceipt, ConfigError> {
    let args: Vec<_> = args.into_iter().collect();
    let (operation, rest) = args.split_first().ok_or(CLI_ERROR)?;
    if !matches!(operation.as_str(), "provision-bark" | "rotate-bark") {
        return Err(CLI_ERROR);
    }
    let (pairs, remainder) = rest.as_chunks::<2>();
    if !remainder.is_empty() {
        return Err(CLI_ERROR);
    }
    let mut flags = BTreeMap::new();
    for pair in pairs {
        if !matches!(
            pair[0].as_str(),
            "--deployment-config" | "--credential-id" | "--secret-file" | "--expected-revision"
        ) || flags.insert(pair[0].as_str(), pair[1].clone()).is_some()
        {
            return Err(CLI_ERROR);
        }
    }
    let path = flags.remove("--deployment-config").ok_or(CLI_ERROR)?;
    let id = flags.remove("--credential-id").ok_or(CLI_ERROR)?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(CLI_ERROR);
    }
    let expected = flags
        .remove("--expected-revision")
        .map(|s| {
            let n: i64 = s.parse().map_err(|_| CLI_ERROR)?;
            if n < 0 || n == i64::MAX || n.to_string() != s {
                Err(CLI_ERROR)
            } else {
                Ok(n)
            }
        })
        .transpose()?;
    let secret_file = flags.remove("--secret-file");
    if (operation == "rotate-bark" && (expected.is_none() || secret_file.is_some()))
        || (operation == "provision-bark" && secret_file.is_none())
    {
        return Err(CLI_ERROR);
    }
    let deployment = DeploymentConfig::from_args(["--deployment-config".into(), path])?;
    let keys = deployment
        .credentials
        .as_ref()
        .ok_or(ConfigError("credential keys are not configured"))?
        .load()?;
    let secret = if let Some(path) = secret_file {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err(CLI_ERROR);
        }
        let secret = read_secret_file(&path, 4096)
            .map_err(|_| ConfigError("Bark secret unavailable or invalid"))?;
        if !secret.expose().iter().all(u8::is_ascii_graphic) {
            return Err(ConfigError("Bark secret unavailable or invalid"));
        }
        Some(secret)
    } else {
        None
    };
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| CLI_ERROR)?
        .as_millis();
    let now = i64::try_from(now).map_err(|_| CLI_ERROR)?;
    let db = Storage::open(&deployment.server.database)
        .await
        .map_err(|_| ConfigError("credential database unavailable"))?;
    let store = SecurityStore {
        storage: &db,
        keys: &keys,
    };
    let result = match (secret.as_ref(), expected) {
        (Some(secret), None) => store.create_credential(&id, "bark", secret, now).await,
        (Some(secret), Some(revision)) => {
            store
                .replace_credential_for_kind(&id, "bark", revision, secret, now)
                .await
        }
        (None, Some(revision)) => {
            store
                .rotate_credential_for_kind(&id, "bark", revision, now)
                .await
        }
        _ => Err(SecurityError::InvalidInput),
    };
    db.close().await;
    result.map_err(|_| ConfigError("credential operation denied or failed"))?;
    Ok(CredentialReceipt {
        credential_id: id,
        revision: expected.map_or(0, |n| n + 1),
    })
}
