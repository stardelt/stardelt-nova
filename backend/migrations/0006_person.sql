-- The person directory (stardelt-nova#235, person 4/4): one row per signed-in subject, written
-- best effort by the authentication middleware. `sub` is the OIDC subject and, once authz 8/8
-- is live, the engine user in the query log; `username` is the sign-in name Nova knows
-- (`preferred_username`), `display_name` the human name it shows. The query log reads
-- `display_name` so a UUID principal becomes a name. `last_seen` is refreshed at most every ten
-- minutes per person, never on every request.
-- NEVER edit this file after it has been merged: add 0007_*.sql instead.
CREATE TABLE person (
    sub          text        PRIMARY KEY,
    username     text        NOT NULL,
    display_name text        NOT NULL,
    email        text,
    last_seen    timestamptz NOT NULL DEFAULT now()
);
