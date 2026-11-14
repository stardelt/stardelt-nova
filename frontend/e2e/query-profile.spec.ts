import { readFileSync } from 'node:fs'
import { test, expect, type Route } from '@playwright/test'

// The Profile group of an expanded query-log row (stardelt-nova#788 S2): the operators that
// used the most CPU with their share of it, and the engine's plan as it indents it. A query
// without a stored profile says so in one calm sentence — red is for errors.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const NOW = Date.UTC(2026, 10, 14, 4, 0, 0)

const LIST = {
  items: [
    {
      id: 'q1',
      state: 'FINISHED',
      user: 'alice',
      sql: 'SELECT mktsegment FROM tpch.sf1.customer',
      createdAt: NOW,
      wallMs: 2000,
      outputRows: 5,
      errorCode: null,
    },
  ],
  nextCursor: null,
}

const DETAIL = {
  id: 'q1',
  state: 'FINISHED',
  user: 'alice',
  source: 'web',
  clientTags: [],
  warehouse: 'warehouse',
  catalog: 'tpch',
  schema: 'sf1',
  queryType: 'SELECT',
  sql: 'SELECT mktsegment FROM tpch.sf1.customer',
  sqlHash: 'h',
  createdAt: NOW,
  startedAt: NOW,
  endedAt: NOW + 2000,
  timing: { queuedMs: 0, planningMs: 10, executionMs: 1990, wallMs: 2000, cpuMs: 2000 },
  data: {
    inputRows: 12000,
    inputBytes: 656,
    outputRows: 5,
    outputBytes: 100,
    writtenRows: 0,
    writtenBytes: 0,
    peakMemoryBytes: 656,
  },
  tablesRead: ['tpch.sf1.customer'],
  tablesWritten: [],
  error: null,
}

const PROFILE = {
  plan: 'Output[columnNames = [mktsegment]]\n└─ TableScan[table = tpch:sf1:customer]\n',
  operators: [
    {
      operatorType: 'TableScanOperator',
      planNodeId: '396',
      stageId: 0,
      pipelineId: 0,
      inputPositions: 12000,
      outputPositions: 12000,
      addInputCpu: '1.00s',
      finishCpu: '0.00ns',
      getOutputCpu: '0.00ns',
      peakTotalMemoryReservation: '656B',
    },
    {
      operatorType: 'AggregationOperator',
      planNodeId: '397',
      stageId: 1,
      pipelineId: 0,
      inputPositions: 12000,
      outputPositions: 5,
      addInputCpu: '1.00s',
      finishCpu: '0.00ns',
      getOutputCpu: '0.00ns',
    },
  ],
  spilledBytes: 0,
  completedSplits: 4,
  inputs: [],
  truncated: false,
}

/** `missingProfile` answers the profile route the way the backend answers a query without one. */
async function stub(page: import('@playwright/test').Page, missingProfile = false, profile: unknown = PROFILE) {
  await page.route('**/api/**', async (r) => {
    const path = new URL(r.request().url()).pathname

    if (path === '/api/me') {
      return json(r, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analytics'] })
    }
    if (path === '/api/query-log') return json(r, LIST)
    if (path === '/api/query-log/q1') return json(r, DETAIL)
    if (path === '/api/query-log/q1/profile') {
      if (missingProfile) {
        return json(
          r,
          { error: 'no profile for this query (it is still running, older than the profile retention, or the engine sent none)' },
          404,
        )
      }
      return json(r, profile)
    }
    return json(r, {})
  })
}

/** The Profile group of the expanded row. */
const profileGroup = (page: import('@playwright/test').Page) =>
  page.locator('#ql-q1 details').filter({ has: page.locator('summary', { hasText: 'Profile' }) })

/** The real Trino 480 completed event the S0 spike captured (#922). */
const REAL_EVENT = JSON.parse(
  readFileSync(new URL('../../backend/src/testdata/query_log/completed_profile.json', import.meta.url), 'utf8'),
)

/** What the backend keeps of that event at `/api/query-log/{id}/profile`. */
const realProfile = {
  plan: REAL_EVENT.metadata.plan ?? null,
  operators: (REAL_EVENT.statistics.operatorSummaries as unknown[]).map((o) =>
    typeof o === 'string' ? JSON.parse(o) : o,
  ),
  spilledBytes: 0,
  completedSplits: REAL_EVENT.statistics.completedSplits,
  inputs: ((REAL_EVENT.ioMetadata?.inputs ?? []) as Record<string, unknown>[]).map((i) => ({
    catalogName: i.catalogName,
    schema: i.schema,
    table: i.table,
    physicalInputBytes: i.physicalInputBytes,
    physicalInputRows: i.physicalInputRows,
  })),
  truncated: false,
}

/** Expand the one row, then open the Profile group. */
async function openProfile(page: import('@playwright/test').Page) {
  await page.getByRole('button', { name: 'Show details' }).click()
  await page.locator('summary').filter({ hasText: 'Profile' }).click()
}

test('the profile names the operators and their share', async ({ page }) => {
  await stub(page)
  await page.goto('/queries')

  await openProfile(page)
  // Plain words, the engine's own type in the tooltip.
  const group = profileGroup(page)
  await expect(group.getByText('Read', { exact: true })).toBeVisible()
  await expect(group.getByText('Aggregate', { exact: true })).toBeVisible()
  await expect(group.getByTitle('TableScanOperator')).toBeVisible()
  await expect(group.getByTitle('AggregationOperator')).toBeVisible()
  // Two operators, 1.00s of CPU each: half the time went into each.
  await expect(page.getByText('50 %').first()).toBeVisible()
})

test('the plan is there, as the engine indents it', async ({ page }) => {
  await stub(page)
  await page.goto('/queries')

  await openProfile(page)
  await page.locator('summary').filter({ hasText: 'Plan' }).click()
  await expect(page.locator('pre').filter({ hasText: 'TableScan[table = tpch:sf1:customer]' })).toBeVisible()
  await expect(page.getByText('└─ TableScan', { exact: false })).toBeVisible()
})

test('a query without a profile says so calmly', async ({ page }) => {
  await stub(page, true)
  await page.goto('/queries')

  await openProfile(page)
  const panel = page.locator('#ql-q1')
  await expect(
    panel.getByText(
      'no profile for this query (it is still running, older than the profile retention, or the engine sent none)',
    ),
  ).toBeVisible()
  await expect(panel.locator('.text-bad')).toHaveCount(0)
})

test('the real engine event reads in plain words', async ({ page }) => {
  await stub(page, false, realProfile)
  await page.goto('/queries')

  await openProfile(page)
  const group = profileGroup(page)
  // Eleven engine types, every one of them in words: no class name reaches the screen.
  await expect(group.getByText(/Operator\b/)).toHaveCount(0)
  await expect(group.getByText('Join', { exact: true })).toBeVisible()
  await expect(group.getByText('Read and filter', { exact: true })).toBeVisible()
  // Every one of the five rows has a share bar.
  await expect(group.locator('li > div.basis-full')).toHaveCount(5)
})

test('an answer from table metadata says so', async ({ page }) => {
  await stub(page, false, {
    ...PROFILE,
    inputs: [{ catalogName: 'iceberg', schema: 's0', table: 'orders', physicalInputBytes: 0, physicalInputRows: 300343 }],
  })
  await page.goto('/queries')

  await openProfile(page)
  await expect(page.getByText('Answered from table metadata')).toBeVisible()
})
