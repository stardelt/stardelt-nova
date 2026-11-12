-- One row per pipeline: the last finished run Nova has noticed (nova#815 slice c).
CREATE TABLE pipeline_notice (
    namespace    TEXT   NOT NULL,
    pipeline     TEXT   NOT NULL,
    last_run_id  TEXT   NOT NULL,
    last_state   TEXT   NOT NULL,
    noticed_at   BIGINT NOT NULL,
    PRIMARY KEY (namespace, pipeline)
);
-- Which run a delivery belonged to; the test message of #768 has none.
ALTER TABLE destination_delivery ADD COLUMN subject_id TEXT;
