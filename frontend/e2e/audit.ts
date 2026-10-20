import type { Page } from '@playwright/test'

/** One rule violation on one page. `rule` ids are documented in stardelt-internal/design/ui-conformance.md. */
export type Finding = { rule: string; detail: string }

/** Words and shapes that must never reach a user (rule U8). */
const DEV_TEXT: [RegExp, string][] = [
  [/\bundefined\b/, 'undefined'],
  [/\bNaN\b/, 'NaN'],
  [/\bnull\b/, 'null'],
  [/\[object Object\]/, '[object Object]'],
  [/\b1 rows\b/, '"1 rows"'],
  [/\bMVP\b/, 'MVP'],
  [/\bTODO\b/, 'TODO'],
  [/\bphase \d\b/i, 'phase N'],
  [/ApiError/, 'ApiError'],
  [/\/api\//, 'an /api/ path'],
]

/** Credentials or default logins printed on a page (rule U10). */
const SECRET_TEXT: RegExp[] = [/admin\s*\/\s*admin/i, /default (login|password)/i, /password\s*[:=]\s*\S+/i]

/** Static checks of the page as it stands; the caller has already waited for it to settle. */
export async function auditPage(page: Page): Promise<Finding[]> {
  const out: Finding[] = []
  const add = (rule: string, detail: string) => out.push({ rule, detail })

  const title = await page.title()
  if (!title) add('U2', 'the document has no title')
  const s = await page.evaluate(() => ({
    lang: document.documentElement.lang,
    h1: document.querySelectorAll('h1').length,
    main: document.querySelectorAll('main, [role=main]').length,
    overflow: document.documentElement.scrollWidth - window.innerWidth,
    text: document.body.innerText,
    badRel: [...document.querySelectorAll('a[target=_blank]')]
      .filter((a) => !/noopener|noreferrer/.test((a as HTMLAnchorElement).rel))
      .map((a) => (a as HTMLAnchorElement).href.slice(0, 80)),
  }))
  if (!s.lang) add('U3', '<html> has no lang attribute')
  if (s.h1 !== 1) add('U3', `${s.h1} <h1> elements (want exactly 1)`)
  if (s.main !== 1) add('U3', `${s.main} main landmarks (want exactly 1)`)
  if (s.overflow > 1) add('U4', `the page is ${s.overflow}px wider than the viewport`)
  for (const href of s.badRel) add('U9', `link to ${href} opens a tab without rel="noopener noreferrer"`)
  for (const [re, label] of DEV_TEXT) if (re.test(s.text)) add('U8', `visible text contains ${label}`)
  for (const re of SECRET_TEXT) {
    const m = s.text.match(re)
    if (m) add('U10', `visible text contains "${m[0]}"`)
  }

  // U5: every interactive element that is on screen has an accessible name.
  const unnamed = await page.evaluate(() => {
    const name = (el: Element): string => {
      const h = el as HTMLElement & { value?: string; type?: string }
      const aria = el.getAttribute('aria-label')?.trim()
      if (aria) return aria
      const by = el.getAttribute('aria-labelledby')
      if (by) {
        const t = by.split(/\s+/).map((i) => document.getElementById(i)?.textContent ?? '').join(' ').trim()
        if (t) return t
      }
      if (h.id) {
        const l = document.querySelector(`label[for="${CSS.escape(h.id)}"]`)
        if (l?.textContent?.trim()) return l.textContent.trim()
      }
      const wrap = el.closest('label')
      if (wrap?.textContent?.trim() && el.tagName !== 'BUTTON') return wrap.textContent.trim()
      const title = el.getAttribute('title')?.trim()
      if (title) return title
      const t = (el.textContent ?? '').trim()
      if (t) return t
      const img = el.querySelector('img[alt]') as HTMLImageElement | null
      if (img?.alt.trim()) return img.alt.trim()
      if (el.tagName === 'INPUT' && ['button', 'submit'].includes(h.type ?? '') && h.value) return h.value
      return ''
    }
    const sel = 'a[href], button, input:not([type=hidden]), select, textarea, [role=button], [role=tab], [role=switch]'
    const bad: string[] = []
    for (const el of document.querySelectorAll(sel)) {
      const r = el.getBoundingClientRect()
      if (r.width === 0 || r.height === 0) continue
      if (!name(el)) bad.push(el.outerHTML.slice(0, 100))
    }
    return bad
  })
  for (const u of new Set(unnamed)) add('U5', `no accessible name: ${u}`)

  // U7: text contrast, WCAG AA (4.5, or 3 for large text), against the real background.
  const low = await page.evaluate(() => {
    type C = { r: number; g: number; b: number; a: number }
    const parse = (c: string): C | null => {
      const m = c.match(/rgba?\(([^)]+)\)/)
      if (!m) return null
      const p = m[1].split(/[ ,/]+/).filter(Boolean).map(Number)
      return { r: p[0], g: p[1], b: p[2], a: p[3] ?? 1 }
    }
    const lum = (c: C) => {
      const f = (v: number) => ((v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4)
      return 0.2126 * f(c.r) + 0.7152 * f(c.g) + 0.0722 * f(c.b)
    }
    const over = (t: C, b: C): C => ({
      r: t.r * t.a + b.r * (1 - t.a), g: t.g * t.a + b.g * (1 - t.a), b: t.b * t.a + b.b * (1 - t.a), a: 1,
    })
    const bg = (el: Element): C => {
      const stack: C[] = []
      for (let e: Element | null = el; e; e = e.parentElement) {
        const c = parse(getComputedStyle(e).backgroundColor)
        if (c && c.a > 0) { stack.push(c); if (c.a === 1) break }
      }
      let acc: C = { r: 255, g: 255, b: 255, a: 1 }
      if (!stack.length || stack[stack.length - 1].a < 1) {
        const body = parse(getComputedStyle(document.body).backgroundColor)
        if (body && body.a > 0) acc = over(body, acc)
      }
      for (const c of stack.reverse()) acc = over(c, acc)
      return acc
    }
    const res = new Map<string, string[]>()
    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT)
    for (let n = walker.nextNode(); n; n = walker.nextNode()) {
      const t = (n.textContent ?? '').trim()
      const el = n.parentElement
      if (t.length < 2 || !el || !el.checkVisibility({ opacityProperty: true, visibilityProperty: true })) continue
      const cs = getComputedStyle(el)
      const fg = parse(cs.color)
      if (!fg) continue
      const b = bg(el)
      const L1 = lum(over(fg, b)), L2 = lum(b)
      const ratio = (Math.max(L1, L2) + 0.05) / (Math.min(L1, L2) + 0.05)
      const size = parseFloat(cs.fontSize)
      const need = size >= 24 || (size >= 18.66 && parseInt(cs.fontWeight) >= 700) ? 3 : 4.5
      if (ratio < need) {
        const k = `${ratio.toFixed(2)} < ${need}: ${cs.color} on rgb(${Math.round(b.r)}, ${Math.round(b.g)}, ${Math.round(b.b)})`
        const a = res.get(k) ?? []
        if (a.length < 3) a.push(`"${t.slice(0, 24)}"`)
        res.set(k, a)
      }
    }
    return [...res.entries()].map(([k, v]) => `${k}, e.g. ${v.join(', ')}`)
  })
  for (const l of low) add('U7', l)
  return out
}

/** U6: the first tab stops show a focus indicator (outline or ring). */
export async function focusAudit(page: Page, stops = 12): Promise<Finding[]> {
  const out = new Set<string>()
  for (let i = 0; i < stops; i++) {
    await page.keyboard.press('Tab')
    const r = await page.evaluate(() => {
      const el = document.activeElement
      if (!el || el === document.body) return null
      const cs = getComputedStyle(el)
      const visible = (cs.outlineStyle !== 'none' && parseFloat(cs.outlineWidth) > 0) || (cs.boxShadow && cs.boxShadow !== 'none')
      return { visible: !!visible, d: `${el.tagName.toLowerCase()} "${(el.getAttribute('aria-label') || el.textContent || '').trim().slice(0, 30)}"` }
    })
    if (r && !r.visible) out.add(r.d)
  }
  return [...out].map((d) => ({ rule: 'U6', detail: `no focus indicator on ${d}` }))
}
