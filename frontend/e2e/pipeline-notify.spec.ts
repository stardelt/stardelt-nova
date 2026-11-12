import { test, expect, type Page, type Route } from '@playwright/test'

// "Tell … when a run fails" in the schedule dialog and on the pipeline page (stardelt-nova#815
// slice d): the group's destinations are the checkboxes, the dialog sends `notify.onFailure` in
// every create and update, and the page names the destinations when the list is not empty.

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: '', groups: ['sales'] }

const DESTINATIONS = [
  {
    id: 2,
    owner_group: 'sales',
    name: 'data-team',
    kind: 'email',
    config: { recipients: ['data@example.com'] },
    created_at: 0,
    updated_at: 0,
  },
  {
    id: 1,
    owner_group: 'sales',
    name: 'on-call',
    kind: 'webhook',
    config: { url: 'https://hooks.example/x' },
    created_at: 0,
    updated_at: 0,
  },
]

/** A group's destination that belongs to somebody else; it must never appear in the dialog. */
const FOREIGN = {
  id: 3,
  owner_group: 'finance',
  name: 'finance-page',
  kind: 'email',
  config: { recipients: ['finance@example.com'] },
  created_at: 0,
  updated_at: 0,
}

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

function detail(notify: unknown) {
  return {
    name: 'training',
    namespace: 'stardelt',
    ownerGroup: 'sales',
    canRun: true,
    canEdit: true,
    schedule: '0 2 * * * (Europe/Berlin)',
    paused: false,
    loaded: true,
    nextRun: null,
    state: 'idle',
    lastRun: null,
    tasks: [{ name: 'load', kind: 'sql', sql: 'select 1' }],
    runAs: 'sp-sales',
    concurrency: 'Forbid',
    createdAt: '2026-11-01T09:00:00Z',
    options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
    runs: [],
    params: {},
    retries: null,
    notify,
  }
}

async function open(page: Page, pipeline: ReturnType<typeof detail>, destinations: unknown) {
  let putBody: Record<string, unknown> | null = null
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    const method = route.request().method()
    if (url.pathname === '/api/me') return json(route, ME)
    if (url.pathname === '/api/destinations' && method === 'GET') return json(route, destinations)
    if (url.pathname === '/api/pipelines/stardelt/training') {
      if (method === 'PUT') {
        putBody = JSON.parse(route.request().postData() ?? '{}') as Record<string, unknown>
        return json(route, pipeline)
      }
      return json(route, pipeline)
    }
    return json(route, [])
  })
  await page.goto('/pipelines/stardelt/training')
  return { puts: () => putBody as Record<string, unknown> | null }
}

test('a ticked destination is sent as notify.onFailure with the rest of the schedule', async ({ page }) => {
  const { puts } = await open(page, detail({ onFailure: [] }), [...DESTINATIONS, FOREIGN])

  await page.getByRole('button', { name: 'Edit schedule' }).click()
  const dialog = page.getByRole('dialog', { name: 'Edit the schedule' })
  await expect(dialog).toBeVisible()
  // Native `<details>`, closed by default: the destinations are not on screen yet.
  await expect(dialog.getByRole('checkbox')).toHaveCount(0)
  await dialog.getByText('Advanced', { exact: true }).click()

  // Only the pipeline's own group is listed.
  await expect(dialog.getByLabel('on-call (webhook)')).toBeVisible()
  await expect(dialog.getByLabel('data-team (email)')).toBeVisible()
  await expect(dialog.getByText('finance-page')).toHaveCount(0)

  await dialog.getByLabel('on-call (webhook)').check()
  await dialog.getByRole('button', { name: 'Save schedule' }).click()

  await expect.poll(() => puts()).not.toBeNull()
  expect(puts()).toEqual({
    sql: 'select 1',
    schedule: { cron: '0 2 * * *', timezone: 'Europe/Berlin' },
    params: {},
    options: { retries: 1, backoffMinutes: 5, timeoutMinutes: 120, concurrency: 'Forbid' },
    notify: { onFailure: ['on-call'] },
  })
})

test('a stored notification starts ticked and the page names it', async ({ page }) => {
  await open(page, detail({ onFailure: ['on-call'] }), DESTINATIONS)

  await expect(page.getByText('Tells on failure')).toBeVisible()
  await expect(page.getByText('on-call', { exact: true })).toBeVisible()

  await page.getByRole('button', { name: 'Edit schedule' }).click()
  const dialog = page.getByRole('dialog', { name: 'Edit the schedule' })
  await dialog.getByText('Advanced', { exact: true }).click()
  await expect(dialog.getByLabel('on-call (webhook)')).toBeChecked()
  await expect(dialog.getByLabel('data-team (email)')).not.toBeChecked()
})

test('an empty group says so and links to the Destinations page', async ({ page }) => {
  await open(page, detail({ onFailure: [] }), [])

  await page.getByRole('button', { name: 'Edit schedule' }).click()
  const dialog = page.getByRole('dialog', { name: 'Edit the schedule' })
  await dialog.getByText('Advanced', { exact: true }).click()
  await expect(dialog.getByText('No destination in this group yet.')).toBeVisible()
  await expect(dialog.getByRole('link', { name: 'Add one' })).toHaveAttribute('href', '/destinations')
})
