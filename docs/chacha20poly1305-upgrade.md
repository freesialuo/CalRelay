# XChaCha20Poly1305 0.11 compatibility (#63)

## Compatibility and security decisions

The upstream 0.11.0 changelog raises the crate MSRV to Rust 1.85 / edition 2024, upgrades aead/cipher/chacha20/poly1305, removes std/stream features and permits later patch MSRV bumps. CalRelay pins 0.11.0 and its transitive lockfile; validation uses Rust 1.99.1. This is not a claim of testing every older supported toolchain.

Source review found a security-relevant feature change beyond the deprecated nonce API: 0.10.1 always cleared its key and enabled chacha20 zeroization; 0.11.0 gates key, temporary MAC-key and cipher-state zeroization on the optional zeroize feature. Enable that feature explicitly. The compile-time ZeroizeOnDrop bound guards this configuration. It does not prove that every transient copy is erased, nor replace an upstream cryptographic audit.

Nonce construction uses a fixed 24-byte array for encryption and fallible slice conversion for persisted data. Wrong nonce lengths fail with Authentication before decryption; no unwrap/panic is introduced in the production path. Existing OS randomness, 32-byte key ring, 24-byte nonce, 16-byte appended tag, canonical JSON AAD tuple, credential size limits, errors and database schema remain unchanged. No real keys or runtime data were used.

## Verification contract

A fixed synthetic 0.10.1 ciphertext verifies that 0.11 can read the existing credential envelope and emits exactly the same bytes for the same key/nonce/plaintext/AAD. This catches changes that same-version round trips miss. The fixture uses key [7;32], nonce [9;24], plaintext `synthetic legacy credential`, and JSON AAD tuple (`CalRelay/credential/v1`, `legacy`, `bearer`, 1i64). It was generated with a temporary independent Rust program depending exactly on chacha20poly1305 0.10.1 and serde_json, calling Aead::encrypt with those inputs; the resulting bytes are fixed in the unit test. These constants are test-only and must never become production randomness.

The fixture test also rejects changed identity, ciphertext and malformed nonce lengths. Existing real SQLite security integration tests cover credential persistence/reopen, rotation without the old key, key-version/kind/identity AAD tampering, token separation and transaction rollback. Cloud results: full workspace all-features locked tests, all-targets/all-features locked Clippy with warnings denied, formatting, Python 17 tests, repository checks and whitespace checks passed. A controlled production AAD mutation from v1 to v2 made the fixed legacy test fail with Authentication; restoring v1 returned it to green. This demonstrates the persisted-format gate detects a real compatibility break.

## Official sources checked

- [0.11.0 changelog](https://github.com/RustCrypto/AEADs/blob/chacha20poly1305-v0.11.0/chacha20poly1305/CHANGELOG.md).
- [0.11.0 manifest and features](https://github.com/RustCrypto/AEADs/blob/chacha20poly1305-v0.11.0/chacha20poly1305/Cargo.toml).
- [0.11.0 key/drop implementation](https://github.com/RustCrypto/AEADs/blob/chacha20poly1305-v0.11.0/chacha20poly1305/src/lib.rs) and [MAC key handling](https://github.com/RustCrypto/AEADs/blob/chacha20poly1305-v0.11.0/chacha20poly1305/src/cipher.rs), compared with the fixed 0.10.1 published source.

The lockfile compatibility repair adds only zeroize feature edges to chacha20 and chacha20poly1305. Other open PR lockfiles must later be reconciled without dropping #65's calendar-server dependencies. No schema/data rewrite or broader product change is necessary.
