-- Browser sessions in the state store (stardelt-nova#103 step 4, slice b), so that a
-- Nova restart does not sign everyone out. `id_hash` is the SHA-256 (hex) of the
-- cookie's session id, never the id itself. `sealed` holds the session's tokens,
-- sealed with a key derived from NOVA_SESSION_SECRET (`seal.rs`); nothing in this
-- table is readable without that secret.
CREATE TABLE browser_session (
    id_hash     TEXT   PRIMARY KEY CHECK (char_length(id_hash) = 64),
    sealed      BYTEA  NOT NULL,
    expires_at  BIGINT NOT NULL,
    updated_at  BIGINT NOT NULL
);
CREATE INDEX browser_session_expires_at_idx ON browser_session (expires_at);
