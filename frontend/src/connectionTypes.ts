/**
 * The connection types the platform knows, as a person reads them.
 *
 * The API sends the raw name (`postgresql`, `rest-api`, …). An unknown one keeps its raw
 * name rather than disappearing: a type that a newer operator added is still a fact worth
 * showing. The per-type form fields arrive with the create dialog.
 */
export const CONNECTION_TYPES: Record<string, { label: string }> = {
  postgresql: { label: 'PostgreSQL' },
  mysql: { label: 'MySQL' },
  sqlserver: { label: 'SQL Server' },
  oracle: { label: 'Oracle' },
  'rest-api': { label: 'REST API' },
  s3: { label: 'Object store (S3)' },
  kafka: { label: 'Kafka' },
  sftp: { label: 'SFTP' },
  generic: { label: 'Generic' },
}

/** The label for a connection type; the raw name when the list above has never heard of it. */
export function connectionTypeLabel(type: string): string {
  return CONNECTION_TYPES[type]?.label ?? type
}
