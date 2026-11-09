import { test, expect, type Route } from '@playwright/test'

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

const me = (sub: string) => ({
  user: sub, auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false,
  groups: ['analytics'], sub,
})

test("a second person does not see the first person's cached catalog names", async ({ page }) => {
  let current = 'alice'
  let catalogDelayMs = 0
  await page.route('**/api/**', async (r) => {
    const path = new URL(r.request().url()).pathname
    if (path === '/api/me') {
      // Bob's identity answers late, so the tree has time to mount before anyone knows who he is.
      if (current === 'bob') await new Promise((resolve) => setTimeout(resolve, 800))
      return json(r, me(current))
    }
    if (path === '/api/catalog/catalogs') {
      // Alice's answer lands after `/api/me` (as a real network does), so her names are
      // written into the browser's cache rather than being cleared by the owner check.
      if (catalogDelayMs) await new Promise((resolve) => setTimeout(resolve, catalogDelayMs))
      else if (current === 'alice') await new Promise((resolve) => setTimeout(resolve, 300))
      const name = current === 'alice' ? 'alice-private' : 'bob-team'
      return json(r, { catalogs: [{ name }], default: name })
    }
    if (path === '/api/catalog/namespaces') return json(r, { namespaces: [] })
    return json(r, {})
  })

  // 1. Alice's page writes her names into localStorage.
  await page.goto('/catalog')
  await expect(page.getByRole('button', { name: /alice-private/ })).toBeVisible()

  // 2. Bob loads: /api/me is slow and the catalog list has not answered yet.
  current = 'bob'
  catalogDelayMs = 2000
  await page.reload()
  await page.waitForTimeout(200)
  const tree = page.getByRole('list', { name: 'Catalogs' })
  // A hard check at this instant: `toHaveCount(0)` retries, so it would also pass once
  // Bob's own answer replaces Alice's names at 2000 ms and thus never show the leak.
  const leaked = await tree.getByText('alice-private').count()
  expect(leaked, "Alice's cached catalog names must not paint for Bob").toBe(0)

  // 3. Bob's own answer arrives and replaces nothing of Alice's.
  await expect(page.getByRole('button', { name: /bob-team/ })).toBeVisible()
  await expect(page.getByText('alice-private')).toHaveCount(0)
})
