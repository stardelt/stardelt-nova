import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const me = {
  user: 'e2e',
  auth_mode: 'static-dev',
  demo: false,
  warehouse: 'warehouse',
  is_admin: true,
  peers: [{ name: 'prod', url: 'https://nova.prod.example' }],
}

const comparison = (repository: string) => ({
  peer: 'prod',
  peerUrl: 'https://nova.prod.example',
  status: 'ok',
  here: [
    { bundle: 'shop', target: 'prod', name: 'daily', revision: 'aaaaaaa', repository },
  ],
  there: [{ bundle: 'shop', target: 'prod', name: 'daily', revision: 'bbbbbbb' }],
})

async function openComparison(page: import('@playwright/test').Page, repository: string) {
  await page.route('**/api/**', (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') return json(r, me)
    if (path === '/api/stages/compare') return json(r, comparison(repository))
    if (path === '/api/pipelines') return json(r, { pipelines: [] })
    return json(r, {})
  })
  await page.goto('/deployments')
  await expect(page.getByRole('heading', { name: 'Deployments', exact: true })).toBeVisible()
  await page.getByRole('button', { name: 'prod', exact: true }).click()
}

// nova#676: a repository of just `https://` passed the deploy check, then `new URL('https://')`
// threw while the compare table rendered and the route's ErrorBoundary blanked the whole page.
test('a peer repository without a host does not blank the compare table', async ({ page }) => {
  await openComparison(page, 'https://')

  await expect(page.getByText('different commit')).toBeVisible()
  await expect(page.getByRole('link', { name: 'Promote via pull request' })).toHaveCount(0)
  // The ErrorBoundary replaces the heading, so this proves the page survived the render.
  await expect(page.getByRole('heading', { name: 'Deployments', exact: true })).toBeVisible()
})

test('a well-formed repository still links the forge compare page', async ({ page }) => {
  await openComparison(page, 'https://github.com/stardelt/shop')

  const link = page.getByRole('link', { name: 'Promote via pull request' })
  await expect(link).toHaveAttribute(
    'href',
    'https://github.com/stardelt/shop/compare/bbbbbbb...aaaaaaa',
  )
})
