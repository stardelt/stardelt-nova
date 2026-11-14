import { test, expect, type Page, type Route } from '@playwright/test'

// A run page lists the statements the run sent to the engine, slowest first
// (stardelt-nova#788 S5). Every `/api/**` call is stubbed; no cluster is involved.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
  is_admin: false, groups: ['analytics', 'finance', 'ops'],
}

const RUN = {
  runId: 'manual__2', state: 'succeeded', type: 'manual', logicalDate: null,
  startedAt: '2026-11-14T03:00:00Z', endedAt: '2026-11-14T03:05:00Z',
  durationSeconds: 300, triggeredBy: 'alice', note: null,
}

const PIPELINE = {
  name: 'nightly_upload', namespace: 'stardelt', ownerGroup: 'finance',
  canRun: true, canEdit: true,
  schedule: '0 2 * * *', paused: false, loaded: true, nextRun: null,
  state: 'succeeded',
  lastRun: null, runAs: 'svc-pipeline-finance', concurrency: 'Allow',
  createdAt: '2026-11-01T09:00:00Z', runtime: 'airflow', env: [],
  sparkConf: {}, idleStopMinutes: 0,
  tasks: [{ name: 'load', kind: 'sql' }],
  runs: [RUN], params: {}, retries: null,
}

const RUN_DETAIL = {
  run: RUN,
  tasks: [{
    taskId: 'load', state: 'success', tryNumber: 1,
    startedAt: '2026-11-14T03:00:10Z', endedAt: '2026-11-14T03:02:00Z',
    durationSeconds: 110, operator: 'SqlOperator',
  }],
}

const STARTED = Date.UTC(2026, 10, 14, 3, 0, 0)

const QUERIES = {
  queries: [
    {
      id: 'q2', state: 'FINISHED', taskId: 'load', startedAt: STARTED,
      wallMs: 6200, executionMs: 6000, inputBytes: 4096, inputRows: 10,
      sql: 'insert into gold select 1',
    },
    {
      id: 'q1', state: 'FINISHED', taskId: 'load', startedAt: STARTED,
      wallMs: 400, executionMs: 300, inputBytes: 1024, inputRows: 2,
      sql: 'select 1',
    },
  ],
}

async function open(page: Page, queries: unknown) {
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    if (url.pathname === '/api/me') return json(route, ME)
    if (url.pathname === '/api/pipelines/stardelt/nightly_upload') return json(route, PIPELINE)
    if (url.pathname === '/api/pipelines/stardelt/nightly_upload/runs/manual__2') return json(route, RUN_DETAIL)
    if (url.pathname === '/api/pipelines/stardelt/nightly_upload/runs/manual__2/queries') return json(route, queries)
    return json(route, [])
  })
  await page.goto('/pipelines/stardelt/nightly_upload')
  await page.getByRole('button', { name: 'Show tasks of run manual__2' }).click()
  return page
}

test.describe('the statements of a run', () => {
  test('the run page lists them slowest first', async ({ page }) => {
    await open(page, QUERIES)
    await expect(page.getByRole('heading', { name: 'Statements this run sent, slowest first' })).toBeVisible()
    const walls = page.locator('td').filter({ hasText: /^(\d+\.\d s|\d+ ms)$/ })
    await expect(walls).toHaveText(['6.2 s', '400 ms'])
    await expect(page.getByText('insert into gold select 1')).toBeVisible()
  })

  test('an empty run shows no statements section', async ({ page }) => {
    await open(page, { queries: [] })
    // The tasks table renders with the panel: waiting for it makes the absence below mean something.
    await expect(page.getByText('Tasks of run')).toBeVisible()
    await expect(page.getByRole('heading', { name: 'Statements this run sent, slowest first' })).toHaveCount(0)
  })
})
