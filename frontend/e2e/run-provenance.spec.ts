import { test, expect, type Page, type Route } from '@playwright/test'

// The run page's provenance header (stardelt-nova#749): what ran — the pinned notebook
// version, its stored parameters, when it started, how long it took, what started it and
// as whom. Every `/api/**` call is stubbed; the executed notebook still renders.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
  is_admin: false, groups: ['analytics', 'finance', 'ops'],
}

const PIPELINE = {
  name: 'nightly_upload', namespace: 'stardelt', ownerGroup: 'finance',
  schedule: '0 2 * * *', paused: false, loaded: true, nextRun: null,
  state: { status: 'success', since: '2026-11-09T09:06:00Z' },
  lastRun: null, runAs: 'svc-pipeline-finance', concurrency: 'Allow',
  createdAt: '2026-11-01T09:00:00Z', runtime: 'airflow', env: [],
  sparkConf: {}, idleStopMinutes: 0,
  tasks: [
    {
      name: 'load',
      kind: 'notebook',
      notebook: 'nightly_upload.ipynb',
      objectVersion: '9f2c8a1b4e5d6c7b8a9f0e1d2c3b4a5f',
      notebookParams: { month: '2026-11' },
    },
  ],
  runs: [], params: {}, retries: null,
}

const RUN = {
  runId: 'manual__2', state: 'failed', type: 'manual', logicalDate: null,
  startedAt: '2026-11-09T09:05:00Z', endedAt: '2026-11-09T09:06:00Z',
  durationSeconds: 61, triggeredBy: 'alice', note: null,
}

const NOTEBOOK = {
  nbformat: 4,
  nbformat_minor: 5,
  metadata: {},
  cells: [{ cell_type: 'code', source: 'print("hi")\n', outputs: [], execution_count: 1 }],
}

async function open(page: Page, runState: string = RUN.state) {
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    if (url.pathname === '/api/me') return json(route, ME)
    if (url.pathname === '/api/pipelines/stardelt/nightly_upload') return json(route, PIPELINE)
    if (url.pathname === '/api/pipelines/stardelt/nightly_upload/runs/manual__2') {
      return json(route, { run: { ...RUN, state: runState }, tasks: [] })
    }
    if (url.pathname === '/api/pipelines/stardelt/nightly_upload/runs/manual__2/tasks/load/notebook') {
      return json(route, NOTEBOOK)
    }
    return json(route, [])
  })
  await page.goto('/pipelines/stardelt/nightly_upload/runs/manual__2/notebook/load')
  return page
}

test.describe('run provenance', () => {
  test('the header names the notebook version, who started the run and its principal', async ({ page }) => {
    await open(page)
    const provenance = page.getByTestId('run-provenance')
    await expect(provenance).toContainText('nightly_upload.ipynb')
    await expect(provenance).toContainText('version 9f2c8a1b')
    await expect(provenance).toContainText('started by alice')
    await expect(provenance).toContainText('as svc-pipeline-finance')
    await page.getByText('Parameters (1)').click()
    await expect(page.getByText('month = 2026-11')).toBeVisible()
  })

  test('a failed run offers Retry failed tasks (nova#816)', async ({ page }) => {
    let retried = 0
    await open(page)
    // Registered after open(): Playwright runs the most recently registered route first.
    await page.route('**/api/pipelines/stardelt/nightly_upload/runs/manual__2/retry', (route) => {
      retried += 1
      return json(route, RUN)
    })
    await page.getByRole('button', { name: 'Retry failed tasks' }).click()
    await expect.poll(() => retried).toBe(1)
  })

  test('a successful run offers no Retry failed tasks (nova#816)', async ({ page }) => {
    await open(page, 'success')
    // The controls render with the executed notebook: waiting for "Run again" is what makes
    // the absence below mean something (a bare count of 0 passes before the page has painted).
    await expect(page.getByRole('button', { name: 'Run again' })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Retry failed tasks' })).toHaveCount(0)
  })
})
