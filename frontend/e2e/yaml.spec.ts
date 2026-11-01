import { test, expect } from '@playwright/test'
import { toYaml } from '../src/yaml'

// The YAML writer behind the pipeline definition view (stardelt-nova#351). It never starts a
// browser: it is a pure function, and what it must get right is what a reader could mistake.

test.describe('toYaml', () => {
  test('a pipeline document is block-style YAML, SQL as a block scalar', () => {
    const document = {
      name: 'daily-trips',
      ownerGroup: 'analytics',
      sql: 'select a,\n       b\nfrom t',
      schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' },
      params: { day: '2026-11-01' },
      options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
    }
    expect(toYaml(document)).toBe([
      'name: daily-trips',
      'ownerGroup: analytics',
      'sql: |-',
      '  select a,',
      '         b',
      '  from t',
      'schedule:',
      '  cron: "0 3 * * *"',
      '  timezone: Europe/Berlin',
      'params:',
      '  day: "2026-11-01"',
      'options:',
      '  retries: 1',
      '  backoffMinutes: 5',
      '  timeoutMinutes: 120',
      '  concurrency: Forbid',
      '',
    ].join('\n'))
  })

  test('a string a reader could take for something else is quoted', () => {
    const risky = ['true', 'No', 'null', '123', '2026-11-01', '', 'a: b', '#c', ' lead', 'trail ', '- dash',
      '*star', '&anchor', '!tag', '{x}', '[y]', 'say "hi"', 'back\\slash', '<img src=x onerror="a()">']
    for (const value of risky) {
      expect(toYaml({ v: value }), JSON.stringify(value)).toBe(`v: ${JSON.stringify(value)}\n`)
    }
    for (const value of ['Forbid', 'select 1', 'Europe/Berlin', 'a.b-c_d']) {
      expect(toYaml({ v: value }), value).toBe(`v: ${value}\n`)
    }
  })

  test('keys, numbers, booleans, null and empty containers', () => {
    expect(toYaml({ 'odd key': 1, ok: true, none: null, empty: {}, list: [] })).toBe(
      ['"odd key": 1', 'ok: true', 'none: null', 'empty: {}', 'list: []', ''].join('\n'),
    )
  })

  test('arrays of scalars and of objects', () => {
    expect(toYaml({ tasks: [{ name: 'run', kind: 'sql' }, 'x'], tags: ['a', 'b c'] })).toBe(
      ['tasks:', '  - name: run', '    kind: sql', '  - x', 'tags:', '  - a', '  - b c', ''].join('\n'),
    )
  })

  test('a string with a trailing line break, control characters or a leading space is not a block', () => {
    expect(toYaml({ s: 'one\ntwo\n' })).toBe('s: |\n  one\n  two\n')
    expect(toYaml({ s: 'one\n\n' })).toBe(`s: ${JSON.stringify('one\n\n')}\n`)
    expect(toYaml({ s: ' one\ntwo' })).toBe(`s: ${JSON.stringify(' one\ntwo')}\n`)
    expect(toYaml({ s: 'one\r\ntwo' })).toBe(`s: ${JSON.stringify('one\r\ntwo')}\n`)
    expect(toYaml({ s: 'bell\x07\nline' })).toBe(`s: ${JSON.stringify('bell\x07\nline')}\n`)
  })
})
