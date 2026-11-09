-- Sharing a saved query with a group (nova#103 step 2b). The owner inserts a row;
-- members of the group see the query when their token lists it. Deleting the query
-- deletes its shares, and a share is unique per (query, group).
-- NEVER edit this file after it has been merged; add 0009_*.sql instead.
CREATE TABLE query_share (
    saved_query_id bigint      NOT NULL REFERENCES saved_query (id) ON DELETE CASCADE,
    group_name     text        NOT NULL CHECK (char_length(group_name) BETWEEN 1 AND 64),
    created_at     timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (saved_query_id, group_name)
);

CREATE INDEX query_share_group_idx ON query_share (group_name);
