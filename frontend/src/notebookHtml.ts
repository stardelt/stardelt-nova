// Renders an executed notebook into one standalone HTML document (#683).
//
// The document is put into an `<iframe sandbox="allow-scripts">` with no
// `allow-same-origin`: its scripts run, but they cannot read Nova's cookies, its DOM
// or its network. Because nothing untrusted touches Nova's own DOM, output HTML and
// JavaScript go in as they are; only text that is meant to be text is escaped.
//
// See `design/scheduling-and-notebooks.md` and #519 for the render-safety decision.

import { marked } from 'marked'
import type { NotebookCell, NotebookDoc, NotebookOutput } from './api'

/** `string | string[]` as nbformat stores it. */
function join(source: string | string[] | undefined): string {
  return Array.isArray(source) ? source.join('') : source ?? ''
}

/** Text that is rendered as text, not as markup. */
function esc(text: string): string {
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;')
}

/** A traceback carries colour codes meant for a terminal, not for a page. */
function stripAnsi(text: string): string {
  return text.replace(/\x1b\[[0-9;]*m/g, '')
}

const HEAD = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; img-src data:; style-src 'unsafe-inline'; script-src 'unsafe-inline'">
<style>
  :root { color-scheme: light }
  body {
    margin: 0;
    padding: 16px;
    background: #ffffff;
    color: #1f2328;
    font-family: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", Arial, sans-serif;
    font-size: 14px;
    line-height: 1.5;
  }
  .cell { margin: 0 0 16px }
  .cell.failed { border-left: 3px solid #d1242f; padding-left: 12px }
  pre { margin: 0 0 8px; padding: 10px; border-radius: 8px; background: #f6f8fa; overflow-x: auto; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12.5px; white-space: pre-wrap; word-break: break-word }
  pre.src { background: #f6f8fa; color: #1f2328 }
  pre.err { background: #fff1f0; color: #82071e }
  pre.out { background: #f6f8fa }
  img, svg { max-width: 100% }
  table { border-collapse: collapse }
  th, td { border: 1px solid #d0d7de; padding: 4px 8px }
</style>`

const SCRIPT = `<script>const r=()=>parent.postMessage({nbHeight:document.documentElement.scrollHeight},'*');addEventListener('load',()=>{r();document.getElementById('failed')?.scrollIntoView()});new ResizeObserver(r).observe(document.body)</script>`

function outputHtml(output: NotebookOutput): string {
  switch (output.output_type) {
    case 'stream': {
      const cls = output.name === 'stderr' ? 'err' : 'out'
      return `<pre class="${cls}">${esc(join(output.text))}</pre>`
    }
    case 'execute_result':
    case 'display_data': {
      const data = output.data ?? {}
      if (data['text/html'] != null) return join(data['text/html'])
      if (data['image/svg+xml'] != null) return join(data['image/svg+xml'])
      if (data['image/png'] != null) {
        return `<img src="data:image/png;base64,${join(data['image/png'])}" alt="output">`
      }
      if (data['image/jpeg'] != null) {
        return `<img src="data:image/jpeg;base64,${join(data['image/jpeg'])}" alt="output">`
      }
      return `<pre class="out">${esc(join(data['text/plain']))}</pre>`
    }
    case 'error': {
      const traceback = stripAnsi((output.traceback ?? []).join('\n'))
      return `<pre class="err">${esc(traceback)}</pre>`
    }
    default:
      return ''
  }
}

function cellHtml(cell: NotebookCell, failed: boolean): string {
  const body: string[] = []
  if (cell.cell_type === 'markdown') {
    body.push(marked.parse(join(cell.source), { async: false }))
  } else if (cell.cell_type === 'code') {
    body.push(`<pre class="src">${esc(join(cell.source))}</pre>`)
    for (const output of cell.outputs ?? []) body.push(outputHtml(output))
  } else {
    // `raw` and anything newer: the source, as text.
    body.push(`<pre class="src">${esc(join(cell.source))}</pre>`)
  }
  const classes = failed ? 'cell failed' : 'cell'
  const id = failed ? ' id="failed"' : ''
  return `<div class="${classes}"${id}>${body.join('')}</div>`
}

/**
 * The whole notebook as one HTML document, the first failing cell marked, and — when a
 * cell failed — the cell's position (0-based) and the exception for the page above it.
 */
export function renderNotebook(doc: NotebookDoc): {
  html: string
  failedCell: number | null
  error: { ename: string; evalue: string; traceback: string } | null
} {
  const cells = doc.cells ?? []
  let failedCell: number | null = null
  let error: { ename: string; evalue: string; traceback: string } | null = null

  for (let index = 0; index < cells.length; index += 1) {
    const failure = (cells[index].outputs ?? []).find((output) => output.output_type === 'error')
    if (!failure) continue
    failedCell = index
    error = {
      ename: failure.ename ?? '',
      evalue: failure.evalue ?? '',
      traceback: stripAnsi((failure.traceback ?? []).join('\n')),
    }
    break
  }

  const body = cells.map((cell, index) => cellHtml(cell, index === failedCell)).join('\n')
  return { html: `${HEAD}\n<body>${body}\n${SCRIPT}</body>`, failedCell, error }
}
