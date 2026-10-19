-- Personal settings: one JSON value per (person, key). `owner_sub` is the OIDC `sub`
-- claim (or the static dev user in `dev` mode). The key shape is also enforced in
-- `user_settings.rs`; the CHECK is the backstop.
-- NEVER edit this file after it has been merged; add 0004_*.sql instead.
CREATE TABLE user_setting (
    owner_sub  text        NOT NULL,
    key        text        NOT NULL CHECK (key ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    value      jsonb       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (owner_sub, key)
);
