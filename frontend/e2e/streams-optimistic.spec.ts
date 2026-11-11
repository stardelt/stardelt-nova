import { test, expect } from '@playwright/test'

const STREAM = {
  namespace: 'shop',
  name: 'orders',
  owner_group: 'stardelt-users',
  phase: 'Running',
  reason: null,
  last_error: null,
  last_checkpoint_at: null,
  attempts: 1,
  last_submitted_at: null,
  stopped: false,
  source_connection: 'bus',
  source_topic: 'orders',
  sink_catalog: 'bronze',
  sink_table: 'orders',
  sink_mode: 'append',
  run_as: null,
}

test('Stop shows Stopped before the PATCH answers', async ({ page }) => {
  await page.route('**/api/streams', route =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ streams: [STREAM] }) }),
  )
  await page.route('**/api/streams/*/*', async route => {
    await new Promise(resolve => setTimeout(resolve, 3000))
    return route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({ ...STREAM, stopped: true, phase: 'Stopped' }),
    })
  })
  await page.goto('/streams')
  await page.getByRole('button', { name: 'Stop orders' }).click()
  await expect(page.getByText('Stopped', { exact: true })).toBeVisible({ timeout: 1500 })
  await expect(page.getByRole('button', { name: 'Resume orders' })).toBeVisible({ timeout: 1500 })
})

test('a failed PATCH restores Running and the Stop button', async ({ page }) => {
  await page.route('**/api/streams', route =>
    route.fulfill({ status: 200, contentType: 'application/json', body: JSON.stringify({ streams: [STREAM] }) }),
  )
  await page.route('**/api/streams/*/*', async route => {
    await new Promise(resolve => setTimeout(resolve, 1000))
    return route.fulfill({
      status: 500,
      contentType: 'application/json',
      body: JSON.stringify({ error: 'the operator refused the change' }),
    })
  })
  await page.goto('/streams')
  await page.getByRole('button', { name: 'Stop orders' }).click()
  await expect(page.getByText('Stopped', { exact: true })).toBeVisible({ timeout: 1500 })
  await expect(page.getByRole('button', { name: 'Stop orders' })).toBeVisible({ timeout: 5000 })
  await expect(page.getByText('Running', { exact: true })).toBeVisible()
})
