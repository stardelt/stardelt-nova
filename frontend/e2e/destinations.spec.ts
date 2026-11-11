import { test, expect, type Route } from '@playwright/test'

// The Destinations page (stardelt-nova#273, UI slice): a group's webhook and email
// endpoints — listed, created, tested and deleted, with the API's own sentence when a
// delivery fails. The API decides which groups the caller sees; the page only renders.

const ROW = {
  id: 1,
  owner_group: 'sales',
  name: 'on-call',
  kind: 'webhook',
  config: { url: 'https://hooks.example/x' },
  created_at: 0,
  updated_at: 0,
}

const ME = { user: 'e2e', auth_mode: 'static-dev', demo: false, warehouse: '', groups: ['sales'] }

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

test('a destination is listed, and a new one is created', async ({ page }) => {
  await page.route('**/api/me', (route) => json(route, ME))
  await page.route('**/api/destinations', (route) => {
    if (route.request().method() === 'POST') {
      expect(route.request().postDataJSON()).toEqual({
        owner_group: 'sales',
        name: 'alerts',
        kind: 'webhook',
        config: { url: 'https://hooks.example/y' },
      })
      return json(route, { ...ROW, id: 2, name: 'alerts', config: { url: 'https://hooks.example/y' } }, 201)
    }
    return json(route, [ROW])
  })

  await page.goto('/destinations')
  await expect(page.getByRole('heading', { name: 'Group: sales' })).toBeVisible()
  await expect(page.getByRole('cell', { name: 'on-call', exact: true })).toBeVisible()
  await expect(page.getByText('https://hooks.example/x')).toBeVisible()

  await page.getByRole('button', { name: 'New destination' }).click()
  await page.getByLabel('Name').fill('alerts')
  await page.getByLabel('URL').fill('https://hooks.example/y')
  await page.getByRole('button', { name: 'Create destination' }).click()
  await expect(page.getByText('Created “alerts”.')).toBeVisible()
})

test('a webhook is tested, and delete asks by name', async ({ page }) => {
  let deletes = 0
  await page.route('**/api/me', (route) => json(route, ME))
  await page.route('**/api/destinations', (route) => json(route, [ROW]))
  await page.route('**/api/destinations/1/test', (route) =>
    json(route, { ok: false, status: 502, detail: 'the webhook answered 502' }),
  )
  await page.route('**/api/destinations/1', (route) => {
    if (route.request().method() !== 'DELETE') return json(route, { error: 'not found' }, 404)
    deletes++
    return route.fulfill({ status: 204, body: '' })
  })

  await page.goto('/destinations')
  await page.getByRole('button', { name: 'Test', exact: true }).click()
  await expect(page.getByText('the webhook answered 502')).toBeVisible()

  await page.getByRole('button', { name: 'Delete “on-call”' }).click()
  await expect(page.getByRole('dialog').getByRole('heading', { name: 'Delete “on-call”?' })).toBeVisible()
  await page.getByRole('dialog').getByRole('button', { name: 'Delete', exact: true }).click()
  await expect(page.getByText('Deleted “on-call”.')).toBeVisible()
  expect(deletes).toBe(1)
})
