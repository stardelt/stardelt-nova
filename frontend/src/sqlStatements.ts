/**
 * Which statement the SQL editor runs. Trino takes one statement per request and refuses a
 * trailing `;`, so the editor sends the selection, or else the statement at the caret.
 *
 * Pure: no React, no DOM, no package. A `;` splits only outside strings ('it''s'),
 * quoted identifiers ("a;b"), line comments (-- a;b) and block comments.
 */

export interface Statement {
  /** The statement without its `;` and without surrounding whitespace. */
  text: string
  /** Offsets of `text` in the editor's whole text, so the editor can select it. */
  start: number
  end: number
}

export type RunTarget = { statement: Statement } | { error: string }

/** The pieces of `sql` between top-level `;`. A piece that holds only blanks or comments is dropped. */
export function splitStatements(sql: string): Statement[] {
  const out: Statement[] = []
  let pieceStart = 0
  let hasCode = false
  let i = 0
  const close = (endExclusive: number) => {
    if (hasCode) {
      const raw = sql.slice(pieceStart, endExclusive)
      const lead = raw.length - raw.trimStart().length
      const text = raw.trim()
      out.push({ text, start: pieceStart + lead, end: pieceStart + lead + text.length })
    }
    hasCode = false
  }
  while (i < sql.length) {
    const c = sql[i]
    const next = sql[i + 1]
    if (c === '-' && next === '-') {
      const nl = sql.indexOf('\n', i)
      i = nl === -1 ? sql.length : nl + 1
    } else if (c === '/' && next === '*') {
      const endIdx = sql.indexOf('*/', i + 2)
      i = endIdx === -1 ? sql.length : endIdx + 2
    } else if (c === "'" || c === '"') {
      hasCode = true
      i++
      while (i < sql.length) {
        if (sql[i] === c && sql[i + 1] === c) i += 2 // '' or "" inside the quotes
        else if (sql[i] === c) { i++; break }
        else i++
      }
    } else if (c === ';') {
      close(i)
      i++
      pieceStart = i
    } else {
      if (!/\s/.test(c)) hasCode = true
      i++
    }
  }
  close(sql.length)
  return out
}

/**
 * What Run sends. With a selection: the selected text, which must hold exactly one statement.
 * Without: the statement the caret is in or touches; on a blank line between two
 * statements, the one above.
 */
export function statementToRun(sql: string, selStart: number, selEnd: number): RunTarget {
  if (selEnd > selStart) {
    const inSel = splitStatements(sql.slice(selStart, selEnd))
    if (inSel.length === 0) return { error: 'The selection holds no statement.' }
    if (inSel.length > 1) {
      return { error: 'Nova runs one statement at a time. Select one, or put the cursor in it.' }
    }
    const s = inSel[0]
    return { statement: { text: s.text, start: selStart + s.start, end: selStart + s.end } }
  }
  const all = splitStatements(sql)
  if (all.length === 0) return { error: 'Write a statement first.' }
  // The last statement that starts at or before the caret; the first one if the caret is above all.
  let pick = all[0]
  for (const s of all) if (s.start <= selStart) pick = s
  return { statement: pick }
}
