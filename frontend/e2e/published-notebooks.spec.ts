import { test, expect, type Browser, type Route } from '@playwright/test'
import { auditPage, focusAudit } from './audit'

// The "Published notebooks" section of the Notebooks page (stardelt-nova#438): the newest version
// of every notebook the caller's groups have published, a group filter, and a row that opens the
// versions and the parameters. Every `/api/**` call is stubbed.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
  is_admin: false, groups: ['analytics', 'finance'],
}
const V3 = '4f9c2a1b7e6d5c4b3a29180706050403020100ffaa'
const V2 = '9b8a7c6d5e4f30211203f4e5d6c7b8a990817263'
const V1 = '1a2b3c4d5e6f708192a3b4c5d6e7f809a1b2c3d4'
const ANALYTICS = {
  group: 'analytics', name: 'smoke.ipynb', objectVersion: V3, size: 2048,
  uploadedAt: '2026-10-20T08:00:00Z', uploadedBy: 'alice',
}
const FINANCE = {
  group: 'finance', name: 'finance.ipynb', objectVersion: V1, size: 1024,
  uploadedAt: '2026-10-18T10:00:00Z', uploadedBy: 'bob',
}
const SMOKE_VERSIONS = [
  { objectVersion: V3, size: 2048, uploadedAt: '2026-10-20T08:00:00Z', uploadedBy: 'alice', sha256: 'aaaaaaaaaaaabbbb' },
  { objectVersion: V2, size: 2000, uploadedAt: '2026-10-19T09:30:00Z', uploadedBy: 'bob', sha256: 'ccccccccccccdddd' },
]
const SMOKE_PARAMS = {
  parameters: [{ name: 'days', default: '1' }, { name: 'region', default: 'eu' }],
  objectVersion: V3,
}
const EMPTY_SENTENCE = 
  'No notebook is published in your groups yet. Publish one from JupyterLab with stardelt.publish("file.ipynb").'

async function open(browser: Browser, {
  notebooks = [ANALYTICS, FINANCE] as unknown[],
  listStatus = 200,
  viewport = { width: 1280, height: 800 },
  theme = 'dark',
}: {
  notebooks?: unknown[]
  listStatus?: number
  viewport?: { width: number; height: number }
  theme?: string
} = {}) {
  const context = await browser.newContext({ viewport })
  await context.addInitScript((t) => {
    try {
      localStorage.setItem('stardelt-nova:time', 'utc')
      localStorage.setItem('stardelt-nova:theme', t)
    } catch { /* ignore */ }
  }, theme)
  const page = await context.newPage()
  const requests: string[] = []
  page.on('request', (request) => {
    const url = new URL(request.url())
    if (url.pathname.startsWith('/api/notebooks')) requests.push(url.pathname + url.search)
  })
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/notebooks') {
      return listStatus === 200
        ? json(route, notebooks)
        : json(route, { error: 'The notebook store is not configured.' }, listStatus)
    }
    const notebookRoute = /^\/api\/notebooks\/([^/]+)\/([^/]+)\/(versions|parameters)$/.exec(path)
    if (notebookRoute) {
      const smoke = notebookRoute[1] === 'analytics' && notebookRoute[2] === 'smoke.ipynb'
      if (notebookRoute[3] === 'versions') return json(route, smoke ? SMOKE_VERSIONS : [])
      return json(route, smoke ? SMOKE_PARAMS : { parameters: [], objectVersion: '' })
    }
    return json(route, [])
  })
  await page.goto('/notebooks')
  return { context, page, requests }
}

test.describe('published notebooks', () => {
  test('lists the newest version of each notebook with its group, size and publisher', async ({ browser }) => {
    const { context, page } = await open(browser)
    const section = page.getByRole('region', { name: 'Published notebooks' })
    const smoke = section.getByRole('row', { name: /smoke\.ipynb/ })
    await expect(smoke).toContainText('analytics')
    await expect(smoke).toContainText('4f9c2a1b')
    await expect(smoke).not.toContainText(V3)
    await expect(smoke).toContainText('2.0 KiB')
    await expect(smoke).toContainText('2026-10-20 08:00:00 UTC')
    await expect(smoke).toContainText('alice')
    await expect(section.getByRole('row', { name: /finance\.ipynb/ })).toContainText('bob')
    await context.close()
  })

  test('the group filter narrows the list and offers only the groups of the answer', async ({ browser }) => {
    const { context, page, requests } = await open(browser)
    const section = page.getByRole('region', { name: 'Published notebooks' })
    const filter = section.getByLabel('Group')
    await expect(filter.locator('option')).toHaveText(['All groups', 'analytics', 'finance'])
    await filter.selectOption('finance')
    await expect(section.getByRole('row', { name: /finance\.ipynb/ })).toBeVisible()
    await expect(section.getByRole('row', { name: /smoke\.ipynb/ })).toHaveCount(0)
    expect(requests).toEqual(['/api/notebooks'])
    await context.close()
  })

  test('a person with one group gets no filter', async ({ browser }) => {
    const { context, page } = await open(browser, { notebooks: [ANALYTICS] })
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await expect(section.getByRole('row', { name: /smoke\.ipynb/ })).toBeVisible()
    await expect(section.getByLabel('Group')).toHaveCount(0)
    await context.close()
  })

  test('a row opens the versions and the parameters, and asks for them only then', async ({ browser }) => {
    const { context, page, requests } = await open(browser)
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await expect(section.getByRole('row', { name: /smoke\.ipynb/ })).toBeVisible()
    expect(requests).toEqual(['/api/notebooks'])
    const toggle = section.getByRole('button', { name: 'Versions of smoke.ipynb in analytics' })
    await expect(toggle).toHaveAttribute('aria-expanded', 'false')
    await toggle.click()
    await expect(toggle).toHaveAttribute('aria-expanded', 'true')
    await expect(section.getByText('9b8a7c6d')).toBeVisible()
    await expect(section.getByText('sha256 aaaaaaaaaaaa')).toBeVisible()
    await expect(section.getByText('days')).toBeVisible()
    await expect(section.getByText('region')).toBeVisible()
    expect(requests).toContain('/api/notebooks/analytics/smoke.ipynb/versions')
    expect(requests).toContain('/api/notebooks/analytics/smoke.ipynb/parameters')
    await toggle.click()
    await expect(section.getByText('9b8a7c6d')).toHaveCount(0)
    await context.close()
  })

  test('an empty list says how to publish', async ({ browser }) => {
    const { context, page } = await open(browser, { notebooks: [] })
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await expect(section.getByText(EMPTY_SENTENCE)).toBeVisible()
    await expect(section.getByRole('table')).toHaveCount(0)
    await context.close()
  })

  test('a missing notebook store is stated and the JupyterHub entries stay', async ({ browser }) => {
    const { context, page } = await open(browser, { listStatus: 503 })
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await expect(section.getByRole('alert')).toHaveText('The notebook store is not configured.')
    await expect(section.getByRole('button', { name: 'Retry' })).toHaveCount(0)
    await expect(page.getByRole('button', { name: 'My server' })).toBeVisible()
    await context.close()
  })

  test('a hostile notebook name is shown as text', async ({ browser }) => {
    const evil = '<img src=x onerror="window.__xss=1">'
    const { context, page } = await open(browser, { notebooks: [{ ...ANALYTICS, name: evil }] })
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await expect(section.getByText(evil, { exact: true })).toBeVisible()
    expect(await page.evaluate(() => (window as unknown as { __xss?: number }).__xss)).toBeUndefined()
    await context.close()
  })

  for (const variant of [
    { name: 'desktop-dark', viewport: { width: 1280, height: 800 }, theme: 'dark', keyboard: true },
    { name: 'desktop-light', viewport: { width: 1280, height: 800 }, theme: 'light', keyboard: false },
    { name: 'phone-dark', viewport: { width: 360, height: 740 }, theme: 'dark', keyboard: false },
  ]) {
    test(`the filled page passes the audit (${variant.name})`, async ({ browser }) => {
      const { context, page } = await open(browser, { viewport: variant.viewport, theme: variant.theme })
      const section = page.getByRole('region', { name: 'Published notebooks' })
      await section.getByRole('button', { name: 'Versions of smoke.ipynb in analytics' }).click()
      await expect(section.getByText('region')).toBeVisible()
      await page.waitForTimeout(300)
      const findings = await auditPage(page)
      if (variant.keyboard) findings.push(...(await focusAudit(page)))
      expect(findings).toEqual([])
      await context.close()
    })
  }
})
