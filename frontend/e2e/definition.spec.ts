import { test, expect } from '@playwright/test'
import { MAX_DEFINITION_BYTES, editorRule, parseDefinition, toUpdateRequest, toValidateRequest } from '../src/definition'
import type { PipelineDocument } from '../src/api'

// The rules behind the definition editor (stardelt-nova#351). Pure functions, no browser: what
// the person typed is turned into a document or into a sentence with its line, before Nova is
// asked anything.

const OPTIONS = { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' }
const DOCUMENT: PipelineDocument = {
  name: 'daily',
  ownerGroup: 'etl',
  sql: 'select 1',
  schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' },
  params: { day: '2026-11-01' },
  options: OPTIONS,
}

test.describe('parseDefinition', () => {
  test('a document is read as it was written', () => {
    const text = [
      'name: daily', 'ownerGroup: etl', 'sql: |-', '  select a,', '         b', 'schedule:',
      '  cron: "0 3 * * *"', '  timezone: Europe/Berlin', 'params:', '  day: "2026-11-01"', '',
    ].join('\n')
    expect(parseDefinition(text)).toEqual({
      ok: true,
      document: {
        name: 'daily', ownerGroup: 'etl', sql: 'select a,\n       b',
        schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' }, params: { day: '2026-11-01' },
      },
    })
  })

  test('a syntax error names its line and carries no code frame', () => {
    const parsed = parseDefinition('name: daily\n\tsql: 1\n')
    expect(parsed.ok).toBe(false)
    if (parsed.ok) return
    expect(parsed.line).toBe(2)
    expect(parsed.message).not.toContain('\n')
    expect(parsed.message).not.toMatch(/at line \d+/)
    expect(parsed.message.length).toBeGreaterThan(0)
  })

  test('a key twice, an anchor and a text that is not a mapping are refused', () => {
    const twice = parseDefinition('name: a\nname: b\n')
    expect(twice.ok).toBe(false)
    if (!twice.ok) expect(twice.line).toBe(2)

    const anchor = parseDefinition('name: a\nparams:\n  x: &v one\n  y: *v\n')
    expect(anchor).toEqual({ ok: false, message: 'Anchors and aliases (& and *) are not allowed in a definition.', line: null })

    for (const text of ['', '- a\n- b\n', 'just words\n', '42\n']) {
      const parsed = parseDefinition(text)
      expect(parsed.ok, JSON.stringify(text)).toBe(false)
      if (!parsed.ok) expect(parsed.message).toContain('must be a mapping')
    }
  })

  test('a key that is not part of a pipeline is named, with its line, before Nova silently ignores it', () => {
    expect(parseDefinition('name: a\nshedule:\n  cron: x\n')).toEqual({
      ok: false,
      message: 'Unknown key "shedule"; the keys are name, ownerGroup, sql, notebook, schedule, params, options.',
      line: 2,
    })
  })

  test('a definition above 64 KiB is refused without being parsed', () => {
    const big = `name: a\nsql: "${'x'.repeat(MAX_DEFINITION_BYTES)}"\n`
    expect(parseDefinition(big)).toEqual({ ok: false, message: 'The definition is larger than 64 KiB.', line: null })
  })
})

test.describe('editorRule and the requests', () => {
  test('the name and the group identify the pipeline and cannot change', () => {
    expect(editorRule(DOCUMENT, DOCUMENT)).toBeNull()
    expect(editorRule({ ...DOCUMENT, name: 'other' }, DOCUMENT)).toBe('The name cannot change here: it would be another pipeline.')
    expect(editorRule({ ...DOCUMENT, ownerGroup: 'finance' }, DOCUMENT)).toBe('The group of a pipeline cannot change.')
    const nameless = { ...DOCUMENT } as Partial<PipelineDocument>
    delete nameless.name
    expect(editorRule(nameless as PipelineDocument, DOCUMENT)).toBe('The definition needs a name and an ownerGroup.')
  })

  test('the update carries the version that was read; the check does not', () => {
    expect(toUpdateRequest(DOCUMENT, '7')).toEqual({
      sql: 'select 1', schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' },
      params: { day: '2026-11-01' }, options: OPTIONS, resourceVersion: '7',
    })
    const bare = { name: 'daily', ownerGroup: 'etl', sql: 'select 1', options: OPTIONS }
    expect(toUpdateRequest(bare, null)).toEqual({ sql: 'select 1', schedule: null, params: {}, options: OPTIONS })
    expect(toValidateRequest(bare)).toEqual({
      name: 'daily', ownerGroup: 'etl', sql: 'select 1', schedule: null, options: OPTIONS,
    })
  })
})
