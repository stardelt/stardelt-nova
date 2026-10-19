-- Saved queries: a person's named SQL snippets. `owner_sub` is the OIDC `sub` claim
-- (or the static dev user in `dev` mode); only the owner can list or delete a row.
-- NEVER edit this file after it has been merged; add 0003_*.sql instead.
CREATE TABLE saved_query (
    id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    owner_sub  text        NOT NULL,
    title      text        NOT NULL CHECK (char_length(title) BETWEEN 1 AND 200),
    sql        text        NOT NULL CHECK (char_length(sql) BETWEEN 1 AND 100000),
    warehouse  text        NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX saved_query_owner_idx ON saved_query (owner_sub, updated_at DESC);
