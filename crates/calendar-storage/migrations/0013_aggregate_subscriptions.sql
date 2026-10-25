-- #76 sole owner; #92 owns 0012. Legacy feed_subscriptions is unchanged.
CREATE TABLE aggregate_subscriptions (
 id TEXT PRIMARY KEY CHECK(length(id) BETWEEN 1 AND 128),
 public_id TEXT NOT NULL UNIQUE,
 token_digest BLOB NOT NULL UNIQUE CHECK(length(token_digest)=32),
 configuration_json TEXT NOT NULL CHECK(json_valid(configuration_json) AND length(CAST(configuration_json AS BLOB))<=65536),
 revision INTEGER NOT NULL CHECK(revision>0),
 revoked_at INTEGER
) STRICT;
CREATE TABLE aggregate_subscription_sources (
 subscription_id TEXT NOT NULL REFERENCES aggregate_subscriptions(id),
 source_id TEXT NOT NULL REFERENCES sources(id),
 PRIMARY KEY(subscription_id,source_id)
) STRICT;
CREATE INDEX aggregate_sources_by_source ON aggregate_subscription_sources(source_id,subscription_id);
CREATE TABLE aggregate_subscription_audit (
 subscription_id TEXT NOT NULL REFERENCES aggregate_subscriptions(id),
 revision INTEGER NOT NULL CHECK(revision>0),
 action TEXT NOT NULL CHECK(action IN ('create','update','rotate','retire')),
 actor_digest TEXT NOT NULL CHECK(length(actor_digest)=64),
 changed_at INTEGER NOT NULL,
 PRIMARY KEY(subscription_id,revision)
) STRICT;
