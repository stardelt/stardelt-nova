-- Query profile (stardelt-nova#788 S1): plan and per-operator statistics of a finished
-- query. Kept shorter than the log row: NOVA_QUERY_PROFILE_RETENTION_DAYS, default 14.
CREATE TABLE query_profile (
    query_id   text PRIMARY KEY REFERENCES query_log (query_id) ON DELETE CASCADE,
    profile    jsonb NOT NULL,
    bytes      integer NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX query_profile_created_at ON query_profile (created_at);
