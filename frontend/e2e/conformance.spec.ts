import { test, expect, type Browser, type Locator, type Page, type Route } from '@playwright/test'
import { KNOWN, type Known } from './known'
import { auditPage, focusAudit, type Finding } from './audit'
import { formatTime, relativeTime, setTimeDisplay, sqlTableRef } from '../src/format'

// Rules U1–U13 are written down in stardelt-internal/design/ui-conformance.md.

const ROUTES = ['/', '/catalog', '/query', '/data', '/connections', '/jobs', '/pipelines', '/queries',
  '/apps', '/admin/catalogs', '/lineage', '/dashboards', '/notebooks', '/health', '/settings', '/preferences', '/profile',
  '/no-such-page']
const VARIANTS = [
  { name: 'desktop-dark', viewport: { width: 1280, height: 800 }, theme: 'dark', keyboard: true },
  { name: 'desktop-light', viewport: { width: 1280, height: 800 }, theme: 'light', keyboard: false },
  // The two neutral themes (stardelt-nova#221): every page, every rule, including U7.
  { name: 'desktop-graphite', viewport: { width: 1280, height: 800 }, theme: 'graphite', keyboard: false },
  { name: 'desktop-silver', viewport: { width: 1280, height: 800 }, theme: 'silver', keyboard: false },
  { name: 'phone-dark', viewport: { width: 360, height: 740 }, theme: 'dark', keyboard: false },
]
const matched = new Set<Known>()

/** Splits findings into known (reported) and new (fail the test). */
function judge(route: string, findings: Finding[]) {
  const fresh: string[] = []
  for (const f of findings) {
    const k = KNOWN.find((e) => e.rule === f.rule && (!e.route || e.route === route) && (!e.contains || f.detail.includes(e.contains)))
    if (k) {
      matched.add(k)
      test.info().annotations.push({ type: 'known', description: `${k.ticket} ${route} ${f.rule}: ${f.detail}` })
    } else fresh.push(`${f.rule} ${route}: ${f.detail}`)
  }
  expect(fresh, 'new findings: fix the page, do not add them to e2e/known.ts').toEqual([])
}

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

test.describe('every page, every variant', () => {
  for (const v of VARIANTS) {
    for (const route of ROUTES) {
      test(`${v.name} ${route}`, async ({ browser }) => {
        const ctx = await browser.newContext({ viewport: v.viewport })
        await ctx.addInitScript((t) => { try { localStorage.setItem('stardelt-nova:theme', t) } catch { /* ignore */ } }, v.theme)
        const page = await ctx.newPage()
        const found: Finding[] = []
        // U1: no uncaught exception, no console error. The browser's own "Failed to load resource"
        // lines only mirror a failed request; U12 judges the status code instead.
        page.on('console', (m) => {
          if (['error', 'warning'].includes(m.type()) && !m.text().startsWith('Failed to load resource'))
            found.push({ rule: 'U1', detail: m.text().slice(0, 160) })
        })
        page.on('pageerror', (e) => found.push({ rule: 'U1', detail: `uncaught: ${String(e).slice(0, 160)}` }))
        // U12: an unreachable upstream is a 502/503/504, never a 500 (a 500 means a bug in Nova).
        page.on('response', (r) => {
          if (r.status() === 500 && r.url().includes('/api/'))
            found.push({ rule: 'U12', detail: `500 from ${new URL(r.url()).pathname}` })
        })
        await page.goto(route)
        // U11: the answer (data or an error) is on screen within 6 s; "loading" never stays.
        await page.waitForFunction(() => !/loading/i.test(document.body.innerText), null, { timeout: 6000 })
          .catch(() => found.push({ rule: 'U11', detail: 'still "loading" 6 s after the page opened' }))
        await page.waitForTimeout(500)
        if (route !== '/' && (await page.title()) === 'stardelt Nova')
          found.push({ rule: 'U2', detail: 'the title does not name the page' })
        found.push(...(await auditPage(page)))
        if (v.keyboard) found.push(...(await focusAudit(page)))
        await ctx.close()
        judge(route, found)
      })
    }
  }
})

test.describe('keyboard', () => {
  test('U17 the first tab stop is a Skip to content link that focuses main', async ({ browser }) => {
    const page = await (await browser.newContext({ viewport: { width: 1280, height: 800 } })).newPage()
    await page.goto('/')
    await expect(page.getByRole('link', { name: 'Skip to content' })).toBeAttached()

    await page.keyboard.press('Tab')
    const focused = page.locator(':focus')
    await expect(focused).toHaveText('Skip to content')
    const box = await focused.boundingBox()
    const viewport = page.viewportSize()!
    expect(box).not.toBeNull()
    expect(box!.x).toBeGreaterThanOrEqual(0)
    expect(box!.y).toBeGreaterThanOrEqual(0)
    expect(box!.x + box!.width).toBeLessThanOrEqual(viewport.width)
    expect(box!.y + box!.height).toBeLessThanOrEqual(viewport.height)

    await page.keyboard.press('Enter')
    expect(await page.evaluate(() => document.activeElement?.id)).toBe('main')
    await page.context().close()
  })
})

test.describe('project footer', () => {
  // The project names itself at the bottom of the sidebar (stardelt-nova#268): the version
  // that is running, and where to read more or report a problem. The version is the build's
  // own — the frontend package's version, baked in by `vite.config.ts` — so nothing is
  // asked of the network to show it.
  const DOCS = 'https://stardelt.io'
  const SOURCE = 'https://github.com/stardelt/stardelt'
  const LINKS = [['Documentation', DOCS], ['Source', SOURCE]] as const

  test('the footer names the project, its version and the two places to read more', async ({ browser }) => {
    const page = await (await browser.newContext({ viewport: { width: 1280, height: 800 } })).newPage()
    const outbound: string[] = []
    page.on('request', (request) => {
      const url = request.url()
      if (url.startsWith('https://stardelt.io') || url.startsWith('https://github.com')) outbound.push(url)
    })
    await page.goto('/')

    const footer = page.locator('#primary-nav footer')
    await expect(footer).toBeVisible()
    // The version of the frontend package (frontend/package.json), read at build time.
    await expect(footer).toContainText('stardelt v0.1.0')

    for (const [name, href] of LINKS) {
      const link = footer.getByRole('link', { name })
      await expect(link).toBeVisible()
      await expect(link).toHaveAttribute('href', href)
      await expect(link).toHaveAttribute('target', '_blank')
      await expect(link).toHaveAttribute('rel', 'noopener noreferrer')
    }
    // Nothing left the browser: the links are anchors, and the version is in the build.
    expect(outbound).toEqual([])
    await page.context().close()
  })

  test('both links are reachable with the keyboard at 360 px', async ({ browser }) => {
    const page = await (await browser.newContext({ viewport: { width: 360, height: 740 } })).newPage()
    await page.goto('/')
    // On a phone the sidebar is a drawer; open it so the links are where a person can see them.
    await page.getByRole('button', { name: 'Open menu' }).click()
    await expect(page.getByRole('button', { name: 'Close menu' })).toBeVisible()
    // The drawer slides in (`transition-transform`): measure after it has stopped, or a link that is
    // still on its way in has a negative x and the test fails on a slow runner.
    await page.waitForFunction(() => document.getAnimations().every((a) => a.playState !== 'running'))

    for (const [name] of LINKS) {
      await expect(page.locator('#primary-nav footer').getByRole('link', { name })).toBeVisible()
    }

    // Walk the tab order from the top of the document: the navigation comes first, the
    // footer links last.
    await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur())
    const reached: string[] = []
    for (let i = 0; i < 60 && reached.length < LINKS.length; i++) {
      await page.keyboard.press('Tab')
      const focused = await page.evaluate(() => {
        const el = document.activeElement as HTMLElement | null
        if (!el || el.tagName !== 'A') return null
        const style = getComputedStyle(el)
        return {
          text: (el.textContent ?? '').trim(),
          // U6: the focused link shows where the keyboard is.
          indicator: (style.outlineStyle !== 'none' && parseFloat(style.outlineWidth) > 0) || style.boxShadow !== 'none',
        }
      })
      if (!focused || !LINKS.some(([name]) => name === focused.text)) continue
      reached.push(focused.text)
      expect(focused.indicator, `${focused.text} has no focus indicator`).toBe(true)
      // The focused link is on the screen, inside the 360 px phone.
      const box = await page.getByRole('link', { name: focused.text }).boundingBox()
      expect(box).not.toBeNull()
      expect(box!.x).toBeGreaterThanOrEqual(0)
      expect(box!.x + box!.width).toBeLessThanOrEqual(360)
    }
    expect(reached).toEqual(['Documentation', 'Source'])
    await page.context().close()
  })
})

test.describe('roles', () => {
  test('U13 a signed-in non-admin sees no admin surface and calls no admin endpoint', async ({ browser }) => {
    const page = await (await browser.newContext({ viewport: { width: 1280, height: 800 } })).newPage()
    const hits: string[] = []
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      hits.push(path)
      if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analysts'] })
      if (/^\/api\/(workloads|trino\/cluster)/.test(path) || (path.startsWith('/api/apps') && path !== '/api/apps/links'))
        return json(r, { error: 'the administrator role is required' }, 403)
      return json(r, path.includes('namespaces') ? { namespaces: [] } : [])
    })
    await page.goto('/')
    await page.waitForTimeout(2000)
    const nav = await page.evaluate(() => [...document.querySelectorAll('nav a')].map((a) => a.textContent?.trim()))
    expect(nav.filter((t) => /^(Apps|Health|Settings)$/.test(t ?? ''))).toEqual([])
    expect(hits.filter((h) => /^\/api\/(workloads|trino\/cluster)/.test(h) || (h.startsWith('/api/apps') && h !== '/api/apps/links'))).toEqual([])
    const text = await page.evaluate(() => document.body.innerText)
    expect(text).not.toMatch(/not installed/i)
    for (const route of ['/apps', '/health', '/settings']) {
      await page.goto(route)
      await expect(page.getByText(/for platform administrators/i)).toBeVisible()
    }
  })
})

test.describe('menus', () => {
  test('U16 the Columns menu closes with Escape and returns focus to its opener', async ({ browser }) => {
    const page = await (await browser.newContext({ viewport: { width: 1280, height: 800 } })).newPage()
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
      if (path === '/api/query-log') return json(r, { items: [], nextCursor: null })
      return json(r, [])
    })
    await page.goto('/queries')
    const summary = page.locator('summary', { hasText: 'Columns' })
    const details = summary.locator('..')
    await summary.focus()
    await page.keyboard.press('Enter')
    await expect(details).toHaveJSProperty('open', true)
    await page.keyboard.press('Escape')
    await expect(details).toHaveJSProperty('open', false)
    await expect(summary).toBeFocused()
    // A click on the page outside the menu dismisses it as well.
    await summary.click()
    await expect(details).toHaveJSProperty('open', true)
    await page.locator('table').click({ position: { x: 10, y: 10 } })
    await expect(details).toHaveJSProperty('open', false)
  })
})

test.describe('hostile data', () => {
  test('U14 markup and 400-character names render as text and do not widen the page', async ({ browser }) => {
    const page = await (await browser.newContext({ viewport: { width: 1280, height: 800 } })).newPage()
    await page.addInitScript(() => { (window as unknown as { __xss: number }).__xss = 0 })
    const evil = '<img src=x onerror="window.__xss=1">'
    const long = 'x'.repeat(400)
    const now = Date.now()
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      if (path === '/api/me') return json(r, { user: evil, auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true })
      if (path === '/api/catalog/warehouse') return json(r, { name: 'warehouse', id: 'w' })
      if (path === '/api/catalog/namespaces') return json(r, { namespaces: [[evil], [long]] })
      if (/\/tables$/.test(path)) return json(r, { identifiers: [{ namespace: [evil], name: evil }, { namespace: [long], name: long }] })
      if (path === '/api/query-log') {
        return json(r, { nextCursor: null, items: [
          { id: 'a1', state: 'FAILED', user: evil, source: evil, warehouse: long, queryType: 'SELECT', sql: `select '${evil}' ${long}`, createdAt: now - 60000, wallMs: 1200, outputRows: 1, errorCode: 'SYNTAX_ERROR' },
          { id: 'a2', state: 'FINISHED', user: 'alice', source: null, warehouse: 'warehouse', queryType: 'SELECT', sql: 'select 1', createdAt: now - 5000, wallMs: 5, outputRows: 0, errorCode: null },
          { id: 'a3', state: 'QUEUED', user: 'alice', source: 'trino-python-client', warehouse: 'warehouse', queryType: null, sql: 'select 2', createdAt: now - 2000, wallMs: null, outputRows: null, errorCode: null },
        ] })
      }
      if (path === '/api/jobs') return json(r, [{ id: '1', kind: 'query', title: evil, status: 'failed', started_at: now, detail: evil, rows: 1, user: evil }])
      return json(r, [])
    })
    for (const route of ['/', '/catalog', '/queries', '/jobs']) {
      await page.goto(route)
      await page.waitForTimeout(1500)
      const s = await page.evaluate(() => ({
        xss: (window as unknown as { __xss: number }).__xss,
        imgs: document.querySelectorAll('img[src="x"]').length,
        overflow: document.documentElement.scrollWidth - window.innerWidth,
      }))
      expect(s, route).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
      expect(s.overflow, `${route} overflow`).toBeLessThanOrEqual(1)
      if (route === '/queries') {
        const text = await page.evaluate(() => document.querySelector('main')?.innerText ?? '')
        // U15: a status is never carried by colour alone.
        for (const word of ['Failed', 'Succeeded', 'In progress']) expect(text).toContain(word)
      }
    }
  })

  test('U14 connection names and settings render as text', async ({ browser }) => {
    const page = await (await browser.newContext({ viewport: { width: 1280, height: 800 } })).newPage()
    await page.addInitScript(() => { (window as unknown as { __xss: number }).__xss = 0 })
    const evil = '<img src=x onerror="window.__xss=1">'
    const long = 'x'.repeat(400)
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
      if (path === '/api/connections') {
        return json(r, { connections: [
          // One row per scope: the name, the setting values and the owning group are markup.
          { name: evil, scope: 'personal', type: 'postgresql', config: { host: evil, port: '5432' }, secretKeys: ['password'], version: 1 },
          { name: 'team-db', scope: 'group', type: 'mysql', ownerGroup: evil, config: { host: 'db.internal' }, secretKeys: ['password'], version: 1, sync: { ready: true, reason: 'Synced' } },
          // A setting that is one 400-character word must wrap or scroll, never widen the page.
          { name: 'platform-s3', scope: 'managed', type: 's3', config: { bucket: long }, secretKeys: ['secretAccessKey'], version: 1, sync: { ready: false, reason: 'ReconcileFailed', message: evil } },
        ] })
      }
      return json(r, [])
    })
    for (const width of [1280, 360]) {
      await page.setViewportSize({ width, height: 800 })
      await page.goto('/connections')
      await page.waitForTimeout(1000)
      const s = await page.evaluate(() => ({
        xss: (window as unknown as { __xss: number }).__xss,
        imgs: document.querySelectorAll('img[src="x"]').length,
        overflow: document.documentElement.scrollWidth - window.innerWidth,
      }))
      expect(s, `${width}px`).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
      expect(s.overflow, `${width}px overflow`).toBeLessThanOrEqual(1)
      const text = await page.locator('main').innerText()
      // A status is the word, not only the colour; a failing sync says why on a second line.
      for (const word of ['Stored', 'Ready', 'Not ready', evil]) expect(text, `${width}px: ${word}`).toContain(word)
      // The Secret fields column holds the field names, never a value.
      expect(text).toContain('secretAccessKey')
    }
  })
})

test.describe('errors', () => {
  test('U8 a failed statement shows the engine message and no API path', async ({ browser }) => {
    const page = await (await browser.newContext({ viewport: { width: 1280, height: 800 } })).newPage()
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      if (path === '/api/me') return json(r, { user: 'qa', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse' })
      if (path === '/api/query') return json(r, { error: "line 1:8: Column 'x' cannot be resolved" }, 400)
      return json(r, [])
    })
    await page.goto('/query')
    await page.getByRole('button', { name: /^Run/ }).click()
    const main = page.locator('main')
    await expect(main).toContainText('cannot be resolved')
    const text = await main.innerText()
    const found: Finding[] = []
    if (/ApiError/.test(text)) found.push({ rule: 'U8', detail: 'visible text contains ApiError' })
    if (/\/api\/|→ \d{3}/.test(text)) found.push({ rule: 'U8', detail: 'visible text contains an /api/ path' })
    judge('/query', found)
  })

  test('U11 a failed load says why and offers Retry', async ({ browser }) => {
    // The first Retry on each page belongs to this endpoint, in the order the page renders them.
    const failing: Record<string, string> = {
      '/catalog': '/api/catalog/namespaces',
      '/jobs': '/api/jobs',
      '/queries': '/api/query-log',
      '/health': '/api/apps',
      '/apps': '/api/apps',
    }
    for (const route of Object.keys(failing)) {
      const page = await (await browser.newContext({ viewport: { width: 1280, height: 800 } })).newPage()
      const hits = new Map<string, number>()
      await page.route('**/api/**', (r) => {
        const path = new URL(r.request().url()).pathname
        if (path === '/api/me') return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'] })
        hits.set(path, (hits.get(path) ?? 0) + 1)
        return json(r, { error: 'the upstream is not reachable' }, 502)
      })
      await page.goto(route)
      // The sentence the API sent, not a path or a status, and in time to read it (rule U11).
      await expect(page.getByText('the upstream is not reachable').first(), route).toBeVisible({ timeout: 4000 })
      const retry = page.getByRole('button', { name: 'Retry' }).first()
      await expect(retry, route).toBeVisible()
      const before = hits.get(failing[route]) ?? 0
      await retry.click()
      await expect.poll(() => hits.get(failing[route]) ?? 0, { message: `${route}: Retry asked again` }).toBeGreaterThan(before)
      await page.context().close()
    }
  })
})

test.describe('connections', () => {
  // The canary is the only secret value in the scenarios. `P1` fails if any of these
  // scenarios lets it out of the password field and the one request that carries it.
  const CANARY = 'canary-7f3a-DO-NOT-LEAK'
  const ADMIN = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: true, groups: ['analysts', 'data-eng'],
  }

  type Recorded = { method: string; url: string; body: string | null }

  /** A `/connections` page with every `/api/**` call stubbed; writes are the test's job. */
  async function open(browser: Browser, {
    me = ADMIN,
    rows = [] as unknown[],
    write,
    init,
  }: {
    me?: unknown
    rows?: unknown[]
    write?: (route: Route, path: string, method: string, body: string | null) => boolean
    init?: () => void
  } = {}) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    if (init) await page.addInitScript(init)
    const requests: Recorded[] = []
    const messages: string[] = []
    page.on('request', (request) => requests.push({
      method: request.method(),
      url: request.url(),
      body: request.postData(),
    }))
    page.on('console', (message) => messages.push(message.text()))
    await page.route('**/api/**', (route) => {
      const request = route.request()
      const path = new URL(request.url()).pathname
      const method = request.method()
      if (path === '/api/me' && method === 'GET') return json(route, me)
      if (path === '/api/connections' && method === 'GET') return json(route, { connections: rows })
      if (method !== 'GET' && write?.(route, path, method, request.postData())) return
      return json(route, [])
    })
    await page.goto('/connections')
    await expect(page.locator('main')).not.toContainText('Loading connections')
    return { context, page, requests, messages }
  }

  async function fillNewConnection(page: Page, name = 'warehouse-db') {
    await page.getByRole('button', { name: 'New connection' }).click()
    await page.getByRole('radio', { name: 'Group' }).check()
    await page.getByLabel('Owner group').selectOption('analysts')
    await page.getByLabel('Type').selectOption('postgresql')
    await page.getByLabel('Name').fill(name)
    await page.getByLabel('Host').fill('db.internal')
    await page.getByLabel('User').fill('svc')
    await page.getByLabel('Password').fill(CANARY)
  }

  test('P1 creating a connection never leaves the value anywhere', async ({ browser }) => {
    const { context, page, requests, messages } = await open(browser, {
      write: (route, path, method) => {
        if (path !== '/api/connections' || method !== 'POST') return false
        json(route, { name: 'warehouse-db', scope: 'group', type: 'postgresql', ownerGroup: 'analysts', config: {}, secretKeys: [], version: 1 }, 201)
        return true
      },
    })
    await expect(page.getByRole('heading', { name: 'Managed' })).toBeVisible()
    await fillNewConnection(page)
    // The field masks the value before anything is sent.
    await expect(page.getByLabel('Password')).toHaveAttribute('type', 'password')
    await page.getByRole('button', { name: 'Create connection' }).click()
    await expect(page.getByRole('status')).toHaveText('Created warehouse-db.')

    const html = await page.evaluate(() => document.documentElement.outerHTML)
    expect(html).not.toContain(CANARY)
    const stored = await page.evaluate(() => {
      const dump = (storage: Storage) => Object.fromEntries(
        Array.from({ length: storage.length }, (_, index) => {
          const key = storage.key(index) ?? ''
          return [key, storage.getItem(key) ?? '']
        }),
      )
      return JSON.stringify({ local: dump(localStorage), session: dump(sessionStorage) })
    })
    expect(stored).not.toContain(CANARY)
    expect(await page.evaluate(() => location.href)).not.toContain(CANARY)
    expect(messages.filter((message) => message.includes(CANARY))).toEqual([])

    // Exactly one request carries the value: the POST. No URL carries it.
    const carrying = requests.filter((request) => (request.body ?? '').includes(CANARY))
    expect(carrying).toHaveLength(1)
    expect(carrying[0].method).toBe('POST')
    expect(new URL(carrying[0].url).pathname).toBe('/api/connections')
    expect(requests.filter((request) => request.url.includes(CANARY))).toEqual([])
    expect(JSON.parse(carrying[0].body ?? '{}').secret).toEqual({ user: 'svc', password: CANARY })
    await context.close()
  })

  test('P1 a failed create keeps the value out of the error text', async ({ browser }) => {
    const { context, page, messages } = await open(browser, {
      write: (route, path, method) => {
        if (path !== '/api/connections' || method !== 'POST') return false
        json(route, { error: 'host must not be empty' }, 400)
        return true
      },
    })
    await fillNewConnection(page)
    await page.getByRole('button', { name: 'Create connection' }).click()

    const alert = page.getByRole('alert')
    await expect(alert).toContainText('host must not be empty')
    expect(await alert.innerText()).not.toContain(CANARY)
    expect(messages.filter((message) => message.includes(CANARY))).toEqual([])
    // The dialog stays open and keeps what was typed so it can be fixed.
    await expect(page.getByRole('dialog')).toBeVisible()
    await expect(page.getByLabel('Password')).toHaveValue(CANARY)
    await context.close()
  })

  test('P1 rotate sends only the filled fields for a group connection and every field for a managed one', async ({ browser }) => {
    const rotate = { name: '', scope: 'personal', type: 'postgresql', config: {}, secretKeys: ['user', 'password'], version: 2 }
    const { context, page, requests } = await open(browser, {
      rows: [
        { name: 'team-db', scope: 'group', type: 'postgresql', ownerGroup: 'analysts', config: { host: 'db' }, secretKeys: ['user', 'password'], version: 1, sync: { ready: true, reason: 'Synced' } },
        { name: 'platform-s3', scope: 'managed', type: 's3', config: { bucket: 'b' }, secretKeys: ['accessKeyId', 'secretAccessKey'], version: 1, sync: { ready: true, reason: 'Synced' } },
      ],
      write: (route, path, method) => {
        if (method !== 'PUT') return false
        json(route, { ...rotate, name: path.includes('team-db') ? 'team-db' : 'platform-s3' })
        return true
      },
    })

    await page.getByRole('button', { name: 'Rotate team-db' }).click()
    await page.getByLabel('password').fill(CANARY)
    await page.getByRole('button', { name: 'Rotate connection' }).click()
    await expect(page.getByRole('status')).toHaveText('Rotated team-db.')

    await page.getByRole('button', { name: 'Rotate platform-s3' }).click()
    await expect(page.getByText('A managed connection cannot be read back, so enter every field.')).toBeVisible()
    const managedButton = page.getByRole('button', { name: 'Rotate connection' })
    await expect(managedButton).toBeDisabled()
    await page.getByLabel('accessKeyId').fill(CANARY)
    await expect(managedButton).toBeDisabled()
    await page.getByLabel('secretAccessKey').fill(`${CANARY}-2`)
    await expect(managedButton).toBeEnabled()
    await managedButton.click()
    await expect(page.getByRole('status')).toHaveText('Rotated platform-s3.')

    const puts = requests.filter((request) => request.method === 'PUT')
    expect(puts).toHaveLength(2)
    expect(new URL(puts[0].url).pathname).toBe('/api/connections/group/analysts/team-db/secret')
    expect(Object.keys(JSON.parse(puts[0].body ?? '{}').secret)).toEqual(['password'])
    expect(new URL(puts[1].url).pathname).toBe('/api/connections/managed/platform-s3/secret')
    expect(Object.keys(JSON.parse(puts[1].body ?? '{}').secret).sort()).toEqual(['accessKeyId', 'secretAccessKey'])
    await context.close()
  })

  test('P4 delete names the object, asks once, and Escape cancels', async ({ browser }) => {
    const { context, page, requests } = await open(browser, {
      rows: [
        { name: 'warehouse-db', scope: 'group', type: 'postgresql', ownerGroup: 'analysts', config: { host: 'db' }, secretKeys: ['user', 'password'], version: 1, sync: { ready: true, reason: 'Synced' } },
      ],
      write: (route, _path, method) => {
        if (method !== 'DELETE') return false
        void route.fulfill({ status: 204 })
        return true
      },
    })
    const del = page.getByRole('button', { name: 'Delete warehouse-db' })
    await del.click()
    const dialog = page.getByRole('dialog')
    await expect(dialog).toBeVisible()
    await expect(dialog).toContainText('Delete warehouse-db?')
    await expect(dialog).toContainText('Notebooks that use it will fail.')

    await page.keyboard.press('Escape')
    await expect(dialog).toHaveCount(0)
    expect(requests.filter((request) => request.method === 'DELETE')).toEqual([])
    await expect(del).toBeFocused()

    await del.click()
    await page.getByRole('dialog').getByRole('button', { name: 'Delete connection' }).click()
    await expect(page.getByRole('status')).toHaveText('Deleted warehouse-db.')
    const deletes = requests.filter((request) => request.method === 'DELETE')
    expect(deletes).toHaveLength(1)
    expect(new URL(deletes[0].url).pathname).toBe('/api/connections/group/analysts/warehouse-db')
    await context.close()
  })

  test('a non-admin has no Managed scope', async ({ browser }) => {
    const { context, page } = await open(browser, {
      me: {
        user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
        is_admin: false, groups: ['analysts', 'data-eng'],
      },
    })
    await page.getByRole('button', { name: 'New connection' }).click()
    await expect(page.getByRole('dialog')).toBeVisible()
    expect(await page.getByRole('radio', { name: 'Managed' }).count()).toBe(0)
    expect(await page.getByRole('heading', { name: 'Managed' }).count()).toBe(0)
    await context.close()
  })

  // stardelt-nova#204: an administrator sees every group's connections (backend #203). A row of
  // a group they are not in is visible and deletable, readable through break-glass, and never
  // rotatable here.
  const ADMIN_OF_ANALYTICS = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: true, groups: ['analytics'],
  }
  const ANALYTICS_ROW = {
    name: 'analytics-db', scope: 'group', type: 'postgresql', ownerGroup: 'analytics',
    config: { host: 'db.analytics' }, secretKeys: ['user', 'password'], version: 1,
    member: true, sync: { ready: true, reason: 'Synced' },
  }
  const FINANCE_ROW = {
    name: 'finance-db', scope: 'group', type: 'postgresql', ownerGroup: 'finance',
    config: { host: 'db.finance' }, secretKeys: ['password'], version: 1,
    member: false, sync: { ready: true, reason: 'Synced' },
  }
  /** The section around a `Group: <name>` heading. */
  const groupSection = (page: Page, name: string) =>
    page.locator('section').filter({ has: page.getByRole('heading', { name: new RegExp(`^Group: ${name}`) }) })

  test("an administrator sees other groups' rows without Rotate", async ({ browser }) => {
    const { context, page } = await open(browser, {
      me: ADMIN_OF_ANALYTICS,
      rows: [ANALYTICS_ROW, FINANCE_ROW],
    })

    const finance = groupSection(page, 'finance')
    await expect(finance.getByRole('heading', { name: /^Group: finance/ })).toBeVisible()
    await expect(finance.getByText('Not your group', { exact: true })).toBeVisible()
    await expect(finance).toContainText(
      'You see this because you are an administrator. You cannot create or rotate here; you can delete it in an incident or read it with a break-glass read, which tells the group.',
    )
    await expect(finance.getByRole('button', { name: 'Delete finance-db' })).toBeVisible()
    await expect(finance.getByRole('button', { name: 'Break-glass read finance-db' })).toBeVisible()
    expect(await finance.getByRole('button', { name: 'Rotate finance-db' }).count()).toBe(0)
    // The person cannot use the credential either, so the notebook snippet is absent too.
    expect(await finance.getByText('Use in a notebook').count()).toBe(0)

    const analytics = groupSection(page, 'analytics')
    await expect(analytics.getByText('Not your group')).toHaveCount(0)
    await expect(analytics.getByRole('button', { name: 'Rotate analytics-db' })).toBeVisible()
    await expect(analytics.getByRole('button', { name: 'Delete analytics-db' })).toBeVisible()
    await expect(analytics.getByRole('button', { name: 'Break-glass read analytics-db' })).toBeVisible()
    await expect(analytics.getByText('Use in a notebook')).toBeVisible()

    // The page, its badge and its extra buttons keep an accessible name (rule U5).
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    await context.close()
  })

  test('the banner has no Rotate now for a non-member', async ({ browser }) => {
    const { context, page } = await open(browser, {
      me: ADMIN_OF_ANALYTICS,
      rows: [{
        ...FINANCE_ROW,
        breakGlass: { by: 'alice', at: '2026-10-20T12:00:00Z', reason: 'incident 4711: the pipeline is blocked' },
      }],
    })
    const banner = page.getByRole('status')
    await expect(banner).toContainText('Read by')
    await expect(banner).toContainText('incident 4711: the pipeline is blocked.')
    await expect(banner).toContainText('Consider rotating the credential.')
    expect(await banner.getByRole('button', { name: 'Rotate now' }).count()).toBe(0)
    // The group's own members still get the action (the same banner is only informational for us).
    await context.close()
  })

  test('an administrator may delete another group connection from its row', async ({ browser }) => {
    const { context, page, requests } = await open(browser, {
      me: ADMIN_OF_ANALYTICS,
      rows: [FINANCE_ROW],
      write: (route, _path, method) => {
        if (method !== 'DELETE') return false
        void route.fulfill({ status: 204 })
        return true
      },
    })
    await page.getByRole('button', { name: 'Delete finance-db' }).click()
    const dialog = page.getByRole('dialog')
    await expect(dialog).toContainText('Delete finance-db?')
    await expect(dialog).toContainText('You are deleting a connection of a group you are not in.')
    await dialog.getByRole('button', { name: 'Delete connection' }).click()
    await expect(page.getByRole('status')).toHaveText('Deleted finance-db.')

    const deletes = requests.filter((request) => request.method === 'DELETE')
    expect(deletes).toHaveLength(1)
    expect(new URL(deletes[0].url).pathname).toBe('/api/connections/group/finance/finance-db')
    await context.close()
  })

  test("an administrator may break-glass read another group's connection", async ({ browser }) => {
    const { context, page, requests } = await open(browser, {
      me: ADMIN_OF_ANALYTICS,
      rows: [FINANCE_ROW],
      write: (route, path, method) => {
        if (method !== 'POST' || !path.endsWith('/break-glass')) return false
        json(route, { fields: { password: CANARY }, readAt: '2026-10-20T12:00:00Z' })
        return true
      },
    })
    await page.getByRole('button', { name: 'Break-glass read finance-db' }).click()
    const dialog = page.getByRole('dialog', { name: 'Break-glass read of finance-db' })
    await dialog.getByLabel('Reason').fill('incident 4711: the pipeline is blocked')
    await dialog.getByRole('button', { name: 'Read the credential' }).click()
    const answer = page.getByRole('dialog', { name: 'Credential finance-db' })
    await expect(answer).toContainText('••••••••')
    // The value is masked until Show; the list request carries no value.
    expect(await page.evaluate(() => document.documentElement.outerHTML)).not.toContain(CANARY)

    const posts = requests.filter((request) => request.method === 'POST')
    expect(posts).toHaveLength(1)
    expect(new URL(posts[0].url).pathname).toBe('/api/connections/group/finance/finance-db/break-glass')
    await context.close()
  })

  test('the new connection dialog lists only my groups', async ({ browser }) => {
    const { context, page } = await open(browser, { me: ADMIN_OF_ANALYTICS })
    await page.getByRole('button', { name: 'New connection' }).click()
    await page.getByRole('radio', { name: 'Group' }).check()
    const options = page.getByLabel('Owner group').locator('option')
    expect(await options.allTextContents()).toEqual(['analytics'])
    await context.close()
  })

  test('a normal person sees no Not your group badge', async ({ browser }) => {
    const { context, page } = await open(browser, {
      me: { ...ADMIN_OF_ANALYTICS, user: 'bob', is_admin: false },
      rows: [ANALYTICS_ROW],
    })
    await expect(page.getByText('Not your group')).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'Rotate analytics-db' })).toBeVisible()
    await context.close()
  })

  test('required fields and names are checked before sending', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.getByRole('button', { name: 'New connection' }).click()
    const create = page.getByRole('button', { name: 'Create connection' })
    await expect(create).toBeDisabled()

    await page.getByLabel('Name').fill('Bad_Name')
    await expect(create).toBeDisabled()
    await page.getByLabel('Name').fill('warehouse-db')
    await expect(create).toBeDisabled()

    await page.getByLabel('Host').fill('db.internal')
    await page.getByLabel('User').fill('svc')
    await expect(create).toBeDisabled()
    await page.getByLabel('Password').fill(CANARY)
    await expect(create).toBeEnabled()

    // A type with variants: every field of the chosen variant is required.
    await page.getByLabel('Type').selectOption('rest-api')
    await page.getByLabel('Base URL').fill('https://api.example.com')
    await expect(create).toBeDisabled()
    // "Token" labels both the variant radio and the field; the field is the password input.
    await page.locator('input[type="password"]').fill(CANARY)
    await expect(create).toBeEnabled()
    await page.getByRole('radio', { name: 'Client credentials' }).check()
    await expect(create).toBeDisabled()
    await page.getByLabel('Client ID').fill('client')
    await expect(create).toBeDisabled()
    await page.getByLabel('Client secret').fill(CANARY)
    await expect(create).toBeEnabled()
    await context.close()
  })

  test('U5 the New connection dialog has accessible names', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.getByRole('button', { name: 'New connection' }).click()
    await expect(page.getByRole('dialog', { name: 'New connection' })).toBeVisible()
    const findings = await auditPage(page)
    expect(findings.filter((finding) => finding.rule === 'U5')).toEqual([])
    await context.close()
  })

  test('the sentences are true today', async ({ browser }) => {
    // The exact three sentences (stardelt-nova#202). A sentence that promises a consumer
    // that does not exist yet fails here, so the page cannot drift ahead of the product.
    const { context, page } = await open(browser, {
      rows: [
        { name: 'my-db', scope: 'personal', type: 'postgresql', config: { host: 'localhost' }, secretKeys: ['password'], version: 1 },
        { name: 'crm-db', scope: 'group', type: 'postgresql', ownerGroup: 'analytics', config: { host: 'db' }, secretKeys: ['user', 'password'], version: 1, sync: { ready: true, reason: 'Synced' } },
        { name: 'platform-s3', scope: 'managed', type: 's3', config: { bucket: 'b' }, secretKeys: ['secretAccessKey'], version: 1, sync: { ready: true, reason: 'Synced' } },
      ],
    })
    const text = await page.locator('main').innerText()
    expect(text).toContain('Only you can see and use these, from a Python notebook. They are never used by pipelines or SQL.')
    expect(text).toContain('Every member of the group can use and rotate these from a Python notebook.')
    expect(text).toContain('For platform services only. Nobody can read the values.')
    // No sentence may name a surface that reads a connection today.
    for (const phrase of ['SQL editor', 'federated', 'pipelines that run']) {
      expect(text, `the page still says "${phrase}"`).not.toContain(phrase)
    }
    await context.close()
  })

  test('the Use disclosure shows how a notebook reads a connection, and only names fields', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const { context, page } = await open(browser, {
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      rows: [
        { name: 'crm-db', scope: 'group', type: 'postgresql', ownerGroup: 'analytics', config: { host: 'db' }, secretKeys: ['user', 'password'], version: 1, sync: { ready: true, reason: 'Synced' } },
        { name: evil, scope: 'personal', type: 'generic', config: {}, secretKeys: ['password'], version: 1 },
        { name: 'platform-s3', scope: 'managed', type: 's3', config: { bucket: 'b' }, secretKeys: ['secretAccessKey'], version: 1, sync: { ready: true, reason: 'Synced' } },
      ],
    })

    // A group row names the group and the first secret field, and nothing else.
    const groupRow = page.getByRole('row').filter({ hasText: 'crm-db' })
    await groupRow.getByText('Use in a notebook').click()
    expect(await groupRow.locator('pre').innerText()).toBe(
      'import stardelt\nc = stardelt.connection("crm-db", group="analytics")\nc.value("user")',
    )

    // A managed row has no Python reader, so it has nothing to disclose.
    const managedRow = page.getByRole('row').filter({ hasText: 'platform-s3' })
    expect(await managedRow.getByText('Use in a notebook').count()).toBe(0)
    expect(await managedRow.locator('pre').count()).toBe(0)

    // A hostile name stays text in the snippet, and a personal row names no group.
    const evilRow = page.getByRole('row').filter({ hasText: evil })
    await evilRow.getByText('Use in a notebook').click()
    const evilSnippet = await evilRow.locator('pre').innerText()
    expect(evilSnippet).toBe(`import stardelt\nc = stardelt.connection("${evil}")\nc.value("password")`)
    expect(await page.evaluate(() => (window as unknown as { __xss?: number }).__xss ?? 0)).toBe(0)
    expect(await page.locator('img[src="x"]').count()).toBe(0)
    await context.close()
  })

  test('the Other secret type is named and explains itself', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.getByRole('button', { name: 'New connection' }).click()
    const type = page.getByLabel('Type')
    await expect(type.locator('option[value="generic"]')).toHaveText('Other secret (any fields)')
    expect(await page.getByText('For any secret that is not one of the types above: name the settings and secret fields yourself.').count()).toBe(0)
    await type.selectOption('generic')
    await expect(page.getByText('For any secret that is not one of the types above: name the settings and secret fields yourself.')).toBeVisible()
    await context.close()
  })
})

// Last: an entry in known.ts that no page produces any more is a stale line (rule: the list only shrinks).
test('known.ts has no stale entries', () => {
  const stale = KNOWN.filter((k) => !matched.has(k)).map((k) => `${k.ticket} ${k.rule} ${k.route ?? ''} ${k.contains ?? ''}`)
  expect(stale, 'these findings are fixed: delete their lines from e2e/known.ts').toEqual([])
})

test.describe('break-glass', () => {
  const CANARY = 'canary-7f3a-DO-NOT-LEAK'
  const REASON = 'incident 4711: pipeline blocked'
  const ADMIN = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: true, groups: ['data-eng'],
  }
  const GROUP_ROW = {
    name: 'team-db', scope: 'group', type: 'postgresql', ownerGroup: 'data-eng',
    config: { host: 'db.internal' }, secretKeys: ['user', 'password'], version: 1,
    sync: { ready: true, reason: 'Synced' },
  }
  const PERSONAL_ROW = {
    name: 'my-db', scope: 'personal', type: 'postgresql',
    config: { host: 'localhost' }, secretKeys: ['password'], version: 1,
  }
  const MANAGED_ROW = {
    name: 'platform-s3', scope: 'managed', type: 's3',
    config: { bucket: 'b' }, secretKeys: ['secretAccessKey'], version: 1,
    sync: { ready: true, reason: 'Synced' },
  }

  type Recorded = { method: string; url: string; body: string | null; headers: Record<string, string> }

  /** The Connections page with every `/api/**` call stubbed; the test decides the read's answer. */
  async function open(browser: Browser, {
    me = ADMIN,
    rows = [GROUP_ROW] as unknown[],
    afterRead,
    read,
    init,
  }: {
    me?: unknown
    rows?: unknown[]
    /** Rows returned by the list after a successful read (the one carrying the notice). */
    afterRead?: unknown[]
    read?: (route: Route, path: string, body: string | null) => void
    init?: () => void
  } = {}) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    if (init) await page.addInitScript(init)
    const requests: Recorded[] = []
    const messages: string[] = []
    page.on('request', (request) => requests.push({
      method: request.method(),
      url: request.url(),
      body: request.postData(),
      headers: request.headers(),
    }))
    page.on('console', (message) => messages.push(message.text()))
    let gets = 0
    let readDone = false
    await page.route('**/api/**', (route) => {
      const request = route.request()
      const path = new URL(request.url()).pathname
      const method = request.method()
      if (path === '/api/me' && method === 'GET') return json(route, me)
      if (path === '/api/connections' && method === 'GET') {
        gets += 1
        return json(route, { connections: readDone && afterRead ? afterRead : rows })
      }
      if (method === 'POST' && path.endsWith('/break-glass')) {
        if (read) return read(route, path, request.postData())
        readDone = true
        return json(route, {
          fields: { user: 'svc', password: CANARY },
          readAt: '2026-10-20T12:00:00Z',
        })
      }
      return json(route, [])
    })
    await page.goto('/connections')
    await expect(page.locator('main')).not.toContainText('Loading connections')
    return { context, page, requests, messages, gets: () => gets }
  }

  /** Open the dialog, give a reason and read; the dialog is on step 2. */
  async function reveal(page: Page) {
    await page.getByRole('button', { name: 'Break-glass read team-db' }).click()
    // The page keeps a closed break-glass dialog mounted; only one is open at a time.
    const dialog = page.locator('dialog[open]')
    await dialog.getByLabel('Reason').fill(REASON)
    await dialog.getByRole('button', { name: 'Read the credential' }).click()
    await expect(dialog).toContainText('Credential team-db')
    return dialog
  }

  /** Show the password field's plain value. */
  async function showPassword(dialog: Locator) {
    const field = dialog.locator('li', { hasText: 'password' })
    await field.getByRole('button', { name: 'Show' }).click()
    await expect(field).toContainText(CANARY)
  }

  async function stored(page: Page): Promise<string> {
    return page.evaluate(() => {
      const dump = (storage: Storage) => Object.fromEntries(
        Array.from({ length: storage.length }, (_, index) => {
          const key = storage.key(index) ?? ''
          return [key, storage.getItem(key) ?? '']
        }),
      )
      return JSON.stringify({ local: dump(localStorage), session: dump(sessionStorage) })
    })
  }

  async function expectGone(page: Page) {
    await expect.poll(async () => page.evaluate(() => document.documentElement.outerHTML).then((h) => h.includes(CANARY)))
      .toBe(false)
    expect(await stored(page)).not.toContain(CANARY)
  }

  test('P3 break-glass asks for a reason first', async ({ browser }) => {
    const { context, page, requests } = await open(browser)
    await page.getByRole('button', { name: 'Break-glass read team-db' }).click()
    const dialog = page.getByRole('dialog', { name: 'Break-glass read of team-db' })
    await expect(dialog).toBeVisible()
    const read = page.getByRole('button', { name: 'Read the credential' })
    const reason = page.getByLabel('Reason')

    await expect(reason).toBeFocused()
    await expect(read).toBeDisabled()
    await reason.fill('x'.repeat(19))
    await expect(read).toBeDisabled()
    await expect(page.getByText('19 / 500')).toBeVisible()
    await reason.fill(`  ${'x'.repeat(20)}  `)
    await expect(read).toBeEnabled()
    expect(requests.filter((request) => request.method === 'POST')).toEqual([])

    await read.click()
    const posts = requests.filter((request) => request.method === 'POST')
    expect(posts).toHaveLength(1)
    expect(new URL(posts[0].url).pathname).toBe('/api/connections/group/data-eng/team-db/break-glass')
    expect(JSON.parse(posts[0].body ?? '{}')).toEqual({ reason: 'x'.repeat(20) })
    await context.close()
  })

  test('P3 the value is masked until Show, and lives only in one element', async ({ browser }) => {
    const { context, page } = await open(browser)
    const dialog = await reveal(page)
    const field = dialog.locator('li', { hasText: 'password' })

    expect(await page.evaluate(() => document.documentElement.outerHTML)).not.toContain(CANARY)
    const attrs = await page.evaluate(() => [...document.querySelectorAll('*')]
      .flatMap((el) => [...el.attributes].map((a) => a.value)).join('\n'))
    expect(attrs).not.toContain(CANARY)
    await expect(field).toContainText('••••••••')
    await expect(field.getByRole('button', { name: 'Show' })).toHaveAttribute('aria-pressed', 'false')

    await field.getByRole('button', { name: 'Show' }).click()
    await expect(field.getByRole('button', { name: 'Hide' })).toHaveAttribute('aria-pressed', 'true')
    const shown = await page.evaluate(() => document.documentElement.outerHTML)
    expect(shown.split(CANARY)).toHaveLength(2)

    await field.getByRole('button', { name: 'Hide' }).click()
    await expect(field.getByRole('button', { name: 'Show' })).toHaveAttribute('aria-pressed', 'false')
    expect(await page.evaluate(() => document.documentElement.outerHTML)).not.toContain(CANARY)
    await context.close()
  })

  test('P3 the value is gone after the time-to-live', async ({ browser }) => {
    const { context, page, gets } = await open(browser)
    await page.clock.install()
    const dialog = await reveal(page)
    await showPassword(dialog)
    await expect(dialog.getByRole('timer')).toContainText('60 seconds')

    const before = gets()
    await page.clock.fastForward(10_000)
    await expect(dialog.getByRole('timer')).toContainText('50 seconds')
    await page.clock.fastForward(50_000)
    await expect(dialog).not.toBeVisible()
    await expectGone(page)
    await expect.poll(() => gets(), { message: 'the list is read again after the dialog closes' }).toBeGreaterThan(before)
    await context.close()
  })

  test('P3 closing, Escape and leaving the page drop the value', async ({ browser }) => {
    const { context, page } = await open(browser)

    let dialog = await reveal(page)
    await showPassword(dialog)
    await page.getByRole('button', { name: 'Close' }).click()
    await expect(dialog).not.toBeVisible()
    await expectGone(page)

    dialog = await reveal(page)
    await showPassword(dialog)
    await page.keyboard.press('Escape')
    await expect(dialog).not.toBeVisible()
    await expectGone(page)

    dialog = await reveal(page)
    await showPassword(dialog)
    await page.goto('/catalog')
    await page.goBack()
    await expect(page.getByRole('heading', { name: 'Connections' })).toBeVisible()
    await expectGone(page)
    await context.close()
  })

  test('P3 the value never leaves the one answer', async ({ browser }) => {
    const { context, page, requests, messages } = await open(browser)
    const dialog = await reveal(page)
    await showPassword(dialog)
    // While the value is on screen as text, no attribute may carry it.
    const shownAttrs = await page.evaluate(() => [...document.querySelectorAll('*')]
      .flatMap((el) => [...el.attributes].map((a) => a.value)).join('\n'))
    expect(shownAttrs, 'an attribute holds the canary').not.toContain(CANARY)
    await page.getByRole('button', { name: 'Close' }).click()
    await expectGone(page)

    const html = await page.evaluate(() => document.documentElement.outerHTML)
    const attrs = await page.evaluate(() => [...document.querySelectorAll('*')]
      .flatMap((el) => [...el.attributes].map((a) => a.value)).join('\n'))
    const location = await page.evaluate(() => `${location.href}\n${JSON.stringify(history.state)}`)
    for (const [where, text] of [
      ['the DOM', html],
      ['an attribute', attrs],
      ['the URL or history', location],
      ['storage', await stored(page)],
      ['a console message', messages.join('\n')],
      ['a request body', requests.map((request) => request.body ?? '').join('\n')],
      ['a request URL', requests.map((request) => request.url).join('\n')],
      ['a request header', requests.map((request) => JSON.stringify(request.headers)).join('\n')],
    ] as const) {
      expect(text, `the canary is in ${where}`).not.toContain(CANARY)
    }
    await context.close()
  })

  test('only administrators on group rows see the action', async ({ browser }) => {
    const rows = [GROUP_ROW, PERSONAL_ROW, MANAGED_ROW]
    const nonAdmin = await open(browser, { me: { ...ADMIN, is_admin: false }, rows })
    expect(await nonAdmin.page.getByRole('button', { name: /^Break-glass read / }).count()).toBe(0)
    await nonAdmin.context.close()

    const admin = await open(browser, { rows })
    await expect(admin.page.getByRole('button', { name: 'Break-glass read team-db' })).toHaveCount(1)
    expect(await admin.page.getByRole('button', { name: 'Break-glass read my-db' }).count()).toBe(0)
    expect(await admin.page.getByRole('button', { name: 'Break-glass read platform-s3' }).count()).toBe(0)
    await admin.context.close()
  })

  test('the group sees the notice', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const reason = `${evil}${'x'.repeat(300)}`
    const { context, page } = await open(browser, {
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      rows: [{ ...GROUP_ROW, breakGlass: { by: evil, at: '2026-10-20T12:00:00Z', reason } }],
    })
    await page.setViewportSize({ width: 360, height: 740 })
    const banner = page.getByRole('status')
    await expect(banner).toContainText('Read by')
    await expect(banner).toContainText(evil)
    await expect(banner).toContainText('Consider rotating the credential.')
    expect(await page.evaluate(() => (window as unknown as { __xss?: number }).__xss ?? 0)).toBe(0)
    expect(await page.locator('img[src="x"]').count()).toBe(0)
    // The human sees 200 characters; the full reason and name are only the title attribute.
    await expect(banner).toContainText(reason.slice(0, 200))
    expect(await page.evaluate(() => [...document.querySelectorAll('main [title]')]
      .map((element) => element.getAttribute('title')))).toContain(reason)
    expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)).toBeLessThanOrEqual(1)

    await banner.getByRole('button', { name: 'Rotate now' }).click()
    await expect(page.getByRole('dialog')).toContainText('Rotate team-db')
    await context.close()
  })

  test('errors keep the dialog and the reason', async ({ browser }) => {
    for (const [status, sentence, body] of [
      [403, 'only administrators may do this', 'only administrators may do this'],
      [429, 'Too many break-glass reads. Wait ten minutes.', 'too many break-glass reads; wait'],
    ] as const) {
      const { context, page } = await open(browser, {
        read: (route) => json(route, { error: body }, status),
      })
      await page.getByRole('button', { name: 'Break-glass read team-db' }).click()
      await page.getByLabel('Reason').fill(REASON)
      await page.getByRole('button', { name: 'Read the credential' }).click()

      await expect(page.getByRole('alert'), `${status}`).toContainText(sentence)
      await expect(page.getByLabel('Reason'), `${status}`).toHaveValue(REASON)
      await expect(page.getByRole('dialog', { name: 'Break-glass read of team-db' })).toBeVisible()
      await context.close()
    }
  })

  test('U5 the break-glass dialog has accessible names at both steps', async ({ browser }) => {
    const { context, page } = await open(browser)
    // The page must stay inside a 360 px phone: the dialog is `min(44rem, 100vw - 2rem)`.
    await page.setViewportSize({ width: 360, height: 740 })
    await page.getByRole('button', { name: 'Break-glass read team-db' }).click()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])

    await page.getByLabel('Reason').fill(REASON)
    await page.getByRole('button', { name: 'Read the credential' }).click()
    await expect(page.getByRole('dialog', { name: 'Credential team-db' })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    const box = await page.getByRole('dialog').boundingBox()
    expect(box).not.toBeNull()
    expect(box!.x).toBeGreaterThanOrEqual(0)
    expect(box!.x + box!.width).toBeLessThanOrEqual(361)
    expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)).toBeLessThanOrEqual(1)
    await context.close()
  })
})

test.describe('table documentation', () => {
  const EVIL = '<img src=x onerror="window.__xss=1">'
  const TAG_RULE = 'Tags are lower-case letters, digits, dash and underscore, up to 32 characters'
  const ADMIN = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: true, groups: ['admins'],
  }
  const MEMBER = {
    user: 'bob', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: false, groups: ['data-eng', 'analytics'],
  }
  const TABLE = '/api/catalog/tables/sales/orders'
  const FIELDS = [
    { id: 1, name: 'id', required: true, type: 'long' },
    { id: 2, name: 'amount', required: false, type: 'double' },
  ]
  const METADATA = {
    'metadata-location': 's3://warehouse/sales/orders/metadata/00001.metadata.json',
    metadata: {
      'table-uuid': '00000000-0000-0000-0000-000000000001',
      location: 's3://warehouse/sales/orders',
      'last-updated-ms': 1_760_000_000_000,
      'current-schema-id': 0,
      schemas: [{ 'schema-id': 0, fields: FIELDS }],
      'partition-specs': [],
      properties: {},
    },
  }

  type Doc = {
    description: string | null
    tags: string[]
    owner: string | null
    domain: string | null
    columns: Array<{ name: string; type: string; description: string | null }>
    canEdit: boolean
    editRule: string
  }

  const makeDoc = (over: Partial<Doc> = {}): Doc => ({
    description: 'Orders from the shop.',
    tags: ['finance', 'pii'],
    owner: 'data-eng',
    domain: 'sales',
    columns: [
      { name: 'id', type: 'long', description: 'the order id' },
      { name: 'amount', type: 'double', description: 'the amount' },
    ],
    canEdit: true,
    editRule: 'You can edit because you belong to the owner group data-eng.',
    ...over,
  })

  function applyDoc(base: Doc, body: Record<string, unknown>): Doc {
    const next: Doc = { ...base, columns: base.columns.map((column) => ({ ...column })) }
    if ('description' in body) next.description = (body.description as string | null) ?? null
    if ('tags' in body) next.tags = (body.tags as string[] | null) ?? []
    if ('owner' in body) next.owner = (body.owner as string | null) ?? null
    if ('domain' in body) next.domain = (body.domain as string | null) ?? null
    return next
  }

  type Recorded = { method: string; url: string; body: string | null }

  /**
   * The table page with every `/api/**` call stubbed. The documentation route keeps a
   * mutable copy of the answer, so a successful write is read back like the real API's
   * freshly loaded answer.
   */
  async function open(browser: Browser, {
    me = MEMBER as unknown,
    doc = makeDoc(),
    write,
    init,
  }: {
    me?: unknown
    doc?: Doc
    write?: (route: Route, path: string, body: Record<string, unknown>) => boolean
    init?: () => void
  } = {}) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    if (init) await page.addInitScript(init)
    const requests: Recorded[] = []
    page.on('request', (request) => requests.push({
      method: request.method(),
      url: request.url(),
      body: request.postData(),
    }))
    let current = doc
    await page.route('**/api/**', (route) => {
      const request = route.request()
      const path = new URL(request.url()).pathname
      const method = request.method()
      if (path === '/api/me' && method === 'GET') return json(route, me)
      if (path === '/api/catalog/warehouse' && method === 'GET') return json(route, { name: 'warehouse', id: 'w' })
      if (path === '/api/catalog/namespaces' && method === 'GET') return json(route, { namespaces: [['sales']] })
      if (path === '/api/catalog/namespaces/sales/tables' && method === 'GET') {
        return json(route, { identifiers: [{ namespace: ['sales'], name: 'orders' }] })
      }
      if (path === TABLE && method === 'GET') return json(route, METADATA)
      if (path === `${TABLE}/documentation` && method === 'GET') return json(route, current)
      if (path === `${TABLE}/documentation` && method === 'PUT') {
        const body = JSON.parse(request.postData() ?? '{}') as Record<string, unknown>
        if (write?.(route, path, body)) return
        current = applyDoc(current, body)
        return json(route, current)
      }
      const column = path.match(/^\/api\/catalog\/tables\/sales\/orders\/columns\/([^/]+)\/description$/)
      if (method === 'PUT' && column) {
        const body = JSON.parse(request.postData() ?? '{}') as Record<string, unknown>
        if (write?.(route, path, body)) return
        const name = decodeURIComponent(column[1])
        current = {
          ...current,
          columns: current.columns.map((c) => c.name === name
            ? { ...c, description: (body.description as string | null) ?? null }
            : c),
        }
        return json(route, current)
      }
      if (path === '/api/query' && method === 'POST') return json(route, { columns: [], rows: [], row_count: 0 })
      return json(route, { error: 'not found' }, 404)
    })
    await page.goto('/catalog?ns=sales&tbl=orders')
    const card = page.getByRole('region', { name: 'About this table' })
    await expect(card).toBeVisible()
    await expect(card).not.toContainText('Loading…')
    return { context, page, requests, card, doc: () => current, setDoc: (next: Doc) => { current = next } }
  }

  const puts = (requests: Recorded[]) => requests.filter((request) => request.method === 'PUT')

  test('read-only view', async ({ browser }) => {
    const rule = 'Only members of the owner group data-eng and administrators may edit this.'
    const { context, page, card } = await open(browser, {
      doc: makeDoc({ canEdit: false, editRule: rule }),
    })
    await expect(card).toContainText('Orders from the shop.')
    for (const tag of ['finance', 'pii']) await expect(card.getByText(tag, { exact: true })).toBeVisible()
    await expect(card.getByText('data-eng', { exact: true })).toBeVisible()
    await expect(card.getByText('sales', { exact: true })).toBeVisible()
    await expect(card.getByText(rule, { exact: true })).toBeVisible()
    expect(await page.getByRole('button', { name: /^Edit/ }).count()).toBe(0)

    await page.getByRole('button', { name: 'columns' }).click()
    await expect(page.getByText('the amount', { exact: true })).toBeVisible()
    expect(await page.getByRole('button', { name: /^Edit/ }).count()).toBe(0)
    await context.close()
  })

  test('markup is text', async ({ browser }) => {
    const { context, page, card, setDoc, doc } = await open(browser, {
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      doc: makeDoc({
        description: EVIL,
        tags: [EVIL],
        owner: EVIL,
        domain: EVIL,
        canEdit: false,
        columns: [
          { name: 'id', type: 'long', description: EVIL },
          { name: 'amount', type: 'double', description: 'the amount' },
        ],
      }),
    })
    await expect(card).toContainText(EVIL)
    await page.getByRole('button', { name: 'columns' }).click()
    await expect(page.getByText(EVIL).first()).toBeVisible()
    const rendered = await page.evaluate(() => ({
      xss: (window as unknown as { __xss: number }).__xss,
      imgs: document.querySelectorAll('img[src="x"]').length,
    }))
    expect(rendered).toEqual({ xss: 0, imgs: 0 })

    setDoc({ ...doc(), description: 'x'.repeat(400), tags: ['finance'], owner: 'data-eng', domain: 'sales' })
    for (const width of [1280, 360]) {
      await page.setViewportSize({ width, height: 800 })
      await page.reload()
      await expect(page.getByText('x'.repeat(400), { exact: true })).toBeVisible()
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
      expect(overflow, `${width}px`).toBeLessThanOrEqual(1)
      expect(await page.locator('img[src="x"]').count()).toBe(0)
    }
    await context.close()
  })

  test('editing sends only what changed', async ({ browser }) => {
    const { context, page, requests } = await open(browser, { doc: makeDoc() })
    const save = () => page.getByRole('button', { name: 'Save', exact: true })

    // Description only.
    await page.getByRole('button', { name: 'Edit about this table' }).click()
    await expect(save()).toBeDisabled()
    await page.getByLabel('Description').fill('Settled orders from the shop.')
    await save().click()
    await expect(page.getByRole('status')).toHaveText('Saved.')
    let bodies = puts(requests).map((request) => JSON.parse(request.body ?? '{}'))
    expect(bodies).toEqual([{ description: 'Settled orders from the shop.' }])

    // One tag added: the body carries the whole list.
    await page.getByRole('button', { name: 'Edit about this table' }).click()
    await page.getByLabel('Tags').fill('urgent')
    await page.getByLabel('Tags').press('Enter')
    await save().click()
    await expect(page.getByRole('status')).toHaveText('Saved.')
    bodies = puts(requests).map((request) => JSON.parse(request.body ?? '{}'))
    expect(bodies[1]).toEqual({ tags: ['finance', 'pii', 'urgent'] })

    // A cleared description is sent as null.
    await page.getByRole('button', { name: 'Edit about this table' }).click()
    await page.getByLabel('Description').fill('')
    await save().click()
    await expect(page.getByRole('status')).toHaveText('Saved.')
    bodies = puts(requests).map((request) => JSON.parse(request.body ?? '{}'))
    expect(bodies[2]).toEqual({ description: null })
    await context.close()
  })

  test('limits and tag rules', async ({ browser }) => {
    const { context, page, requests } = await open(browser, { doc: makeDoc({ tags: [] }) })
    await page.getByRole('button', { name: 'Edit about this table' }).click()

    const description = page.getByLabel('Description')
    await description.fill('x'.repeat(4001))
    await expect(description).toHaveValue('x'.repeat(4000))
    await expect(page.getByText('4000 / 4000')).toBeVisible()

    const tags = page.getByLabel('Tags')
    await tags.fill('Finance')
    await tags.press('Enter')
    await expect(page.getByText(TAG_RULE, { exact: true })).toBeVisible()
    expect(puts(requests)).toEqual([])

    for (let i = 0; i < 20; i++) {
      await tags.fill(`tag-${i}`)
      await tags.press('Enter')
    }
    await expect(page.getByRole('button', { name: /^Remove tag / })).toHaveCount(20)
    await tags.fill('tag-20')
    await tags.press('Enter')
    await expect(page.getByText('At most 20 tags', { exact: true })).toBeVisible()
    expect(puts(requests)).toEqual([])
    await context.close()
  })

  test('owner choices', async ({ browser }) => {
    const member = await open(browser)
    await member.page.getByRole('button', { name: 'Edit about this table' }).click()
    const select = member.page.getByLabel('Owner')
    expect(await select.evaluate((element) => element.tagName)).toBe('SELECT')
    await expect(select.locator('option')).toHaveText(['data-eng', 'analytics'])
    expect(await select.locator('option[value=""]').count()).toBe(0)
    await member.context.close()

    const admin = await open(browser, { me: ADMIN })
    await admin.page.getByRole('button', { name: 'Edit about this table' }).click()
    const input = admin.page.getByLabel('Owner')
    expect(await input.evaluate((element) => element.tagName)).toBe('INPUT')
    await expect(input).toHaveValue('data-eng')
    await admin.context.close()

    const ownerless = await open(browser, { doc: makeDoc({ owner: null }) })
    await expect(ownerless.card.getByText('No owner', { exact: true })).toBeVisible()
    await ownerless.page.getByRole('button', { name: 'Edit about this table' }).click()
    const claim = ownerless.page.getByLabel('Owner')
    await expect(claim.locator('option')).toHaveText(['data-eng', 'analytics'])
    expect(await claim.locator('option[value=""]').count()).toBe(0)
    await expect(ownerless.page.getByText('The first editor claims the table for one of their groups.'))
      .toBeVisible()
    await ownerless.context.close()
  })

  test('cancel and Escape drop the edit', async ({ browser }) => {
    const { context, page, card, requests } = await open(browser, { doc: makeDoc() })
    await page.getByRole('button', { name: 'Edit about this table' }).click()
    await page.getByLabel('Description').fill('this must not be saved')
    await page.keyboard.press('Escape')
    await expect(page.getByRole('button', { name: 'Edit about this table' })).toBeFocused()
    await expect(card).toContainText('Orders from the shop.')
    await expect(card).not.toContainText('this must not be saved')
    expect(puts(requests)).toEqual([])
    await context.close()
  })

  test('a 400 or 502 keeps the form', async ({ browser }) => {
    let attempts = 0
    const opened = await open(browser, {
      write: (route, path, body) => {
        if (!path.endsWith('/documentation')) return false
        attempts += 1
        if (attempts === 1) {
          json(route, { error: 'the catalog is busy, try again' }, 502)
          return true
        }
        const next = applyDoc(opened.doc(), body)
        opened.setDoc(next)
        json(route, next)
        return true
      },
    })
    const { context, page } = opened
    await page.getByRole('button', { name: 'Edit about this table' }).click()
    await page.getByLabel('Description').fill('kept after the failure')
    await page.getByRole('button', { name: 'Save', exact: true }).click()

    await expect(page.getByRole('alert')).toContainText('the catalog is busy, try again')
    await expect(page.getByLabel('Description')).toHaveValue('kept after the failure')
    await expect(page.getByRole('button', { name: 'Save', exact: true })).toBeVisible()

    await page.getByRole('button', { name: 'Save', exact: true }).click()
    await expect(page.getByRole('status')).toHaveText('Saved.')
    await expect(page.getByRole('button', { name: 'Edit about this table' })).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)
    await expect(page.getByRole('region', { name: 'About this table' }))
      .toContainText('kept after the failure')
    await context.close()
  })

  test('column description edit', async ({ browser }) => {
    const { context, page, requests } = await open(browser)
    await page.getByRole('button', { name: 'columns' }).click()
    await page.getByRole('button', { name: 'Edit description of amount' }).click()
    await page.getByRole('textbox', { name: 'Description of amount' }).fill('the amount in cents')
    await page.getByRole('button', { name: 'Save description of amount' }).click()

    await expect(page.getByRole('button', { name: 'Edit description of amount' })).toBeFocused()
    const columnPuts = puts(requests)
    expect(columnPuts).toHaveLength(1)
    expect(new URL(columnPuts[0].url).pathname).toBe('/api/catalog/tables/sales/orders/columns/amount/description')
    expect(JSON.parse(columnPuts[0].body ?? '{}')).toEqual({ description: 'the amount in cents' })

    const amountRow = page.getByRole('row', { name: /amount/ })
    const idRow = page.getByRole('row', { name: /the order id/ })
    await expect(amountRow).toContainText('the amount in cents')
    await expect(idRow).toContainText('the order id')
    await expect(page.getByRole('textbox', { name: 'Description of id' })).toHaveCount(0)
    await context.close()
  })

  test('U5 the form and a column row have accessible names', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.getByRole('button', { name: 'Edit about this table' }).click()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    await page.getByRole('button', { name: 'Cancel' }).click()

    await page.getByRole('button', { name: 'columns' }).click()
    await page.getByRole('button', { name: 'Edit description of amount' }).click()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    await context.close()
  })
})

test.describe('catalog switcher', () => {
  // Several catalogs (stardelt-nova#234): the switcher offers exactly what the API says
  // the person may browse, the choice lives in the URL, and every catalog request names
  // the chosen catalog. A catalog they may not browse is never offered — it is not in
  // the list the engine answers for them.
  const ME = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: false, groups: ['analysts'],
  }
  const TWO = {
    catalogs: [{ name: 'finance', id: 'wid-finance' }, { name: 'warehouse', id: 'wid-warehouse' }],
    default: 'warehouse',
  }
  const METADATA = {
    'metadata-location': 's3://finance/seed/nation/metadata/00001.metadata.json',
    metadata: {
      'table-uuid': '00000000-0000-0000-0000-000000000002',
      location: 's3://finance/seed/nation',
      'last-updated-ms': 1_760_000_000_000,
      'current-schema-id': 0,
      schemas: [{ 'schema-id': 0, fields: [{ id: 1, name: 'n_nationkey', required: true, type: 'long' }] }],
      'partition-specs': [],
      properties: {},
    },
  }
  const DOC = {
    description: null, tags: [], owner: null, domain: null, canEdit: false,
    editRule: 'Only members of the owner group and administrators may edit this.',
    columns: [{ name: 'n_nationkey', type: 'long', description: null }],
  }

  /** The Catalog page with two catalogs; `seen` records every namespaces request's query. */
  async function open(browser: Browser, catalogs: { status?: number; body: unknown }) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    const seen: string[] = []
    await page.route('**/api/**', (route) => {
      const url = new URL(route.request().url())
      if (url.pathname === '/api/me') return json(route, ME)
      if (url.pathname === '/api/catalog/catalogs') {
        return json(route, catalogs.body, catalogs.status ?? 200)
      }
      if (url.pathname === '/api/catalog/namespaces') {
        seen.push(url.search)
        if (!url.searchParams.get('catalog') && (catalogs.body as { catalogs?: unknown[] }).catalogs?.length === 0) {
          return json(route, { error: 'no catalog is open to you yet; ask an administrator for access' }, 403)
        }
        return json(route, { namespaces: url.searchParams.get('catalog') === 'finance' ? [['seed']] : [['nyc_taxi']] })
      }
      if (url.pathname === '/api/catalog/namespaces/seed/tables') {
        return json(route, { identifiers: [{ namespace: ['seed'], name: 'nation' }] })
      }
      if (url.pathname === '/api/catalog/namespaces/nyc_taxi/tables') {
        return json(route, { identifiers: [{ namespace: ['nyc_taxi'], name: 'trips' }] })
      }
      if (url.pathname === '/api/catalog/tables/seed/nation') return json(route, METADATA)
      if (url.pathname === '/api/catalog/tables/seed/nation/documentation') return json(route, DOC)
      if (url.pathname === '/api/query') return json(route, { columns: [], rows: [], row_count: 0 })
      return json(route, [])
    })
    return { context, page, seen }
  }

  test('switcher_groups_by_layer_in_order', async ({ browser }) => {
    const { context, page } = await open(browser, {
      body: {
        catalogs: [
          { name: 'bronze', id: 'wid-bronze', layer: 'bronze' },
          { name: 'silver', id: 'wid-silver', layer: 'silver' },
          { name: 'gold', id: 'wid-gold', layer: 'gold' },
          { name: 'warehouse', id: 'wid-warehouse' },
          { name: 'shared', id: 'wid-shared' },
        ],
        default: 'warehouse',
      },
    })
    await page.goto('/catalog')
    const select = page.getByRole('combobox', { name: 'Catalog' })
    await expect(select).toBeVisible()

    // The three medallion layers come first, in bronze → silver → gold order, and every
    // catalog without a layer follows in the API's order under Other.
    const groups = select.locator('optgroup')
    await expect(groups).toHaveCount(4)
    expect(await groups.evaluateAll((elements) => elements.map((group) => group.getAttribute('label'))))
      .toEqual(['Bronze', 'Silver', 'Gold', 'Other'])
    await expect(select.locator('option')).toHaveText(['bronze', 'silver', 'gold', 'warehouse', 'shared'])
    await expect(select).toHaveValue('warehouse')

    // A grouped option still selects and names the catalog, as the flat list does.
    await select.selectOption('gold')
    await expect(page).toHaveURL(/[?&]catalog=gold\b/)
    await expect(page.getByRole('button', { name: /nyc_taxi/ })).toBeVisible()
    judge('/catalog', await auditPage(page))
    await context.close()
  })

  test('no_layer_no_group_headers', async ({ browser }) => {
    const { context, page } = await open(browser, { body: TWO })
    await page.goto('/catalog')
    const select = page.getByRole('combobox', { name: 'Catalog' })
    await expect(select).toBeVisible()
    // No catalog has a layer: the switcher stays the flat list it was before layers.
    await expect(select.locator('optgroup')).toHaveCount(0)
    await expect(select.locator('option')).toHaveText(['finance', 'warehouse'])
    await context.close()
  })

  test('the second catalog is what every request then names', async ({ browser }) => {
    const { context, page, seen } = await open(browser, { body: TWO })
    await page.goto('/catalog')
    const select = page.getByRole('combobox', { name: 'Catalog' })
    await expect(select).toBeVisible()
    await expect(select.locator('option')).toHaveText(['finance', 'warehouse'])
    await expect(select).toHaveValue('warehouse')
    await expect(page.getByRole('button', { name: /nyc_taxi/ })).toBeVisible()

    await select.selectOption('finance')
    // The choice is in the URL, so a reload or a pasted link opens the same catalog …
    await expect(page).toHaveURL(/[?&]catalog=finance\b/)
    // … and the table list is asked for in that catalog, not the default one.
    await expect.poll(() => seen.at(-1) ?? '', { message: 'the next namespaces request names the catalog' })
      .toContain('catalog=finance')
    await expect(page.getByRole('button', { name: /seed/ })).toBeVisible()
    await expect(page.getByRole('button', { name: /nyc_taxi/ })).toHaveCount(0)

    // A generated statement names the chosen catalog, never the historical default.
    await page.getByRole('button', { name: /nation/ }).click()
    const link = page.getByRole('link', { name: 'Query table' })
    const sql = await link.evaluate((a) => new URL((a as HTMLAnchorElement).href).searchParams.get('sql'))
    expect(sql).toBe('SELECT * FROM finance.seed.nation LIMIT 100')

    // Escape leaves the switcher alone and the page on the catalog the person chose.
    await select.focus()
    await page.keyboard.press('Escape')
    await expect(select).toBeFocused()
    await expect(page).toHaveURL(/[?&]catalog=finance\b/)

    // The same U rules as every other page: headings, landmarks, labels, contrast, text.
    judge('/catalog', await auditPage(page))
    await context.close()
  })

  test('the SQL editor generates statements in the chosen catalog', async ({ browser }) => {
    const { context, page } = await open(browser, { body: TWO })
    await page.goto('/query')
    const editor = page.getByLabel('SQL query')
    await expect(editor).toHaveValue('SELECT * FROM warehouse.nyc_taxi.trips LIMIT 100')

    await page.getByRole('combobox', { name: 'Catalog' }).selectOption('finance')
    // The sample is regenerated for the catalog the person chose, and the choice is in the URL.
    await expect(editor).toHaveValue('SELECT * FROM finance.seed.nation LIMIT 100')
    await expect(page).toHaveURL(/[?&]catalog=finance\b/)
    await expect(page.getByText('Samples and inserted table names use this catalog.')).toBeVisible()
    judge('/query', await auditPage(page))
    await context.close()
  })

  test('no catalog is open says so and offers no Retry', async ({ browser }) => {
    const sentence = /no catalog is open to you yet; ask an administrator for access/
    // The list route itself may answer 403. The page shows that one sentence once, and no
    // Retry: a second ask cannot change the person's grants.
    const denied = await open(browser, {
      status: 403,
      body: { error: 'no catalog is open to you yet; ask an administrator for access' },
    })
    await denied.page.goto('/catalog')
    await expect(denied.page.getByRole('alert')).toContainText(sentence)
    await expect(denied.page.getByRole('alert')).toHaveCount(1)
    await expect(denied.page.getByRole('button', { name: 'Retry' })).toHaveCount(0)
    await denied.context.close()

    // The real backend answers an empty list, with every catalog route refusing: the
    // namespaces refusal is the same sentence, and a Retry cannot change the grants either.
    const empty = await open(browser, { body: { catalogs: [], default: 'warehouse' } })
    await empty.page.goto('/catalog')
    await expect(empty.page.getByRole('alert')).toContainText(sentence)
    await expect(empty.page.getByRole('alert')).toHaveCount(1)
    await expect(empty.page.getByRole('button', { name: 'Retry' })).toHaveCount(0)
    await expect(empty.page.getByRole('combobox', { name: 'Catalog' })).toHaveCount(0)
    await empty.context.close()
  })
})

test.describe('pipelines', () => {
  // The Pipelines page (J1): every state is a word, the actions send one request and show
  // the API's own sentence when it refuses, and a run's log stays text.
  const ADMIN = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: true, groups: ['analytics'],
  }
  const NS = 'stardelt'
  const NOT_LOADED_TEXT =
    'The scheduler did not accept this pipeline. Ask an administrator to check the loader log.'

  type Recorded = { method: string; url: string; body: string | null }

  function run(overrides: Record<string, unknown> = {}) {
    return {
      runId: 'manual__2026-10-20T08:00:00+00:00',
      state: 'failed',
      type: 'manual',
      logicalDate: '2026-10-20T08:00:00Z',
      startedAt: '2026-10-20T08:00:01Z',
      endedAt: '2026-10-20T08:00:09Z',
      durationSeconds: 8,
      triggeredBy: 'alice',
      note: 'Started from Nova by Alice',
      ...overrides,
    }
  }

  function pipeline(overrides: Record<string, unknown> = {}) {
    return {
      name: 'team-smoke',
      namespace: NS,
      ownerGroup: 'analytics',
      schedule: '0 3 * * * (Europe/Berlin)',
      paused: false,
      loaded: true,
      nextRun: '2026-10-21T01:00:00Z',
      state: 'idle',
      lastRun: null,
      tasks: [{ name: 'load', kind: 'sql' }],
      runAs: 'sp-analytics',
      concurrency: 'Forbid',
      createdAt: '2026-10-19T10:00:00Z',
      ...overrides,
    }
  }

  const STATE_ROWS = [
    pipeline({ name: 'loading-one', state: 'loading', loaded: false }),
    pipeline({ name: 'refused-one', state: 'not-loaded', loaded: false }),
    pipeline({ name: 'paused-one', state: 'paused', paused: true }),
    pipeline({ name: 'running-one', state: 'running', lastRun: run({ state: 'running', endedAt: null, durationSeconds: null }) }),
    pipeline({ name: 'failed-one', state: 'failed', lastRun: run() }),
    pipeline({ name: 'succeeded-one', state: 'succeeded', lastRun: run({ state: 'success', note: null }) }),
    pipeline({ name: 'idle-one', state: 'idle' }),
  ]

  /** A Pipelines page with every `/api/**` call stubbed; `read`/`write` decide the rest. */
  async function open(browser: Browser, {
    me = ADMIN,
    rows = [] as unknown[],
    start = '/pipelines',
    init,
    clock,
    read,
    write,
  }: {
    me?: unknown
    rows?: unknown[]
    start?: string
    init?: () => void
    clock?: boolean
    read?: (route: Route, path: string, method: string, url: URL) => boolean
    write?: (route: Route, path: string, method: string) => boolean
  } = {}) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    if (init) await page.addInitScript(init)
    if (clock) await page.clock.install()
    const requests: Recorded[] = []
    page.on('request', (request) => requests.push({
      method: request.method(),
      url: request.url(),
      body: request.postData(),
    }))
    await page.route('**/api/**', (route) => {
      const request = route.request()
      const url = new URL(request.url())
      const path = url.pathname
      const method = request.method()
      if (path === '/api/me' && method === 'GET') return json(route, me)
      if (read?.(route, path, method, url)) return
      if (path === '/api/pipelines' && method === 'GET') return json(route, { pipelines: rows })
      if (method !== 'GET' && write?.(route, path, method)) return
      return json(route, [])
    })
    await page.goto(start)
    return { context, page, requests }
  }

  const pathOf = (request: Recorded) => new URL(request.url).pathname
  const postsTo = (requests: Recorded[], suffix: string) =>
    requests.filter((request) => request.method === 'POST' && pathOf(request).endsWith(suffix)).length

  test('list shows every state in words', async ({ browser }) => {
    const { context, page, requests } = await open(browser, { rows: STATE_ROWS, clock: true })
    await expect(page.getByRole('heading', { name: 'Pipelines' })).toBeVisible()
    const expected = [
      ['loading-one', 'Loading into the scheduler'],
      ['refused-one', 'Not loaded'],
      ['paused-one', 'Paused'],
      ['running-one', 'Running'],
      ['failed-one', 'Failed'],
      ['succeeded-one', 'Succeeded'],
      ['idle-one', 'Not run yet'],
    ] as const
    for (const [name, word] of expected) {
      await expect(page.locator('tbody tr', { hasText: name }), name).toContainText(word)
    }
    await expect(page.locator('tbody tr', { hasText: 'refused-one' })).toContainText(NOT_LOADED_TEXT)

    const gets = () =>
      requests.filter((request) => request.method === 'GET' && pathOf(request) === '/api/pipelines').length
    const before = gets()
    await page.clock.fastForward(10_000)
    await expect.poll(gets, { message: 'the list refetches after the interval' }).toBeGreaterThan(before)
    await context.close()
  })

  test('hostile names are text', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const long = 'x'.repeat(400)
    const evilRow = pipeline({ name: evil, ownerGroup: evil, state: 'failed', lastRun: run({ note: evil }) })
    const longRow = pipeline({ name: long, ownerGroup: long, state: 'failed', lastRun: run({ note: long }) })

    const detailFor = (name: string) => ({
      ...(name === long ? longRow : evilRow),
      runs: [run({ runId: name, note: name })],
      params: {},
      retries: null,
    })
    const runFor = (name: string) => ({
      run: run({ runId: name }),
      tasks: [{
        taskId: name, state: 'failed', tryNumber: 1,
        startedAt: '2026-10-20T08:00:01Z', endedAt: '2026-10-20T08:00:09Z',
        durationSeconds: 8, operator: name,
      }],
    })

    const { context, page } = await open(browser, {
      rows: [evilRow, longRow],
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      read: (route, path, method) => {
        if (method !== 'GET' || !path.startsWith('/api/pipelines/')) return false
        const parts = path.split('/')
        const last = decodeURIComponent(parts[parts.length - 1] ?? '')
        if (path.endsWith('/logs')) {
          json(route, {
            lines: [
              { timestamp: '2026-10-20T08:00:02Z', level: 'error', text: evil },
              { timestamp: '2026-10-20T08:00:03Z', level: 'info', text: long },
            ],
            nextToken: null,
            truncated: false,
          })
          return true
        }
        if (parts.includes('runs')) {
          json(route, runFor(last === long ? long : evil))
          return true
        }
        json(route, detailFor(last === long ? long : evil))
        return true
      },
    })

    async function openLog(name: string) {
      await page.getByRole('button', { name: `Show tasks of run ${name}` }).click()
      await page.getByRole('button', { name: `View log of ${name} try 1` }).click()
      await expect(page.locator('main')).toContainText(evil)
    }

    async function expectNoOverflow(label: string) {
      await page.waitForTimeout(300)
      const s = await page.evaluate(() => ({
        xss: (window as unknown as { __xss?: number }).__xss ?? 0,
        imgs: document.querySelectorAll('img[src="x"]').length,
        overflow: document.documentElement.scrollWidth - window.innerWidth,
      }))
      expect(s, label).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
      expect(s.overflow, `${label} overflow`).toBeLessThanOrEqual(1)
    }

    await page.getByRole('link', { name: evil }).click()
    await expect(page.getByRole('heading', { name: evil })).toBeVisible()
    await openLog(evil)
    await expectNoOverflow('1280px')

    for (const width of [1280, 360]) {
      await page.setViewportSize({ width, height: 740 })
      await page.goto(`/pipelines/${NS}/${encodeURIComponent(long)}`)
      await expect(page.getByRole('heading', { name: long })).toBeVisible()
      await openLog(long)
      await expectNoOverflow(`${width}px long`)
    }
    await context.close()
  })

  test('run now', async ({ browser }) => {
    const queued = run({ state: 'queued', startedAt: null, endedAt: null, durationSeconds: null })
    const rows = [
      pipeline({ name: 'team-a' }),
      pipeline({ name: 'team-b' }),
      pipeline({ name: 'team-paused', paused: true, state: 'paused' }),
    ]
    const { context, page, requests } = await open(browser, {
      rows,
      write: (route, path, method) => {
        if (method !== 'POST' || !path.endsWith('/runs')) return false
        if (path.includes('/team-b/')) {
          json(route, { error: 'the pipeline is paused: resume it first' }, 409)
        } else {
          json(route, queued, 201)
        }
        return true
      },
    })

    const pausedRun = page.getByRole('button', { name: 'Run now team-paused' })
    await expect(pausedRun).toBeDisabled()
    await expect(pausedRun).toHaveAttribute('title', 'Resume the pipeline first')

    await page.getByRole('button', { name: 'Run now team-a' }).click()
    await expect(page.getByRole('status')).toHaveText('Started a run of team-a.')

    await page.getByRole('button', { name: 'Run now team-b' }).click()
    await expect(page.getByRole('alert')).toHaveText('the pipeline is paused: resume it first')

    expect(postsTo(requests, '/runs')).toBe(2)
    expect(requests.filter((request) => request.method === 'POST').map(pathOf).sort()).toEqual([
      `/api/pipelines/${NS}/team-a/runs`,
      `/api/pipelines/${NS}/team-b/runs`,
    ])
    await context.close()
  })

  test('pause and resume', async ({ browser }) => {
    let paused = false
    const { context, page, requests } = await open(browser, {
      read: (route, path, method) => {
        if (path !== '/api/pipelines' || method !== 'GET') return false
        json(route, { pipelines: [pipeline({ name: 'team-a', paused, state: paused ? 'paused' : 'idle' })] })
        return true
      },
      write: (route, path, method) => {
        if (method !== 'POST') return false
        if (path.endsWith('/pause')) {
          paused = true
          json(route, pipeline({ name: 'team-a', paused: true, state: 'paused' }))
          return true
        }
        if (path.endsWith('/resume')) {
          paused = false
          json(route, pipeline({ name: 'team-a' }))
          return true
        }
        return false
      },
    })

    const gets = () =>
      requests.filter((request) => request.method === 'GET' && pathOf(request) === '/api/pipelines').length
    const beforePause = gets()
    await page.getByRole('button', { name: 'Pause team-a' }).click()
    await expect(page.getByRole('button', { name: 'Resume team-a' })).toBeVisible()
    await expect.poll(gets, { message: 'the list is read again after pause' }).toBeGreaterThan(beforePause)
    await expect(page.getByRole('dialog')).toHaveCount(0)

    const beforeResume = gets()
    await page.getByRole('button', { name: 'Resume team-a' }).click()
    await expect(page.getByRole('button', { name: 'Pause team-a' })).toBeVisible()
    await expect.poll(gets, { message: 'the list is read again after resume' }).toBeGreaterThan(beforeResume)
    expect(postsTo(requests, '/pause')).toBe(1)
    expect(postsTo(requests, '/resume')).toBe(1)
    await context.close()
  })

  test('pause and resume refusals show the API sentence', async ({ browser }) => {
    const sentence = 'the scheduler has not loaded this pipeline yet'
    for (const [name, paused] of [['pause-me', false], ['resume-me', true]] as const) {
      const { context, page } = await open(browser, {
        rows: [pipeline({ name, paused, state: paused ? 'paused' : 'idle' })],
        write: (route, _path, method) => {
          if (method !== 'POST') return false
          json(route, { error: sentence }, 409)
          return true
        },
      })
      await page.getByRole('button', { name: paused ? `Resume ${name}` : `Pause ${name}` }).click()
      await expect(page.getByRole('alert'), name).toHaveText(sentence)
      await expect(page.getByRole('dialog'), name).toHaveCount(0)
      await context.close()
    }
  })

  test('P4 stop run names the run, asks once, and Escape cancels', async ({ browser }) => {
    const running = run({
      runId: 'manual__2026-10-20T09:00:00+00:00', state: 'running',
      startedAt: '2026-10-20T09:00:01Z', endedAt: null, durationSeconds: null, note: null,
    })
    const detail = {
      ...pipeline({ name: 'team-a', state: 'running', lastRun: running }),
      runs: [running], params: {}, retries: null,
    }
    let stopCalls = 0
    const { context, page, requests } = await open(browser, {
      start: `/pipelines/${NS}/team-a`,
      read: (route, path, method) => {
        if (method !== 'GET') return false
        if (path.endsWith(`/runs/${encodeURIComponent(running.runId)}`)) {
          json(route, { run: running, tasks: [] })
          return true
        }
        if (path === `/api/pipelines/${NS}/team-a`) {
          json(route, detail)
          return true
        }
        return false
      },
      write: (route, path, method) => {
        if (method !== 'POST' || !path.endsWith('/stop')) return false
        stopCalls += 1
        if (stopCalls === 1) json(route, { error: 'the run is not active' }, 409)
        else json(route, running)
        return true
      },
    })

    const opener = page.getByRole('button', { name: `Stop run ${running.runId}` })
    await expect(opener).toBeVisible()
    await opener.click()
    const dialog = page.getByRole('dialog', { name: 'Stop this run?' })
    await expect(dialog).toBeVisible()
    await expect(dialog).toContainText(running.runId)
    await expect(dialog.getByRole('button', { name: 'Cancel' })).toBeFocused()
    expect(postsTo(requests, '/stop')).toBe(0)

    await page.keyboard.press('Escape')
    await expect(dialog).toHaveCount(0)
    expect(postsTo(requests, '/stop')).toBe(0)
    await expect(opener).toBeFocused()

    await opener.click()
    await expect(dialog).toBeVisible()
    await dialog.getByRole('button', { name: 'Stop run' }).click()
    await expect(dialog.getByRole('alert')).toHaveText('the run is not active')
    await expect(dialog).toBeVisible()
    expect(postsTo(requests, '/stop')).toBe(1)

    await dialog.getByRole('button', { name: 'Stop run' }).click()
    await expect(page.getByRole('status')).toHaveText(`Stopped run ${running.runId}.`)
    await expect(dialog).toHaveCount(0)
    expect(postsTo(requests, '/stop')).toBe(2)
    await context.close()
  })

  test('definition: the document as YAML, read only, read when asked for', async ({ browser }) => {
    const detail = {
      ...pipeline({ name: 'team-a', state: 'idle' }),
      runs: [], params: {}, retries: null,
    }
    const spec = {
      document: {
        name: 'team-a',
        ownerGroup: 'analytics',
        sql: 'select a,\n       b\nfrom t',
        schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' },
        params: { day: '2026-11-01' },
        options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
      },
      resourceVersion: '7',
      managedBy: 'nova',
      editable: true,
    }
    const { context, page, requests } = await open(browser, {
      rows: [detail],
      start: `/pipelines/${NS}/team-a`,
      read: (route, path, method) => {
        if (method !== 'GET') return false
        if (path === `/api/pipelines/${NS}/team-a/spec`) {
          json(route, spec)
          return true
        }
        if (path === `/api/pipelines/${NS}/team-a`) {
          json(route, detail)
          return true
        }
        return false
      },
    })
    const specGets = () =>
      requests.filter((request) => request.method === 'GET' && pathOf(request).endsWith('/spec')).length

    await expect(page.getByRole('heading', { name: 'team-a', exact: true })).toBeVisible()
    expect(specGets(), 'nothing is read until the person asks').toBe(0)
    await page.getByRole('button', { name: 'Show definition' }).click()
    const yaml = page.getByRole('region', { name: 'Definition as YAML' })
    await expect(yaml).toContainText('name: team-a')
    expect(await yaml.textContent()).toBe([
      'name: team-a',
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
    await expect(page.getByRole('button', { name: 'Hide definition' })).toHaveAttribute('aria-expanded', 'true')
    expect(specGets()).toBe(1)
    await expect(page.locator('main')).not.toContainText('managed outside Nova')
    await expect(yaml.locator('textarea, input')).toHaveCount(0)

    await page.getByRole('button', { name: 'Hide definition' }).click()
    await expect(yaml).toHaveCount(0)
    await page.getByRole('button', { name: 'Show definition' }).click()
    await expect(yaml).toBeVisible()
    expect(specGets(), 'the second opening uses what was read').toBe(1)
    await context.close()
  })

  test('definition: a pipeline from outside Nova says so, hostile text stays text, a refusal shows its sentence', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const detailOf = (name: string) => ({
      ...pipeline({ name, state: 'idle' }),
      runs: [], params: {}, retries: null,
    })
    const outside = {
      document: {
        name: 'from-git',
        ownerGroup: 'analytics',
        sql: evil,
        options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
      },
      resourceVersion: '12',
      managedBy: 'other',
      editable: false,
    }
    const { context, page } = await open(browser, {
      rows: [detailOf('from-git'), detailOf('hidden')],
      start: `/pipelines/${NS}/from-git`,
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      read: (route, path, method) => {
        if (method !== 'GET') return false
        if (path === `/api/pipelines/${NS}/from-git/spec`) {
          json(route, outside)
          return true
        }
        if (path === `/api/pipelines/${NS}/hidden/spec`) {
          json(route, { error: 'no such pipeline' }, 404)
          return true
        }
        if (path === `/api/pipelines/${NS}/from-git`) {
          json(route, detailOf('from-git'))
          return true
        }
        if (path === `/api/pipelines/${NS}/hidden`) {
          json(route, detailOf('hidden'))
          return true
        }
        return false
      },
    })

    await page.setViewportSize({ width: 360, height: 740 })
    await page.getByRole('button', { name: 'Show definition' }).click()
    const yaml = page.getByRole('region', { name: 'Definition as YAML' })
    await expect(yaml).toContainText(`sql: ${JSON.stringify(evil)}`)
    await expect(page.locator('main')).toContainText(
      'This pipeline is managed outside Nova (kubectl or GitOps); change it where it comes from.',
    )
    const state = await page.evaluate(() => ({
      xss: (window as unknown as { __xss?: number }).__xss ?? 0,
      imgs: document.querySelectorAll('img[src="x"]').length,
      overflow: document.documentElement.scrollWidth - window.innerWidth,
    }))
    expect(state).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
    expect(state.overflow, 'the definition scrolls inside its box, not the page').toBeLessThanOrEqual(1)

    await page.goto(`/pipelines/${NS}/hidden`)
    await page.getByRole('button', { name: 'Show definition' }).click()
    await expect(page.getByRole('alert')).toContainText('no such pipeline')
    await expect(page.getByRole('region', { name: 'Definition as YAML' })).toHaveCount(0)
    await context.close()
  })

  // The definition editor (stardelt-nova#351, slice S4b): the text is checked while it is typed,
  // nothing is written until "Apply changes", and what is sent carries the version that was read.
  const EDIT_OPTIONS = { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' }
  const EDIT_SQL = 'select a,\n       b\nfrom t'
  const editSpec = (cron = '0 3 * * *', extra: Record<string, unknown> = {}) => ({
    document: {
      name: 'team-a',
      ownerGroup: 'analytics',
      sql: EDIT_SQL,
      schedule: { cron, timezone: 'Europe/Berlin' },
      params: { day: '2026-11-01' },
      options: EDIT_OPTIONS,
    },
    resourceVersion: '7',
    managedBy: 'nova',
    editable: true,
    ...extra,
  })
  const editYaml = (cron: string) => [
    'name: team-a', 'ownerGroup: analytics', 'sql: |-', '  select a,', '         b', '  from t',
    'schedule:', `  cron: "${cron}"`, '  timezone: Europe/Berlin', 'params:', '  day: "2026-11-01"',
    'options:', '  retries: 1', '  backoffMinutes: 5', '  timeoutMinutes: 120', '  concurrency: Forbid', '',
  ].join('\n')

  /** The pipeline page of `team-a` with the spec, the check and the update stubbed. */
  async function openEditor(browser: Browser, {
    specs = [editSpec()],
    check,
    update,
  }: {
    specs?: unknown[]
    check: (body: Record<string, unknown>) => unknown
    update?: (route: Route) => void
  }) {
    const detail = { ...pipeline({ name: 'team-a', state: 'idle' }), runs: [], params: {}, retries: null }
    let specReads = 0
    const { context, page, requests } = await open(browser, {
      rows: [detail],
      start: `/pipelines/${NS}/team-a`,
      read: (route, path, method) => {
        if (method !== 'GET') return false
        if (path === `/api/pipelines/${NS}/team-a/spec`) {
          json(route, specs[Math.min(specReads, specs.length - 1)])
          specReads += 1
          return true
        }
        if (path === `/api/pipelines/${NS}/team-a`) {
          json(route, detail)
          return true
        }
        return false
      },
      write: (route, path, method) => {
        if (method === 'POST' && path === '/api/pipelines/validate') {
          json(route, check(JSON.parse(route.request().postData() ?? '{}')))
          return true
        }
        if (method === 'PUT' && path === `/api/pipelines/${NS}/team-a` && update) {
          update(route)
          return true
        }
        return false
      },
    })
    const sent = (method: string, suffix: string) =>
      requests.filter((request) => request.method === method && pathOf(request).endsWith(suffix))
    await page.getByRole('button', { name: 'Show definition' }).click()
    await page.getByRole('button', { name: 'Edit definition' }).click()
    const editor = page.getByLabel('Definition as YAML (editable)')
    return { context, page, requests, editor, sent, specReads: () => specReads }
  }
  const unchanged = { ok: true, action: 'update', errors: [], changes: [], nextRuns: [] }

  test('definition editor: checked while typed, reviewed, applied with the version it read', async ({ browser }) => {
    const nextRuns = ['2026-11-02T04:00:00+01:00', '2026-11-03T04:00:00+01:00']
    const { context, page, editor, sent } = await openEditor(browser, {
      check: (body) =>
        (body.schedule as { cron: string }).cron === '0 4 * * *'
          ? { ok: true, action: 'update', errors: [], changes: ['schedule'], nextRuns }
          : unchanged,
      update: (route) => json(route, pipeline({ name: 'team-a' })),
    })

    await expect(editor).toHaveValue(editYaml('0 3 * * *'))
    await expect(page.getByText('Nothing changed.')).toBeVisible()
    await expect(page.getByRole('button', { name: 'Apply changes' })).toBeDisabled()
    expect(sent('PUT', '/team-a')).toHaveLength(0)

    await editor.fill(editYaml('0 4 * * *'))
    await expect(page.getByText('This changes: the schedule.')).toBeVisible()
    await expect(page.getByText(/^Next runs: /)).toBeVisible()
    const checks = sent('POST', '/validate')
    expect(JSON.parse(checks[checks.length - 1].body ?? '{}')).toEqual({
      name: 'team-a', ownerGroup: 'analytics', sql: EDIT_SQL,
      schedule: { cron: '0 4 * * *', timezone: 'Europe/Berlin' },
      params: { day: '2026-11-01' }, options: EDIT_OPTIONS,
    })
    expect(sent('PUT', '/team-a'), 'checking writes nothing').toHaveLength(0)

    await page.getByRole('button', { name: 'Apply changes' }).click()
    await expect(page.getByRole('status').filter({ hasText: 'Saved team-a.' })).toBeVisible()
    await expect(editor).toHaveCount(0)
    const puts = sent('PUT', '/team-a')
    expect(puts).toHaveLength(1)
    expect(JSON.parse(puts[0].body ?? '{}')).toEqual({
      sql: EDIT_SQL,
      schedule: { cron: '0 4 * * *', timezone: 'Europe/Berlin' },
      params: { day: '2026-11-01' }, options: EDIT_OPTIONS,
      resourceVersion: '7',
    })
    await context.close()
  })

  test('definition editor: a text that is not a pipeline is explained without asking Nova', async ({ browser }) => {
    const { context, page, editor, sent } = await openEditor(browser, { check: () => unchanged })
    await expect(page.getByText('Nothing changed.')).toBeVisible()
    const before = sent('POST', '/validate').length

    const cases: [string, RegExp][] = [
      ['name: team-a\n\tsql: 1\n', /^Line 2: Tabs are not allowed/],
      ['name: team-a\nshedule: x\n', /^Line 2: Unknown key "shedule"/],
      [editYaml('0 3 * * *').replace('name: team-a', 'name: other'), /^The name cannot change here/],
      [editYaml('0 3 * * *').replace('ownerGroup: analytics', 'ownerGroup: finance'), /^The group of a pipeline cannot change/],
      ['- a\n- b\n', /must be a mapping/],
    ]
    for (const [text, message] of cases) {
      await editor.fill(text)
      await expect(page.getByRole('alert')).toHaveText(message)
      await expect(page.getByRole('button', { name: 'Apply changes' })).toBeDisabled()
    }
    await page.waitForTimeout(800)
    expect(sent('POST', '/validate').length, 'Nova is not asked about text that is not a pipeline').toBe(before)
    expect(sent('PUT', '/team-a')).toHaveLength(0)
    await context.close()
  })

  test('definition editor: Nova refuses, then a concurrent change is refused and reloaded', async ({ browser }) => {
    let puts = 0
    const { context, page, editor, sent, specReads } = await openEditor(browser, {
      specs: [editSpec(), editSpec('0 5 * * *', { resourceVersion: '8' })],
      check: (body) => {
        const cron = (body.schedule as { cron: string }).cron
        if (cron === '0 3 * * 9') return { ok: false, errors: [{ message: 'a cron has five fields' }], changes: [], nextRuns: [] }
        return cron === '0 3 * * *' ? unchanged : { ok: true, action: 'update', errors: [], changes: ['schedule'], nextRuns: [] }
      },
      update: (route) => {
        puts += 1
        json(route, { error: 'the pipeline changed while you edited it: reload' }, 409)
      },
    })

    await editor.fill(editYaml('0 3 * * 9'))
    await expect(page.getByRole('alert')).toHaveText('a cron has five fields')
    await expect(page.getByRole('button', { name: 'Apply changes' })).toBeDisabled()

    await editor.fill(editYaml('0 4 * * *'))
    await page.getByRole('button', { name: 'Apply changes' }).click()
    await expect(page.getByRole('alert')).toContainText('the pipeline changed while you edited it: reload')
    expect(puts).toBe(1)
    expect(sent('PUT', '/team-a')).toHaveLength(1)
    await expect(page.getByRole('button', { name: 'Apply changes' })).toBeDisabled()

    expect(specReads()).toBe(1)
    await page.getByRole('button', { name: 'Reload the definition' }).click()
    await expect(editor).toHaveValue(editYaml('0 5 * * *'))
    expect(specReads(), 'the definition was read again').toBe(2)
    await expect(page.getByRole('alert')).toHaveCount(0)
    await context.close()
  })

  test('definition editor: a pipeline Nova cannot change has no edit button', async ({ browser }) => {
    const detail = { ...pipeline({ name: 'from-git', state: 'idle' }), runs: [], params: {}, retries: null }
    const { context, page } = await open(browser, {
      rows: [detail],
      start: `/pipelines/${NS}/from-git`,
      read: (route, path, method) => {
        if (method !== 'GET') return false
        if (path === `/api/pipelines/${NS}/from-git/spec`) {
          json(route, { ...editSpec(), managedBy: 'other', editable: false })
          return true
        }
        if (path === `/api/pipelines/${NS}/from-git`) {
          json(route, detail)
          return true
        }
        return false
      },
    })
    await page.getByRole('button', { name: 'Show definition' }).click()
    await expect(page.getByRole('region', { name: 'Definition as YAML' })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Edit definition' })).toHaveCount(0)
    await expect(page.getByLabel('Definition as YAML (editable)')).toHaveCount(0)
    await context.close()
  })

  test('deployment: a pipeline a bundle deployed says where from, one made by hand does not', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const commit = '0123abcdef0123abcdef0123abcdef0123abcdef'
    const detailOf = (name: string, deployment?: Record<string, unknown>) => ({
      ...pipeline({ name, state: 'idle', ...(deployment ? { deployment } : {}) }),
      runs: [], params: {}, retries: null,
    })
    const pages: Record<string, unknown> = {
      deployed: detailOf('deployed', { bundle: 'shop-etl', target: 'prod', revision: commit, deployedBy: evil, deployedAt: '2026-11-01T08:00:00Z' }),
      dirty: detailOf('dirty', { bundle: 'shop-etl', target: 'dev', revision: `${commit}-dirty` }),
      bare: detailOf('bare', { bundle: 'shop-etl', target: 'test' }),
      drifted: detailOf('drifted', { bundle: 'shop-etl', target: 'prod', revision: commit, modified: true }),
      'by-hand': detailOf('by-hand'),
    }
    const { context, page } = await open(browser, {
      rows: Object.values(pages),
      start: `/pipelines/${NS}/deployed`,
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      read: (route, path, method) => {
        const name = path.replace(`/api/pipelines/${NS}/`, '')
        if (method !== 'GET' || !(name in pages)) return false
        json(route, pages[name])
        return true
      },
    })

    const fact = page.locator('dl > div', { has: page.getByText('Deployed from', { exact: true }) })
    await expect(fact).toContainText('shop-etl · prod · 0123abc')
    await expect(fact.locator('[title]')).toHaveAttribute('title', commit)
    await expect(fact).toContainText(`by ${evil}`)
    await expect(fact).not.toContainText('Changed since the last deploy')
    expect(await page.evaluate(() => ({
      xss: (window as unknown as { __xss?: number }).__xss ?? 0,
      imgs: document.querySelectorAll('img[src="x"]').length,
    }))).toEqual({ xss: 0, imgs: 0 })

    await page.goto(`/pipelines/${NS}/dirty`)
    await expect(fact).toContainText('shop-etl · dev · 0123abc (uncommitted changes)')
    await page.goto(`/pipelines/${NS}/bare`)
    await expect(fact).toContainText('shop-etl · test')
    await expect(fact).not.toContainText('by ')
    await page.goto(`/pipelines/${NS}/drifted`)
    await expect(fact).toContainText('Changed since the last deploy. The next deploy of this bundle overwrites the change.')
    await page.goto(`/pipelines/${NS}/by-hand`)
    await expect(page.getByRole('heading', { name: 'by-hand', exact: true })).toBeVisible()
    await expect(page.getByText('Deployed from', { exact: true })).toHaveCount(0)
    await context.close()
  })

  test('log viewer', async ({ browser }) => {
    const failed = run({ state: 'failed', note: null })
    const detail = {
      ...pipeline({ name: 'team-a', state: 'failed', lastRun: failed }),
      runs: [failed], params: {}, retries: null,
    }
    const script = '<script>window.__xss=1</script>'
    const { context, page, requests } = await open(browser, {
      start: `/pipelines/${NS}/team-a`,
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      read: (route, path, method, url) => {
        if (method !== 'GET') return false
        if (path.endsWith('/logs')) {
          if (url.searchParams.get('token') === 'T') {
            json(route, {
              lines: [{ timestamp: '2026-10-20T08:00:05Z', level: 'warning', text: 'second page' }],
              nextToken: null,
              truncated: false,
            })
          } else {
            json(route, {
              lines: [
                { timestamp: '2026-10-20T08:00:02Z', level: 'error', text: 'first line' },
                { timestamp: '2026-10-20T08:00:03Z', level: 'info', text: script },
              ],
              nextToken: 'T',
              truncated: true,
            })
          }
          return true
        }
        if (path.endsWith(`/runs/${encodeURIComponent(failed.runId)}`)) {
          json(route, {
            run: failed,
            tasks: [{
              taskId: 'load', state: 'failed', tryNumber: 1,
              startedAt: '2026-10-20T08:00:01Z', endedAt: '2026-10-20T08:00:09Z',
              durationSeconds: 8, operator: 'BashOperator',
            }],
          })
          return true
        }
        if (path === `/api/pipelines/${NS}/team-a`) {
          json(route, detail)
          return true
        }
        return false
      },
    })

    await page.getByRole('button', { name: `Show tasks of run ${failed.runId}` }).click()
    await page.getByRole('button', { name: 'View log of load try 1' }).click()

    const main = page.locator('main')
    await expect(main).toContainText('first line')
    // Levels are words; the script-like line is text, not an element.
    await expect(main).toContainText('Error')
    await expect(main).toContainText('Info')
    await expect(main).toContainText(script)
    await expect(main).toContainText('The log was cut at 2000 lines.')
    expect(await page.locator('script', { hasText: 'window.__xss=1' }).count()).toBe(0)
    expect(await page.evaluate(() => (window as unknown as { __xss?: number }).__xss ?? 0)).toBe(0)

    const logCalls = (token: string | null) => requests.filter((request) => {
      const url = new URL(request.url)
      return url.pathname.endsWith('/logs') && url.searchParams.get('token') === token
    }).length
    expect(logCalls('T')).toBe(0)
    expect(requests.filter((request) => new URL(request.url).searchParams.get('try') === '1')).toHaveLength(1)

    await page.getByRole('button', { name: 'Load more' }).click()
    await expect(main).toContainText('second page')
    await expect.poll(() => logCalls('T'), { message: 'one request carries the token' }).toBe(1)
    await expect(page.getByRole('button', { name: 'Load more' })).toHaveCount(0)
    await context.close()
  })

  test('404 for a hidden pipeline', async ({ browser }) => {
    const { context, page } = await open(browser, {
      start: `/pipelines/${NS}/hidden`,
      read: (route, path, method) => {
        if (method !== 'GET' || path !== `/api/pipelines/${NS}/hidden`) return false
        json(route, { error: 'no such pipeline' }, 404)
        return true
      },
    })
    await expect(page.getByRole('heading', { name: 'Not found' })).toBeVisible()
    await expect(page.getByRole('link', { name: 'Back to Pipelines' })).toBeVisible()
    await expect(page.locator('main')).toContainText('no such pipeline')
    await context.close()
  })

  test('states: loading, empty, a retryable error and 503 without Retry', async ({ browser }) => {
    const empty = await open(browser)
    await expect(empty.page.getByText('No pipelines yet. Open the SQL editor, write a statement and press Schedule.')).toBeVisible()
    await expect(empty.page.getByRole('link', { name: 'Open the SQL editor' })).toHaveAttribute('href', '/query')
    await empty.context.close()

    const loading = await open(browser, {
      read: (route, path, method) => {
        if (path !== '/api/pipelines' || method !== 'GET') return false
        void new Promise((resolve) => setTimeout(resolve, 1500)).then(() => json(route, { pipelines: [] }))
        return true
      },
    })
    await expect(loading.page.getByText('Loading pipelines…')).toBeVisible()
    await expect(loading.page.getByText('No pipelines yet. Open the SQL editor, write a statement and press Schedule.')).toBeVisible()
    await loading.context.close()

    const broken = await open(browser, {
      read: (route, path, method) => {
        if (path !== '/api/pipelines' || method !== 'GET') return false
        json(route, { error: 'the scheduler did not answer' }, 502)
        return true
      },
    })
    await expect(broken.page.getByText('the scheduler did not answer')).toBeVisible()
    const retry = broken.page.getByRole('button', { name: 'Retry' })
    await expect(retry).toBeVisible()
    const gets = () => broken.requests.filter((request) => request.method === 'GET' && pathOf(request) === '/api/pipelines').length
    const before = gets()
    await retry.click()
    await expect.poll(gets, { message: 'Retry asks again' }).toBeGreaterThan(before)
    await broken.context.close()

    const missing = await open(browser, {
      read: (route, path, method) => {
        if (path !== '/api/pipelines' || method !== 'GET') return false
        json(route, { error: 'the scheduler is not configured' }, 503)
        return true
      },
    })
    await expect(missing.page.getByText('the scheduler is not configured')).toBeVisible()
    await expect(missing.page.getByText('An administrator has to set up the scheduler connection for this installation.')).toBeVisible()
    await expect(missing.page.getByRole('button', { name: 'Retry' })).toHaveCount(0)
    await missing.context.close()
  })

  test('U5 the list, a run, the stop dialog and the log have accessible names', async ({ browser }) => {
    const running = run({
      runId: 'manual__2026-10-20T10:00:00+00:00', state: 'running',
      startedAt: '2026-10-20T10:00:01Z', endedAt: null, durationSeconds: null, note: null,
    })
    const detail = {
      ...pipeline({ name: 'team-a', state: 'running', lastRun: running }),
      runs: [running], params: {}, retries: null,
    }
    const { context, page } = await open(browser, {
      rows: [pipeline({ name: 'team-a', state: 'running', lastRun: running })],
      read: (route, path, method) => {
        if (method !== 'GET') return false
        if (path.endsWith('/logs')) {
          json(route, { lines: [{ timestamp: '2026-10-20T10:00:02Z', level: 'info', text: 'hello' }], nextToken: null, truncated: false })
          return true
        }
        if (path.endsWith(`/runs/${encodeURIComponent(running.runId)}`)) {
          json(route, {
            run: running,
            tasks: [{
              taskId: 'load', state: 'running', tryNumber: 1,
              startedAt: '2026-10-20T10:00:01Z', endedAt: null,
              durationSeconds: null, operator: 'BashOperator',
            }],
          })
          return true
        }
        if (path === `/api/pipelines/${NS}/team-a`) {
          json(route, detail)
          return true
        }
        return false
      },
    })

    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])

    await page.getByRole('link', { name: 'team-a' }).click()
    await expect(page.getByRole('heading', { name: 'team-a' })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])

    await page.getByRole('button', { name: `Show tasks of run ${running.runId}` }).click()
    await page.getByRole('button', { name: `Stop run ${running.runId}` }).click()
    await expect(page.getByRole('dialog', { name: 'Stop this run?' })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])

    await page.keyboard.press('Escape')
    await page.getByRole('button', { name: 'View log of load try 1' }).click()
    await expect(page.locator('main')).toContainText('hello')
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    await context.close()
  })
})

test.describe('schedule', () => {
  // *Schedule* (stardelt-nova#211): the SQL editor's dialog, the same dialog in edit mode on
  // the pipeline page, and the one destructive confirmation there. Every `/api/**` call is
  // stubbed; the principals route reports one ready group and one without a principal.
  const NS = 'stardelt'
  const ADMIN = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: true, groups: ['analytics', 'finance'],
  }
  const READY = { group: 'analytics', principal: 'pipeline-analytics', ready: true, message: null }
  const NO_PRINCIPAL = {
    group: 'finance',
    principal: null,
    ready: false,
    message: 'No principal for this group yet: an administrator creates one with make sync-principal NAME=pipeline-finance GROUP=finance',
  }

  type Recorded = { method: string; url: string; body: string | null }

  function pipeline(overrides: Record<string, unknown> = {}) {
    return {
      name: 'team-smoke',
      namespace: NS,
      ownerGroup: 'analytics',
      schedule: '0 3 * * * (Europe/Berlin)',
      paused: false,
      loaded: true,
      nextRun: null,
      state: 'idle',
      lastRun: null,
      tasks: [{ name: 'run', kind: 'sql', sql: 'select 1' }],
      runAs: 'pipeline-analytics',
      concurrency: 'Forbid',
      createdAt: '2026-10-19T10:00:00Z',
      ...overrides,
    }
  }

  /** The editor or the pipeline page with every `/api/**` call stubbed; `write` decides writes. */
  async function open(browser: Browser, {
    start = '/query',
    me = ADMIN,
    groups = [READY, NO_PRINCIPAL] as unknown[],
    rows = [] as unknown[],
    detail,
    write,
    init,
  }: {
    start?: string
    me?: unknown
    groups?: unknown[]
    rows?: unknown[]
    detail?: Record<string, unknown>
    write?: (route: Route, path: string, method: string, body: string | null) => boolean
    init?: () => void
  } = {}) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    if (init) await page.addInitScript(init)
    const requests: Recorded[] = []
    page.on('request', (request) => requests.push({
      method: request.method(),
      url: request.url(),
      body: request.postData(),
    }))
    await page.route('**/api/**', (route) => {
      const request = route.request()
      const path = new URL(request.url()).pathname
      const method = request.method()
      if (path === '/api/me' && method === 'GET') return json(route, me)
      if (path === '/api/catalog/namespaces' && method === 'GET') return json(route, { namespaces: [] })
      if (path === '/api/pipelines/principals' && method === 'GET') return json(route, { groups })
      if (path === '/api/pipelines' && method === 'GET') return json(route, { pipelines: rows })
      if (method !== 'GET' && write?.(route, path, method, request.postData())) return
      // The options of stardelt-nova#224 ask two more routes without changing the write:
      // a settled cadence previews its next runs and a statement can be checked.
      if (path === '/api/pipelines/preview-schedule' && method === 'POST') {
        return json(route, {
          runs: [
            '2026-10-21T03:00:00+02:00',
            '2026-10-22T03:00:00+02:00',
            '2026-10-23T03:00:00+02:00',
            '2026-10-24T03:00:00+02:00',
            '2026-10-25T03:00:00+02:00',
          ],
        })
      }
      if (path === '/api/pipelines/check' && method === 'POST') return json(route, { checked: true, ok: true })
      if (method === 'GET' && detail
        && path === `/api/pipelines/${NS}/${encodeURIComponent(String(detail.name))}`) {
        return json(route, detail)
      }
      return json(route, [])
    })
    await page.goto(start)
    return { context, page, requests }
  }

  /** Open the editor's dialog and return it. */
  async function openDialog(page: Page): Promise<Locator> {
    await page.getByRole('button', { name: 'Schedule this statement' }).click()
    const dialog = page.getByRole('dialog', { name: 'Schedule this statement' })
    await expect(dialog).toBeVisible()
    return dialog
  }

  /** The values of every attribute on the page, one per line. */
  async function attributes(page: Page): Promise<string> {
    return page.evaluate(() => [...document.querySelectorAll('*')]
      .flatMap((element) => [...element.attributes].map((attribute) => attribute.value))
      .join('\n'))
  }

  test('schedule button needs a statement and opens with it', async ({ browser }) => {
    const { context, page } = await open(browser)
    const editor = page.getByLabel('SQL query')
    const opener = page.getByRole('button', { name: 'Schedule this statement' })

    await expect(opener).toBeEnabled()
    await editor.fill('')
    await expect(opener).toBeDisabled()
    await expect(opener).toHaveAttribute('title', 'Write a statement first')
    expect(await page.getByRole('dialog').count()).toBe(0)

    await editor.fill('select 1')
    await expect(opener).toBeEnabled()
    const dialog = await openDialog(page)
    await expect(dialog.getByRole('textbox', { name: 'Statement', exact: true })).toHaveValue('select 1')
    await expect(dialog.getByRole('textbox', { name: 'Name', exact: true })).toBeFocused()
    await context.close()
  })

  test('groups without a principal are disabled and say why', async ({ browser }) => {
    const first = await open(browser)
    let dialog = await openDialog(first.page)
    const options = dialog.getByRole('combobox', { name: 'Group', exact: true }).locator('option')
    expect(await options.filter({ hasText: 'analytics' }).evaluate((element) => (element as HTMLOptionElement).disabled)).toBe(false)
    expect(await options.filter({ hasText: 'finance' }).evaluate((element) => (element as HTMLOptionElement).disabled)).toBe(true)
    // The ready group is the default, so Create works without a detour.
    await expect(dialog.getByRole('combobox', { name: 'Group', exact: true })).toHaveValue('analytics')
    await expect(dialog.getByRole('button', { name: 'Create schedule' })).toBeEnabled()
    await first.context.close()

    // No group is ready: the first group's sentence, Create stays disabled.
    const third = await open(browser, {
      groups: [NO_PRINCIPAL, { ...READY, group: 'analytics', principal: null, ready: false, message: 'pipeline-analytics is not ready' }],
    })
    dialog = await openDialog(third.page)
    await expect(dialog.getByText(NO_PRINCIPAL.message, { exact: true })).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Create schedule' })).toBeDisabled()
    // A pending group cannot be chosen, so the dialog says why for the one it shows.
    await expect(dialog.getByRole('combobox', { name: 'Group', exact: true }).locator('option').filter({ hasText: 'analytics' }))
      .toHaveJSProperty('disabled', true)
    await third.context.close()

    // No group at all is said plainly.
    const none = await open(browser, { groups: [] })
    dialog = await openDialog(none.page)
    await expect(dialog.getByText('You are not a member of any group, so you cannot schedule.', { exact: true })).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Create schedule' })).toBeDisabled()
    await none.context.close()
  })

  test('the cron rule matches the backend table', async () => {
    // The ticket asks for the rule's table as plain assertions (not through the dialog).
    const { validateCron } = await import('../src/schedule')
    const accepted = [
      '0 3 * * *',
      '0 8 * * 1',
      '0 9 1 * *',
      '0 3 * * 1-5',
      '30 6 * * 0,6',
      '0 3 * * 7',
      '0 3 * * SUN',
      '0 3 * JAN mon',
      '0,30 3 * * *',
      '0-5/5 3 1-15 * *',
      '  0 3 * * *  ',
    ]
    for (const text of accepted) expect(validateCron(text), text).toBeNull()

    const refused: Array<[string, string]> = [
      ['* * * * *', 'at most one run every 5 minutes'],
      ['*/2 * * * *', 'at most one run every 5 minutes'],
      ['0,1 3 * * *', 'at most one run every 5 minutes'],
      ['0 3 * *', 'a schedule has five fields: minute hour day month weekday'],
      ['0 3 * * * *', 'a schedule has five fields: minute hour day month weekday'],
      ['@daily', 'a schedule has five fields: minute hour day month weekday'],
      ['', 'a schedule has five fields: minute hour day month weekday'],
      ['0 25 * * *', 'a field is out of range'],
      ['0 3 1 13 *', 'a field is out of range'],
      ['0 3 * * 8', 'a field is out of range'],
      ['0 3 * * 1/2', 'a field is out of range'],
      ['*/0 * * * *', 'a field is out of range'],
      ['0 3 * nope *', 'a field is out of range'],
    ]
    for (const [text, sentence] of refused) expect(validateCron(text), text).toBe(sentence)
  })

  test('presets build the cron', async ({ browser }) => {
    const { context, page } = await open(browser)
    const dialog = await openDialog(page)
    const shows = (text: string) => expect(dialog.getByText(text, { exact: true })).toBeVisible()

    await dialog.getByRole('radio', { name: 'Every hour' }).check()
    await shows('0 * * * *')

    await dialog.getByRole('radio', { name: 'Every day at' }).check()
    await dialog.getByRole('textbox', { name: 'Time of day', exact: true }).fill('06:30')
    await shows('30 6 * * *')

    await dialog.getByRole('radio', { name: 'Every weekday at' }).check()
    await shows('30 6 * * 1-5')

    await dialog.getByRole('radio', { name: 'Every week on' }).check()
    await dialog.getByRole('combobox', { name: 'Weekday', exact: true }).selectOption('1')
    await dialog.getByRole('textbox', { name: 'Time of day', exact: true }).fill('08:00')
    await shows('0 8 * * 1')

    await dialog.getByRole('radio', { name: 'Every month on the 1st at' }).check()
    await dialog.getByRole('textbox', { name: 'Time of day', exact: true }).fill('09:00')
    await shows('0 9 1 * *')

    await dialog.getByRole('radio', { name: 'On demand only' }).check()
    await expect(dialog.getByText('0 9 1 * *')).toHaveCount(0)
    await context.close()
  })

  test('custom cron is checked like the backend', async ({ browser }) => {
    const { context, page } = await open(browser)
    const dialog = await openDialog(page)
    const create = dialog.getByRole('button', { name: 'Create schedule' })
    await dialog.getByRole('radio', { name: 'Custom' }).check()
    const field = dialog.getByRole('textbox', { name: 'Custom schedule', exact: true })

    for (const [text, sentence] of [
      ['* * * * *', 'at most one run every 5 minutes'],
      ['*/2 * * * *', 'at most one run every 5 minutes'],
      ['0 3 * *', 'a schedule has five fields: minute hour day month weekday'],
      ['0 25 * * *', 'a field is out of range'],
      ['@daily', 'a schedule has five fields: minute hour day month weekday'],
    ] as const) {
      await field.fill(text)
      await expect(dialog.getByText(sentence, { exact: true }), text).toBeVisible()
      await expect(create, text).toBeDisabled()
    }

    await field.fill('0 3 * * *')
    await expect(dialog.getByText('at most one run every 5 minutes', { exact: true })).toHaveCount(0)
    await expect(dialog.getByText('a schedule has five fields: minute hour day month weekday', { exact: true })).toHaveCount(0)
    await expect(create).toBeEnabled()
    await context.close()
  })

  test('create sends exactly the expected body and shows the status line', async ({ browser }) => {
    const detail = pipeline({ name: 'nightly-report' })
    let posted: unknown = null
    const first = await open(browser, {
      write: (route, path, method, body) => {
        if (path !== '/api/pipelines' || method !== 'POST') return false
        posted = JSON.parse(body ?? '{}')
        json(route, { ...detail, runs: [], params: {}, retries: null }, 201)
        return true
      },
    })
    await first.page.getByLabel('SQL query').fill('  select 1  ')
    const dialog = await openDialog(first.page)
    await expect(dialog.getByRole('textbox', { name: 'Statement', exact: true })).toHaveValue('  select 1  ')
    await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('nightly-report')
    await dialog.getByRole('radio', { name: 'Every day at' }).check()
    await dialog.getByRole('textbox', { name: 'Time of day', exact: true }).fill('03:00')
    await dialog.getByRole('combobox', { name: 'Time zone', exact: true }).selectOption('Europe/Berlin')
    await dialog.getByRole('button', { name: 'Create schedule' }).click()

    await expect(first.page.getByRole('status')).toContainText(
      'Scheduled nightly-report. It appears under Pipelines within a minute.',
    )
    await expect(first.page.getByRole('link', { name: 'nightly-report' }))
      .toHaveAttribute('href', `/pipelines/${NS}/nightly-report`)
    expect(posted).toEqual({
      name: 'nightly-report',
      ownerGroup: 'analytics',
      sql: 'select 1',
      schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' },
      params: {},
      options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
    })
    expect(await first.page.getByRole('dialog').count()).toBe(0)
    await first.context.close()
  })

  test('a 409 keeps the dialog, its sentence and the typed fields', async ({ browser }) => {
    const { context, page } = await open(browser, {
      write: (route, path, method) => {
        if (path !== '/api/pipelines' || method !== 'POST') return false
        json(route, { error: 'a pipeline with this name already exists' }, 409)
        return true
      },
    })
    await page.getByLabel('SQL query').fill('select 2')
    const dialog = await openDialog(page)
    await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('taken-name')
    await dialog.getByRole('radio', { name: 'Every weekday at' }).check()
    await dialog.getByRole('button', { name: 'Create schedule' }).click()

    await expect(dialog.getByRole('alert')).toHaveText('a pipeline with this name already exists')
    await expect(dialog).toBeVisible()
    await expect(dialog.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue('taken-name')
    await expect(dialog.getByRole('textbox', { name: 'Statement', exact: true })).toHaveValue('select 2')
    await expect(dialog.getByRole('radio', { name: 'Every weekday at' })).toBeChecked()
    await context.close()
  })

  test('edit sends the complete editable state, and delete asks once', async ({ browser }) => {
    const detail = { ...pipeline(), runs: [], params: {}, retries: null }
    const { context, page, requests } = await open(browser, {
      start: `/pipelines/${NS}/team-smoke`,
      detail,
      write: (route, path, method) => {
        if (path !== `/api/pipelines/${NS}/team-smoke`) return false
        if (method === 'PUT') {
          json(route, detail)
          return true
        }
        if (method === 'DELETE') {
          void route.fulfill({ status: 204 })
          return true
        }
        return false
      },
    })

    await page.getByRole('button', { name: 'Edit schedule' }).click()
    const dialog = page.getByRole('dialog', { name: 'Edit the schedule' })
    await expect(dialog).toBeVisible()
    await expect(dialog.getByRole('textbox', { name: 'Name', exact: true })).toHaveValue('team-smoke')
    await expect(dialog.getByRole('textbox', { name: 'Group', exact: true })).toHaveValue('analytics')
    await expect(dialog.getByRole('textbox', { name: 'Statement', exact: true })).toHaveValue('select 1')
    await dialog.getByRole('textbox', { name: 'Time of day', exact: true }).fill('04:00')
    await dialog.getByRole('button', { name: 'Save schedule' }).click()
    await expect(page.getByRole('status')).toHaveText('Saved team-smoke.')

    const puts = requests.filter((request) => request.method === 'PUT')
    expect(puts.map((request) => new URL(request.url).pathname)).toEqual([`/api/pipelines/${NS}/team-smoke`])
    const putBody = JSON.parse(puts[0].body ?? '{}') as Record<string, unknown>
    expect(Object.keys(putBody).sort()).toEqual(['options', 'params', 'schedule', 'sql'])
    expect(putBody).toEqual({
      sql: 'select 1',
      schedule: { cron: '0 4 * * *', timezone: 'Europe/Berlin' },
      params: {},
      options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
    })

    const opener = page.getByRole('button', { name: 'Delete pipeline' })
    await opener.click()
    const confirm = page.getByRole('dialog', { name: 'Delete team-smoke?' })
    await expect(confirm).toContainText(
      'The pipeline and its schedule are removed. Past runs stay readable for a while in Airflow but disappear from this page.',
    )
    await expect(confirm.getByRole('button', { name: 'Cancel' })).toBeFocused()
    expect(requests.filter((request) => request.method === 'DELETE')).toEqual([])

    await page.keyboard.press('Escape')
    await expect(confirm).toHaveCount(0)
    expect(requests.filter((request) => request.method === 'DELETE')).toEqual([])
    await expect(opener).toBeFocused()

    await opener.click()
    await page.getByRole('dialog', { name: 'Delete team-smoke?' })
      .getByRole('button', { name: 'Delete pipeline' })
      .click()
    await expect(page.getByRole('status')).toHaveText('Deleted team-smoke.')
    const deletes = requests.filter((request) => request.method === 'DELETE')
    expect(deletes).toHaveLength(1)
    expect(new URL(deletes[0].url).pathname).toBe(`/api/pipelines/${NS}/team-smoke`)
    await context.close()
  })

  test('edit without a statement from the read API asks for it and stays truthful', async ({ browser }) => {
    // The read API does not carry `tasks[].sql` yet (stardelt-nova#211 asks for it); until it
    // does, edit mode shows an empty statement and says so rather than inventing one.
    const detail = {
      ...pipeline({ tasks: [{ name: 'run', kind: 'sql' }] }),
      runs: [],
      params: {},
      retries: null,
    }
    const { context, page } = await open(browser, { start: `/pipelines/${NS}/team-smoke`, detail })
    await page.getByRole('button', { name: 'Edit schedule' }).click()
    const dialog = page.getByRole('dialog', { name: 'Edit the schedule' })
    await expect(dialog.getByText(
      "This pipeline's statement did not arrive with the pipeline, so it is empty here. Type it to change the schedule.",
      { exact: true },
    )).toBeVisible()
    await expect(dialog.getByRole('textbox', { name: 'Statement', exact: true })).toHaveValue('')
    await expect(dialog.getByRole('button', { name: 'Save schedule' })).toBeDisabled()
    await dialog.getByRole('textbox', { name: 'Statement', exact: true }).fill('select 7')
    await expect(dialog.getByRole('button', { name: 'Save schedule' })).toBeEnabled()
    await context.close()
  })

  test('hostile statement, name and refusal stay text and out of attributes', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const canary = 'canary-7f3a'
    const first = await open(browser, {
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      write: (route, path, method) => {
        if (path !== '/api/pipelines' || method !== 'POST') return false
        json(route, { error: evil }, 409)
        return true
      },
    })
    await first.page.getByLabel('SQL query').fill(`select '${canary}' -- ${evil}`)
    const dialog = await openDialog(first.page)
    await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('hostile-one')
    await dialog.getByRole('button', { name: 'Create schedule' }).click()
    await expect(dialog.getByRole('alert')).toHaveText(evil)
    expect(await first.page.evaluate(() => ({
      xss: (window as unknown as { __xss?: number }).__xss ?? 0,
      imgs: document.querySelectorAll('img[src="x"]').length,
    }))).toEqual({ xss: 0, imgs: 0 })

    await first.page.keyboard.press('Escape')
    await expect(dialog).toHaveCount(0)
    // The statement may live in the textarea; it may not live in an attribute or anywhere
    // outside a textarea (a title, a URL, an error box).
    expect(await attributes(first.page)).not.toContain(canary)
    const outsideTextareas = await first.page.evaluate(() => {
      const clone = document.documentElement.cloneNode(true) as HTMLElement
      clone.querySelectorAll('textarea').forEach((element) => element.remove())
      return clone.outerHTML
    })
    expect(outsideTextareas).not.toContain(canary)
    await first.context.close()

    // A hostile pipeline name is text in the heading and in the confirmation's title.
    const detail = { ...pipeline({ name: evil }), runs: [], params: {}, retries: null }
    const second = await open(browser, {
      start: `/pipelines/${NS}/${encodeURIComponent(evil)}`,
      detail,
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
    })
    await expect(second.page.getByRole('heading', { name: evil })).toBeVisible()
    await second.page.getByRole('button', { name: 'Delete pipeline' }).click()
    await expect(second.page.getByRole('dialog')).toContainText(`Delete ${evil}?`)
    expect(await second.page.evaluate(() => (window as unknown as { __xss?: number }).__xss ?? 0)).toBe(0)
    expect(await second.page.locator('img[src="x"]').count()).toBe(0)
    await second.context.close()
  })

  test('U5 the create dialog, the edit dialog and the delete dialog have accessible names', async ({ browser }) => {
    const detail = { ...pipeline(), runs: [], params: {}, retries: null }
    const { context, page } = await open(browser, { detail })
    await page.setViewportSize({ width: 360, height: 740 })

    await openDialog(page)
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    const box = await page.getByRole('dialog').boundingBox()
    expect(box).not.toBeNull()
    expect(box!.x).toBeGreaterThanOrEqual(0)
    expect(box!.x + box!.width).toBeLessThanOrEqual(361)
    expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)).toBeLessThanOrEqual(1)
    await page.keyboard.press('Escape')

    await page.goto(`/pipelines/${NS}/team-smoke`)
    await expect(page.getByRole('heading', { name: 'team-smoke' })).toBeVisible()
    await page.getByRole('button', { name: 'Edit schedule' }).click()
    await expect(page.getByRole('dialog', { name: 'Edit the schedule' })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    await page.keyboard.press('Escape')

    await page.getByRole('button', { name: 'Delete pipeline' }).click()
    await expect(page.getByRole('dialog', { name: 'Delete team-smoke?' })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    await context.close()
  })
})

test.describe('schedule options', () => {
  // The dialog's parameters, placeholder chips, statement check, next runs and advanced
  // options (stardelt-nova#223 API, #224 dialog). Every `/api/**` call is stubbed; the
  // preview and check routes may be replaced per test.
  const NS = 'stardelt'
  const ME = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: true, groups: ['analytics', 'finance'],
  }
  const READY = { group: 'analytics', principal: 'pipeline-analytics', ready: true, message: null }
  const READY_2 = { group: 'finance', principal: 'pipeline-finance', ready: true, message: null }
  const EVIL = '<img src=x onerror="window.__xss=1">'
  /** Five fire times in Europe/Berlin; the last is after the October change, so no DST edge. */
  const RUNS = [
    '2026-10-21T03:00:00+02:00',
    '2026-10-22T03:00:00+02:00',
    '2026-10-23T03:00:00+02:00',
    '2026-10-24T03:00:00+02:00',
    '2026-10-26T03:00:00+01:00',
  ]

  type Recorded = { method: string; url: string; body: string | null }
  type CheckBody = { sql?: string; params?: Record<string, string> }
  type PreviewBody = { cron?: string; timezone?: string; count?: number }

  function pipeline(overrides: Record<string, unknown> = {}) {
    return {
      name: 'team-smoke',
      namespace: NS,
      ownerGroup: 'analytics',
      schedule: '0 3 * * * (Europe/Berlin)',
      paused: false,
      loaded: true,
      nextRun: null,
      state: 'idle',
      lastRun: null,
      tasks: [{ name: 'run', kind: 'sql', sql: 'select 1' }],
      runAs: 'pipeline-analytics',
      concurrency: 'Forbid',
      createdAt: '2026-10-19T10:00:00Z',
      runs: [],
      params: {},
      retries: null,
      options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
      ...overrides,
    }
  }

  /** The editor or a pipeline page with every `/api/**` call stubbed. */
  async function open(browser: Browser, {
    start = '/query',
    detail,
    write,
    check,
    preview,
    timezoneId = 'Europe/Berlin',
    init,
  }: {
    start?: string
    detail?: Record<string, unknown>
    write?: (route: Route, path: string, method: string, body: string | null) => boolean
    check?: (body: CheckBody) => unknown
    preview?: (route: Route, body: PreviewBody) => void
    timezoneId?: string
    init?: () => void
  } = {}) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, timezoneId })
    const page = await context.newPage()
    if (init) await page.addInitScript(init)
    const requests: Recorded[] = []
    page.on('request', (request) => requests.push({
      method: request.method(),
      url: request.url(),
      body: request.postData(),
    }))
    await page.route('**/api/**', (route) => {
      const request = route.request()
      const path = new URL(request.url()).pathname
      const method = request.method()
      const parsed = (): unknown => {
        try {
          return JSON.parse(request.postData() ?? '{}')
        } catch {
          return {}
        }
      }
      if (path === '/api/me' && method === 'GET') return json(route, ME)
      if (path === '/api/catalog/namespaces' && method === 'GET') return json(route, { namespaces: [] })
      if (path === '/api/pipelines/principals' && method === 'GET') return json(route, { groups: [READY, READY_2] })
      if (path === '/api/pipelines' && method === 'GET') return json(route, { pipelines: [] })
      if (method !== 'GET' && write?.(route, path, method, request.postData())) return
      if (path === '/api/pipelines/check' && method === 'POST') {
        return json(route, check ? check(parsed() as CheckBody) : { checked: true, ok: true })
      }
      if (path === '/api/pipelines/preview-schedule' && method === 'POST') {
        if (preview) return preview(route, parsed() as PreviewBody)
        return json(route, { runs: RUNS })
      }
      if (method === 'GET' && detail
        && path === `/api/pipelines/${NS}/${encodeURIComponent(String(detail.name))}`) {
        return json(route, detail)
      }
      return json(route, [])
    })
    await page.goto(start)
    return { context, page, requests }
  }

  async function openDialog(page: Page): Promise<Locator> {
    await page.getByRole('button', { name: 'Schedule this statement' }).click()
    const dialog = page.getByRole('dialog', { name: 'Schedule this statement' })
    await expect(dialog).toBeVisible()
    return dialog
  }

  /** The JSON bodies of one route's requests, oldest first. */
  function bodies(requests: Recorded[], path: string, method = 'POST'): Array<Record<string, unknown>> {
    return requests
      .filter((request) => request.method === method && new URL(request.url).pathname === path)
      .map((request) => JSON.parse(request.body ?? '{}') as Record<string, unknown>)
  }

  test('param_rows_validate_and_are_sent', async ({ browser }) => {
    let posted: Record<string, unknown> | null = null
    const { context, page } = await open(browser, {
      write: (route, path, method, requestBody) => {
        if (path !== '/api/pipelines' || method !== 'POST') return false
        posted = JSON.parse(requestBody ?? '{}') as Record<string, unknown>
        json(route, { ...pipeline({ name: 'param-run' }), params: { days: '2026-10-20' } }, 201)
        return true
      },
    })
    await page.getByLabel('SQL query').fill("select * from t where ds = '${params.days}'")
    const dialog = await openDialog(page)
    const create = dialog.getByRole('button', { name: 'Create schedule' })
    await expect(dialog.getByText('No parameters.', { exact: false })).toBeVisible()
    // The statement names a parameter the table does not define, so Create waits.
    await expect(dialog.getByText('unknown placeholder ${params.days}', { exact: true })).toBeVisible()
    await expect(create).toBeDisabled()

    await dialog.getByRole('button', { name: 'Add parameter' }).click()
    const name = dialog.getByRole('textbox', { name: 'Parameter 1 name' })
    const value = dialog.getByRole('textbox', { name: 'Parameter 1 value' })
    await name.fill('Days')
    await expect(dialog.getByText('parameter names are lower-case letters, digits and underscores', { exact: true })).toBeVisible()
    await expect(create).toBeDisabled()
    await name.fill('days')
    await expect(dialog.getByText('parameter names are lower-case letters, digits and underscores', { exact: true })).toHaveCount(0)

    // Two rows with one name would collapse silently in the resource's map: refused here.
    await dialog.getByRole('button', { name: 'Add parameter' }).click()
    await dialog.getByRole('textbox', { name: 'Parameter 2 name' }).fill('days')
    await expect(dialog.getByText('two parameters have the same name', { exact: true })).toBeVisible()
    await expect(create).toBeDisabled()
    await dialog.getByRole('button', { name: 'Remove parameter 2' }).click()

    await value.fill('2026-10-20')
    await expect(dialog.getByText('unknown placeholder ${params.days}', { exact: true })).toHaveCount(0)
    await expect(dialog.getByText('parameter names are lower-case letters, digits and underscores', { exact: true })).toHaveCount(0)
    await expect(create).toBeEnabled()

    // At most 16 rows; an empty row is not a parameter and does not block Create.
    for (let i = 0; i < 15; i += 1) await dialog.getByRole('button', { name: 'Add parameter' }).click()
    await expect(dialog.getByRole('button', { name: 'Add parameter' })).toBeDisabled()
    await expect(create).toBeEnabled()
    await dialog.getByRole('button', { name: 'Remove parameter 16' }).click()
    await expect(dialog.getByRole('button', { name: 'Add parameter' })).toBeEnabled()

    await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('param-run')
    await create.click()
    expect(posted).toEqual({
      name: 'param-run',
      ownerGroup: 'analytics',
      sql: "select * from t where ds = '${params.days}'",
      schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' },
      params: { days: '2026-10-20' },
      options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
    })
    await context.close()
  })

  test('chips_insert_at_the_caret', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.getByLabel('SQL query').fill("select * from t where ds = ''")
    const dialog = await openDialog(page)
    await dialog.getByRole('button', { name: 'Add parameter' }).click()
    await dialog.getByRole('textbox', { name: 'Parameter 1 name' }).fill('days')
    const statement = dialog.getByRole('textbox', { name: 'Statement', exact: true })

    // Put the caret between the two quotes; the chip must not append at the end.
    await statement.evaluate((element) => {
      const field = element as HTMLTextAreaElement
      field.focus()
      const at = field.value.indexOf("''") + 1
      field.setSelectionRange(at, at)
    })
    await dialog.getByRole('button', { name: '${run.date}', exact: true }).click()
    await expect(statement).toHaveValue("select * from t where ds = '${run.date}'")
    await expect.poll(() => statement.evaluate((element) => (element as HTMLTextAreaElement).selectionStart))
      .toBe("select * from t where ds = '${run.date}".length)

    await dialog.getByRole('button', { name: '${params.days}', exact: true }).click()
    await expect(statement).toHaveValue("select * from t where ds = '${run.date}${params.days}'")
    await expect(dialog.getByText('Filled in when the run starts.', { exact: true })).toBeVisible()
    // Every chip is offered: the run's two values and one per named parameter.
    for (const name of ['${run.date}', '${run.id}', '${params.days}']) {
      await expect(dialog.getByRole('button', { name, exact: true })).toBeVisible()
    }
    await context.close()
  })

  test('unknown_placeholder_blocks_create', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.getByLabel('SQL query').fill("select * from t where ds = '${params.nope}' and id = '${run.id}'")
    const dialog = await openDialog(page)
    const create = dialog.getByRole('button', { name: 'Create schedule' })
    await expect(dialog.getByText('unknown placeholder ${params.nope}', { exact: true })).toBeVisible()
    await expect(create).toBeDisabled()

    await dialog.getByRole('button', { name: 'Add parameter' }).click()
    await dialog.getByRole('textbox', { name: 'Parameter 1 name' }).fill('nope')
    await expect(dialog.getByText('unknown placeholder ${params.nope}', { exact: true })).toHaveCount(0)
    await expect(create).toBeEnabled()

    // A `params.x` without a row is the same refusal, named the same way.
    await dialog.getByRole('textbox', { name: 'Statement', exact: true })
      .fill("select * from t where ds = '${params.days}'")
    await expect(dialog.getByText('unknown placeholder ${params.days}', { exact: true })).toBeVisible()
    await expect(create).toBeDisabled()
    await context.close()
  })

  test('advanced_defaults_and_values_sent', async ({ browser }) => {
    let posted: Record<string, unknown> | null = null
    const { context, page } = await open(browser, {
      write: (route, path, method, requestBody) => {
        if (path !== '/api/pipelines' || method !== 'POST') return false
        posted = JSON.parse(requestBody ?? '{}') as Record<string, unknown>
        json(route, pipeline({ name: 'advanced-run' }), 201)
        return true
      },
    })
    await page.getByLabel('SQL query').fill('select 1')
    const dialog = await openDialog(page)
    // Native `<details>`, closed by default: the fields are not on screen yet.
    await expect(dialog.getByRole('combobox', { name: 'Retries', exact: true })).toHaveCount(0)
    await dialog.getByText('Advanced', { exact: true }).click()

    const retries = dialog.getByRole('combobox', { name: 'Retries', exact: true })
    const backoff = dialog.getByRole('combobox', { name: 'Wait between retries', exact: true })
    const limit = dialog.getByRole('combobox', { name: 'Time limit', exact: true })
    await expect(retries).toHaveValue('1')
    await expect(backoff).toHaveValue('5')
    await expect(limit).toHaveValue('120')
    await expect(dialog.getByRole('radio', { name: 'One run at a time' })).toBeChecked()

    await retries.selectOption('3')
    await backoff.selectOption('10')
    await limit.selectOption('360')
    await dialog.getByRole('radio', { name: 'Allow runs in parallel' }).check()
    await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('advanced-run')
    await dialog.getByRole('button', { name: 'Create schedule' }).click()

    expect(posted).not.toBeNull()
    expect((posted as Record<string, unknown>).options).toEqual({
      retries: 3,
      backoffMinutes: 10,
      timeoutMinutes: 360,
      concurrency: 'Allow',
    })
    await context.close()
  })

  test('check_shows_the_three_outcomes', async ({ browser }) => {
    const checks: Array<Record<string, unknown>> = []
    const { context, page, requests } = await open(browser, {
      check: (body) => {
        checks.push(body as Record<string, unknown>)
        const sql = body.sql ?? ''
        if (sql.startsWith('drop')) return { checked: false }
        if (sql.includes('missing')) {
          return { checked: true, ok: false, message: 'Table warehouse.default.missing does not exist' }
        }
        return { checked: true, ok: true }
      },
    })
    await page.getByLabel('SQL query').fill('select 1')
    const dialog = await openDialog(page)
    const check = dialog.getByRole('button', { name: 'Check statement' })
    const create = dialog.getByRole('button', { name: 'Create schedule' })

    await check.click()
    await expect(dialog.getByRole('status').filter({ hasText: 'Checked: the engine accepts it' })).toBeVisible()

    await dialog.getByRole('textbox', { name: 'Statement', exact: true }).fill('drop table x')
    await expect(dialog.getByText('Checked: the engine accepts it', { exact: true })).toHaveCount(0)
    await check.click()
    await expect(dialog.getByRole('status')).toHaveText('This kind of statement cannot be checked here')
    await expect(create).toBeEnabled()

    await dialog.getByRole('textbox', { name: 'Statement', exact: true }).fill('select * from missing')
    await check.click()
    await expect(dialog.getByRole('alert')).toHaveText(
      'The engine says: Table warehouse.default.missing does not exist',
    )
    await expect(create).toBeEnabled()

    // The check carries the parameter table: the engine sees the statement it would plan.
    await dialog.getByRole('textbox', { name: 'Statement', exact: true })
      .fill("select * from t where ds = '${params.days}'")
    await dialog.getByRole('button', { name: 'Add parameter' }).click()
    await dialog.getByRole('textbox', { name: 'Parameter 1 name' }).fill('days')
    await dialog.getByRole('textbox', { name: 'Parameter 1 value' }).fill('2026-10-20')
    await check.click()
    await expect.poll(() => checks.length).toBe(4)
    expect(checks[3]).toEqual({
      sql: "select * from t where ds = '${params.days}'",
      params: { days: '2026-10-20' },
    })
    expect(bodies(requests, '/api/pipelines/check')).toHaveLength(4)
    await context.close()
  })

  test('next_runs_listed_and_debounced', async ({ browser }) => {
    const previews: Array<Record<string, unknown>> = []
    const { context, page } = await open(browser, {
      preview: (route, requestBody) => {
        previews.push(requestBody as Record<string, unknown>)
        json(route, { runs: RUNS })
      },
    })
    const dialog = await openDialog(page)
    // The default cadence asks once, after the pause.
    await expect.poll(() => previews.length).toBe(1)
    expect(previews[0]).toEqual({ cron: '0 3 * * *', timezone: 'Europe/Berlin', count: 5 })
    for (const run of RUNS) {
      await expect(dialog.getByText(formatTime(run, 'Europe/Berlin'), { exact: true })).toBeVisible()
    }

    // Three quick changes settle into one request for the last cadence.
    const before = previews.length
    const time = dialog.getByRole('textbox', { name: 'Time of day', exact: true })
    await time.fill('04:00')
    await time.fill('04:30')
    await time.fill('05:00')
    await expect.poll(() => previews.length).toBe(before + 1)
    expect(previews[previews.length - 1]).toEqual({
      cron: '0 5 * * *', timezone: 'Europe/Berlin', count: 5,
    })

    // On demand asks nothing and says so.
    const settled = previews.length
    await dialog.getByRole('radio', { name: 'On demand only' }).check()
    await expect(dialog.getByText('On demand: no scheduled runs', { exact: true })).toBeVisible()
    await page.waitForTimeout(700)
    expect(previews.length).toBe(settled)
    await context.close()
  })

  test('next_runs_show_the_api_sentence', async ({ browser }) => {
    const { context, page } = await open(browser, {
      preview: (route) => json(route, { error: 'the scheduler cannot read its clock' }, 503),
    })
    const dialog = await openDialog(page)
    await expect(dialog.getByRole('alert')).toHaveText('the scheduler cannot read its clock')
    // The preview is advice: Create stays available.
    await expect(dialog.getByRole('button', { name: 'Create schedule' })).toBeEnabled()
    await context.close()
  })

  test('edit_mode_prefills', async ({ browser }) => {
    const stored = pipeline({
      params: { days: '1', region: 'eu' },
      options: { retries: 2, backoffMinutes: 10, timeoutMinutes: 360, concurrency: 'Allow' },
    })
    const { context, page, requests } = await open(browser, {
      start: `/pipelines/${NS}/team-smoke`,
      detail: stored,
      write: (route, path, method) => {
        if (path !== `/api/pipelines/${NS}/team-smoke` || method !== 'PUT') return false
        json(route, stored)
        return true
      },
    })
    await page.getByRole('button', { name: 'Edit schedule' }).click()
    const dialog = page.getByRole('dialog', { name: 'Edit the schedule' })
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 name' })).toHaveValue('days')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 value' })).toHaveValue('1')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 2 name' })).toHaveValue('region')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 2 value' })).toHaveValue('eu')

    await dialog.getByText('Advanced', { exact: true }).click()
    await expect(dialog.getByRole('combobox', { name: 'Retries', exact: true })).toHaveValue('2')
    await expect(dialog.getByRole('combobox', { name: 'Wait between retries', exact: true })).toHaveValue('10')
    await expect(dialog.getByRole('combobox', { name: 'Time limit', exact: true })).toHaveValue('360')
    await expect(dialog.getByRole('radio', { name: 'Allow runs in parallel' })).toBeChecked()

    await dialog.getByRole('textbox', { name: 'Parameter 2 value' }).fill('us')
    await dialog.getByRole('button', { name: 'Save schedule' }).click()
    const puts = bodies(requests, `/api/pipelines/${NS}/team-smoke`, 'PUT')
    expect(puts).toEqual([{
      sql: 'select 1',
      schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' },
      params: { days: '1', region: 'us' },
      options: { retries: 2, backoffMinutes: 10, timeoutMinutes: 360, concurrency: 'Allow' },
    }])
    await context.close()
  })

  test('hostile_text_in_params_and_messages_is_text', async ({ browser }) => {
    const { context, page } = await open(browser, {
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      check: () => ({ checked: true, ok: false, message: EVIL }),
    })
    await page.getByLabel('SQL query').fill('select 1')
    const dialog = await openDialog(page)
    await dialog.getByRole('button', { name: 'Add parameter' }).click()
    const value = dialog.getByRole('textbox', { name: 'Parameter 1 value' })
    await dialog.getByRole('textbox', { name: 'Parameter 1 name' }).fill('note')
    await value.fill(EVIL)
    await expect(value).toHaveValue(EVIL)
    expect(await page.evaluate(() => ({
      xss: (window as unknown as { __xss?: number }).__xss ?? 0,
      imgs: document.querySelectorAll('img[src="x"]').length,
    }))).toEqual({ xss: 0, imgs: 0 })

    await dialog.getByRole('button', { name: 'Check statement' }).click()
    await expect(dialog.getByRole('alert')).toHaveText(`The engine says: ${EVIL}`)
    expect(await page.evaluate(() => ({
      xss: (window as unknown as { __xss?: number }).__xss ?? 0,
      imgs: document.querySelectorAll('img[src="x"]').length,
    }))).toEqual({ xss: 0, imgs: 0 })
    await context.close()
  })

  test('U5 audit over the open dialog and Advanced open', async ({ browser }) => {
    const { context, page } = await open(browser, { detail: pipeline() })
    await page.getByLabel('SQL query').fill('select 1')
    await page.setViewportSize({ width: 360, height: 740 })
    const dialog = await openDialog(page)
    await dialog.getByText('Advanced', { exact: true }).click()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth))
      .toBeLessThanOrEqual(1)
    const box = await dialog.boundingBox()
    expect(box).not.toBeNull()
    expect(box!.x).toBeGreaterThanOrEqual(0)
    expect(box!.x + box!.width).toBeLessThanOrEqual(361)
    await context.close()
  })
})

test.describe('schedule notebook', () => {
  // The Schedule dialog's Notebook mode (stardelt-nova#238): a published notebook, the exact
  // version it pins and the parameters of that version. The notebook routes are the ones of
  // stardelt-nova#237, stubbed here; every other `/api/**` call is stubbed too.
  const NS = 'stardelt'
  const ME = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: true, groups: ['analytics', 'finance'],
  }
  const READY = { group: 'analytics', principal: 'pipeline-analytics', ready: true, message: null }
  const READY_2 = { group: 'finance', principal: 'pipeline-finance', ready: true, message: null }
  const EVIL = '<img src=x onerror="window.__xss=1">'
  /** Long, opaque S3 version ids; the dialog shows their first eight characters. */
  const V3 = '4f9c2a1b7e6d5c4b3a29180706050403020100ffaa'
  const V2 = '9b8a7c6d5e4f30211203f4e5d6c7b8a990817263'
  const V1 = '1a2b3c4d5e6f708192a3b4c5d6e7f809a1b2c3d4'
  const AT3 = '2026-10-20T08:00:00Z'
  const AT2 = '2026-10-19T09:30:00Z'
  const AT1 = '2026-10-18T10:00:00Z'
  const NO_NOTEBOOK =
    'No notebook is published in this group yet. Publish one from JupyterLab with stardelt.publish("file.ipynb").'
  const PIN_HELPER = 'The schedule runs exactly this version. Publishing again does not change it.'

  const ANALYTICS_NOTEBOOK = {
    group: 'analytics', name: 'smoke.ipynb', objectVersion: V3, size: 2048, uploadedAt: AT3, uploadedBy: 'alice',
  }
  const FINANCE_NOTEBOOK = {
    group: 'finance', name: 'finance.ipynb', objectVersion: V1, size: 1024, uploadedAt: AT1, uploadedBy: 'bob',
  }
  const SMOKE_VERSIONS = [
    { objectVersion: V3, size: 2048, uploadedAt: AT3, uploadedBy: 'alice', sha256: 'aaa' },
    { objectVersion: V2, size: 2000, uploadedAt: AT2, uploadedBy: 'bob', sha256: 'bbb' },
  ]
  const FINANCE_VERSIONS = [
    { objectVersion: V1, size: 1024, uploadedAt: AT1, uploadedBy: 'bob', sha256: 'ccc' },
  ]
  const SMOKE_PARAMS: Record<string, unknown> = {
    [V3]: {
      parameters: [{ name: 'days', default: '1' }, { name: 'region', default: 'eu' }],
      objectVersion: V3,
    },
    [V2]: { parameters: [{ name: 'days', default: '7' }], objectVersion: V2 },
  }
  const FINANCE_PARAMS = {
    parameters: [{ name: 'month', default: '2026-09' }],
    objectVersion: V1,
  }

  type Recorded = { method: string; url: string; body: string | null }
  type Write = (route: Route, path: string, method: string, body: string | null) => boolean

  /** A Pipelines page or detail page with every `/api/**` call stubbed; notebook routes included. */
  async function open(browser: Browser, {
    start = '/pipelines',
    notebooks = [ANALYTICS_NOTEBOOK, FINANCE_NOTEBOOK] as unknown[],
    versionsOf = (name: string) => (name === 'smoke.ipynb' ? SMOKE_VERSIONS : name === 'finance.ipynb' ? FINANCE_VERSIONS : []),
    parametersOf = (version: string | null) =>
      version === V2 ? SMOKE_PARAMS[V2] : version === V1 ? FINANCE_PARAMS : SMOKE_PARAMS[V3],
    detail,
    write,
    init,
    timezoneId = 'Europe/Berlin',
  }: {
    start?: string
    notebooks?: unknown[]
    versionsOf?: (name: string) => unknown[]
    parametersOf?: (version: string | null) => unknown
    detail?: Record<string, unknown>
    write?: Write
    init?: () => void
    timezoneId?: string
  } = {}) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, timezoneId })
    const page = await context.newPage()
    // Version labels show a full time; pin the display to UTC so the expected text does not
    // depend on the zone the test runner itself happens to use.
    await context.addInitScript(() => {
      try { localStorage.setItem('stardelt-nova:time', 'utc') } catch { /* ignore */ }
    })
    if (init) await page.addInitScript(init)
    const requests: Recorded[] = []
    page.on('request', (request) => requests.push({
      method: request.method(),
      url: request.url(),
      body: request.postData(),
    }))
    await page.route('**/api/**', (route) => {
      const request = route.request()
      const url = new URL(request.url())
      const path = url.pathname
      const method = request.method()
      if (path === '/api/me' && method === 'GET') return json(route, ME)
      if (path === '/api/catalog/namespaces' && method === 'GET') return json(route, { namespaces: [] })
      if (path === '/api/pipelines/principals' && method === 'GET') return json(route, { groups: [READY, READY_2] })
      if (path === '/api/pipelines' && method === 'GET') return json(route, { pipelines: [] })
      if (path === '/api/notebooks' && method === 'GET') return json(route, notebooks)
      const notebookRoute = /^\/api\/notebooks\/([^/]+)\/(.+)\/(versions|parameters)$/.exec(path)
      if (notebookRoute && method === 'GET') {
        const name = decodeURIComponent(notebookRoute[2])
        if (notebookRoute[3] === 'versions') return json(route, versionsOf(name))
        return json(route, parametersOf(url.searchParams.get('version')))
      }
      if (method !== 'GET' && write?.(route, path, method, request.postData())) return
      if (path === '/api/pipelines/check' && method === 'POST') return json(route, { checked: true, ok: true })
      if (path === '/api/pipelines/preview-schedule' && method === 'POST') return json(route, { runs: [] })
      if (method === 'GET' && detail
        && path === `/api/pipelines/${NS}/${encodeURIComponent(String(detail.name))}`) {
        return json(route, detail)
      }
      return json(route, [])
    })
    await page.goto(start)
    return { context, page, requests }
  }

  async function openFromPipelines(page: Page): Promise<Locator> {
    await page.getByRole('button', { name: 'Schedule a notebook' }).click()
    const dialog = page.getByRole('dialog', { name: 'Schedule this notebook' })
    await expect(dialog).toBeVisible()
    return dialog
  }

  /**
   * Choose what the schedule runs. The radios are native but visually hidden inside their
   * labels (a segmented control), so a person clicks the word — the label forwards it.
   */
  async function pickKind(dialog: Locator, label: string) {
    await dialog.getByRole('group', { name: 'What to run' }).getByText(label, { exact: true }).click()
  }

  /** The JSON bodies of one route's requests, oldest first. */
  function bodies(requests: Recorded[], path: string, method = 'POST'): Array<Record<string, unknown>> {
    return requests
      .filter((request) => request.method === method && new URL(request.url).pathname === path)
      .map((request) => JSON.parse(request.body ?? '{}') as Record<string, unknown>)
  }

  /** What a create answers: only the name and namespace are used by the list it returns to. */
  function created(name: string) {
    return {
      name, namespace: NS, ownerGroup: 'analytics', schedule: null, paused: false, loaded: false,
      nextRun: null, state: 'loading', lastRun: null, tasks: [], runAs: null, concurrency: 'Forbid',
      createdAt: null,
    }
  }

  function pipeline(overrides: Record<string, unknown> = {}) {
    return {
      name: 'nightly',
      namespace: NS,
      ownerGroup: 'analytics',
      schedule: '0 3 * * * (Europe/Berlin)',
      paused: false,
      loaded: true,
      nextRun: null,
      state: 'idle',
      lastRun: null,
      tasks: [{ name: 'run', kind: 'notebook', notebook: 'smoke.ipynb', objectVersion: V2, notebookParams: { days: '9' } }],
      runAs: 'pipeline-analytics',
      concurrency: 'Forbid',
      createdAt: '2026-10-19T10:00:00Z',
      runs: [],
      params: {},
      retries: null,
      options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
      ...overrides,
    }
  }

  test('mode_switch_shows_notebook_fields', async ({ browser }) => {
    const { context, page } = await open(browser, { start: '/query' })
    await page.getByLabel('SQL query').fill('select 1')
    await page.getByRole('button', { name: 'Schedule this statement' }).click()
    const sqlDialog = page.getByRole('dialog', { name: 'Schedule this statement' })
    await expect(sqlDialog.getByRole('textbox', { name: 'Statement', exact: true })).toBeVisible()
    await expect(sqlDialog.getByRole('combobox', { name: 'Notebook', exact: true })).toHaveCount(0)
    const choices = sqlDialog.getByRole('group', { name: 'What to run' })
    await expect(choices.getByRole('radio', { name: 'SQL statement', exact: true })).toBeChecked()
    await pickKind(sqlDialog, 'Notebook')

    // The title follows the choice; the statement and its table give way to the notebook fields.
    const dialog = page.getByRole('dialog', { name: 'Schedule this notebook' })
    await expect(dialog.getByRole('radio', { name: 'Notebook', exact: true })).toBeChecked()
    await expect(dialog.getByRole('textbox', { name: 'Statement', exact: true })).toHaveCount(0)
    await expect(dialog.getByText('Add parameter', { exact: true })).toHaveCount(0)
    await expect(dialog.getByRole('combobox', { name: 'Notebook', exact: true })).toHaveValue('smoke.ipynb')
    await expect(dialog.getByRole('combobox', { name: 'Version', exact: true })).toHaveValue(V3)
    await expect(dialog.getByText(PIN_HELPER, { exact: true })).toBeVisible()
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 name' })).toHaveValue('days')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 value' })).toHaveValue('1')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 2 name' })).toHaveValue('region')
    // The statement was not thrown away by the switch.
    await pickKind(dialog, 'SQL statement')
    const back = page.getByRole('dialog', { name: 'Schedule this statement' })
    await expect(back.getByRole('textbox', { name: 'Statement', exact: true })).toHaveValue('select 1')
    await expect(back.getByRole('group', { name: 'What to run' })).toBeVisible()
    await context.close()
  })

  test('no_published_notebook_message_blocks_create', async ({ browser }) => {
    const { context, page } = await open(browser, { notebooks: [FINANCE_NOTEBOOK] })
    const dialog = await openFromPipelines(page)
    // The chosen group (analytics) has none: the sentence says how to publish one, and Create waits.
    await expect(dialog.getByText(NO_NOTEBOOK, { exact: true })).toBeVisible()
    await expect(dialog.getByRole('combobox', { name: 'Notebook', exact: true })).toHaveCount(0)
    await expect(dialog.getByRole('button', { name: 'Create schedule' })).toBeDisabled()

    // Another group's notebook is offered when that group is chosen, with its own version and values.
    await dialog.getByRole('combobox', { name: 'Group', exact: true }).selectOption('finance')
    const notebook = dialog.getByRole('combobox', { name: 'Notebook', exact: true })
    await expect(notebook).toHaveValue('finance.ipynb')
    await expect(dialog.getByRole('combobox', { name: 'Version', exact: true })).toHaveValue(V1)
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 name' })).toHaveValue('month')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 value' })).toHaveValue('2026-09')
    await expect(dialog.getByText(NO_NOTEBOOK, { exact: true })).toHaveCount(0)
    await expect(dialog.getByRole('button', { name: 'Create schedule' })).toBeEnabled()

    await dialog.getByRole('combobox', { name: 'Group', exact: true }).selectOption('analytics')
    await expect(dialog.getByText(NO_NOTEBOOK, { exact: true })).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Create schedule' })).toBeDisabled()
    await context.close()
  })

  test('version_select_and_pin_helper', async ({ browser }) => {
    let posted: Record<string, unknown> | null = null
    const { context, page, requests } = await open(browser, {
      write: (route, path, method, requestBody) => {
        if (path !== '/api/pipelines' || method !== 'POST') return false
        posted = JSON.parse(requestBody ?? '{}') as Record<string, unknown>
        json(route, created(String((posted as Record<string, unknown>).name)), 201)
        return true
      },
    })
    const dialog = await openFromPipelines(page)
    const version = dialog.getByRole('combobox', { name: 'Version', exact: true })
    // Newest first; each choice names the short id, when it was published and by whom.
    expect(await version.locator('option').allTextContents()).toEqual([
      `${V3.slice(0, 8)} · ${formatTime(AT3, 'UTC')} · alice`,
      `${V2.slice(0, 8)} · ${formatTime(AT2, 'UTC')} · bob`,
    ])
    await expect(version).toHaveValue(V3)
    await expect(dialog.getByText(PIN_HELPER, { exact: true })).toBeVisible()

    // The parameter table belongs to the chosen version, not to the notebook's newest one.
    await version.selectOption(V2)
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 name' })).toHaveValue('days')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 value' })).toHaveValue('7')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 2 name' })).toHaveCount(0)

    await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('pinned-run')
    await dialog.getByRole('button', { name: 'Create schedule' }).click()
    expect(posted).not.toBeNull()
    expect((posted as Record<string, unknown>).notebook).toEqual({
      name: 'smoke.ipynb', objectVersion: V2, params: { days: '7' },
    })
    expect(bodies(requests, '/api/pipelines')).toHaveLength(1)
    await context.close()
  })

  test('parameters_prefilled_and_sent_exactly', async ({ browser }) => {
    let posted: Record<string, unknown> | null = null
    const { context, page } = await open(browser, {
      write: (route, path, method, requestBody) => {
        if (path !== '/api/pipelines' || method !== 'POST') return false
        posted = JSON.parse(requestBody ?? '{}') as Record<string, unknown>
        json(route, created('notebook-run'), 201)
        return true
      },
    })
    const dialog = await openFromPipelines(page)
    const days = dialog.getByRole('textbox', { name: 'Parameter 1 value' })
    const region = dialog.getByRole('textbox', { name: 'Parameter 2 value' })
    // The default is both the initial value and the placeholder; the name itself is read-only.
    await expect(days).toHaveValue('1')
    await expect(days).toHaveAttribute('placeholder', '1')
    await expect(region).toHaveValue('eu')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 name' })).toHaveAttribute('readonly', '')
    await expect(days).toHaveAttribute('maxlength', '256')
    await days.fill('2026-10-20')
    await region.fill('us')
    await dialog.getByRole('textbox', { name: 'Name', exact: true }).fill('notebook-run')
    await dialog.getByRole('button', { name: 'Create schedule' }).click()

    expect(posted).toEqual({
      name: 'notebook-run',
      ownerGroup: 'analytics',
      notebook: { name: 'smoke.ipynb', objectVersion: V3, params: { days: '2026-10-20', region: 'us' } },
      schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' },
      options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
    })
    // Exactly one of the two: the notebook, and no statement at all.
    expect(Object.keys(posted as Record<string, unknown>).sort())
      .toEqual(['name', 'notebook', 'options', 'ownerGroup', 'schedule'])
    await context.close()
  })

  test('edit_mode_prefills_and_locks_kind', async ({ browser }) => {
    const stored = pipeline()
    const { context, page, requests } = await open(browser, {
      start: `/pipelines/${NS}/nightly`,
      detail: stored,
      write: (route, path, method) => {
        if (path !== `/api/pipelines/${NS}/nightly` || method !== 'PUT') return false
        json(route, stored)
        return true
      },
    })
    await page.getByRole('button', { name: 'Edit schedule' }).click()
    const dialog = page.getByRole('dialog', { name: 'Edit the schedule' })
    // A pipeline keeps its kind: both choices are present, neither can be changed.
    await expect(dialog.getByRole('radio', { name: 'Notebook', exact: true })).toBeChecked()
    await expect(dialog.getByRole('radio', { name: 'Notebook', exact: true })).toBeDisabled()
    await expect(dialog.getByRole('radio', { name: 'SQL statement', exact: true })).toBeDisabled()
    await expect(dialog.getByRole('textbox', { name: 'Statement', exact: true })).toHaveCount(0)

    const version = dialog.getByRole('combobox', { name: 'Version', exact: true })
    await expect(version).toHaveValue(V2)
    expect(await version.locator('option').first().textContent()).toBe('Use newest version')
    // The stored values win over the tagged cell's defaults; the defaults stay as the placeholder.
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 value' })).toHaveValue('9')
    await expect(dialog.getByRole('textbox', { name: 'Parameter 1 value' })).toHaveAttribute('placeholder', '7')

    await dialog.getByRole('button', { name: 'Save schedule' }).click()
    const first = bodies(requests, `/api/pipelines/${NS}/nightly`, 'PUT')
    expect(first).toEqual([{
      notebook: { name: 'smoke.ipynb', objectVersion: V2, params: { days: '9' } },
      schedule: { cron: '0 3 * * *', timezone: 'Europe/Berlin' },
      options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
    }])
    expect(first[0]).not.toHaveProperty('sql')

    // "Use newest version" still pins a concrete id: the parameters route names the version it read.
    await page.getByRole('button', { name: 'Edit schedule' }).click()
    const again = page.getByRole('dialog', { name: 'Edit the schedule' })
    await again.getByRole('combobox', { name: 'Version', exact: true }).selectOption('')
    await expect(again.getByRole('textbox', { name: 'Parameter 1 value' })).toHaveValue('1')
    await again.getByRole('button', { name: 'Save schedule' }).click()
    await expect.poll(() => bodies(requests, `/api/pipelines/${NS}/nightly`, 'PUT').length).toBe(2)
    const second = bodies(requests, `/api/pipelines/${NS}/nightly`, 'PUT')[1]
    expect(second.notebook).toEqual({
      name: 'smoke.ipynb', objectVersion: V3, params: { days: '1', region: 'eu' },
    })
    expect(second).not.toHaveProperty('sql')
    await context.close()
  })

  test('hostile_names_and_values_are_text', async ({ browser }) => {
    const evilVersion = 'deadbeefcafebabe0123456789abcdef01234567'
    const evilNotebook = {
      group: 'analytics', name: `${EVIL}.ipynb`, objectVersion: evilVersion, size: 10,
      uploadedAt: AT3, uploadedBy: EVIL,
    }
    const { context, page } = await open(browser, {
      notebooks: [evilNotebook],
      versionsOf: () => [{
        objectVersion: evilVersion, size: 10, uploadedAt: AT3, uploadedBy: EVIL, sha256: 'x',
      }],
      parametersOf: (version) => ({
        parameters: [{ name: 'note', default: EVIL }], objectVersion: version ?? evilVersion,
      }),
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
    })
    const dialog = await openFromPipelines(page)
    await expect(dialog.getByRole('combobox', { name: 'Notebook', exact: true })).toHaveValue(`${EVIL}.ipynb`)
    const value = dialog.getByRole('textbox', { name: 'Parameter 1 value' })
    await expect(value).toHaveValue(EVIL)
    expect(await value.getAttribute('placeholder')).toBe(EVIL)
    // The option's label is text, not markup; the uploaded-by name is rendered the same way.
    expect(await dialog.getByRole('combobox', { name: 'Version', exact: true }).locator('option').first().textContent())
      .toContain(EVIL)
    expect(await page.evaluate(() => ({
      xss: (window as unknown as { __xss?: number }).__xss ?? 0,
      imgs: document.querySelectorAll('img[src="x"]').length,
      overflow: document.documentElement.scrollWidth - window.innerWidth,
    }))).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
    expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth))
      .toBeLessThanOrEqual(1)

    // The detail page repeats the notebook, its pinned version and the values as text.
    const detail = pipeline({
      name: 'evil-pipeline',
      tasks: [{
        name: 'run', kind: 'notebook', notebook: `${EVIL}.ipynb`,
        objectVersion: evilVersion, notebookParams: { note: EVIL },
      }],
    })
    const second = await open(browser, {
      start: `/pipelines/${NS}/evil-pipeline`,
      detail,
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
    })
    const task = second.page.locator('section', { hasText: 'Tasks' }).locator('li')
    await expect(task).toContainText(`Notebook ${EVIL}.ipynb · version ${evilVersion.slice(0, 8)}`)
    await expect(task).toContainText(EVIL)
    expect(await second.page.evaluate(() => ({
      xss: (window as unknown as { __xss?: number }).__xss ?? 0,
      imgs: document.querySelectorAll('img[src="x"]').length,
    }))).toEqual({ xss: 0, imgs: 0 })
    await context.close()
    await second.context.close()
  })

  test('U5 audit over the open dialog in both modes', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.setViewportSize({ width: 360, height: 740 })
    const notebook = await openFromPipelines(page)
    await expect(notebook.getByRole('combobox', { name: 'Version', exact: true })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth))
      .toBeLessThanOrEqual(1)
    const notebookBox = await notebook.boundingBox()
    expect(notebookBox).not.toBeNull()
    expect(notebookBox!.x).toBeGreaterThanOrEqual(0)
    expect(notebookBox!.x + notebookBox!.width).toBeLessThanOrEqual(361)

    await pickKind(notebook, 'SQL statement')
    const statement = page.getByRole('dialog', { name: 'Schedule this statement' })
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    expect(await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth))
      .toBeLessThanOrEqual(1)
    const statementBox = await statement.boundingBox()
    expect(statementBox).not.toBeNull()
    expect(statementBox!.x).toBeGreaterThanOrEqual(0)
    expect(statementBox!.x + statementBox!.width).toBeLessThanOrEqual(361)
    await context.close()
  })
})

test.describe('preferences', () => {
  const ME = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: false, groups: ['analysts'],
  }
  const NO_STORE =
    'Your preferences are kept in this browser only: this installation has no state store.'

  /** A Preferences page with every `/api/**` call stubbed; writes are recorded for the test. */
  async function open(browser: Browser, {
    settings = {} as Record<string, unknown>,
    settingsStatus,
    timezoneId,
    rows = [] as unknown[],
  }: {
    settings?: Record<string, unknown>
    settingsStatus?: number
    timezoneId?: string
    rows?: unknown[]
  } = {}) {
    const context = await browser.newContext({
      viewport: { width: 1280, height: 800 },
      ...(timezoneId ? { timezoneId } : {}),
    })
    const page = await context.newPage()
    const writes: { method: string; path: string; body: string | null }[] = []
    await page.route('**/api/**', (route) => {
      const request = route.request()
      const path = new URL(request.url()).pathname
      if (path === '/api/me') return json(route, ME)
      if (path === '/api/settings') {
        return settingsStatus
          ? json(route, { error: 'the state store is not configured' }, settingsStatus)
          : json(route, settings)
      }
      if (path.startsWith('/api/settings/')) {
        writes.push({ method: request.method(), path, body: request.postData() })
        return json(route, { key: path.split('/').pop(), value: null })
      }
      if (path === '/api/apps/links') return json(route, [])
      if (path === '/api/jobs') return json(route, [])
      if (path === '/api/query-log') return json(route, { items: rows, nextCursor: null })
      if (path === '/api/catalog/warehouse') return json(route, { name: 'warehouse', id: 'w' })
      if (path === '/api/catalog/namespaces') return json(route, { namespaces: [] })
      return json(route, [])
    })
    return { context, page, writes }
  }

  /** The theme library (stardelt-nova#221): id and label, as the picker shows them. */
  const THEMES = [
    { id: 'dark', label: 'Dark blue' },
    { id: 'graphite', label: 'Graphite' },
    { id: 'light', label: 'Light' },
    { id: 'silver', label: 'Silver' },
  ] as const

  test('choosing a theme applies and saves it', async ({ browser }) => {
    // Each start theme differs from the one selected, so every choice is a change and
    // sends exactly one PUT; the theme applies at once and is kept in both places.
    for (const entry of THEMES) {
      const start = entry.id === 'dark' ? 'light' : 'dark'
      const { context, page, writes } = await open(browser)
      await context.addInitScript((t) => { localStorage.setItem('stardelt-nova:theme', t) }, start)
      await page.goto('/preferences')
      await expect(page.locator('html')).toHaveAttribute('data-theme', start)

      await page.getByRole('radio', { name: entry.label, exact: true }).check()

      await expect(page.locator('html')).toHaveAttribute('data-theme', entry.id)
      expect(await page.evaluate(() => localStorage.getItem('stardelt-nova:theme'))).toBe(entry.id)
      await expect.poll(() => writes.length).toBe(1)
      expect(writes[0]).toEqual({
        method: 'PUT', path: '/api/settings/theme', body: JSON.stringify(entry.id),
      })
      await context.close()
    }
  })

  test('an unknown stored theme falls back to dark', async ({ browser }) => {
    // An id from an older or newer version is not a theme: neither the stored value nor
    // the personal setting may put the UI into it (or overwrite it behind the person).
    const { context, page } = await open(browser, { settings: { theme: 'neon' } })
    await context.addInitScript(() => { localStorage.setItem('stardelt-nova:theme', 'neon') })

    await page.goto('/')
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark')
    expect(await page.evaluate(() => localStorage.getItem('stardelt-nova:theme'))).toBe('neon')

    await page.goto('/preferences')
    await expect(page.getByRole('radio', { name: 'Dark blue', exact: true })).toBeChecked()
    await context.close()
  })

  test('the top-bar toggle pairs dark with light and graphite with silver', async ({ browser }) => {
    const PAIRS = [
      { start: 'dark', other: 'light', startLabel: 'Dark blue', otherLabel: 'Light' },
      { start: 'light', other: 'dark', startLabel: 'Light', otherLabel: 'Dark blue' },
      { start: 'graphite', other: 'silver', startLabel: 'Graphite', otherLabel: 'Silver' },
      { start: 'silver', other: 'graphite', startLabel: 'Silver', otherLabel: 'Graphite' },
    ] as const
    for (const pair of PAIRS) {
      const { context, page } = await open(browser)
      await context.addInitScript((t) => { localStorage.setItem('stardelt-nova:theme', t) }, pair.start)
      await page.goto('/')

      const toggle = page.getByRole('button', { name: 'Toggle theme' })
      await expect(toggle).toHaveAttribute('title', `Switch to ${pair.otherLabel}`)
      // Four clicks from each start: the pair alternates, never a third theme.
      for (let click = 0; click < 4; click++) {
        await toggle.click()
        const onOther = click % 2 === 0
        await expect(page.locator('html')).toHaveAttribute('data-theme', onOther ? pair.other : pair.start)
        await expect(toggle).toHaveAttribute('title', `Switch to ${onOther ? pair.startLabel : pair.otherLabel}`)
      }
      await context.close()
    }
  })

  test('the Appearance options are a real radio group', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.goto('/preferences')
    const group = page.getByRole('group', { name: 'Appearance' })
    const card = (id: string) => page.locator(`label:has(input[value="${id}"])`)

    // Four native radios, exactly one checked: dark blue, the default.
    await expect(group.getByRole('radio')).toHaveCount(4)
    await expect(group.locator('input[type=radio]:checked')).toHaveCount(1)
    await expect(group.getByRole('radio', { name: 'Dark blue', exact: true })).toBeChecked()
    await expect(group.getByText('Selected', { exact: true })).toHaveCount(1)
    await expect(card('dark')).toContainText('Selected')

    // Arrow keys move the selection: right/down forward, left back, wrapping round.
    await group.getByRole('radio', { name: 'Dark blue', exact: true }).focus()
    await page.keyboard.press('ArrowRight')
    await expect(group.getByRole('radio', { name: 'Graphite', exact: true })).toBeChecked()
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'graphite')
    await page.keyboard.press('ArrowDown')
    await expect(group.getByRole('radio', { name: 'Light', exact: true })).toBeChecked()
    await page.keyboard.press('ArrowDown')
    await expect(group.getByRole('radio', { name: 'Silver', exact: true })).toBeChecked()
    await page.keyboard.press('ArrowDown')
    await expect(group.getByRole('radio', { name: 'Dark blue', exact: true })).toBeChecked()
    await page.keyboard.press('ArrowLeft')
    await expect(group.getByRole('radio', { name: 'Silver', exact: true })).toBeChecked()

    // The word, not only the accent border, marks the selected card (U15).
    await expect(group.getByText('Selected', { exact: true })).toHaveCount(1)
    await expect(card('silver')).toContainText('Selected')

    // Each card shows three swatches, decorative and hidden from the accessible name.
    await expect(group.locator('[aria-hidden="true"]')).toHaveCount(12)
    await context.close()
  })

  test('a new browser follows the saved setting', async ({ browser }) => {
    const { context, page } = await open(browser, {
      settings: { theme: 'light', 'time.display': 'utc' },
    })
    // No local choice: the first page of a new browser opens with the person's theme.
    await page.goto('/')
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
    expect(await page.evaluate(() => localStorage.getItem('stardelt-nova:time'))).toBe('utc')
    await page.goto('/preferences')
    await expect(page.getByRole('radio', { name: 'Light', exact: true })).toBeChecked()
    await expect(page.getByLabel('UTC', { exact: true })).toBeChecked()
    expect(await page.evaluate(() => localStorage.getItem('stardelt-nova:theme'))).toBe('light')
    await context.close()
  })

  test('time display', async ({ browser }) => {
    // A fixed instant: 14:05:09 UTC is 16:05:09 in Berlin (summer time, October 2026).
    const createdAt = Date.UTC(2026, 9, 20, 14, 5, 9)
    const row = {
      id: 'q1', state: 'FINISHED', user: 'alice', source: null, warehouse: 'warehouse',
      queryType: 'SELECT', sql: 'select 1', createdAt, wallMs: 5, outputRows: 0, errorCode: null,
    }
    const { context, page, writes } = await open(browser, { timezoneId: 'Europe/Berlin', rows: [row] })

    await page.goto('/queries')
    const cell = page.locator('td[title]').first()
    await expect(cell).toHaveAttribute('title', /^2026-10-20 16:05:09 \S+$/)

    await page.goto('/preferences')
    await page.getByLabel('UTC', { exact: true }).check()
    await expect.poll(() => writes.length).toBe(1)
    expect(writes[0]).toEqual({ method: 'PUT', path: '/api/settings/time.display', body: '"utc"' })
    expect(await page.evaluate(() => localStorage.getItem('stardelt-nova:time'))).toBe('utc')

    await page.goto('/queries')
    await expect(page.locator('td[title]').first()).toHaveAttribute('title', '2026-10-20 14:05:09 UTC')

    await page.goto('/preferences')
    await page.getByLabel(/^Local time/).check()
    await expect.poll(() => writes.length).toBe(2)
    expect(writes[1]).toEqual({ method: 'PUT', path: '/api/settings/time.display', body: '"local"' })
    expect(await page.evaluate(() => localStorage.getItem('stardelt-nova:time'))).toBe('local')

    await page.goto('/queries')
    await expect(page.locator('td[title]').first()).toHaveAttribute('title', /^2026-10-20 16:05:09 \S+$/)
    await context.close()
  })

  test('no state store', async ({ browser }) => {
    const { context, page } = await open(browser, { settingsStatus: 503 })
    await page.goto('/preferences')
    await expect(page.getByText(NO_STORE)).toBeVisible()
    await expect(page.getByRole('alert')).toHaveCount(0)

    // Both controls still work through this browser's storage.
    await page.getByRole('radio', { name: 'Light', exact: true }).check()
    await expect(page.locator('html')).toHaveAttribute('data-theme', 'light')
    await page.getByLabel('UTC', { exact: true }).check()
    expect(await page.evaluate(() => localStorage.getItem('stardelt-nova:theme'))).toBe('light')
    expect(await page.evaluate(() => localStorage.getItem('stardelt-nova:time'))).toBe('utc')
    await expect(page.getByRole('alert')).toHaveCount(0)
    await context.close()
  })

  test('formatTime prints the same instant in both modes', () => {
    // Plain check, no browser: a fixed zone makes the local text deterministic.
    const previous = process.env.TZ
    process.env.TZ = 'Europe/Berlin'
    try {
      const epoch = Date.UTC(2026, 9, 20, 14, 5, 9)
      setTimeDisplay('local')
      expect(formatTime(epoch)).toMatch(/^2026-10-20 16:05:09 \S+$/)
      expect(formatTime('2026-10-20T14:05:09Z')).toBe(formatTime(epoch))
      setTimeDisplay('utc')
      expect(formatTime(epoch)).toBe('2026-10-20 14:05:09 UTC')
      expect(formatTime('2026-10-20T14:05:09Z')).toBe('2026-10-20 14:05:09 UTC')
    } finally {
      if (previous === undefined) delete process.env.TZ
      else process.env.TZ = previous
      setTimeDisplay('local')
    }
  })

  test('U5 the page has accessible names', async ({ browser }) => {
    // In every theme: the appearance cards change with the choice, so the names must hold
    // whether a card is selected or not.
    for (const entry of THEMES) {
      const { context, page } = await open(browser)
      await context.addInitScript((t) => { localStorage.setItem('stardelt-nova:theme', t) }, entry.id)
      await page.goto('/preferences')
      expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
      await context.close()
    }
  })
})

test.describe('admin catalogs', () => {
  // stardelt-nova#251: an administrator sees every Catalog and the permissions assigned
  // on it, by catalog and transposed as a matrix. Both views read one API answer; the
  // tests stub exactly that answer and let the rest of the shell use the dev backend.
  const ADMIN = {
    user: 'admin', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: true, groups: ['stardelt-admins'],
  }
  const NON_ADMIN = {
    user: 'outsider', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: false, groups: ['analysts'],
  }
  const READY = [
    { type: 'CatalogReady', status: 'True', reason: 'Ready', message: 'catalog and its grants are ready' },
    { type: 'GrantsReady', status: 'True', reason: 'Ready', message: 'grants match the resource' },
    { type: 'Drift', status: 'False', reason: 'Clean', message: 'no foreign grants' },
  ]
  /** One `AdminCatalog` row as the API sends it; each test overrides what it cares about. */
  const row = (fields: Record<string, unknown>) => ({
    displayName: null, description: null, owner: null, managedAccess: false,
    keyPrefix: null, phase: 'Ready', warehouseId: null, generation: 1,
    observedGeneration: 1, resourceVersion: '1', grants: [], conditions: [],
    ...fields,
  })
  // The four catalogs of the rollout check, in the API's order (bronze/silver/gold first,
  // then alphabetical — stardelt-nova#109). The page keeps that order.
  const CATALOGS = [
    row({
      name: 'finance', displayName: 'Finance', owner: 'stardelt-finops', managedAccess: true,
      grants: [
        { group: 'stardelt-finops', verb: 'browse' },
        { group: 'stardelt-analytics', verb: 'select' },
      ],
      conditions: READY,
    }),
    row({
      name: 'product', owner: 'stardelt-product', phase: 'Invalid',
      conditions: [
        { type: 'CatalogReady', status: 'False', reason: 'RoleConflict', message: 'role stardelt-product-rw already exists' },
        { type: 'GrantsReady', status: 'False', reason: 'UnknownGroup', message: 'group stardelt-ghost does not exist in the realm' },
      ],
    }),
    row({ name: 'sandbox', displayName: 'Sandbox', owner: 'stardelt-sandbox', phase: 'Unknown' }),
    row({
      name: 'warehouse', displayName: 'Shared warehouse', owner: 'stardelt-admins',
      grants: ['browse', 'select', 'write', 'create'].map((verb) => ({ group: 'stardelt-users', verb })),
      conditions: READY,
    }),
  ]

  /** The page with its one admin answer stubbed; `me` overrides `/api/me` when a role matters. */
  async function open(
    browser: Browser,
    { catalogs = CATALOGS, me, viewport = { width: 1280, height: 800 }, init }: {
      catalogs?: unknown
      me?: Record<string, unknown>
      viewport?: { width: number; height: number }
      init?: () => void
    } = {},
  ) {
    const context = await browser.newContext({ viewport })
    const page = await context.newPage()
    if (init) await page.addInitScript(init)
    await page.route('**/api/admin/catalogs', (route) => json(route, catalogs))
    if (me) await page.route('**/api/me', (route) => json(route, me))
    return { context, page }
  }

  test('by_catalog_lists_owner_grants_and_conditions', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.goto('/admin/catalogs')

    await expect(page.getByRole('heading', { level: 1, name: 'Catalogs' })).toBeVisible()
    await expect(page.getByText(
      'Platform administrators can do everything in every catalog; they are not listed. Permissions are catalog-wide today.',
    )).toBeVisible()
    await expect(page.getByRole('radio', { name: 'By catalog' })).toBeChecked()

    const table = page.getByRole('table', { name: 'All catalogs' })
    await expect(table).toBeVisible()
    // The API's order is kept: bronze/silver/gold first, then alphabetical.
    await expect(table.locator('tbody tr td:first-child > div:first-child > span:first-child'))
      .toHaveText(['finance', 'product', 'sandbox', 'warehouse'])

    const finance = table.locator('tbody tr').filter({ hasText: 'finance' })
    await expect(finance).toContainText('Finance')                  // the muted displayName
    await expect(finance).toContainText('stardelt-finops')
    await expect(finance.getByTitle('Ready: catalog and its grants are ready')).toBeVisible()
    await expect(finance).toContainText('2 groups')
    await expect(finance.getByText('Managed access')).toBeVisible()
    // The grants are text lines, one per group, revealed by the row's own details.
    await finance.getByText('Grants', { exact: true }).click()
    await expect(finance.getByText('stardelt-finops: browse')).toBeVisible()
    await expect(finance.getByText('stardelt-analytics: select')).toBeVisible()
    // All three conditions are badges; the reason is the text and the sentence is the title.
    await expect(finance.getByText('CatalogReady: Ready')).toBeVisible()
    await expect(finance.getByText('GrantsReady: Ready')).toBeVisible()
    await expect(finance.getByText('Drift: Clean')).toBeVisible()
    await expect(finance.getByRole('link', { name: 'Open catalog finance' }))
      .toHaveAttribute('href', '/catalog?catalog=finance')

    const product = table.locator('tbody tr').filter({ hasText: 'product' })
    await expect(product.getByTitle('RoleConflict: role stardelt-product-rw already exists')).toBeVisible()
    await expect(product.getByText('GrantsReady: UnknownGroup')).toBeVisible()

    const sandbox = table.locator('tbody tr').filter({ hasText: 'sandbox' })
    await expect(sandbox.getByTitle('The access-sync job has not observed this catalog yet.')).toBeVisible()
    await expect(sandbox).toContainText('0 groups')

    await expect(table.locator('tbody tr').filter({ hasText: 'warehouse' })).toContainText('1 group')
    await context.close()
  })

  test('admin_page_badge', async ({ browser }) => {
    // The API sends the layers first and marks them; the admin table keeps that order and
    // badges each catalog with its layer (stardelt-nova#109).
    const catalogs = [
      row({ name: 'bronze', layer: 'bronze', conditions: READY }),
      row({ name: 'silver', layer: 'silver', conditions: READY }),
      row({ name: 'gold', layer: 'gold', conditions: READY }),
      row({ name: 'warehouse', conditions: READY }),
    ]
    const { context, page } = await open(browser, { catalogs })
    await page.goto('/admin/catalogs')

    const table = page.getByRole('table', { name: 'All catalogs' })
    const rows = table.locator('tbody tr')
    await expect(rows.locator('td:first-child > div:first-child > span:first-child'))
      .toHaveText(['bronze', 'silver', 'gold', 'warehouse'])
    await expect(rows.nth(0).locator('td').first().getByText('Bronze', { exact: true })).toBeVisible()
    await expect(rows.nth(1).locator('td').first().getByText('Silver', { exact: true })).toBeVisible()
    await expect(rows.nth(2).locator('td').first().getByText('Gold', { exact: true })).toBeVisible()
    await expect(rows.nth(3).locator('td').first().getByText(/^(Bronze|Silver|Gold)$/)).toHaveCount(0)
    judge('/admin/catalogs', await auditPage(page))
    await context.close()

    // The same badge and the same order on the light theme; U7 checks its contrast.
    const light = await open(browser, {
      catalogs,
      init: () => { localStorage.setItem('stardelt-nova:theme', 'light') },
    })
    await light.page.goto('/admin/catalogs')
    await expect(light.page.getByRole('table', { name: 'All catalogs' })).toBeVisible()
    judge('/admin/catalogs', await auditPage(light.page))
    await light.context.close()
  })

  test('matrix_shows_groups_by_catalog_and_filters', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.goto('/admin/catalogs')
    await page.getByRole('group', { name: 'View' }).getByText('Matrix', { exact: true }).click()
    await expect(page.getByRole('radio', { name: 'Matrix' })).toBeChecked()

    const table = page.getByRole('table', { name: 'Who holds what' })
    await expect(table).toBeVisible()
    // Rows are the union of owners and grant groups, sorted; columns are the catalogs.
    await expect(table.getByRole('rowheader')).toHaveText([
      'stardelt-admins', 'stardelt-analytics', 'stardelt-finops',
      'stardelt-product', 'stardelt-sandbox', 'stardelt-users',
    ])
    await expect(table.getByRole('columnheader')).toHaveText([
      'Group', 'finance', 'product', 'sandbox', 'warehouse',
    ])

    const offered = (group: string) =>
      table.getByRole('row', { name: new RegExp(`^${group}`) }).locator('td')
    // Ownership is prefixed, the verbs are text, and an empty cell is an em dash.
    await expect(offered('stardelt-finops')).toHaveText(['owner, browse', '—', '—', '—'])
    await expect(offered('stardelt-analytics')).toHaveText(['select', '—', '—', '—'])
    await expect(offered('stardelt-admins')).toHaveText(['—', '—', '—', 'owner'])
    await expect(offered('stardelt-users')).toHaveText(['—', '—', '—', 'browse, select, write, create'])

    const filter = page.getByLabel('Filter groups or catalogs')
    await filter.fill('fin')
    await expect(table.getByRole('rowheader')).toHaveText(['stardelt-finops'])
    await expect(table.getByRole('columnheader')).toHaveText(['Group', 'finance'])

    // Naming a group narrows the rows; every catalog stays visible so its grants can be read.
    await filter.fill('stardelt-users')
    await expect(table.getByRole('rowheader')).toHaveText(['stardelt-users'])
    await expect(table.getByRole('columnheader')).toHaveText([
      'Group', 'finance', 'product', 'sandbox', 'warehouse',
    ])

    // Naming a catalog narrows the columns; the rows stay.
    await filter.fill('warehouse')
    await expect(table.getByRole('columnheader')).toHaveText(['Group', 'warehouse'])

    await filter.fill('no-such-group-or-catalog')
    await expect(page.getByText('No group or catalog matches that filter.')).toBeVisible()
    await context.close()
  })

  test('unknown_group_condition_is_visible', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.goto('/admin/catalogs')

    // `GrantsReady: False` is the API's own reason in words, not a colour; the sentence
    // the access-sync job wrote stays available as the badge's title.
    const badge = page.getByText('GrantsReady: UnknownGroup')
    await expect(badge).toBeVisible()
    await expect(badge).toHaveAttribute('title', 'group stardelt-ghost does not exist in the realm')
    // The same reason explains the catalog's phase without the person opening anything.
    await expect(page.locator('span.badge', { hasText: 'Invalid' })).toHaveAttribute(
      'title',
      'RoleConflict: role stardelt-product-rw already exists',
    )
    await context.close()
  })

  test('non_admin_is_refused', async ({ browser }) => {
    const hits: string[] = []
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    await page.route('**/api/me', (route) => json(route, NON_ADMIN))
    await page.route('**/api/admin/catalogs', (route) => {
      hits.push(new URL(route.request().url()).pathname)
      return json(route, { error: 'this view is for platform administrators' }, 403)
    })

    await page.goto('/admin/catalogs')
    // The same explanation `/apps`, `/health` and `/settings` give: a refusal in words,
    // and no request to the admin endpoint at all.
    await expect(page.getByText(/for platform administrators/i)).toBeVisible()
    const nav = await page.evaluate(() => [...document.querySelectorAll('nav a')].map((a) => a.textContent?.trim()))
    expect(nav).not.toContain('Catalogs')
    expect(hits).toEqual([])
    await context.close()
  })

  test('hostile_names_are_text', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const long = 'x'.repeat(400)
    const { context, page } = await open(browser, {
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
      catalogs: [
        row({
          name: evil, displayName: evil, owner: evil, managedAccess: true,
          grants: [{ group: evil, verb: 'browse' }],
          conditions: [
            { type: 'GrantsReady', status: 'False', reason: 'UnknownGroup', message: evil },
          ],
        }),
        row({ name: long, displayName: long, owner: long, grants: [{ group: long, verb: 'select' }] }),
      ],
    })
    await page.goto('/admin/catalogs')

    // Markup in a name is text: no element, no script, no widened page — in either view.
    await expect(page.getByText(evil, { exact: true }).first()).toBeVisible()
    for (const view of ['By catalog', 'Matrix']) {
      if (view !== 'By catalog') {
        await page.getByRole('group', { name: 'View' }).getByText(view, { exact: true }).click()
        await expect(page.getByRole('radio', { name: view })).toBeChecked()
      }
      for (const width of [1280, 360]) {
        await page.setViewportSize({ width, height: 800 })
        const s = await page.evaluate(() => ({
          xss: (window as unknown as { __xss: number }).__xss,
          imgs: document.querySelectorAll('img[src="x"]').length,
          overflow: document.documentElement.scrollWidth - window.innerWidth,
        }))
        expect(s, `${view} ${width}px`).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
        expect(s.overflow, `${view} ${width}px overflow`).toBeLessThanOrEqual(1)
      }
    }
    await context.close()
  })

  test('U5 audit at 360 px for both views', async ({ browser }) => {
    const { context, page } = await open(browser, {
      me: ADMIN,
      viewport: { width: 360, height: 740 },
    })
    await page.goto('/admin/catalogs')
    await expect(page.getByRole('table', { name: 'All catalogs' })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])

    // The segmented control and the matrix survive the phone width; the tables scroll in
    // their card rather than widening the page (rule U4).
    await page.getByRole('group', { name: 'View' }).getByText('Matrix', { exact: true }).click()
    await expect(page.getByRole('radio', { name: 'Matrix' })).toBeChecked()
    await expect(page.getByRole('table', { name: 'Who holds what' })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
    expect(overflow).toBeLessThanOrEqual(1)
    await context.close()
  })

  // ---------------------------------------------------------------------------
  // stardelt-nova#253: the Edit dialog and the write. The API is stubbed; the tests
  // assert the exact PUT body, the confirm step, the poll and the API's own sentences.
  // ---------------------------------------------------------------------------

  type RecordedWrite = { method: string; path: string; body: string | null }

  /** The page with GET and PUT of the catalog API stubbed; `onPut` answers the write. */
  async function openEditable(
    browser: Browser,
    {
      catalogs = CATALOGS,
      onPut,
      viewport = { width: 1280, height: 800 },
      init,
    }: {
      catalogs?: unknown | (() => unknown)
      onPut: (route: Route, body: string | null) => void | Promise<void>
      viewport?: { width: number; height: number }
      init?: () => void
    },
  ) {
    const context = await browser.newContext({ viewport })
    const page = await context.newPage()
    if (init) await page.addInitScript(init)
    const writes: RecordedWrite[] = []
    await page.route('**/api/admin/catalogs**', async (route) => {
      const request = route.request()
      const path = new URL(request.url()).pathname
      const method = request.method()
      if (method === 'GET' && path === '/api/admin/catalogs') {
        const list = typeof catalogs === 'function' ? (catalogs as () => unknown)() : catalogs
        return json(route, list)
      }
      if (method === 'PUT' && /^\/api\/admin\/catalogs\/[^/]+\/access$/.test(path)) {
        const body = request.postData()
        writes.push({ method, path, body })
        return onPut(route, body)
      }
      return json(route, { error: 'not stubbed' }, 404)
    })
    return { context, page, writes }
  }

  /** Open one catalog's Edit dialog and return it. */
  async function openEdit(page: Page, name = 'finance') {
    await page.goto('/admin/catalogs')
    await page.getByRole('button', { name: `Edit permissions for ${name}` }).click()
    const dialog = page.getByRole('dialog')
    await expect(dialog).toBeVisible()
    return dialog
  }

  const editedFinance = (fields: Record<string, unknown> = {}) => ({
    ...CATALOGS.find((catalog) => catalog.name === 'finance')!,
    generation: 2,
    observedGeneration: 1,
    ...fields,
  })

  test('edit_dialog_prefills_owner_and_grants', async ({ browser }) => {
    const { context, page } = await openEditable(browser, {
      onPut: (route) => json(route, editedFinance()),
    })
    const dialog = await openEdit(page)

    await expect(dialog.getByRole('heading', { name: 'Edit finance' })).toBeVisible()
    await expect(dialog.getByLabel('Owner')).toHaveValue('stardelt-finops')
    await expect(dialog.getByRole('checkbox', { name: 'Managed access' })).toBeChecked()

    await expect(dialog.getByRole('checkbox', { name: 'browse for stardelt-finops' })).toBeChecked()
    await expect(dialog.getByRole('checkbox', { name: 'select for stardelt-finops' })).not.toBeChecked()
    await expect(dialog.getByRole('checkbox', { name: 'select for stardelt-analytics' })).toBeChecked()
    await expect(dialog.getByRole('checkbox', { name: 'write for stardelt-analytics' })).not.toBeChecked()
    await expect(dialog.getByText('grant and revoke on this catalog')).toHaveCount(2)

    // Both datalists carry every group the page sees, not just this catalog's.
    const offered = await page.locator('datalist option').evaluateAll((options) =>
      options.map((option) => (option as HTMLOptionElement).value),
    )
    for (const group of ['stardelt-admins', 'stardelt-analytics', 'stardelt-finops', 'stardelt-users']) {
      expect(offered, group).toContain(group)
    }

    // Escape closes the dialog and returns focus to the button that opened it.
    await page.keyboard.press('Escape')
    await expect(dialog).toBeHidden()
    await expect(page.getByRole('button', { name: 'Edit permissions for finance' })).toBeFocused()
    await context.close()
  })

  test('save_sends_exact_body_with_resource_version', async ({ browser }) => {
    const { context, page, writes } = await openEditable(browser, {
      onPut: (route) => json(route, editedFinance()),
    })
    const dialog = await openEdit(page)

    await dialog.getByRole('checkbox', { name: 'select for stardelt-analytics' }).uncheck()
    await dialog.getByRole('checkbox', { name: 'write for stardelt-analytics' }).check()
    await dialog.getByLabel('Add group').fill('stardelt-qa')
    await dialog.getByRole('button', { name: 'Add group' }).click()
    await dialog.getByRole('checkbox', { name: 'browse for stardelt-qa' }).check()
    await dialog.getByRole('button', { name: 'Save' }).click()

    await expect.poll(() => writes.length).toBe(1)
    const write = writes[0]
    expect(write.method).toBe('PUT')
    expect(write.path).toBe('/api/admin/catalogs/finance/access')
    expect(JSON.parse(write.body ?? 'null')).toEqual({
      resourceVersion: '1',
      owner: 'stardelt-finops',
      managedAccess: true,
      grants: [
        { group: 'stardelt-finops', verb: 'browse' },
        { group: 'stardelt-analytics', verb: 'write' },
        { group: 'stardelt-qa', verb: 'browse' },
      ],
    })
    await expect(dialog).toBeHidden()
    await context.close()
  })

  test('conflict_409_message', async ({ browser }) => {
    const { context, page, writes } = await openEditable(browser, {
      onPut: (route) =>
        json(route, { error: 'this catalog changed since you opened it; reload' }, 409),
    })
    const dialog = await openEdit(page)

    await dialog.getByLabel('Owner').fill('stardelt-fintax')
    await dialog.getByRole('button', { name: 'Save' }).click()

    await expect(dialog.getByRole('alert')).toHaveText(
      'this catalog changed since you opened it; reload',
    )
    await expect(dialog).toBeVisible()
    await expect(dialog.getByLabel('Owner')).toHaveValue('stardelt-fintax')
    expect(writes).toHaveLength(1)
    await context.close()
  })

  test('confirm_step_for_removing_all_grants', async ({ browser }) => {
    const { context, page, writes } = await openEditable(browser, {
      onPut: (route) => json(route, editedFinance({ grants: [] })),
    })
    const dialog = await openEdit(page)

    await dialog.getByRole('button', { name: 'Remove stardelt-finops' }).click()
    await dialog.getByRole('button', { name: 'Remove stardelt-analytics' }).click()
    await dialog.getByRole('button', { name: 'Save' }).click()

    // Nothing is sent on the first click; the consequence is the second sentence.
    expect(writes).toHaveLength(0)
    await expect(
      dialog.getByText('This hides the catalog from people who are not administrators.'),
    ).toBeVisible()
    const confirm = dialog.getByRole('button', { name: 'Confirm' })
    await expect(confirm).toBeVisible()
    await confirm.click()

    await expect.poll(() => writes.length).toBe(1)
    expect(JSON.parse(writes[0].body ?? 'null').grants).toEqual([])
    await context.close()
  })

  test('applying_then_applied_poll', async ({ browser }) => {
    let saved = false
    const applied = () =>
      CATALOGS.map((catalog) =>
        catalog.name === 'finance'
          ? { ...catalog, generation: 2, observedGeneration: 2, conditions: READY }
          : catalog,
      )
    const { context, page } = await openEditable(browser, {
      catalogs: () => (saved ? applied() : CATALOGS),
      onPut: (route) => {
        saved = true
        json(route, editedFinance())
      },
    })
    const dialog = await openEdit(page)

    await dialog.getByLabel('Owner').fill('stardelt-fintax')
    await dialog.getByRole('button', { name: 'Save' }).click()

    // The row says what is happening until the sync reports the generation it processed.
    await expect(page.getByText('Applying…')).toBeVisible()
    await expect(page.getByText('Applied', { exact: true })).toBeVisible({ timeout: 20_000 })
    await context.close()
  })

  test('unknown_group_condition_shown', async ({ browser }) => {
    let saved = false
    const unresolved = () =>
      CATALOGS.map((catalog) =>
        catalog.name === 'finance'
          ? {
              ...catalog,
              generation: 2,
              observedGeneration: 2,
              conditions: [
                READY[0],
                {
                  type: 'GrantsReady',
                  status: 'False',
                  reason: 'UnknownGroup',
                  message: 'the realm has no group `ghost`',
                },
                READY[2],
              ],
            }
          : catalog,
      )
    const { context, page } = await openEditable(browser, {
      catalogs: () => (saved ? unresolved() : CATALOGS),
      onPut: (route) => {
        saved = true
        json(route, editedFinance())
      },
    })
    const dialog = await openEdit(page)

    await dialog.getByLabel('Owner').fill('stardelt-fintax')
    await dialog.getByRole('button', { name: 'Save' }).click()

    await expect(
      page.getByText('One of the groups does not exist in the realm; nothing is granted for it.'),
    ).toBeVisible({ timeout: 20_000 })
    await expect(page.getByText('Applying…')).toBeHidden()
    await context.close()
  })

  test('validation_sentences_inline', async ({ browser }) => {
    const { context, page, writes } = await openEditable(browser, {
      onPut: (route) => json(route, editedFinance()),
    })
    const dialog = await openEdit(page)
    const owner = dialog.getByLabel('Owner')

    await owner.fill('')
    await expect(dialog.getByText('the owner is one group name')).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Save' })).toBeDisabled()

    await owner.fill('bad group')
    await expect(
      dialog.getByText('a group name is letters, digits, dots, dashes and underscores (up to 64)'),
    ).toBeVisible()

    await owner.fill('stardelt-finops')
    await dialog.getByLabel('Add group').fill('<img src=x onerror="window.__xss=1">')
    await dialog.getByRole('button', { name: 'Add group' }).click()
    await expect(
      dialog.getByText('a group name is letters, digits, dots, dashes and underscores (up to 64)'),
    ).toBeVisible()
    expect(writes).toHaveLength(0)
    await context.close()
  })

  test('hostile_group_name_is_text', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const { context, page } = await openEditable(browser, {
      catalogs: [
        row({
          name: 'finance',
          displayName: evil,
          owner: evil,
          managedAccess: true,
          grants: [{ group: evil, verb: 'browse' }],
        }),
      ],
      onPut: (route) => json(route, editedFinance()),
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
    })
    const dialog = await openEdit(page)

    await expect(dialog.getByLabel('Owner')).toHaveValue(evil)
    await expect(dialog.getByText(evil, { exact: true }).first()).toBeVisible()
    for (const width of [1280, 360]) {
      await page.setViewportSize({ width, height: 800 })
      const s = await page.evaluate(() => ({
        xss: (window as unknown as { __xss: number }).__xss,
        imgs: document.querySelectorAll('img[src="x"]').length,
        overflow: document.documentElement.scrollWidth - window.innerWidth,
      }))
      expect(s, `${width}px`).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
      expect(s.overflow, `${width}px overflow`).toBeLessThanOrEqual(1)
    }
    await context.close()
  })

  test('U5 audit with the dialog open at 360 px', async ({ browser }) => {
    const { context, page } = await openEditable(browser, {
      onPut: (route) => json(route, editedFinance()),
      viewport: { width: 360, height: 740 },
    })
    const dialog = await openEdit(page)
    await expect(dialog).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
    expect(overflow).toBeLessThanOrEqual(1)
    await context.close()
  })
})

test.describe('overview metrics', () => {
  // stardelt-nova#250: the Overview shows the catalog metrics API's numbers per catalog
  // the person may see, instead of the static "Format: Iceberg" tile. The one answer is
  // stubbed; the rest of the shell uses the dev backend, as the other suites do.
  const now = Date.now()
  const CATALOGS = [
    { name: 'gold', schemas: 2, tables: 7, records: 1234, sizeBytes: 1536, lastCommitMs: now - 5 * 60_000, truncated: false, pending: false, updatedAt: now },
    { name: 'silver', schemas: 1, tables: 1, records: 1, sizeBytes: 512, lastCommitMs: now - 2 * 3600_000, truncated: false, pending: false, updatedAt: now },
    { name: 'bronze', schemas: 2, tables: 1, records: 0, sizeBytes: 0, lastCommitMs: null, truncated: false, pending: false, updatedAt: now },
  ]
  const TOTALS = { catalogs: 3, schemas: 5, tables: 9, records: 1235, sizeBytes: 2048 }
  const METRICS = { catalogs: CATALOGS, totals: TOTALS, generatedAt: now }

  /** The Overview with its one metrics answer stubbed; every test overrides what it needs. */
  async function open(browser: Browser, { metrics = METRICS, status, viewport = { width: 1280, height: 800 }, init }: {
    metrics?: unknown
    status?: number
    viewport?: { width: number; height: number }
    init?: () => void
  } = {}) {
    const context = await browser.newContext({ viewport })
    const page = await context.newPage()
    if (init) await page.addInitScript(init)
    await page.route('**/api/catalog/metrics', (route) => json(route, metrics, status))
    return { context, page }
  }

  /** One summary tile, found by its label; the card around it holds the value and hint. */
  const tile = (page: Page, label: string) =>
    page.locator('div.card').filter({ has: page.locator('span.label', { hasText: label }) })

  test('tiles_show_totals_and_no_format_tile', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.goto('/')
    await expect(page.getByRole('heading', { level: 1, name: 'Lakehouse overview' })).toBeVisible()

    // The tiles read the totals: catalogs, schemas, tables, size and rows.
    await expect(tile(page, 'Catalogs')).toContainText('3')
    await expect(tile(page, 'Catalogs')).toContainText('5 schemas')
    await expect(tile(page, 'Tables')).toContainText('9')
    await expect(tile(page, 'Tables')).toContainText('2.0 KiB · 1,235 rows')

    // The static format tile is gone, and its claims with it.
    await expect(page.getByText('Format', { exact: true })).toHaveCount(0)
    await expect(page.getByText('Iceberg', { exact: true })).toHaveCount(0)
    await expect(page.getByText('open tables on S3')).toHaveCount(0)
    await context.close()
  })

  test('catalog_rows_link_to_the_explorer', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.goto('/')

    const table = page.getByRole('table', { name: 'Catalogs you can see' })
    await expect(table).toBeVisible()
    await expect(table.getByRole('columnheader')).toHaveText([
      'Catalog', 'Schemas', 'Tables', 'Size', 'Rows', 'Last change',
    ])
    // One row per catalog, in the API's order, with the numbers it sent.
    await expect(table.locator('tbody tr')).toHaveCount(3)
    const gold = table.locator('tbody tr').filter({ hasText: 'gold' })
    await expect(gold).toContainText('2')
    await expect(gold).toContainText('7')
    await expect(gold).toContainText('1.5 KiB')
    await expect(gold).toContainText('1,234')
    const goldChanged = CATALOGS[0].lastCommitMs as number
    await expect(gold.getByText(relativeTime(goldChanged))).toHaveAttribute('title', formatTime(goldChanged))
    // A catalog with no commit says so instead of inventing a time.
    await expect(table.locator('tbody tr').filter({ hasText: 'bronze' })).toContainText('—')

    const link = table.getByRole('link', { name: 'gold' })
    await expect(link).toHaveAttribute('href', '/catalog?catalog=gold')
    await link.click()
    await expect(page).toHaveURL(/\/catalog\?catalog=gold$/)
    await context.close()
  })

  test('overview_rows_in_layer_order_with_badge', async ({ browser }) => {
    // The API sends bronze/silver/gold first and marks their layer; the table keeps that
    // order and names the layer beside the catalog (stardelt-nova#109).
    const catalogs = [
      { name: 'bronze', layer: 'bronze', schemas: 1, tables: 1, records: 0, sizeBytes: 0, lastCommitMs: null, truncated: false },
      { name: 'silver', layer: 'silver', schemas: 1, tables: 2, records: 10, sizeBytes: 512, lastCommitMs: now, truncated: false },
      { name: 'gold', layer: 'gold', schemas: 2, tables: 7, records: 1234, sizeBytes: 1536, lastCommitMs: now, truncated: false },
      { name: 'warehouse', schemas: 3, tables: 9, records: 99, sizeBytes: 2048, lastCommitMs: now, truncated: false },
    ]
    const { context, page } = await open(browser, {
      metrics: { catalogs, totals: { catalogs: 4, schemas: 7, tables: 19, records: 1343, sizeBytes: 4096 }, generatedAt: now },
    })
    await page.goto('/')

    const table = page.getByRole('table', { name: 'Catalogs you can see' })
    const rows = table.locator('tbody tr')
    await expect(rows.locator('td:first-child a')).toHaveText(['bronze', 'silver', 'gold', 'warehouse'])
    await expect(rows.nth(0).getByText('Bronze', { exact: true })).toBeVisible()
    await expect(rows.nth(1).getByText('Silver', { exact: true })).toBeVisible()
    await expect(rows.nth(2).getByText('Gold', { exact: true })).toBeVisible()
    // Warehouse is not a medallion layer: no badge is invented for it.
    await expect(rows.nth(3).getByText(/^(Bronze|Silver|Gold)$/)).toHaveCount(0)
    judge('/', await auditPage(page))
    await context.close()
  })

  test('hostile_layer_is_text', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const { context, page } = await open(browser, {
      metrics: {
        catalogs: [
          { name: 'shared', layer: evil, schemas: 1, tables: 1, records: 1, sizeBytes: 1, lastCommitMs: now, truncated: false },
        ],
        totals: { catalogs: 1, schemas: 1, tables: 1, records: 1, sizeBytes: 1 },
        generatedAt: now,
      },
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
    })
    await page.goto('/')
    // The API validates the layer, but a value it would never send is still text: no
    // element, no script, no widened page.
    await expect(page.getByText(evil, { exact: true })).toBeVisible()
    const s = await page.evaluate(() => ({
      xss: (window as unknown as { __xss: number }).__xss,
      imgs: document.querySelectorAll('img[src="x"]').length,
      overflow: document.documentElement.scrollWidth - window.innerWidth,
    }))
    expect(s).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
    expect(s.overflow).toBeLessThanOrEqual(1)
    await context.close()
  })

  test('error_row_and_truncated_note', async ({ browser }) => {
    const error = 'the warehouse is not reachable right now'
    const catalogs = [
      { ...CATALOGS[0], truncated: true },
      { name: 'broken', schemas: 0, tables: 0, records: 0, sizeBytes: 0, lastCommitMs: null, truncated: false, error },
    ]
    const { context, page } = await open(browser, { metrics: { catalogs, totals: TOTALS, generatedAt: now } })
    await page.goto('/')

    const table = page.getByRole('table', { name: 'Catalogs you can see' })
    // A failed scan keeps its row and its sentence; it is not dropped, which would read
    // "you have no such catalog".
    const broken = table.locator('tbody tr').filter({ hasText: 'broken' })
    await expect(broken.getByRole('status')).toHaveText(error)
    await expect(table.locator('tbody tr')).toHaveCount(2)

    // Rows and size are a sample in this answer; the note says so under the table.
    await expect(page.getByText('first 200 tables counted')).toBeVisible()
    await context.close()
  })

  test('forbidden_shows_the_sentence', async ({ browser }) => {
    const sentence = 'no catalog is open to you yet; ask an administrator for access'
    const { context, page } = await open(browser, { metrics: { error: sentence }, status: 403 })
    await page.goto('/')

    await expect(page.getByRole('status').filter({ hasText: sentence })).toBeVisible()
    await expect(page.getByText('Ask an administrator for access.')).toBeVisible()
    // The tiles answer with a dash and the API's sentence as the hint, never a zero.
    await expect(tile(page, 'Catalogs')).toContainText('—')
    await expect(tile(page, 'Catalogs')).toContainText(sentence)
    await expect(tile(page, 'Tables')).toContainText('—')
    await expect(page.getByRole('table', { name: 'Catalogs you can see' })).toHaveCount(0)
    await context.close()
  })

  test('hostile_catalog_name_is_text', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const long = 'x'.repeat(400)
    const catalogs = [
      { name: evil, schemas: 1, tables: 1, records: 1, sizeBytes: 1, lastCommitMs: now, truncated: false },
      { name: long, schemas: 1, tables: 1, records: 1, sizeBytes: 1, lastCommitMs: now, truncated: false },
    ]
    const { context, page } = await open(browser, {
      metrics: { catalogs, totals: { catalogs: 2, schemas: 2, tables: 2, records: 2, sizeBytes: 2 }, generatedAt: now },
      init: () => { (window as unknown as { __xss: number }).__xss = 0 },
    })
    await page.goto('/')
    // Markup in a catalog name is text: no element, no script.
    await expect(page.getByText(evil, { exact: true })).toBeVisible()

    for (const width of [1280, 360]) {
      await page.setViewportSize({ width, height: 800 })
      const s = await page.evaluate(() => ({
        xss: (window as unknown as { __xss: number }).__xss,
        imgs: document.querySelectorAll('img[src="x"]').length,
        overflow: document.documentElement.scrollWidth - window.innerWidth,
      }))
      expect(s, `${width}px`).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
      expect(s.overflow, `${width}px overflow`).toBeLessThanOrEqual(1)
    }
    // At the phone width the table scrolls inside its card rather than widening the page.
    const table = page.getByRole('table', { name: 'Catalogs you can see' })
    expect(await table.evaluate((el) => getComputedStyle(el.parentElement as HTMLElement).overflowX)).toBe('auto')
    await context.close()
  })

  test('U5 audit of the page at 360 px', async ({ browser }) => {
    const { context, page } = await open(browser, { viewport: { width: 360, height: 740 } })
    await page.goto('/')
    await expect(page.getByRole('table', { name: 'Catalogs you can see' })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    const overflow = await page.evaluate(() => document.documentElement.scrollWidth - window.innerWidth)
    expect(overflow).toBeLessThanOrEqual(1)
    await context.close()
  })

  // stardelt-nova#263: the numbers are read in the background every five minutes, so the
  // page never waits for a scan. A catalog with none yet says so; the card says how fresh
  // the numbers are; an administrator can ask for a round now.
  test('pending_rows_then_numbers', async ({ browser }) => {
    const pending = {
      catalogs: [
        { name: 'gold', schemas: 0, tables: 0, records: 0, sizeBytes: 0, lastCommitMs: null, truncated: false, pending: true, updatedAt: null },
        { ...CATALOGS[2], name: 'bronze' },
      ],
      // A pending row is not counted: its zeros are not numbers.
      totals: { catalogs: 1, schemas: 2, tables: 1, records: 0, sizeBytes: 0 },
      generatedAt: now,
    }
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    let reads = 0
    await page.route('**/api/catalog/metrics', (route) => {
      reads += 1
      return json(route, reads === 1 ? pending : METRICS)
    })

    await page.goto('/')
    const table = page.getByRole('table', { name: 'Catalogs you can see' })
    const gold = table.locator('tbody tr').filter({ hasText: 'gold' })
    // No numbers yet: "calculating…" in muted text, never a zero and never a spinner.
    await expect(gold.getByText('calculating…')).toHaveCount(5)
    await expect(gold.getByText('calculating…').first()).toHaveClass(/text-text-mute/)
    // The tiles answer at once with the totals the API sent, pending row left out.
    await expect(tile(page, 'Catalogs')).toContainText('1')
    await expect(tile(page, 'Tables')).toContainText('1')
    await expect(tile(page, 'Tables')).toContainText('0 rows')

    // The page asks again five seconds later (the interval only runs while something is
    // pending) and the numbers replace the placeholders.
    await expect(gold).toContainText('1,234', { timeout: 20_000 })
    await expect(gold.getByText('calculating…')).toHaveCount(0)
    await expect(tile(page, 'Tables')).toContainText('9')
    await context.close()
  })

  test('updated_caption_and_admin_refresh_button', async ({ browser }) => {
    const updatedAt = now - 4 * 60_000
    const metrics = { catalogs: CATALOGS.map(catalog => ({ ...catalog, updatedAt })), totals: TOTALS, generatedAt: now }
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    let refreshes = 0
    let reads = 0
    await page.route('**/api/catalog/metrics', (route) => {
      reads += 1
      return json(route, metrics)
    })
    await page.route('**/api/catalog/metrics/refresh', (route) => {
      refreshes += 1
      return json(route, { accepted: true }, 202)
    })

    await page.goto('/')
    // The card says how fresh the numbers are, and the tooltip has the exact moment.
    const caption = page.getByText(`Updated ${relativeTime(updatedAt)}`)
    await expect(caption).toBeVisible()
    await expect(caption).toHaveAttribute('title', formatTime(updatedAt))

    // dev mode is the administrator: the button is there, and the round is a POST.
    const refresh = page.getByRole('button', { name: 'Refresh' })
    await expect(refresh).toBeVisible()
    const before = reads
    await refresh.click()
    await expect.poll(() => refreshes).toBe(1)
    // ...and the numbers follow: the page asks again a few seconds after the round starts.
    await expect.poll(() => reads, { timeout: 20_000 }).toBeGreaterThan(before)
    await context.close()
  })

  test('non_admin_has_no_refresh', async ({ browser }) => {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    await page.route('**/api/me', (route) =>
      json(route, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analysts'] }))
    await page.route('**/api/catalog/metrics', (route) => json(route, METRICS))

    await page.goto('/')
    await expect(page.getByRole('table', { name: 'Catalogs you can see' })).toBeVisible()
    // The numbers are shared and the freshness is everyone's; the round is an operator's.
    await expect(page.getByText(/^Updated /)).toBeVisible()
    await expect(page.getByRole('button', { name: 'Refresh' })).toHaveCount(0)
    await context.close()
  })

  test('error_row_keeps_old_numbers', async ({ browser }) => {
    const error = 'the catalog is not reachable'
    const catalogs = [
      CATALOGS[0],
      {
        name: 'broken', schemas: 4, tables: 9, records: 77, sizeBytes: 2048,
        lastCommitMs: now - 3600_000, truncated: false, pending: false,
        updatedAt: now - 600_000, error,
      },
    ]
    const { context, page } = await open(browser, {
      metrics: { catalogs, totals: TOTALS, generatedAt: now },
    })
    await page.goto('/')

    // A failed refresh keeps the last good numbers and adds the sentence: the page does
    // not blank a catalog it could read ten minutes ago.
    const broken = page.getByRole('table', { name: 'Catalogs you can see' }).locator('tbody tr').filter({ hasText: 'broken' })
    await expect(broken.getByRole('status')).toHaveText(error)
    await expect(broken).toContainText('9')
    await expect(broken).toContainText('77')
    await expect(broken).toContainText('2.0 KiB')
    await expect(broken.getByText('calculating…')).toHaveCount(0)
    await context.close()
  })
})

test.describe('format', () => {
  test('sqlTableRef names the catalog the person works in', () => {
    // Plain check, no browser: the generated SQL is the one a person pastes and runs.
    // Existing callers keep the historical default, so nothing that worked changes.
    expect(sqlTableRef('sales', 'orders')).toBe('warehouse.sales.orders')
    expect(sqlTableRef('sales', 'orders', 'finance')).toBe('finance.sales.orders')
    expect(sqlTableRef('a.b', 'orders', 'finance')).toBe('finance.a.b.orders')
    // Names come from the API or the URL: anything not a plain identifier is quoted, and
    // a quote inside one is doubled instead of ending the identifier early.
    expect(sqlTableRef('sales', 'my table', 'finance')).toBe('finance.sales."my table"')
    expect(sqlTableRef('sales', 'orders', 'x"y')).toBe('"x""y".sales.orders')
  })
})

test.describe('warehouses', () => {
  // `GET /api/warehouses` (stardelt-nova#236): the class a person uses is the `yours` row,
  // shown next to the Run button and on the Profile page.
  const SMALL = {
    name: 'small', displayName: 'Small', size: 'small', running: 10, queued: 100,
    memoryShare: '20%', default: true, canUse: true, yours: false,
  }
  const LARGE = {
    name: 'large', displayName: 'Large', size: 'large', running: 6, queued: 70,
    memoryShare: '70%', default: false, canUse: true, yours: true,
  }
  const ME = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    is_admin: false, groups: ['analysts'], via: 'session',
  }

  /** One page with `/api/warehouses` answered by `warehouses`; the caller closes the context. */
  async function open(browser: Browser, route: string, warehouses: unknown[]) {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      if (path === '/api/warehouses') return json(r, warehouses)
      if (path === '/api/me') return json(r, ME)
      if (path === '/api/settings') return json(r, {})
      if (path === '/api/apps/links') return json(r, [])
      return json(r, [])
    })
    await page.goto(route)
    return { context, page }
  }

  test('the class in use is named next to Run and on the Profile', async ({ browser }) => {
    const { context, page } = await open(browser, '/query', [SMALL, LARGE])
    // The `yours` row, not the default one; the title carries the three numbers.
    const note = page.getByTitle('6 running, 70 queued, 70% memory')
    await expect(note).toHaveText('Warehouse: Large (large)')
    await expect(page.getByTitle('10 running, 100 queued, 20% memory')).toHaveCount(0)

    await page.goto('/profile')
    await expect(page.getByText('SQL warehouse', { exact: true })).toBeVisible()
    await expect(page.getByTitle('6 running, 70 queued, 70% memory')).toHaveText('Warehouse: Large (large)')
    await context.close()
  })

  test('an administrator row has no size and an empty list shows no line', async ({ browser }) => {
    const admin = {
      name: 'admin', displayName: 'Administrators', size: null, running: 50, queued: 100,
      memoryShare: '100%', default: false, canUse: true, yours: true,
    }
    const { context, page } = await open(browser, '/profile', [admin])
    await expect(page.getByTitle('50 running, 100 queued, 100% memory')).toHaveText('Warehouse: Administrators')
    await expect(page.locator('body')).not.toContainText('(null)')
    await context.close()

    const empty = await open(browser, '/query', [])
    await expect(empty.page.getByText(/^Warehouse:/)).toHaveCount(0)
    await empty.context.close()
  })
})

test.describe('profile', () => {
  const ME = {
    user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
    name: 'Alice Anders', email: 'alice@example.test', sub: '1f0c4a52-6b3e-4f1a-9c2d-8e7f6a5b4c3d',
    groups: ['analysts', 'data-eng', '/stardelt/platform'], is_admin: false, via: 'session',
  }
  const MEMBER =
    'Administrators run the platform: they see all connections and pipelines of all groups and manage the platform apps. Ask one to change your role.'
  const NO_GROUPS =
    'You are not in any group. Personal connections and the SQL editor work; features that belong to a group need one.'
  const OVERAGE =
    'Your identity provider sent more groups than fit in your sign-in token, so Nova sees only some of them. Ask an administrator.'

  /** The Profile page with every `/api/**` call stubbed; the caller closes the context. */
  async function open(
    browser: Browser,
    me: Record<string, unknown>,
    {
      permissions = [] as string[],
      viewport = { width: 1280, height: 800 },
      init,
    }: { permissions?: string[]; viewport?: { width: number; height: number }; init?: () => void } = {},
  ) {
    const context = await browser.newContext({ viewport, permissions })
    if (init) await context.addInitScript(init)
    const page = await context.newPage()
    await page.route('**/api/**', (route) => {
      const path = new URL(route.request().url()).pathname
      if (path === '/api/me') return json(route, me)
      if (path === '/api/settings') return json(route, {})
      if (path === '/api/apps/links') return json(route, [])
      return json(route, [])
    })
    await page.goto('/profile')
    return { context, page }
  }

  test('shows account, groups and role', async ({ browser }) => {
    const { context, page } = await open(browser, ME, {
      permissions: ['clipboard-read', 'clipboard-write'],
    })

    await expect(page.getByRole('heading', { level: 1, name: 'Profile' })).toBeVisible()
    await expect(page.getByText('Who Nova thinks you are, and what that gives you.')).toBeVisible()
    const main = page.locator('#main')

    // Account rows and the sign-in facts.
    for (const label of ['Name', 'Username', 'Email', 'User id', 'Method', 'Session']) {
      await expect(main.getByText(label, { exact: true })).toBeVisible()
    }
    for (const value of [ME.name, ME.user, ME.email, ME.sub, 'Single sign-on', 'Browser session']) {
      await expect(main.getByText(value, { exact: true })).toBeVisible()
    }

    // Every group is a chip, and the heading counts them.
    await expect(page.getByRole('heading', { level: 2, name: 'Groups (3)' })).toBeVisible()
    for (const group of ME.groups) {
      await expect(main.getByText(group, { exact: true })).toBeVisible()
    }

    await expect(main.getByText('Member', { exact: true })).toBeVisible()
    await expect(main.getByText(MEMBER)).toBeVisible()

    await page.getByRole('button', { name: 'Copy your user id' }).click()
    expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(ME.sub)
    await context.close()
  })

  test('administrator sentence names the group', async ({ browser }) => {
    const byGroup = await open(browser, {
      ...ME, is_admin: true, admin_group: 'stardelt-admins', admin_by: 'group',
    })
    await expect(
      byGroup.page.getByText('You are an administrator because you belong to the group stardelt-admins.', { exact: true }),
    ).toBeVisible()
    await expect(byGroup.page.getByText('Administrator', { exact: true })).toBeVisible()
    await byGroup.context.close()

    const byRole = await open(browser, {
      ...ME, is_admin: true, admin_group: 'stardelt-admin', admin_by: 'role',
    })
    await expect(
      byRole.page.getByText('You are an administrator because you hold the role stardelt-admin.', { exact: true }),
    ).toBeVisible()
    await byRole.context.close()
  })

  test('no groups and overage', async ({ browser }) => {
    const { context, page } = await open(browser, {
      ...ME, groups: [], groups_overage: true,
    })
    await expect(page.getByRole('heading', { level: 2, name: 'Groups (0)' })).toBeVisible()
    await expect(page.getByText(NO_GROUPS)).toBeVisible()
    await expect(page.getByRole('status')).toHaveText(OVERAGE)
    await context.close()
  })

  test('session end', async ({ browser }) => {
    const epoch = Date.UTC(2026, 9, 20, 14, 5, 9) / 1000
    const withEnd = await open(browser, { ...ME, session_expires_at: epoch })
    const sentence = withEnd.page.getByText(/Your session ends \d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}.* You sign in again then\./)
    await expect(sentence).toBeVisible()
    await expect(sentence).toHaveAttribute('title', new Date(epoch * 1000).toISOString())
    await withEnd.context.close()

    const withoutEnd = await open(browser, ME)
    await expect(withoutEnd.page.getByText(/Your session ends/)).toHaveCount(0)
    await withoutEnd.context.close()
  })

  test('dev mode', async ({ browser }) => {
    const { context, page } = await open(browser, {
      user: 'operator', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse',
    })
    await expect(
      page.getByText('Development mode: nobody signs in; you are the operator of this installation.'),
    ).toBeVisible()
    await expect(page.getByText('None (development mode)', { exact: true })).toBeVisible()
    // No rows that would hold nothing: dev mode has no sub, name, email or groups.
    await expect(page.getByText('User id', { exact: true })).toHaveCount(0)
    await expect(page.getByText('Name', { exact: true })).toHaveCount(0)
    await expect(page.getByText('Email', { exact: true })).toHaveCount(0)
    await expect(page.getByRole('list', { name: 'Your groups' })).toHaveCount(0)
    // The operator still sees the role `isAdmin()` grants without `is_admin`.
    await expect(page.getByText('Administrator', { exact: true })).toBeVisible()
    await expect(page.getByText(MEMBER)).toHaveCount(0)
    await context.close()
  })

  test('hostile text', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const long = 'x'.repeat(400)
    const { context, page } = await open(
      browser,
      {
        ...ME,
        name: evil,
        email: evil,
        groups: Array.from({ length: 60 }, (_, i) => (i === 0 ? evil : long)),
      },
      { init: () => { (window as unknown as { __xss: number }).__xss = 0 } },
    )

    await expect(page.getByRole('heading', { level: 2, name: 'Groups (60)' })).toBeVisible()
    await expect(page.getByText(evil, { exact: true }).first()).toBeVisible()

    for (const width of [1280, 360]) {
      await page.setViewportSize({ width, height: 800 })
      const s = await page.evaluate(() => ({
        xss: (window as unknown as { __xss: number }).__xss,
        imgs: document.querySelectorAll('img[src="x"]').length,
        overflow: document.documentElement.scrollWidth - window.innerWidth,
      }))
      expect(s, `${width}px`).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
      expect(s.overflow, `${width}px overflow`).toBeLessThanOrEqual(1)
    }
    await context.close()
  })

  test('U5 the page has accessible names', async ({ browser }) => {
    const { context, page } = await open(browser, ME)
    await expect(page.getByRole('heading', { level: 1, name: 'Profile' })).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    await context.close()
  })
})

test.describe('person menu', () => {
  const ME_MEMBER = { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analysts'], email: 'alice@example.com' }
  const ME_ADMIN = { user: 'bob', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: true, groups: ['admins'], email: 'bob@example.com' }
  const ME_DEV = { user: 'operator', auth_mode: 'static-dev', demo: false, warehouse: 'warehouse' }

  async function openPersonMenu(browser: Browser, me: unknown, route = '/') {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      if (path === '/api/me') return json(r, me)
      if (path === '/api/apps/links') return json(r, [])
      return json(r, path.includes('namespaces') ? { namespaces: [] } : [])
    })
    await page.goto(route)
    await page.waitForTimeout(500)
    const button = page.getByRole('button', { name: new RegExp(`Account menu for`) })
    return { context, page, button }
  }

  test('opens with Enter and closes with Escape and focus returns', async ({ browser }) => {
    const { context, page, button } = await openPersonMenu(browser, ME_MEMBER)
    await button.focus()
    await page.keyboard.press('Enter')
    const menu = page.getByRole('menu', { name: 'Account menu' })
    await expect(menu).toBeVisible()
    const first = page.getByRole('menuitem', { name: 'Profile' })
    await expect(first).toBeFocused()
    await page.keyboard.press('Escape')
    await expect(menu).not.toBeVisible()
    await expect(button).toBeFocused()
    await context.close()
  })

  test('arrow keys, Home and End move between items and Tab closes', async ({ browser }) => {
    const { context, page, button } = await openPersonMenu(browser, ME_MEMBER)
    await button.click()
    const menu = page.getByRole('menu', { name: 'Account menu' })
    await expect(menu).toBeVisible()
    const items = [page.getByRole('menuitem', { name: 'Profile' }), page.getByRole('menuitem', { name: 'Preferences' }), page.getByRole('menuitem', { name: 'Sign out' })]
    await expect(items[0]).toBeFocused()
    await page.keyboard.press('ArrowDown')
    await expect(items[1]).toBeFocused()
    await page.keyboard.press('ArrowDown')
    await expect(items[2]).toBeFocused()
    await page.keyboard.press('Home')
    await expect(items[0]).toBeFocused()
    await page.keyboard.press('End')
    await expect(items[2]).toBeFocused()
    await page.keyboard.press('Tab')
    await expect(menu).not.toBeVisible()
    await context.close()
  })

  test('items by role', async ({ browser }) => {
    // member
    const m1 = await openPersonMenu(browser, ME_MEMBER)
    await m1.button.click()
    await expect(m1.page.getByRole('menuitem', { name: 'Profile' })).toBeVisible()
    await expect(m1.page.getByRole('menuitem', { name: 'Preferences' })).toBeVisible()
    await expect(m1.page.getByRole('menuitem', { name: 'Sign out' })).toBeVisible()
    await expect(m1.page.getByRole('menuitem', { name: 'Platform settings' })).toHaveCount(0)
    await expect(m1.page.getByText('Administrator')).toHaveCount(0)
    await m1.context.close()

    // administrator
    const m2 = await openPersonMenu(browser, ME_ADMIN)
    await m2.button.click()
    await expect(m2.page.getByText('Administrator')).toBeVisible()
    await expect(m2.page.getByRole('menuitem', { name: 'Platform settings' })).toBeVisible()
    await m2.context.close()

    // dev mode
    const m3 = await openPersonMenu(browser, ME_DEV)
    await m3.button.click()
    await expect(m3.page.getByText('Development mode (no sign-in)')).toBeVisible()
    await expect(m3.page.getByRole('menuitem', { name: 'Profile' })).toBeVisible()
    await expect(m3.page.getByRole('menuitem', { name: 'Preferences' })).toBeVisible()
    await expect(m3.page.getByRole('menuitem', { name: 'Sign out' })).toHaveCount(0)
    await m3.context.close()
  })

  test('sign out works from the menu', async ({ browser }) => {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    let logoutReq: Request | null = null
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      if (path === '/api/me') return json(r, ME_MEMBER)
      if (path === '/api/apps/links') return json(r, [])
      return json(r, [])
    })
    page.on('request', (req) => {
      if (req.url().includes('/api/auth/logout')) logoutReq = req
    })
    await page.goto('/')
    await page.getByRole('button', { name: /Account menu for/ }).click()
    const logoutBtn = page.getByRole('menuitem', { name: 'Sign out' })
    await expect(logoutBtn).toBeVisible()
    // form submit
    await logoutBtn.click()
    await page.waitForTimeout(100)
    await context.close()
  })

  test('hostile name and email stay text and truncated', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const long = 'x'.repeat(300)
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    await page.addInitScript(() => { (window as unknown as { __xss: number }).__xss = 0 })
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      if (path === '/api/me') return json(r, { user: long, auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analysts'], email: evil, name: long })
      if (path === '/api/apps/links') return json(r, [])
      return json(r, [])
    })
    await page.goto('/')
    await page.getByRole('button', { name: /Account menu for/ }).click()
    await expect(page.getByText(long, { exact: true }).first()).toBeVisible()
    await expect(page.getByText(evil, { exact: true }).first()).toBeVisible()
    for (const width of [1280, 360]) {
      await page.setViewportSize({ width, height: 800 })
      const s = await page.evaluate(() => ({
        xss: (window as unknown as { __xss: number }).__xss,
        imgs: document.querySelectorAll('img[src="x"]').length,
        overflow: document.documentElement.scrollWidth - window.innerWidth,
      }))
      expect(s, `${width}px`).toEqual({ xss: 0, imgs: 0, overflow: expect.any(Number) })
      expect(s.overflow, `${width}px overflow`).toBeLessThanOrEqual(1)
    }
    await context.close()
  })

  test('the sidebar shows the Admin group without scrolling at 1280 x 800', async ({ browser }) => {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    await page.route('**/api/**', (r) => {
      const path = new URL(r.request().url()).pathname
      if (path === '/api/me') return json(r, ME_ADMIN)
      if (path === '/api/apps/links') return json(r, [])
      return json(r, [])
    })
    await page.goto('/')
    await page.waitForTimeout(500)
    const settings = page.locator('nav').getByRole('link', { name: 'Settings' })
    await expect(settings).toBeVisible()
    const box = await settings.boundingBox()
    expect(box).not.toBeNull()
    const nav = page.locator('nav')
    const navBox = await nav.boundingBox()
    expect(navBox).not.toBeNull()
    expect(box!.y + box!.height).toBeLessThanOrEqual(navBox!.y + navBox!.height + 5)
    await context.close()
  })

  test('generic route tests still pass and U5 audit over open menu: no U5 finding', async ({ browser }) => {
    const { context, page, button } = await openPersonMenu(browser, ME_ADMIN)
    await button.click()
    const menu = page.getByRole('menu', { name: 'Account menu' })
    await expect(menu).toBeVisible()
    expect((await auditPage(page)).filter((finding) => finding.rule === 'U5')).toEqual([])
    await context.close()
  })
})

test.describe('activity', () => {
  // stardelt-nova#352: "Jobs" sounded like the scheduled work of Pipelines. The page lists every
  // query, load and transfer, so it is called Activity; the address stays, old links keep working.
  test('the page that listed jobs is called Activity, at the same address', async ({ browser }) => {
    const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
    const page = await context.newPage()
    await page.route('**/api/**', (route) => {
      const path = new URL(route.request().url()).pathname
      if (path === '/api/me') {
        return json(route, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analytics'] })
      }
      if (path === '/api/jobs') return json(route, [])
      // Everything else is unavailable, as on an installation with nothing behind Nova: the pages show
      // their failure states instead of choking on data of the wrong shape.
      return json(route, { error: 'not available in this test' }, 503)
    })

    await page.goto('/jobs')
    await expect(page.getByRole('heading', { name: 'Activity', exact: true })).toBeVisible()
    await expect(page.getByText('No activity yet.')).toBeVisible()
    await expect(page).toHaveTitle('Activity · stardelt Nova')
    const nav = await page.evaluate(() => [...document.querySelectorAll('nav a')].map((a) => a.textContent?.trim()))
    expect(nav).toContain('Activity')
    expect(nav).not.toContain('Jobs')

    await page.goto('/')
    await expect(page.getByText('recent queries, loads and transfers')).toBeVisible()
    await expect(page.getByText('Jobs', { exact: true })).toHaveCount(0)
    await context.close()
  })
})
