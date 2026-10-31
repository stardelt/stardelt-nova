import { test, expect } from '@playwright/test'

// A result can hold 50,000 rows; the table draws the first 1,000 and says so (the CSV holds them all).
test('a result of 3,000 rows draws the first 1,000 and says so', async ({ page }) => {
  const rows = Array.from({ length: 3000 }, (_, i) => [i])
  await page.route('**/api/query', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ columns: [{ name: 'x', type: 'integer' }], rows, row_count: rows.length, query_id: 'q1' }),
    }),
  )
  await page.goto('/query')
  await page.getByRole('button', { name: /^Run/ }).click()
  await expect(page.locator('tbody tr')).toHaveCount(1000)
  await expect(page.getByText('The table shows the first 1,000 of 3,000 rows; the CSV holds all of them.')).toBeVisible()
})

test('a result of 1,000 rows or fewer is drawn whole and has no such note', async ({ page }) => {
  const rows = Array.from({ length: 1000 }, (_, i) => [i])
  await page.route('**/api/query', (route) =>
    route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ columns: [{ name: 'x', type: 'integer' }], rows, row_count: rows.length, query_id: 'q2' }),
    }),
  )
  await page.goto('/query')
  await page.getByRole('button', { name: /^Run/ }).click()
  await expect(page.locator('tbody tr')).toHaveCount(1000)
  await expect(page.getByText(/The table shows the first/)).toHaveCount(0)
})
