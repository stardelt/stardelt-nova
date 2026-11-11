import { test, expect, type Route } from '@playwright/test'

// The ⌘K palette offers the five actions: four navigate, and "Switch theme" runs the
// header's own toggle (stardelt-nova#615, slice c of #509). Every `/api/**` call is
// stubbed, so the test needs no cluster.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const ME = {
  user: 'alice',
  auth_mode: 'oidc',
  demo: false,
  warehouse: 'warehouse',
  is_admin: false,
  groups: ['analytics'],
}

const CATALOGS = { catalogs: [{ name: 'lakehouse' }] }

/** Every API call the shell and the SQL editor make, plus the settings writes the theme makes. */
async function stubApi(page: import('@playwright/test').Page) {
  await page.route('**/api/**', (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/settings') return json(route, {})
    if (path.startsWith('/api/settings/')) return json(route, { key: path.split('/').pop(), value: null })
    if (path === '/api/catalog/catalogs') return json(route, CATALOGS)
    if (path === '/api/catalog/namespaces') return json(route, { namespaces: [] })
    if (path === '/api/apps/links') return json(route, [])
    if (path === '/api/saved-queries') return json(route, [])
    return json(route, {})
  })
}

test('an action opens the SQL editor', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  // The title is set by an effect once `/api/me` resolves: wait so the shell's own
  // keydown listener is attached before the first key press.
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const input = page.getByRole('combobox', { name: 'Search pages, catalogs, schemas, tables and queries' })
  await input.fill('new q')
  const action = page.getByRole('dialog').getByRole('option', { name: /New query/ })
  await expect(action).toBeVisible()
  await action.click()
  await expect(page).toHaveURL(/\/query/)
})

test('Switch theme flips the paired theme and closes the palette', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'graphite')
  // The title is set by an effect once `/api/me` resolves: wait so the shell's own
  // keydown listener is attached before the first key press.
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const input = page.getByRole('combobox', { name: 'Search pages, catalogs, schemas, tables and queries' })
  await input.fill('switch')
  await page.getByRole('dialog').getByRole('option', { name: /Switch theme/ }).click()
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'silver')
  await expect(page.getByRole('dialog')).toHaveCount(0)
})
