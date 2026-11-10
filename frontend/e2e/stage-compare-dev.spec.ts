import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const me = {
  user: 'e2e',
  auth_mode: 'static-dev',
  demo: false,
  warehouse: 'warehouse',
  is_admin: true,
  peers: [{ name: 'prod', url: 'http://127.0.0.1:9' }],
}

const refusal =
  'comparing stages forwards your own token to the peer, so it needs a signed-in caller'

// nova#671: in dev auth mode there is no token to forward, so the peer comparison cannot
// work. The control must still answer with the backend's sentence; before the fix the 401
// went through `checkAuth`, which sent the browser on a sign-in round trip that looked like
// a dead button (the page reloaded and nothing was said).
test('in dev auth mode the compare control shows the reason instead of reloading', async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, me)
    if (path === '/api/stages/compare') return json(r, { error: refusal }, 401)
    if (path === '/api/pipelines') return json(r, { pipelines: [] })
    return json(r, {})
  })

  await page.goto('/deployments')
  await expect(page.getByRole('heading', { name: 'Deployments', exact: true })).toBeVisible()

  await page.getByRole('button', { name: 'prod', exact: true }).click()

  await expect(page.getByRole('alert')).toContainText(refusal)
  await expect(page).toHaveURL(/\/deployments$/)
})
