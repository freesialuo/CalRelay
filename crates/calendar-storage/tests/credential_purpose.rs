use calendar_application::{SecretBytes, SecurityError};
use calendar_storage::{
    security::{read_secret_file, KeyRing, SecurityStore},
    Storage,
};
#[tokio::test]
async fn purpose_bound_rotation_and_replacement_cannot_modify_another_kind() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, [23u8; 32]).unwrap();
    let keys = KeyRing::from_files(1, &[(1, key)].into()).unwrap();
    let db = Storage::open(dir.path().join("db")).await.unwrap();
    let store = SecurityStore {
        storage: &db,
        keys: &keys,
    };
    store
        .create_credential(
            "target",
            "oauth2",
            &SecretBytes::new(b"synthetic-oauth".to_vec()),
            1,
        )
        .await
        .unwrap();
    let before: (Vec<u8>, Vec<u8>, i64) =
        sqlx::query_as("SELECT ciphertext,nonce,revision FROM credentials WHERE id='target'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(
        store
            .replace_credential_for_kind(
                "target",
                "bark",
                0,
                &SecretBytes::new(b"synthetic-bark".to_vec()),
                2
            )
            .await,
        Err(SecurityError::Denied)
    );
    assert_eq!(
        store
            .rotate_credential_for_kind("target", "bark", 0, 2)
            .await,
        Err(SecurityError::Denied)
    );
    let after: (Vec<u8>, Vec<u8>, i64) =
        sqlx::query_as("SELECT ciphertext,nonce,revision FROM credentials WHERE id='target'")
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(before, after);
    db.close().await;
}
#[test]
fn controlled_reader_and_keyring_share_regular_file_and_length_guards() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("key");
    std::fs::write(&key, [19u8; 32]).unwrap();
    assert_eq!(read_secret_file(&key, 32).unwrap().expose(), &[19u8; 32]);
    assert!(read_secret_file(&key, 31).is_err());
    assert!(read_secret_file(&key, 0).is_err());
    assert!(read_secret_file(&key, 65537).is_err());
    assert!(read_secret_file(dir.path(), 4096).is_err());
    #[cfg(unix)]
    {
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&key, &link).unwrap();
        assert!(read_secret_file(&link, 32).is_err());
        assert!(KeyRing::from_files(1, &[(1, link)].into()).is_err());
        let fifo = dir.path().join("fifo");
        assert!(std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success());
        assert!(KeyRing::from_files(1, &[(1, fifo)].into()).is_err());
    }
}
