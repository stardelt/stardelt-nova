import { test, expect, type Route } from '@playwright/test'

// The ⌘K palette finds saved queries and opens one in the SQL editor
// (stardelt-nova#607, slice b2 of #509). Every `/api/**` call is stubbed, so the
// test needs no cluster.

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

const SAVED = [
  { id: 1, title: 'trip revenue', sql: 'SELECT 1', warehouse: 'warehouse', created_at: 0, updated_at: 0 },
]

/** Every API call the shell and the SQL editor make, plus the saved queries the palette reads. */
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
    if (path === '/api/saved-queries') return json(route, SAVED)
    return json(route, {})
  })
}

test('the palette finds a saved query and opens it in the SQL editor', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const dialog = page.getByRole('dialog')
  await expect(dialog).toBeVisible()
  const input = page.getByRole('combobox', { name: 'Search pages, queries and tables' })

  // An empty query shows no saved-query rows, even though the list is loaded.
  await expect(dialog.getByText('Saved queries')).toHaveCount(0)
  await expect(dialog.getByRole('option', { name: /trip revenue/ })).toHaveCount(0)

  // A part of the title matches.
  await input.fill('trip')
  await expect(dialog.getByText('Saved queries')).toBeVisible()
  await expect(dialog.getByRole('option', { name: /trip revenue/ })).toBeVisible()

  // A part of the SQL matches too.
  await input.fill('select')
  await expect(dialog.getByRole('option', { name: /trip revenue/ })).toBeVisible()

  await input.fill('trip')
  await dialog.getByRole('option', { name: /trip revenue/ }).click()
  await expect(page).toHaveURL(/\/query\?sql=SELECT%201$/)
  await expect(page.getByRole('textbox', { name: 'SQL query' })).toHaveValue('SELECT 1')
  await expect(dialog).toHaveCount(0)
})
