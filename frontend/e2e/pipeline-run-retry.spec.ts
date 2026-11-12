import { test, expect, type Page, type Route } from '@playwright/test'

// Retry failed tasks on a failed run from the pipeline page (stardelt-nova#857, nova#816
// decision 1): a SQL pipeline has no notebook page, so the action lives on the run row.
// Every `/api/**` call is stubbed; no cluster is involved.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
  is_admin: false, groups: ['analytics', 'finance', 'ops'],
}

const RUN = {
  runId: 'manual__2', state: 'failed', type: 'manual', logicalDate: null,
  startedAt: '2026-11-09T09:05:00Z', endedAt: '2026-11-09T09:06:00Z',
  durationSeconds: 61, triggeredBy: 'alice', note: null,
}

function pipeline(canRun: boolean) {
  return {
    name: 'nightly_upload', namespace: 'stardelt', ownerGroup: 'finance',
    canRun, canEdit: canRun,
    schedule: '0 2 * * *', paused: false, loaded: true, nextRun: null,
    state: 'succeeded',
    lastRun: null, runAs: 'svc-pipeline-finance', concurrency: 'Allow',
    createdAt: '2026-11-01T09:00:00Z', runtime: 'airflow', env: [],
    sparkConf: {}, idleStopMinutes: 0,
    tasks: [{ name: 'load', kind: 'sql' }],
    runs: [RUN], params: {}, retries: null,
  }
}

async function open(page: Page, canRun: boolean) {
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    if (url.pathname === '/api/me') return json(route, ME)
    if (url.pathname === '/api/pipelines/stardelt/nightly_upload') return json(route, pipeline(canRun))
    return json(route, [])
  })
  await page.goto('/pipelines/stardelt/nightly_upload')
  return page
}

test.describe('a failed run on the pipeline page', () => {
  test('a failed run offers Retry failed tasks', async ({ page }) => {
    let retried = 0
    await open(page, true)
    // Registered after open(): Playwright runs the most recently registered route first.
    await page.route('**/api/pipelines/stardelt/nightly_upload/runs/manual__2/retry', (route) => {
      retried += 1
      return json(route, RUN)
    })
    await page.getByRole('button', { name: 'Retry failed tasks of run manual__2' }).click()
    await expect.poll(() => retried).toBe(1)
    await expect(page.getByText('Retried the failed tasks of run manual__2.')).toBeVisible()
  })

  test('a viewer sees no retry button', async ({ page }) => {
    await open(page, false)
    // The row renders with the table: waiting for its run id is what makes the absence
    // below mean something (a bare count of 0 passes before the page has painted).
    await expect(page.getByRole('button', { name: 'Show tasks of run manual__2' })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Retry failed tasks of run manual__2' })).toHaveCount(0)
  })
})
