-- Nova state store, migration 1: marks the database as Nova's and gives later
-- migrations (saved queries, settings, ...) something to build on.
-- NEVER edit this file after it has been merged: sqlx stores a checksum of every
-- applied migration and refuses to start when one changed. Add 0002_*.sql instead.
CREATE TABLE schema_info (
    key   text PRIMARY KEY,
    value text NOT NULL
);

INSERT INTO schema_info (key, value) VALUES ('store', 'nova');
