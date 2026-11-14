-- Last sync attempt per source, one bounded row. `next_allowed_at` restores the wait between
-- requests after a restart; `config_revision` stops an old status constraining a new configuration.
-- Enumerations and UTC milliseconds only: no error text, endpoint, credential or event content.
CREATE TABLE source_sync_status (
    source_id TEXT PRIMARY KEY REFERENCES sources(id),
    config_revision INTEGER NOT NULL CHECK(config_revision > 0),
    attempted_at INTEGER NOT NULL,
    outcome TEXT NOT NULL CHECK(outcome IN (
        'success', 'not_modified', 'unsupported', 'unavailable', 'conflict', 'adapter',
        'invalid_snapshot', 'budget', 'timeout', 'unexpected_not_modified', 'retry_after',
        'review_required'
    )),
    consecutive_failures INTEGER NOT NULL CHECK(consecutive_failures >= 0),
    next_allowed_at INTEGER NOT NULL
) STRICT;
