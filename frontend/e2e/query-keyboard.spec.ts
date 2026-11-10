import { test, expect } from '@playwright/test'

const RESULT = {
  columns: [{ name: 'x', type: 'integer' }],
  rows: [[1]],
  row_count: 1,
  query_id: 'k1',
}

test('Ctrl+Enter runs the statement in the editor', async ({ page }) => {
  let calls = 0
  await page.route('**/api/query', route => {
    calls += 1
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(RESULT),
    })
  })
  await page.goto('/query')
  const editor = page.getByLabel('SQL query')
  await editor.fill('SELECT 1 AS x')
  await editor.press('Control+Enter')
  await expect(page.locator('tbody tr')).toHaveCount(1)
  await expect(page.locator('tbody tr').first()).toContainText('1')
  expect(calls).toBe(1)
})

test('a second Ctrl+Enter while the statement runs does not send it twice', async ({ page }) => {
  let calls = 0
  await page.route('**/api/query', async route => {
    calls += 1
    await new Promise(resolve => setTimeout(resolve, 750))
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(RESULT),
    })
  })
  await page.goto('/query')
  const editor = page.getByLabel('SQL query')
  await editor.fill('SELECT 1 AS x')
  await editor.press('Control+Enter')
  // Wait for the UI to show the running state before the second press, so the test does not
  // depend on React having flushed the first event's state update.
  await expect(page.getByRole('button', { name: /Running/ })).toBeVisible()
  await editor.press('Control+Enter')
  await expect(page.locator('tbody tr')).toHaveCount(1)
  expect(calls).toBe(1)
})
