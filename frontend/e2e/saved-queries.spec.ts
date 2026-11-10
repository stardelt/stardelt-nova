import { test, expect, type Route } from '@playwright/test'

// The saved queries in the SQL editor (stardelt-nova#582): a named statement is opened
// with a click, deleted only after a confirmation that names it, and the panel says what
// the API said when there is no state store. A query can be shared with a group
// (stardelt-nova#739): the owner sees Share and Unshare, a row shared with the caller is
// read-only.

const ROWS = [
  { id: 1, title: 'Daily trips', sql: 'select 1', warehouse: '', created_at: 0, updated_at: 0, owner_sub: 'e2e', shared_with: [] },
  { id: 2, title: 'Nightly rollup', sql: 'select 2', warehouse: '', created_at: 0, updated_at: 0, owner_sub: 'e2e', shared_with: [] },
]

// The dev server answers `/api/me` without `groups`; the share dialog needs some.
const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: '', groups: ['sales', 'admins'] }

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

test('a saved query opens in the editor, and delete asks by name', async ({ page }) => {
  let deletes = 0
  await page.route('**/api/saved-queries', (route) => json(route, ROWS))
  await page.route('**/api/saved-queries/*', (route) => {
    if (route.request().method() !== 'DELETE') return json(route, { error: 'not found' }, 404)
    deletes++
    return route.fulfill({ status: 204, body: '' })
  })

  await page.goto('/query')
  await expect(page.getByRole('button', { name: 'Daily trips', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Nightly rollup', exact: true })).toBeVisible()

  await page.getByRole('button', { name: 'Daily trips', exact: true }).click()
  await expect(page.getByLabel('SQL query')).toHaveValue('select 1')

  await page.getByRole('button', { name: 'Delete “Daily trips”' }).click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: 'Delete “Daily trips”?' })).toBeVisible()
  await page.keyboard.press('Escape')
  await expect(page.getByRole('dialog')).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Daily trips', exact: true })).toBeVisible()

  await page.getByRole('button', { name: 'Delete “Daily trips”' }).click()
  await page.getByRole('dialog').getByRole('button', { name: 'Delete', exact: true }).click()
  await expect(page.getByText('Deleted “Daily trips”.')).toBeVisible()
  expect(deletes).toBe(1)
})

test('my own saved query is shared with a group from the pane', async ({ page }) => {
  await page.route('**/api/me', (route) => json(route, ME))
  await page.route('**/api/saved-queries', (route) => json(route, [ROWS[0]]))
  await page.route('**/api/saved-queries/*/share', async (route) => {
    expect(route.request().method()).toBe('POST')
    expect(route.request().postDataJSON()).toEqual({ group: 'sales' })
    await route.fulfill({ status: 204, body: '' })
  })

  await page.goto('/query')
  await page.getByRole('button', { name: 'Share', exact: true }).click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: 'Share “Daily trips”' })).toBeVisible()
  await page.getByLabel('Share with group').selectOption('sales')
  await page.getByRole('dialog').getByRole('button', { name: 'Share', exact: true }).click()
  await expect(page.getByText('Shared “Daily trips” with sales.')).toBeVisible()
})

test("somebody else's saved query is read-only", async ({ page }) => {
  await page.route('**/api/me', (route) => json(route, ME))
  await page.route('**/api/saved-queries', (route) =>
    json(route, [
      { ...ROWS[0], owner_sub: 'someone-else', shared_with: ['sales'] },
    ]),
  )

  await page.goto('/query')
  await expect(page.getByText('shared with sales')).toBeVisible()
  await expect(page.getByText('shared with you')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Delete “Daily trips”' })).toHaveCount(0)
})

test('without a state store the panel shows the API sentence and offers no save', async ({ page }) => {
  await page.route('**/api/saved-queries', (route) =>
    json(route, { error: 'the state store is not configured' }, 503),
  )

  await page.goto('/query')
  await expect(page.getByText('the state store is not configured')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Save query' })).toBeDisabled()
})
