// What changed between two published versions of a notebook (stardelt-nova#755, the Versions tab
// of #517): the cells whose source differs and, per cell, a line diff of the source. Outputs are
// not compared — a published version has none. Cells are matched by index (v1; the design allows
// it), so an inserted cell shows as a changed run from that index on.

import type { NotebookDoc } from './api'

/** One line of a source diff. */
export type DiffLine = { kind: 'same' | 'add' | 'del'; text: string }

/** One cell that differs between the two versions. `index` is 0-based in both documents. */
export type CellDiff = { index: number; kind: 'changed' | 'added' | 'removed'; lines: DiffLine[] }

export type NotebookDiff = { cells: CellDiff[]; changed: number; added: number; removed: number }

/** The source of one cell as plain text: an array is joined with no separator, as nbformat stores it. */
export function cellSource(cell: { source?: string | string[] }): string {
  const source = cell.source
  return Array.isArray(source) ? source.join('') : source ?? ''
}

// Past this many lines on either side the longest-common-subsequence table is quadratic memory for
// no gain a person would read: every line is reported as removed, then every line as added.
const TABLE_LIMIT = 2000

/** Line diff of two texts (longest common subsequence over lines). */
export function diffLines(before: string, after: string): DiffLine[] {
  const oldLines = before.split('\n')
  const newLines = after.split('\n')
  if (oldLines.length > TABLE_LIMIT || newLines.length > TABLE_LIMIT) {
    return [
      ...oldLines.map((text): DiffLine => ({ kind: 'del', text })),
      ...newLines.map((text): DiffLine => ({ kind: 'add', text })),
    ]
  }

  // `table[i][j]` is the length of the longest common subsequence of `oldLines[i…]` and
  // `newLines[j…]`, filled from the end so the walk below reads it forwards.
  const table: number[][] = Array.from({ length: oldLines.length + 1 }, () =>
    new Array<number>(newLines.length + 1).fill(0),
  )
  for (let i = oldLines.length - 1; i >= 0; i -= 1) {
    for (let j = newLines.length - 1; j >= 0; j -= 1) {
      table[i][j] =
        oldLines[i] === newLines[j]
          ? table[i + 1][j + 1] + 1
          : Math.max(table[i + 1][j], table[i][j + 1])
    }
  }

  const lines: DiffLine[] = []
  let i = 0
  let j = 0
  while (i < oldLines.length && j < newLines.length) {
    if (oldLines[i] === newLines[j]) {
      lines.push({ kind: 'same', text: oldLines[i] })
      i += 1
      j += 1
    } else if (table[i + 1][j] >= table[i][j + 1]) {
      lines.push({ kind: 'del', text: oldLines[i] })
      i += 1
    } else {
      lines.push({ kind: 'add', text: newLines[j] })
      j += 1
    }
  }
  while (i < oldLines.length) {
    lines.push({ kind: 'del', text: oldLines[i] })
    i += 1
  }
  while (j < newLines.length) {
    lines.push({ kind: 'add', text: newLines[j] })
    j += 1
  }
  return lines
}

const linesOf = (text: string, kind: 'add' | 'del'): DiffLine[] =>
  text.split('\n').map((line): DiffLine => ({ kind, text: line }))

/** What differs between two versions: only the cells that do, in index order. */
export function diffNotebooks(before: NotebookDoc, after: NotebookDoc): NotebookDiff {
  const cells: CellDiff[] = []
  let changed = 0
  let added = 0
  let removed = 0

  const count = Math.max(before.cells.length, after.cells.length)
  for (let index = 0; index < count; index += 1) {
    const oldCell = before.cells[index]
    const newCell = after.cells[index]
    if (oldCell && newCell) {
      const oldSource = cellSource(oldCell)
      const newSource = cellSource(newCell)
      if (oldSource === newSource) continue
      cells.push({ index, kind: 'changed', lines: diffLines(oldSource, newSource) })
      changed += 1
    } else if (newCell) {
      cells.push({ index, kind: 'added', lines: linesOf(cellSource(newCell), 'add') })
      added += 1
    } else if (oldCell) {
      cells.push({ index, kind: 'removed', lines: linesOf(cellSource(oldCell), 'del') })
      removed += 1
    }
  }

  return { cells, changed, added, removed }
}
