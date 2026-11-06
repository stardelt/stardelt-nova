import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

test("the page lists a group's service principals and why one is not ready", async ({ page }) => {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') {
      return json(r, {
        user: 'alice',
        auth_mode: 'oidc',
        demo: false,
        warehouse: 'warehouse',
        is_admin: false,
        groups: ['analytics'],
      })
    }
    if (path === '/api/pipelines') {
      return json(r, { pipelines: [] })
    }
    if (path === '/api/service-principals') {
      return json(r, {
        principals: [
          {
            name: 'pipeline-analytics',
            ownerGroup: 'analytics',
            purpose: 'pipeline',
            audiences: ['trino'],
            ready: true,
            message: null,
          },
          {
            name: 'collector-analytics',
            ownerGroup: 'analytics',
            purpose: 'collector',
            audiences: ['trino', 'lakekeeper'],
            ready: false,
            message: 'the client exists, the Secret is not written yet',
          },
        ],
      })
    }
    return json(r, {})
  })

  await page.goto('/pipelines')
  const main = page.locator('main')
  await expect(main.getByRole('heading', { name: 'Service principals' })).toBeVisible()
  const collectorRow = main.getByRole('row', { name: /collector-analytics/ })
  await expect(collectorRow).toContainText('collector')
  await expect(collectorRow).toContainText('trino, lakekeeper')
  await expect(collectorRow).toContainText('Not ready')
  await expect(collectorRow).toContainText('the client exists, the Secret is not written yet')
  const pipelineRow = main.getByRole('row', { name: /pipeline-analytics/ })
  await expect(pipelineRow).toContainText('Ready')
})
