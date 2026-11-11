-- Notification destinations (stardelt-nova#273, slice a): named email and webhook
-- endpoints owned by one group. The values are read from `config`; nothing secret
-- is stored here (header secrets wait for a Nova service identity in the store).
CREATE TABLE destination (
    id           BIGSERIAL PRIMARY KEY,
    owner_group  TEXT   NOT NULL,
    name         TEXT   NOT NULL,
    kind         TEXT   NOT NULL CHECK (kind IN ('email', 'webhook')),
    config       JSONB  NOT NULL,
    created_at   BIGINT NOT NULL,
    updated_at   BIGINT NOT NULL,
    UNIQUE (owner_group, name),
    CHECK (char_length(name) BETWEEN 1 AND 64),
    CHECK (char_length(owner_group) BETWEEN 1 AND 64)
);
CREATE INDEX destination_owner_group_idx ON destination (owner_group);
