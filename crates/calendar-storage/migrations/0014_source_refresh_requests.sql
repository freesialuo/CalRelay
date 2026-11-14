-- Pending manual refresh, at most one per source. `sequence` lets the scanner clear exactly the
-- request it served; `config_revision` drops requests made against a superseded configuration.
CREATE TABLE source_refresh_requests (
    source_id TEXT PRIMARY KEY REFERENCES sources(id),
    config_revision INTEGER NOT NULL CHECK(config_revision > 0),
    sequence INTEGER NOT NULL CHECK(sequence > 0),
    requested_at INTEGER NOT NULL
) STRICT;
