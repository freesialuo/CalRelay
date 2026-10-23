-- Persistent occurrences, reminder rules, logical deliveries, jobs and planning watermarks.
-- Timestamps are UTC Unix milliseconds; revisions and generations are monotonic positive integers.
CREATE TABLE outputs (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    kind TEXT NOT NULL CHECK (length(kind) > 0),
    config_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(config_json)),
    credential_id TEXT,
    generation INTEGER NOT NULL DEFAULT 1 CHECK (generation >= 1),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))
) STRICT;

CREATE TABLE reminder_rules (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    policy_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(policy_json)),
    generation INTEGER NOT NULL DEFAULT 1 CHECK (generation >= 1),
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1))
) STRICT;

CREATE TABLE rule_outputs (
    rule_id TEXT NOT NULL REFERENCES reminder_rules(id),
    output_id TEXT NOT NULL REFERENCES outputs(id),
    PRIMARY KEY (rule_id, output_id)
) STRICT;

CREATE TABLE planning_watermarks (
    scope_id TEXT PRIMARY KEY CHECK (length(scope_id) > 0),
    source_id TEXT NOT NULL REFERENCES sources(id),
    rule_id TEXT NOT NULL REFERENCES reminder_rules(id),
    output_id TEXT NOT NULL REFERENCES outputs(id),
    source_revision INTEGER NOT NULL CHECK (source_revision >= 0),
    rule_revision INTEGER NOT NULL CHECK (rule_revision >= 1),
    output_revision INTEGER NOT NULL DEFAULT 1 CHECK (output_revision >= 1),
    planned_until INTEGER NOT NULL,
    tzdata_version TEXT NOT NULL CHECK (length(tzdata_version) > 0),
    last_error TEXT,
    FOREIGN KEY (rule_id, output_id) REFERENCES rule_outputs(rule_id, output_id),
    UNIQUE (source_id, rule_id, output_id)
) STRICT;

CREATE TABLE occurrences (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    event_id TEXT NOT NULL REFERENCES events(id),
    original_key TEXT NOT NULL CHECK (length(original_key) > 0),
    event_revision INTEGER NOT NULL CHECK (event_revision >= 1),
    start_utc INTEGER NOT NULL,
    end_utc INTEGER NOT NULL CHECK (end_utc >= start_utc),
    local_date TEXT NOT NULL CHECK (length(local_date) = 10),
    resolved_tzid TEXT NOT NULL CHECK (length(resolved_tzid) > 0),
    cancelled INTEGER NOT NULL DEFAULT 0 CHECK (cancelled IN (0, 1)),
    UNIQUE (event_id, original_key)
) STRICT;

CREATE INDEX occurrences_span ON occurrences(start_utc, end_utc);
CREATE INDEX occurrences_event_revision ON occurrences(event_id, event_revision);

CREATE TABLE logical_deliveries (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    event_id TEXT NOT NULL REFERENCES events(id),
    original_key TEXT NOT NULL CHECK (length(original_key) > 0),
    rule_id TEXT NOT NULL REFERENCES reminder_rules(id),
    output_id TEXT NOT NULL REFERENCES outputs(id),
    notification_kind TEXT NOT NULL DEFAULT 'reminder' CHECK (length(notification_kind) > 0),
    accepted_at INTEGER,
    FOREIGN KEY (rule_id, output_id) REFERENCES rule_outputs(rule_id, output_id),
    UNIQUE (event_id, original_key, rule_id, output_id, notification_kind)
) STRICT;

CREATE TABLE delivery_jobs (
    id TEXT PRIMARY KEY CHECK (length(id) > 0),
    logical_delivery_id TEXT NOT NULL REFERENCES logical_deliveries(id),
    generation INTEGER NOT NULL CHECK (generation >= 1),
    event_revision INTEGER NOT NULL CHECK (event_revision >= 1),
    rule_revision INTEGER NOT NULL CHECK (rule_revision >= 1),
    output_revision INTEGER NOT NULL CHECK (output_revision >= 1),
    due_at INTEGER NOT NULL,
    deadline_at INTEGER,
    state TEXT NOT NULL CHECK (state IN (
        'pending',
        'leased',
        'accepted',
        'retry_wait',
        'failed_permanent',
        'expired',
        'superseded',
        'cancelled'
    )),
    next_attempt_at INTEGER NOT NULL,
    lease_owner TEXT,
    lease_token TEXT,
    lease_until INTEGER,
    UNIQUE (logical_delivery_id, generation)
) STRICT;

CREATE INDEX delivery_jobs_due ON delivery_jobs(state, next_attempt_at, due_at);
CREATE INDEX delivery_jobs_lease ON delivery_jobs(state, lease_until);
