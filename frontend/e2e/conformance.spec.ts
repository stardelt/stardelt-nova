import { test, expect, type Browser, type Locator, type Page, type Route } from '@playwright/test'
import { KNOWN, type Known } from './known'
import { auditPage, focusAudit, type Finding } from './audit'

// Rules U1–U13 are written down in stardelt-internal/design/ui-conformance.md.

const ROUTES = ['/', '/catalog', '/query', '/data', '/connections', '/jobs', '/queries',
  '/apps', '/lineage', '/dashboards', '/notebooks', '/health', '/settings', '/no-such-page']
const VARIANTS = [
  { name: 'desktop-dark', viewport: { width: 1280, height: 800 }, theme: 'dark', keyboard: true },
  { name: 'desktop-light', viewport: { width: 1280, height: 800 }, theme: 'light', keyboard: false },
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
    await expect(dialog).toContainText('Pipelines and catalogs that use it will stop working.')

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
