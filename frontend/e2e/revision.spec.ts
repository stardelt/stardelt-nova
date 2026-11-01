import { test, expect } from '@playwright/test'
import { shortRevision } from '../src/format'

// A commit in a sentence (stardelt-nova#351/#354): seven characters, and the deploy's own mark
// for a working tree with uncommitted changes spelled out.

test.describe('shortRevision', () => {
  test('a commit is its first seven characters', () => {
    expect(shortRevision('0123abcdef0123abcdef0123abcdef0123abcdef')).toBe('0123abc')
    expect(shortRevision('0123abc')).toBe('0123abc')
  })

  test('uncommitted changes are said in words, not as a suffix', () => {
    expect(shortRevision('0123abcdef0123abcdef0123abcdef0123abcdef-dirty')).toBe('0123abc (uncommitted changes)')
    expect(shortRevision('0123abc-dirty')).toBe('0123abc (uncommitted changes)')
  })
})
