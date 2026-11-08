import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const me = {
  user: 'alice',
  auth_mode: 'oidc',
  demo: false,
  warehouse: 'warehouse',
  is_admin: true,
  groups: ['admins'],
}

const runningStream = {
  namespace: 'stardelt',
  name: 'demo',
  owner_group: 'admins',
  phase: 'Running',
  reason: null,
  last_error: null,
  last_checkpoint_at: null,
  stopped: false,
  source_connection: 'bus',
  source_topic: 'events',
  sink_catalog: 'warehouse',
  sink_table: 'events',
  sink_mode: 'append',
  run_as: null,
}

test('empty /api/streams shows the empty state and no button', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, me)
    if (path === '/api/streams') return json(r, { streams: [] })
    return json(r, {})
  })
  await page.goto('/streams')
  await expect(page.locator('main').getByText('No streams yet')).toBeVisible()
  await expect(page.locator('main').getByRole('button')).toHaveCount(0)
})

test('Stop sends PATCH and the button becomes Resume after success', async ({ page }) => {
  let stopped = false
  let patchBody: unknown = null
  await page.route('**/api/**', async (r) => {
    const req = r.request()
    const path = new URL(req.url()).pathname
    if (path === '/api/me') return json(r, me)
    if (path === '/api/streams' && req.method() === 'GET') {
      return json(r, { streams: [{ ...runningStream, stopped }] })
    }
    if (path === '/api/streams/stardelt/demo' && req.method() === 'PATCH') {
      patchBody = req.postDataJSON()
      stopped = true
      return json(r, { ...runningStream, stopped: true })
    }
    return json(r, {})
  })

  await page.goto('/streams')
  await page.getByRole('button', { name: 'Stop demo' }).click()

  expect(patchBody).toEqual({ stopped: true })
  await expect(page.getByRole('button', { name: 'Resume demo' })).toBeVisible()
})

test('a 404 shows an alert sentence, keeps the row and re-enables the button', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const req = r.request()
    const path = new URL(req.url()).pathname
    if (path === '/api/me') return json(r, me)
    if (path === '/api/streams' && req.method() === 'GET') {
      return json(r, { streams: [runningStream] })
    }
    if (path === '/api/streams/stardelt/demo' && req.method() === 'PATCH') {
      return json(r, { error: 'Not found.' }, 404)
    }
    return json(r, {})
  })

  await page.goto('/streams')
  await page.getByRole('button', { name: 'Stop demo' }).click()

  await expect(page.getByRole('alert')).toContainText('Not found.')
  await expect(page.getByText('demo', { exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Stop demo' })).toBeEnabled()
})

test('demo mode lists the sample streams with no action and no alert', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, { ...me, demo: true })
    if (path === '/api/streams') {
      return json(r, {
        streams: [
          { ...runningStream, name: 'pegelonline-live', phase: 'Running' },
          { ...runningStream, name: 'orders-cdc', phase: 'Pending' },
        ],
      })
    }
    return json(r, {})
  })
  await page.goto('/streams')
  const main = page.locator('main')
  await expect(main.getByText('pegelonline-live')).toBeVisible()
  await expect(main.getByText('orders-cdc')).toBeVisible()
  await expect(main.getByRole('button')).toHaveCount(0)
  await expect(main.getByRole('alert')).toHaveCount(0)
})

test('a restarting stream shows the Ready reason beside its phase', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, me)
    if (path === '/api/streams') {
      return json(r, { streams: [{ ...runningStream, reason: 'Restarting' }] })
    }
    return json(r, {})
  })
  await page.goto('/streams')
  const main = page.locator('main')
  await expect(main.getByText('Running', { exact: true })).toBeVisible()
  await expect(main.getByText('Restarting', { exact: true })).toBeVisible()
})

test('a stream without a reported checkpoint says so, never a bare dash', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, me)
    if (path === '/api/streams') return json(r, { streams: [runningStream] })
    return json(r, {})
  })
  await page.goto('/streams')
  await expect(page.locator('main').getByText('not reported yet')).toBeVisible()
  await expect(page.locator('main').getByRole('columnheader', { name: 'Checkpoint' })).toHaveAttribute(
    'title',
    'The newest checkpoint commit, as the operator reports it',
  )
})
