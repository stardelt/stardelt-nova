import { test, expect, type Route } from '@playwright/test'

// The saved queries in the SQL editor (stardelt-nova#582): a named statement is opened
// with a click, deleted only after a confirmation that names it, and the panel says what
// the API said when there is no state store.

const ROWS = [
  { id: 1, title: 'Daily trips', sql: 'select 1', warehouse: '', created_at: 0, updated_at: 0 },
  { id: 2, title: 'Nightly rollup', sql: 'select 2', warehouse: '', created_at: 0, updated_at: 0 },
]

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

test('without a state store the panel shows the API sentence and offers no save', async ({ page }) => {
  await page.route('**/api/saved-queries', (route) =>
    json(route, { error: 'the state store is not configured' }, 503),
  )

  await page.goto('/query')
  await expect(page.getByText('the state store is not configured')).toBeVisible()
  await expect(page.getByRole('button', { name: 'Save query' })).toBeDisabled()
})
