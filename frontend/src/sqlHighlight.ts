/**
 * A tokenizer for the statements people write in the SQL editor (stardelt-nova#501 slice 2).
 *
 * Pure: no React, no DOM, no package. The editor paints these tokens in a `<pre>` under a
 * transparent textarea, so the tokens must put the statement back together exactly:
 * `tokenizeSql(s).map(t => t.text).join('') === s` for every input, including an
 * unterminated string or comment.
 */

export type SqlTokenKind = 'plain' | 'keyword' | 'string' | 'comment' | 'number'

export interface SqlToken {
  text: string
  kind: SqlTokenKind
}

/** Lower-case keywords the editor colours. Everything else is an identifier. */
export const KEYWORDS: Set<string> = new Set([
  'select', 'from', 'where', 'group', 'by', 'order', 'having', 'limit', 'offset',
  'join', 'left', 'right', 'full', 'inner', 'outer', 'cross', 'on', 'as', 'and', 'or',
  'not', 'null', 'is', 'in', 'between', 'like', 'case', 'when', 'then', 'else', 'end',
  'distinct', 'union', 'all', 'with', 'insert', 'into', 'values', 'update', 'set',
  'delete', 'create', 'table', 'view', 'schema', 'drop', 'alter', 'add', 'column',
  'rename', 'comment', 'show', 'describe', 'explain', 'analyze', 'call', 'using',
  'cast', 'try_cast', 'array', 'map', 'row', 'timestamp', 'date', 'interval', 'true',
  'false', 'asc', 'desc', 'first', 'last', 'over', 'partition', 'window', 'fetch',
])

const IDENT = /[A-Za-z_]/
const IDENT_PART = /[A-Za-z0-9_]/
const DIGIT = /[0-9]/

/**
 * Scan `sql` left to right and return one token per lexeme. Characters that begin no
 * lexeme are returned as single-character `plain` tokens.
 */
export function tokenizeSql(sql: string): SqlToken[] {
  const tokens: SqlToken[] = []
  const push = (text: string, kind: SqlTokenKind) => {
    if (text) tokens.push({ text, kind })
  }

  let i = 0
  while (i < sql.length) {
    const ch = sql[i]
    const next = sql[i + 1]

    // `--` line comment: stops before the newline, which stays plain.
    if (ch === '-' && next === '-') {
      let j = i + 2
      while (j < sql.length && sql[j] !== '\n') j++
      push(sql.slice(i, j), 'comment')
      i = j
      continue
    }

    // `/* … */` block comment; an unclosed one runs to the end of the input.
    if (ch === '/' && next === '*') {
      const close = sql.indexOf('*/', i + 2)
      const j = close === -1 ? sql.length : close + 2
      push(sql.slice(i, j), 'comment')
      i = j
      continue
    }

    // `'…'` string literal; `''` is an escaped quote.
    if (ch === "'") {
      let j = i + 1
      while (j < sql.length) {
        if (sql[j] === "'") {
          if (sql[j + 1] === "'") {
            j += 2
            continue
          }
          j++
          break
        }
        j++
      }
      push(sql.slice(i, j), 'string')
      i = j
      continue
    }

    // `"…"` quoted identifier: one plain token, so a keyword inside it stays uncoloured.
    if (ch === '"') {
      let j = i + 1
      while (j < sql.length) {
        if (sql[j] === '"') {
          if (sql[j + 1] === '"') {
            j += 2
            continue
          }
          j++
          break
        }
        j++
      }
      push(sql.slice(i, j), 'plain')
      i = j
      continue
    }

    // A number: digits with an optional fraction.
    if (DIGIT.test(ch)) {
      let j = i
      while (j < sql.length && DIGIT.test(sql[j])) j++
      if (sql[j] === '.' && j + 1 < sql.length && DIGIT.test(sql[j + 1])) {
        j++
        while (j < sql.length && DIGIT.test(sql[j])) j++
      }
      push(sql.slice(i, j), 'number')
      i = j
      continue
    }

    // An identifier is a keyword only when it is one of KEYWORDS, whatever its case.
    if (IDENT.test(ch)) {
      let j = i + 1
      while (j < sql.length && IDENT_PART.test(sql[j])) j++
      const word = sql.slice(i, j)
      push(word, KEYWORDS.has(word.toLowerCase()) ? 'keyword' : 'plain')
      i = j
      continue
    }

    push(ch, 'plain')
    i++
  }

  return tokens
}
