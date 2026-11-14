-- The run page asks which statements carry the tags pipeline=<ns>/<name> and run=<id>
-- (stardelt-nova#788 S5). A GIN index answers the containment test without a scan.
CREATE INDEX query_log_client_tags_idx ON query_log USING gin (client_tags);
