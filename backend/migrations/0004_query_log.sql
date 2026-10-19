-- The query log (decision D25, stardelt-nova#116): one row per query the engine ran, fed by its
-- event listener through the internal collector. A completed event overwrites the row the
-- created event made. `principal` is the engine user (the sign-in name of a person or of a
-- service principal). Times are stored as timestamptz; durations in milliseconds, sizes in bytes.
-- NEVER edit this file after it has been merged: add 0005_*.sql instead.
CREATE TABLE query_log (
    query_id          text        PRIMARY KEY,
    warehouse         text        NOT NULL DEFAULT 'default',
    state             text        NOT NULL CHECK (state IN ('QUEUED', 'FINISHED', 'FAILED', 'CANCELED')),
    principal         text        NOT NULL,
    source            text,
    client_tags       text[]      NOT NULL DEFAULT '{}',
    catalog           text,
    schema_name       text,
    query_type        text,
    sql_text          text        NOT NULL,
    sql_hash          text        NOT NULL,
    created_at        timestamptz NOT NULL,
    started_at        timestamptz,
    ended_at          timestamptz,
    queued_ms         bigint,
    planning_ms       bigint,
    execution_ms      bigint,
    wall_ms           bigint,
    cpu_ms            bigint,
    peak_memory_bytes bigint,
    input_rows        bigint,
    input_bytes       bigint,
    output_rows       bigint,
    output_bytes      bigint,
    written_rows      bigint,
    written_bytes     bigint,
    error_code        text,
    error_message     text,
    tables_read       text[]      NOT NULL DEFAULT '{}',
    tables_written    text[]      NOT NULL DEFAULT '{}',
    updated_at        timestamptz NOT NULL DEFAULT now()
);

-- "My queries, newest first" and "everything, newest first" are the two list shapes.
CREATE INDEX query_log_principal_created_idx ON query_log (principal, created_at DESC);
CREATE INDEX query_log_created_idx ON query_log (created_at DESC);
