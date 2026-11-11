-- One row per delivery attempt of a notification (stardelt-nova#273, slice b). The message body
-- is never stored: a destination's URL and headers are configuration, not log material.
CREATE TABLE destination_delivery (
    id             BIGSERIAL PRIMARY KEY,
    destination_id BIGINT  NOT NULL REFERENCES destination(id) ON DELETE CASCADE,
    subject_kind   TEXT    NOT NULL,
    status         TEXT    NOT NULL CHECK (status IN ('delivered', 'failed')),
    http_status    INT,
    detail         TEXT    NOT NULL,
    attempted_at   BIGINT  NOT NULL
);
CREATE INDEX destination_delivery_destination_idx
    ON destination_delivery (destination_id, attempted_at DESC);
