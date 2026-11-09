import { test, expect, type Route } from '@playwright/test'

// ⌘K / Ctrl+K opens a palette of the pages (stardelt-nova#587, slice a of #509).
// Every `/api/**` call is stubbed, so the test needs no cluster.

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

/** Every API call the shell makes, and the settings writes it sends. */
async function stubApi(page: import('@playwright/test').Page) {
  await page.route('**/api/**', (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/settings') return json(route, {})
    if (path.startsWith('/api/settings/')) return json(route, { key: path.split('/').pop(), value: null })
    if (path === '/api/catalog/catalogs') return json(route, CATALOGS)
    if (path === '/api/apps/links') return json(route, [])
    return json(route, {})
  })
}

test('Ctrl+K opens the palette, filters pages and navigates', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  // The title is set by an effect once `/api/me` resolves: wait so the shell's own
  // keydown listener is attached before the first key press.
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const dialog = page.getByRole('dialog')
  await expect(dialog).toBeVisible()
  const input = page.getByRole('combobox', { name: 'Search pages and tables' })
  await expect(input).toBeFocused()
  await input.fill('streams')
  await expect(dialog.getByRole('option')).toHaveCount(1)
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(/\/streams$/)
  await expect(dialog).toHaveCount(0)

  // The admin filter also drives the search: an admin page is not a hidden result.
  await page.keyboard.press('Control+k')
  await expect(dialog).toBeVisible()
  await input.fill('health')
  await expect(dialog.getByText(/Nothing matches/)).toBeVisible()
  await expect(dialog.getByRole('option')).toHaveCount(0)
})

test('admin pages are absent for a non-admin, and Escape closes', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  // The title is set by an effect once `/api/me` resolves: wait so the shell's own
  // keydown listener is attached before the first key press.
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const dialog = page.getByRole('dialog')
  await expect(dialog).toBeVisible()
  await expect(dialog.getByRole('option', { name: 'Health' })).toHaveCount(0)
  await expect(dialog.getByRole('option', { name: 'Streams' })).toHaveCount(1)
  await page.keyboard.press('Escape')
  await expect(dialog).toHaveCount(0)
})
