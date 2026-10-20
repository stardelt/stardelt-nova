CREATE TABLE break_glass_read (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    at          timestamptz NOT NULL DEFAULT now(),
    admin       text NOT NULL,
    admin_sub   text NOT NULL,
    owner_group text NOT NULL,
    connection  text NOT NULL,
    reason      text NOT NULL,
    outcome     text NOT NULL,
    request_id  text
);
CREATE INDEX break_glass_read_at ON break_glass_read (at DESC);
