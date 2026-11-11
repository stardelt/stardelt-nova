import { test, expect, type Route } from '@playwright/test'

// The ⌘K palette finds tables and opens them in the catalog explorer
// (stardelt-nova#593, slice b1 of #509). Every `/api/**` call is stubbed, so the
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

/** Every API call the shell makes, plus the catalog search the palette sends. */
async function stubApi(page: import('@playwright/test').Page) {
  await page.route('**/api/**', (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/settings') return json(route, {})
    if (path.startsWith('/api/settings/')) return json(route, { key: path.split('/').pop(), value: null })
    if (path === '/api/catalog/catalogs') return json(route, CATALOGS)
    if (path === '/api/apps/links') return json(route, [])
    if (path === '/api/saved-queries') return json(route, [])
    if (path === '/api/catalog/search')
      return json(route, {
        query: 'trip',
        matches: [{ catalog: 'lakehouse', namespace: 'silver', table: 'trips' }],
        truncated: false,
      })
    return json(route, {})
  })
}

test('the palette finds a table and opens it in the catalog explorer', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const dialog = page.getByRole('dialog')
  await expect(dialog).toBeVisible()
  const input = page.getByRole('combobox', { name: 'Search pages, catalogs, schemas, tables and queries' })
  await input.fill('trip')
  await expect(dialog.getByText('Tables')).toBeVisible()
  await dialog.getByRole('option', { name: /silver\.trips/ }).click()
  await expect(page).toHaveURL(/\/catalog\?catalog=lakehouse&ns=silver&tbl=trips$/)
  await expect(dialog).toHaveCount(0)
})

test('the keyboard reaches a table result', async ({ page }) => {
  await stubApi(page)
  await page.goto('/')
  await expect(page).toHaveTitle('Overview · stardelt Nova')
  await page.keyboard.press('Control+k')
  const input = page.getByRole('combobox', { name: 'Search pages, catalogs, schemas, tables and queries' })
  await input.fill('trip')
  await expect(page.getByRole('option', { name: /silver\.trips/ })).toBeVisible()
  await page.keyboard.press('Enter')
  await expect(page).toHaveURL(/\/catalog\?catalog=lakehouse&ns=silver&tbl=trips$/)
})
