-- A bundle's migration plans and their second-person approvals (stardelt-nova#617 slice e).
-- A plan row is what a plan id refers to; an approval row is one person saying "I looked".
CREATE TABLE IF NOT EXISTS migration_plan (
    plan_id    text PRIMARY KEY,
    catalog    text NOT NULL,
    target     text NOT NULL,
    created_by text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS migration_approval (
    plan_id       text NOT NULL REFERENCES migration_plan (plan_id) ON DELETE CASCADE,
    approver_sub  text NOT NULL,
    approver_name text NOT NULL DEFAULT '',
    approved_at   timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (plan_id, approver_sub)
);
