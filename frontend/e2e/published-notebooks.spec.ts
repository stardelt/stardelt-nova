import { test, expect, type Browser, type Route } from '@playwright/test'
import { auditPage, focusAudit } from './audit'

// The "Published notebooks" section of the Notebooks page (stardelt-nova#438): the newest version
// of every notebook the caller's groups have published, a group filter, and a row that opens the
// versions and the parameters; and (stardelt-nova#439) who may schedule a notebook and as whom it
// runs: the group's principal, a Schedule button per row. Every `/api/**` call is stubbed.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
  is_admin: false, groups: ['analytics', 'finance', 'ops'],
}
const JUPYTER = {
  id: 'jupyterhub', name: 'JupyterHub', external_url: 'https://jupyter.example.test',
  status: 'up', embed: {},
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
const OPS = {
  group: 'ops', name: 'ops.ipynb', objectVersion: V2, size: 512,
  uploadedAt: '2026-10-19T09:30:00Z', uploadedBy: 'carol',
}
const READY = { group: 'analytics', principal: 'pipeline-analytics', ready: true, message: null }
const READY_OPS = { group: 'ops', principal: 'pipeline-ops', ready: true, message: null }
const NOT_READY = {
  group: 'finance', principal: null, ready: false,
  message: 'Group finance has no pipeline principal yet. Ask an administrator to create it.',
}
const SMOKE_VERSIONS = [
  { objectVersion: V3, size: 2048, uploadedAt: '2026-10-20T08:00:00Z', uploadedBy: 'alice', sha256: 'aaaaaaaaaaaabbbb' },
  { objectVersion: V2, size: 2000, uploadedAt: '2026-10-19T09:30:00Z', uploadedBy: 'bob', sha256: 'ccccccccccccdddd' },
]
const SMOKE_PARAMS = {
  parameters: [{ name: 'days', default: '1' }, { name: 'region', default: 'eu' }],
  objectVersion: V3,
}
const SMOKE_USAGE = {
  schedules: [
    { namespace: 'stardelt', name: 'smoke-daily', ownerGroup: 'analytics',
      objectVersion: V3, cron: '0 3 * * *', timezone: 'UTC', runAs: 'pipeline-analytics' },
  ],
}
const OPS_VERSIONS = [
  { objectVersion: V2, size: 512, uploadedAt: '2026-10-19T09:30:00Z', uploadedBy: 'carol', sha256: 'eeeeeeeeeeeeffff' },
]
const OPS_PARAMS = { parameters: [{ name: 'limit', default: '10' }], objectVersion: V2 }
/** The stored bytes of each version, by object version: what the page's preview renders (#684). */
const codeCell = (source: string) => ({
  cell_type: 'code', metadata: {}, source: [source], outputs: [], execution_count: null,
})
const SMOKE_CONTENT: Record<string, unknown> = {
  [V3]: { cells: [codeCell("print('newest')")], metadata: {} },
  [V2]: { cells: [codeCell("print('older')")], metadata: {} },
}
const OPS_CONTENT: Record<string, unknown> = {
  [V2]: { cells: [codeCell("print('ops')")], metadata: {} },
}
/** What the notebook routes answer, by `group/name`. */
const NOTEBOOK_ROUTES: Record<
  string,
  { versions: unknown[]; parameters: unknown; content: Record<string, unknown>; newest: string }
> = {
  'analytics/smoke.ipynb': {
    versions: SMOKE_VERSIONS, parameters: SMOKE_PARAMS, content: SMOKE_CONTENT, newest: V3,
  },
  'ops/ops.ipynb': {
    versions: OPS_VERSIONS, parameters: OPS_PARAMS, content: OPS_CONTENT, newest: V2,
  },
}
const EMPTY_SENTENCE =
  'No notebook is published in your groups yet. Publish one from JupyterLab with stardelt.publish("file.ipynb").'

async function open(browser: Browser, {
  notebooks = [ANALYTICS, FINANCE] as unknown[],
  listStatus = 200,
  principals = [READY, NOT_READY, READY_OPS] as unknown[],
  principalsStatus = 200,
  viewport = { width: 1280, height: 800 },
  theme = 'dark',
}: {
  notebooks?: unknown[]
  listStatus?: number
  principals?: unknown[]
  principalsStatus?: number
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
  const posted: Array<Record<string, unknown>> = []
  page.on('request', (request) => {
    const url = new URL(request.url())
    if (url.pathname.startsWith('/api/notebooks')) requests.push(url.pathname + url.search)
  })
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/embeds') return json(route, [JUPYTER])
    if (path === '/api/pipelines/principals') {
      return principalsStatus === 200
        ? json(route, { groups: principals })
        : json(route, { error: 'The principals could not be read.' }, principalsStatus)
    }
    if (path === '/api/pipelines' && route.request().method() === 'GET') return json(route, { pipelines: [] })
    if (path === '/api/pipelines' && route.request().method() === 'POST') {
      const body = JSON.parse(route.request().postData() ?? '{}') as Record<string, unknown>
      posted.push(body)
      return json(route, {
        name: body.name, namespace: 'stardelt', ownerGroup: body.ownerGroup, schedule: null, paused: false,
        loaded: false, nextRun: null, state: 'loading', lastRun: null, tasks: [], runAs: null,
        concurrency: 'Forbid', createdAt: null,
      }, 201)
    }
    // The workspace card (#653): this spec is about published notebooks, so it says
    // `unavailable` and the card keeps only the way in.
    if (path === '/api/workspace') {
      return json(route, { state: 'unavailable', startedAt: null, lastActivity: null, idleStopAt: null })
    }
    if (path === '/api/notebooks') {
      return listStatus === 200
        ? json(route, notebooks)
        : json(route, { error: 'The notebook store is not configured.' }, listStatus)
    }
    const usage = path.match(/^\/api\/notebooks\/([^/]+)\/([^/]+)\/usage$/)
    if (usage) return json(route, decodeURIComponent(usage[1]) === 'analytics' ? SMOKE_USAGE : { schedules: [] })
    const notebookRoute = /^\/api\/notebooks\/([^/]+)\/([^/]+)\/(versions|parameters|content)$/.exec(path)
    if (notebookRoute) {
      const known = NOTEBOOK_ROUTES[`${notebookRoute[1]}/${decodeURIComponent(notebookRoute[2])}`]
      if (notebookRoute[3] === 'versions') return json(route, known ? known.versions : [])
      if (notebookRoute[3] === 'content') {
        // The preview of the picked version (#684): the stored bytes of that version.
        const version = url.searchParams.get('version') ?? known?.newest ?? ''
        return json(route, (known?.content ?? {})[version] ?? { cells: [], metadata: {} })
      }
      return json(route, known ? known.parameters : { parameters: [], objectVersion: '' })
    }
    return json(route, [])
  })
  await page.goto('/notebooks')
  return { context, page, requests, posted }
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

  test('the notebook name opens its page with its versions', async ({ browser }) => {
    const { context, page } = await open(browser)
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await section.getByRole('link', { name: 'smoke.ipynb' }).click()
    await expect(page).toHaveURL('/notebooks/analytics/smoke.ipynb')
    await expect(page.getByRole('heading', { level: 1, name: 'smoke.ipynb' })).toBeVisible()
    const table = page.getByRole('table', { name: 'Published versions of smoke.ipynb, newest first.' })
    await expect(table.getByRole('row')).toHaveCount(3)
    await expect(table.getByRole('row', { name: new RegExp(V3.slice(0, 8)) })).toBeVisible()
    await expect(table.getByRole('row', { name: new RegExp(V2.slice(0, 8)) })).toBeVisible()
    await expect(page.getByRole('heading', { name: `Parameters of ${V3.slice(0, 8)}` })).toBeVisible()
    await expect(page.getByText('days = 1')).toBeVisible()
    await expect(page.getByText('region = eu')).toBeVisible()

    const asked = page.waitForRequest((request) =>
      request.url().includes(`/api/notebooks/analytics/smoke.ipynb/parameters?version=${V2}`),
    )
    await table
      .getByRole('row', { name: new RegExp(V2.slice(0, 8)) })
      .getByRole('button', { name: 'Schedule this version' })
      .click()
    await asked
    await expect(page.getByRole('heading', { name: `Parameters of ${V2.slice(0, 8)}` })).toBeVisible()

    await page.goto('/notebooks/analytics/smoke.ipynb')
    await expect(page.getByRole('heading', { level: 1, name: 'smoke.ipynb' })).toBeVisible()
    await expect(page.getByRole('heading', { name: `Parameters of ${V3.slice(0, 8)}` })).toBeVisible()
    await context.close()
  })

  test('the Preview shows the picked version and follows the version picked', async ({ browser }) => {
    const { context, page, requests } = await open(browser)
    await page.goto('/notebooks/analytics/smoke.ipynb')
    // The latest version first, rendered in the sandboxed frame.
    await expect(page.getByRole('heading', { name: `Preview of ${V3.slice(0, 8)}` })).toBeVisible()
    const frame = page.frameLocator('iframe[title="Preview of ' + V3.slice(0, 8) + '"]')
    await expect(frame.locator('pre.src')).toHaveText("print('newest')")
    expect(requests).toContain(`/api/notebooks/analytics/smoke.ipynb/content?version=${V3}`)

    // Picking an older version in the table changes the preview to that version's bytes.
    const table = page.getByRole('table', { name: 'Published versions of smoke.ipynb, newest first.' })
    await table.getByRole('button', { name: V2.slice(0, 8) }).click()
    await expect(page.getByRole('heading', { name: `Preview of ${V2.slice(0, 8)}` })).toBeVisible()
    const older = page.frameLocator('iframe[title="Preview of ' + V2.slice(0, 8) + '"]')
    await expect(older.locator('pre.src')).toHaveText("print('older')")
    expect(requests).toContain(`/api/notebooks/analytics/smoke.ipynb/content?version=${V2}`)
    await context.close()
  })

  test('the notebook page says where the notebook is used', async ({ browser }) => {
    const { context, page } = await open(browser)
    await page.goto('/notebooks/analytics/smoke.ipynb')
    const card = page.locator('section', {
      has: page.getByRole('heading', { name: 'Where it is used' }),
    })
    const row = card.getByRole('row', { name: /smoke-daily/ })
    await expect(row).toContainText('0 3 * * *')
    await expect(row).toContainText('pipeline-analytics')
    await expect(row.getByRole('link', { name: 'smoke-daily' }))
      .toHaveAttribute('href', '/pipelines/stardelt/smoke-daily')

    await page.goto('/notebooks/ops/ops.ipynb')
    await expect(card.getByText('No schedule runs this notebook yet.')).toBeVisible()
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
    await expect(page.getByRole('link', { name: 'My server' })).toBeVisible()
    await expect(page.getByRole('link', { name: 'Open JupyterLab ↗' })).toBeVisible()
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

  test('the Runs as column names the principal of the group and whether it is ready', async ({ browser }) => {
    const { context, page } = await open(browser)
    const section = page.getByRole('region', { name: 'Published notebooks' })
    const smoke = section.getByRole('row', { name: /smoke\.ipynb/ })
    await expect(smoke).toContainText('pipeline-analytics')
    await expect(smoke).toContainText('ready')
    const finance = section.getByRole('row', { name: /finance\.ipynb/ })
    await expect(finance).toContainText('not ready')
    await expect(finance).toContainText(NOT_READY.message)
    await expect(finance).not.toContainText('pipeline-finance')
    await context.close()
  })

  test('the section says who may schedule a notebook and as whom it runs', async ({ browser }) => {
    const { context, page } = await open(browser)
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await expect(section).toContainText('Every member of the owning group may schedule it.')
    await expect(section).toContainText("runs as the group's service principal, not as the person who published the notebook")
    await context.close()
  })

  test('Schedule waits for a ready principal and says why it does not', async ({ browser }) => {
    const { context, page } = await open(browser)
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await expect(section.getByRole('button', { name: 'Schedule smoke.ipynb in analytics' })).toBeEnabled()
    const blocked = section.getByRole('button', { name: 'Schedule finance.ipynb in finance' })
    await expect(blocked).toBeDisabled()
    await expect(blocked).toHaveAccessibleDescription(new RegExp(NOT_READY.message))
    await context.close()
  })

  test('Schedule opens the dialog on that notebook and version and creates it for the group', async ({ browser }) => {
    const { context, page, posted } = await open(browser, { notebooks: [ANALYTICS, OPS] })
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await section.getByRole('button', { name: 'Schedule ops.ipynb in ops' }).click()
    const dialog = page.getByRole('dialog', { name: 'Schedule this notebook' })
    await expect(dialog.getByRole('combobox', { name: 'Group', exact: true })).toHaveValue('ops')
    await expect(dialog.getByRole('combobox', { name: 'Notebook', exact: true })).toHaveValue('ops.ipynb')
    await expect(dialog.getByRole('combobox', { name: 'Version', exact: true })).toHaveValue(V2)
    await dialog.getByRole('button', { name: 'Create schedule' }).click()
    await expect.poll(() => posted.length).toBe(1)
    expect(posted[0].ownerGroup).toBe('ops')
    expect(posted[0].notebook).toMatchObject({ name: 'ops.ipynb', objectVersion: V2 })
    await expect(page.getByRole('dialog')).toHaveCount(0)
    const notice = section.getByRole('status')
    await expect(notice).toContainText('Scheduled')
    await expect(notice.getByRole('link', { name: 'Open it' }))
      .toHaveAttribute('href', new RegExp('^/pipelines/stardelt/'))
    await context.close()
  })

  test('an unreadable principal list keeps the notebooks and blocks Schedule', async ({ browser }) => {
    const { context, page } = await open(browser, { principalsStatus: 503 })
    const section = page.getByRole('region', { name: 'Published notebooks' })
    await expect(section.getByRole('row', { name: /smoke\.ipynb/ })).toContainText('The principals could not be read.')
    await expect(section.getByRole('button', { name: 'Schedule smoke.ipynb in analytics' })).toBeDisabled()
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
