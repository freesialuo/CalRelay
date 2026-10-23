-- Per-job interpretation context: a shared occurrence row may already have been
-- refreshed by another rule/output when this job is reconciled. NULL denotes
-- legacy jobs whose interpretation context was not recorded.
ALTER TABLE delivery_jobs ADD COLUMN planning_context TEXT
    CHECK (planning_context IS NULL OR json_valid(planning_context));
