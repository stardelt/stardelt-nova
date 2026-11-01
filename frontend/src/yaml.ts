/**
 * A small YAML writer for the documents Nova shows, such as a pipeline's editable document:
 * plain objects, arrays, strings, numbers, booleans and null, in block style.
 *
 * It is for display. Nova parses and validates on the server, so nothing here has to read YAML.
 * Strings are written plain only when no reader could take them for anything else; otherwise
 * they are quoted with JSON rules (a JSON string is a valid YAML double-quoted scalar, so every
 * quote, backslash and control character is escaped). A string with line breaks becomes a block
 * scalar, which keeps a SQL statement readable.
 */

/** Words YAML 1.1 readers turn into booleans or null. */
const RESERVED = /^(true|false|null|yes|no|on|off|y|n)$/i
/** A single-line string that cannot be mistaken for a number, a date, a tag, an alias or a map. */
const PLAIN_VALUE = /^[A-Za-z_][A-Za-z0-9 _./-]*$/
const PLAIN_KEY = /^[A-Za-z_][A-Za-z0-9_.-]*$/
const CONTROL = /[\x00-\x08\x0b-\x1f\x7f]/

type Scalar = string | number | boolean | null

function isScalar(value: unknown): value is Scalar {
  return value === null || ['string', 'number', 'boolean'].includes(typeof value)
}

function key(name: string): string {
  return PLAIN_KEY.test(name) && !RESERVED.test(name) ? name : JSON.stringify(name)
}

function scalar(value: Scalar): string {
  if (typeof value !== 'string') return String(value)
  const plain = PLAIN_VALUE.test(value) && !value.endsWith(' ') && !RESERVED.test(value)
  return plain ? value : JSON.stringify(value)
}

/** The header and the lines of a block scalar, or null when the string is better quoted. */
function block(value: string): { header: string; body: string[] } | null {
  if (!value.includes('\n') || CONTROL.test(value) || value.includes('\r')) return null
  if (value.startsWith(' ') || value.startsWith('\n')) return null
  const clipped = value.endsWith('\n') ? value.slice(0, -1) : value
  if (clipped.endsWith('\n')) return null
  return { header: value.endsWith('\n') ? '|' : '|-', body: clipped.split('\n') }
}

function lines(value: unknown, indent: number): string[] {
  const pad = ' '.repeat(indent)
  if (Array.isArray(value)) {
    return value.flatMap((item) => {
      if (isScalar(item)) return [`${pad}- ${scalar(item)}`]
      const inner = lines(item, indent + 2)
      return [`${pad}- ${inner[0].slice(indent + 2)}`, ...inner.slice(1)]
    })
  }
  if (value !== null && typeof value === 'object') {
    return Object.entries(value as Record<string, unknown>).flatMap(([name, item]) => {
      const head = `${pad}${key(name)}:`
      if (typeof item === 'string') {
        const scalarBlock = block(item)
        if (scalarBlock) {
          return [`${head} ${scalarBlock.header}`, ...scalarBlock.body.map((line) => `${pad}  ${line}`)]
        }
      }
      if (isScalar(item)) return [`${head} ${scalar(item)}`]
      if (Array.isArray(item) && item.length === 0) return [`${head} []`]
      if (item !== null && typeof item === 'object' && Object.keys(item).length === 0) return [`${head} {}`]
      return [head, ...lines(item, indent + 2)]
    })
  }
  return [`${pad}${scalar(value as Scalar)}`]
}

/** `value` as block-style YAML, ending with one line break. */
export function toYaml(value: unknown): string {
  return lines(value, 0).join('\n') + '\n'
}
