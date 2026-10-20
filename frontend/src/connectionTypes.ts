/**
 * The connection types the platform knows, as a person reads them, and the fields the
 * create dialog shows for each.
 *
 * The names are the ones the backend and the operator accept (`config` names and secret
 * keys are checked there; a mismatch answers 400). The API sends the raw name
 * (`postgresql`, `rest-api`, …). An unknown one keeps its raw name rather than
 * disappearing: a type that a newer operator added is still a fact worth showing.
 */

/** One form field: a non-secret setting (`config`) or a secret field (inside a variant). */
export type Field = {
  name: string
  label: string
  required?: boolean
  placeholder?: string
  multiline?: boolean
  number?: boolean
}

/**
 * A choice of secret fields below the settings. Most types have exactly one variant, and
 * then no choice is shown; `rest-api` and `sftp` have two.
 */
export type Variant = { id: string; label: string; secret: Field[] }

/** Everything the form needs for one type. `generic` has no fixed fields (free rows). */
export type TypeSpec = { label: string; config: Field[]; variants: Variant[] }

const USER: Field = { name: 'user', label: 'User', required: true }
const PASSWORD: Field = { name: 'password', label: 'Password', required: true }
const CREDENTIALS: Variant = { id: 'credentials', label: 'Credentials', secret: [USER, PASSWORD] }

export const CONNECTION_TYPES: Record<string, TypeSpec> = {
  postgresql: {
    label: 'PostgreSQL',
    config: [
      { name: 'host', label: 'Host', required: true },
      { name: 'port', label: 'Port', number: true, placeholder: '5432' },
      { name: 'database', label: 'Database' },
    ],
    variants: [CREDENTIALS],
  },
  mysql: {
    label: 'MySQL',
    config: [
      { name: 'host', label: 'Host', required: true },
      { name: 'port', label: 'Port', number: true, placeholder: '3306' },
      { name: 'database', label: 'Database' },
    ],
    variants: [CREDENTIALS],
  },
  sqlserver: {
    label: 'SQL Server',
    config: [
      { name: 'host', label: 'Host', required: true },
      { name: 'port', label: 'Port', number: true, placeholder: '1433' },
      { name: 'database', label: 'Database' },
    ],
    variants: [CREDENTIALS],
  },
  oracle: {
    label: 'Oracle',
    config: [
      { name: 'host', label: 'Host', required: true },
      { name: 'port', label: 'Port', number: true, placeholder: '1521' },
      { name: 'service', label: 'Service' },
    ],
    variants: [CREDENTIALS],
  },
  'rest-api': {
    label: 'REST API',
    config: [
      { name: 'baseUrl', label: 'Base URL', required: true, placeholder: 'https://api.example.com' },
    ],
    variants: [
      { id: 'token', label: 'Token', secret: [{ name: 'token', label: 'Token', required: true }] },
      {
        id: 'client-credentials',
        label: 'Client credentials',
        secret: [
          { name: 'clientId', label: 'Client ID', required: true },
          { name: 'clientSecret', label: 'Client secret', required: true },
        ],
      },
    ],
  },
  s3: {
    label: 'Object store (S3)',
    config: [
      { name: 'bucket', label: 'Bucket', required: true },
      { name: 'endpoint', label: 'Endpoint' },
      { name: 'region', label: 'Region' },
    ],
    variants: [
      {
        id: 'keys',
        label: 'Access keys',
        secret: [
          { name: 'accessKeyId', label: 'Access key ID', required: true },
          { name: 'secretAccessKey', label: 'Secret access key', required: true },
        ],
      },
    ],
  },
  kafka: {
    label: 'Kafka',
    config: [
      {
        name: 'bootstrapServers',
        label: 'Bootstrap servers',
        required: true,
        placeholder: 'broker1:9092,broker2:9092',
      },
    ],
    variants: [
      {
        id: 'credentials',
        label: 'Credentials',
        secret: [USER, PASSWORD],
      },
    ],
  },
  sftp: {
    label: 'SFTP',
    config: [
      { name: 'host', label: 'Host', required: true },
      { name: 'port', label: 'Port', number: true, placeholder: '22' },
    ],
    variants: [
      {
        id: 'password',
        label: 'Password',
        secret: [USER, PASSWORD],
      },
      {
        id: 'private-key',
        label: 'Private key',
        secret: [
          USER,
          { name: 'privateKey', label: 'Private key', required: true, multiline: true },
        ],
      },
    ],
  },
  // Free rows: settings (name and value, up to 32) and secret fields (name and a password
  // value, up to 16). The dialog renders the rows; the type carries no fixed fields.
  generic: { label: 'Generic', config: [], variants: [{ id: 'free', label: 'Free fields', secret: [] }] },
}

/** The label for a connection type; the raw name when the list above has never heard of it. */
export function connectionTypeLabel(type: string): string {
  return CONNECTION_TYPES[type]?.label ?? type
}
