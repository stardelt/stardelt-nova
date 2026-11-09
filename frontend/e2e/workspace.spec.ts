import { expect, test, type Route } from '@playwright/test'

// The "My workspace" card on the Notebooks page (stardelt-nova#653): the state of the caller's
// own JupyterHub server with Start, Stop and Open. Every `/api/**` call is stubbed; the real
// backend answers `unavailable` without hub credentials, which is the fourth case below.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse',
  is_admin: false, groups: ['analytics'],
}
const JUPYTER = {
  id: 'jupyterhub', name: 'JupyterHub', external_url: 'https://jupyter.example.test',
  status: 'up', embed: {},
}
const STARTED = '2026-11-09T09:12:00Z'
const inMinute = (minutes: number) => new Date(Date.now() + minutes * 60_000).toISOString()

type Stub = {
  /** What each `GET /api/workspace` answers, in order; the last one repeats. */
  views: unknown[]
  /** What `POST /api/workspace/start` answers, when it should answer at all. */
  startAnswer?: unknown
  startStatus?: number
  /** What `POST /api/workspace/stop` answers. */
  stopAnswer?: unknown
}

async function open(page: import('@playwright/test').Page, stub: Stub) {
  const calls: string[] = []
  let gets = 0
  await page.addInitScript(() => {
    try {
      localStorage.setItem('stardelt-nova:time', 'utc')
    } catch { /* ignore */ }
  })
  await page.route('**/api/**', (route) => {
    const url = new URL(route.request().url())
    const path = url.pathname
    const method = route.request().method()
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/embeds') return json(route, [JUPYTER])
    if (path === '/api/notebooks') return json(route, [])
    if (path === '/api/pipelines/principals') return json(route, { groups: [] })
    if (path === '/api/pipelines') return json(route, { pipelines: [] })
    if (path === '/api/workspace' && method === 'GET') {
      calls.push('GET /api/workspace')
      const view = stub.views[Math.min(gets, stub.views.length - 1)]
      gets += 1
      return json(route, view)
    }
    if (path === '/api/workspace/start' && method === 'POST') {
      calls.push('POST /api/workspace/start')
      // The next GET is the answer of the spawn.
      gets = 0
      stub.views = [stub.startAnswer ?? { state: 'starting', startedAt: null, lastActivity: null, idleStopAt: null }]
      return json(route, stub.views[0], stub.startStatus ?? 202)
    }
    if (path === '/api/workspace/stop' && method === 'POST') {
      calls.push('POST /api/workspace/stop')
      gets = 0
      stub.views = [stub.stopAnswer ?? { state: 'stopped', startedAt: null, lastActivity: null, idleStopAt: null }]
      return json(route, stub.views[0], 202)
    }
    return json(route, [])
  })
  await page.goto('/notebooks')
  return { calls }
}

const card = (page: import('@playwright/test').Page) =>
  page.getByRole('region', { name: 'My workspace' })

test('a running workspace shows its idle stop and start time, with Open and Stop', async ({ page }) => {
  await open(page, {
    views: [{ state: 'running', startedAt: STARTED, lastActivity: new Date().toISOString(), idleStopAt: inMinute(47) }],
  })
  const workspace = card(page)
  await expect(workspace).toContainText('running')
  await expect(workspace).toContainText('idle stop in 47 min')
  await expect(workspace).toContainText('started 09:12')
  await expect(workspace.getByRole('link', { name: 'Open JupyterLab ↗' }))
    .toHaveAttribute('href', 'https://jupyter.example.test/hub/')
  await expect(workspace.getByRole('button', { name: 'Stop' })).toBeEnabled()
  await expect(workspace.getByRole('button', { name: 'Start' })).toHaveCount(0)
})

test('a stopped workspace offers Start, and Start shows the spawn', async ({ page }) => {
  const { calls } = await open(page, {
    views: [{ state: 'stopped', startedAt: null, lastActivity: null, idleStopAt: null }],
  })
  const workspace = card(page)
  await expect(workspace).toContainText('stopped')
  await expect(workspace.getByRole('button', { name: 'Stop' })).toHaveCount(0)
  await workspace.getByRole('button', { name: 'Start' }).click()
  expect(calls).toContain('POST /api/workspace/start')
  await expect(workspace).toContainText('starting')
})

test('while starting the card polls every three seconds and then shows the running server', async ({ page }) => {
  await open(page, {
    views: [
      { state: 'starting', startedAt: null, lastActivity: null, idleStopAt: null },
      { state: 'running', startedAt: STARTED, lastActivity: new Date().toISOString(), idleStopAt: inMinute(60) },
    ],
  })
  const workspace = card(page)
  await expect(workspace).toContainText('starting')
  // The 3 s poll (refetchInterval) is what turns a spawn into a running server.
  await expect(workspace).toContainText('running', { timeout: 6_000 })
  await expect(workspace.getByRole('button', { name: 'Stop' })).toBeEnabled()
})

test('Stop returns a running workspace to stopped', async ({ page }) => {
  const { calls } = await open(page, {
    views: [{ state: 'running', startedAt: STARTED, lastActivity: new Date().toISOString(), idleStopAt: inMinute(47) }],
  })
  const workspace = card(page)
  await workspace.getByRole('button', { name: 'Stop' }).click()
  expect(calls).toContain('POST /api/workspace/stop')
  await expect(workspace).toContainText('stopped')
})

test('an installation without hub credentials keeps only the way in', async ({ page }) => {
  await open(page, {
    views: [{ state: 'unavailable', startedAt: null, lastActivity: null, idleStopAt: null }],
  })
  const workspace = card(page)
  await expect(workspace.getByRole('link', { name: 'Open JupyterLab ↗' })).toBeVisible()
  await expect(workspace.getByRole('button', { name: 'Start' })).toHaveCount(0)
  await expect(workspace.getByRole('button', { name: 'Stop' })).toHaveCount(0)
  await expect(workspace).not.toContainText('running')
})

test('a refused Start says why in the card', async ({ page }) => {
  await open(page, {
    views: [{ state: 'stopped', startedAt: null, lastActivity: null, idleStopAt: null }],
    startStatus: 409,
    startAnswer: { error: 'Open JupyterLab once to create your workspace.' },
  })
  const workspace = card(page)
  await workspace.getByRole('button', { name: 'Start' }).click()
  await expect(workspace.getByRole('alert')).toHaveText('Open JupyterLab once to create your workspace.')
})
