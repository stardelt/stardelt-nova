#!/usr/bin/env node
// Keeps engine words out of the notebook, pipeline and run pages (nova#516 decision 1).
// A member there reads four nouns: Notebook, Version, Schedule, Run.
// See docs: reference/glossary.

import { readFileSync } from 'node:fs'
import { dirname, join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')

const FILES = [
  'src/routes/Notebooks.tsx',
  'src/routes/NotebookDetail.tsx',
  'src/routes/Pipelines.tsx',
  'src/routes/PipelineDetail.tsx',
  'src/routes/Runs.tsx',
]

// Whole word, case-insensitive, plural allowed.
const BANNED = ['bundle', 'digest', 'target', 'object version', 'DAG', 'CRD'].map(
  (word) => new RegExp(`\\b${word.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?:e?s)?\\b`, 'gi'),
)

const JSX_TEXT = />([^<{]+)</g
const STRING_LITERAL = /'([^'\\\n]*)'|"([^"\\\n]*)"/g
const TEMPLATE_LITERAL = /`([^`]*)`/g

/** Only text a person reads: JSX text, quoted strings and templates without `${`. */
function textsOf(line) {
  const texts = []
  for (const match of line.matchAll(JSX_TEXT)) texts.push(match[1])
  for (const match of line.matchAll(STRING_LITERAL)) texts.push(match[1] ?? match[2])
  for (const match of line.matchAll(TEMPLATE_LITERAL)) {
    if (!match[1].includes('${')) texts.push(match[1])
  }
  return texts
}

let hits = 0
for (const file of FILES) {
  const lines = readFileSync(join(root, file), 'utf8').split('\n')
  const shown = relative(process.cwd(), join(root, file))
  lines.forEach((line, index) => {
    const trimmed = line.trimStart()
    if (trimmed.startsWith('//') || trimmed.startsWith('*') || trimmed.startsWith('/*')) return
    for (const text of textsOf(line)) {
      for (const re of BANNED) {
        for (const match of text.matchAll(re)) {
          hits += 1
          console.log(
            `${shown}:${index + 1}: "${match[0]}" is an engine word; use Notebook, Version, Schedule or Run (docs: reference/glossary)`,
          )
        }
      }
    }
  })
}

if (hits > 0) process.exit(1)
console.log(`vocab-lint: ${FILES.length} files clean`)
