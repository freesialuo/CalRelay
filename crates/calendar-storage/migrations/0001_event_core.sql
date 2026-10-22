-- Event storage foundation only. Later migrations add synchronization, planning and delivery.
-- Timestamps are UTC Unix milliseconds; documents remain canonical EventV1 JSON.
CREATE TABLE sources (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (length(kind) > 0),
    config_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config_json)),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    committed_checkpoint TEXT,
    last_verified_at INTEGER
) STRICT;

CREATE TABLE calendars (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES sources(id),
    external_id TEXT NOT NULL,
    timezone_policy TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(timezone_policy)),
    UNIQUE (source_id, external_id),
    UNIQUE (source_id, id)
) STRICT;

CREATE TABLE events (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES sources(id),
    calendar_id TEXT NOT NULL,
    external_uid TEXT NOT NULL CHECK (length(external_uid) > 0),
    revision INTEGER NOT NULL DEFAULT 0 CHECK (revision >= 0),
    document_json TEXT NOT NULL CHECK (json_valid(document_json)),
    content_hash TEXT NOT NULL CHECK (length(content_hash) > 0),
    deleted_at INTEGER,
    FOREIGN KEY (source_id, calendar_id) REFERENCES calendars(source_id, id),
    UNIQUE (source_id, external_uid),
    UNIQUE (source_id, id)
) STRICT;

CREATE INDEX events_calendar ON events(source_id, calendar_id);

CREATE TABLE event_revisions (
    event_id TEXT NOT NULL REFERENCES events(id),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    document_json TEXT CHECK (document_json IS NULL OR json_valid(document_json)),
    change_kind TEXT NOT NULL CHECK (change_kind IN ('upsert', 'delete')),
    changed_at INTEGER NOT NULL,
    CHECK (change_kind = 'delete' OR document_json IS NOT NULL),
    PRIMARY KEY (event_id, revision),
    UNIQUE (event_id, revision, change_kind)
) STRICT;

CREATE TABLE source_changes (
    source_id TEXT NOT NULL REFERENCES sources(id),
    source_revision INTEGER NOT NULL CHECK (source_revision >= 0),
    event_id TEXT NOT NULL,
    event_revision INTEGER NOT NULL CHECK (event_revision >= 0),
    change_kind TEXT NOT NULL CHECK (change_kind IN ('upsert', 'delete')),
    PRIMARY KEY (source_id, source_revision, event_id),
    FOREIGN KEY (source_id, event_id) REFERENCES events(source_id, id),
    FOREIGN KEY (event_id, event_revision, change_kind)
        REFERENCES event_revisions(event_id, revision, change_kind)
) STRICT;

CREATE INDEX source_changes_event_revision ON source_changes(event_id, event_revision, change_kind);
