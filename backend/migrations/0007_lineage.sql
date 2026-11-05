-- OpenLineage lineage graph (D40, WP-34a)
CREATE TABLE lineage_events (
  id bigserial PRIMARY KEY,
  event_time timestamptz NOT NULL,
  event_type text NOT NULL,
  job_namespace text NOT NULL,
  job_name text NOT NULL,
  run_id uuid NOT NULL,
  producer text,
  schema_url text,
  payload jsonb NOT NULL,
  received_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX lineage_events_run_idx ON lineage_events (run_id);
CREATE INDEX lineage_events_time_idx ON lineage_events (event_time DESC);
CREATE TABLE lineage_edges (
  run_id uuid NOT NULL,
  job_namespace text NOT NULL,
  job_name text NOT NULL,
  dataset_namespace text NOT NULL,
  dataset_name text NOT NULL,
  direction text NOT NULL CHECK (direction IN ('input','output')),
  event_time timestamptz NOT NULL,
  PRIMARY KEY (run_id, dataset_namespace, dataset_name, direction)
);
CREATE INDEX lineage_edges_dataset_idx ON lineage_edges (dataset_namespace, dataset_name);
