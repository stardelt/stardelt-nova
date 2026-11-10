-- The catalog documentation index (design ticket stardelt-nova#104, search slice 1).
CREATE TABLE catalog_doc (
  catalog     text NOT NULL,
  namespace   text NOT NULL,
  table_name  text NOT NULL,
  description text,
  tags        text[] NOT NULL DEFAULT '{}',
  owner       text,
  domain      text,
  column_docs jsonb NOT NULL DEFAULT '[]'::jsonb,
  -- Lowercased text the search slice matches word by word: namespace.table, the
  -- description, every tag, the owner and the domain, separated by one space.
  search_text text NOT NULL DEFAULT '',
  updated_at  timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (catalog, namespace, table_name)
);
CREATE INDEX catalog_doc_tags ON catalog_doc USING gin (tags);
