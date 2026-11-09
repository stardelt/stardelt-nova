-- Read-only queries become readers, not edges (lineage L2, stardelt-nova#647)
CREATE TABLE lineage_reads (
  dataset_namespace text NOT NULL,
  dataset_name text NOT NULL,
  day date NOT NULL,
  job_namespace text NOT NULL,
  job_name text NOT NULL,
  queries integer NOT NULL DEFAULT 1,
  last_read timestamptz NOT NULL,
  PRIMARY KEY (dataset_namespace, dataset_name, day, job_namespace, job_name)
);
