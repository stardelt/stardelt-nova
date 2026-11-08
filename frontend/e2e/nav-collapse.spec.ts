import { test, expect, type Route } from '@playwright/test'

// The navigation collapses to a 64 px icon rail and remembers that per person
// (stardelt-nova#558). Every `/api/**` call is stubbed, so the test needs no cluster.

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
  const writes: { method: string; path: string; body: string | null }[] = []
  await page.route('**/api/**', (route) => {
    const request = route.request()
    const path = new URL(request.url()).pathname
    if (path === '/api/me') return json(route, ME)
    if (path === '/api/settings') return json(route, {})
    if (path.startsWith('/api/settings/')) {
      writes.push({ method: request.method(), path, body: request.postData() })
      return json(route, { key: path.split('/').pop(), value: null })
    }
    if (path === '/api/catalog/catalogs') return json(route, CATALOGS)
    if (path === '/api/apps/links') return json(route, [])
    return json(route, {})
  })
  return writes
}

/** The rail's width once the transform/transition has settled. */
async function navWidth(page: import('@playwright/test').Page) {
  const box = await page.locator('#primary-nav').boundingBox()
  expect(box).not.toBeNull()
  return box!.width
}

test('at 1280 px the navigation collapses to a 64 px rail and back, remembered after a reload', async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } })
  const page = await context.newPage()
  const writes = await stubApi(page)
  await page.goto('/')

  const nav = page.locator('#primary-nav')
  const label = nav.getByText('SQL editor', { exact: true })

  // Expanded (today): 250 px, labels and group headings visible.
  expect(await navWidth(page)).toBe(250)
  await expect(label).toBeVisible()
  await expect(nav.getByText('Explore', { exact: true })).toBeVisible()
  await expect(nav.getByRole('button', { name: 'Collapse navigation' })).toBeVisible()

  // Collapsed: a 64 px rail. The label leaves the layout but stays the link's accessible
  // name (rule U5), so the existing `getByRole('link', { name })` queries keep working.
  await nav.getByRole('button', { name: 'Collapse navigation' }).click()
  await expect.poll(() => navWidth(page)).toBeGreaterThanOrEqual(63)
  expect(await navWidth(page)).toBeLessThanOrEqual(65)
  // `sr-only` is a 1×1 clipped box: still in the DOM and in the accessible name, not read.
  const clipped = await label.boundingBox()
  expect(clipped!.width).toBeLessThanOrEqual(1)
  expect(clipped!.height).toBeLessThanOrEqual(1)
  const heading = await nav.getByText('Explore', { exact: true }).boundingBox()
  expect(heading!.width).toBeLessThanOrEqual(1)
  await expect(nav.getByRole('link', { name: 'SQL editor' })).toBeVisible()
  await expect(nav.getByRole('link', { name: 'SQL editor' })).toHaveAttribute('title', 'SQL editor')
  // The footer is gone; the version moved into the toggle's title.
  await expect(nav.locator('footer')).toBeHidden()
  await expect(nav.getByRole('button', { name: 'Expand navigation' }))
    .toHaveAttribute('title', /stardelt v/)

  // Per person: the choice is written to the personal settings as well as the browser.
  await expect.poll(() => writes).toContainEqual({
    method: 'PUT', path: '/api/settings/nav', body: JSON.stringify('collapsed'),
  })

  // The link is still usable at rail width: it navigates.
  await nav.getByRole('link', { name: 'SQL editor' }).click()
  await expect(page).toHaveURL(/\/query$/)

  // The browser mirror survives a reload, so the first paint is already the rail.
  await page.reload()
  await expect(page.getByRole('button', { name: 'Expand navigation' })).toBeVisible()
  expect(await navWidth(page)).toBeLessThanOrEqual(65)
  expect((await nav.getByText('SQL editor', { exact: true }).boundingBox())!.width).toBeLessThanOrEqual(1)

  // Back to 250 px and the labels.
  await nav.getByRole('button', { name: 'Expand navigation' }).click()
  await expect.poll(() => navWidth(page)).toBe(250)
  await expect(nav.getByText('SQL editor', { exact: true })).toBeVisible()
  expect((await nav.getByText('SQL editor', { exact: true }).boundingBox())!.width).toBeGreaterThan(10)
  await expect(nav.locator('footer')).toBeVisible()
  await expect.poll(() => writes).toContainEqual({
    method: 'PUT', path: '/api/settings/nav', body: JSON.stringify('expanded'),
  })

  await context.close()
})

test('at 360 px the drawer never collapses and always shows labels', async ({ browser }) => {
  const context = await browser.newContext({ viewport: { width: 360, height: 740 } })
  const page = await context.newPage()
  await stubApi(page)
  await page.goto('/')

  const nav = page.locator('#primary-nav')

  // The toggle is a desktop control: a phone has no room for a rail.
  await expect(page.getByRole('button', { name: 'Collapse navigation' })).toBeHidden()
  await expect(page.getByRole('button', { name: 'Expand navigation' })).toBeHidden()

  await page.getByRole('button', { name: 'Open menu' }).click()
  await expect(page.getByRole('button', { name: 'Close menu' })).toBeVisible()
  // The drawer slides in (`transition-transform`): measure after it has stopped.
  await page.waitForFunction(() => document.getAnimations().every((a) => a.playState !== 'running'))

  await expect(nav).toHaveCSS('width', '250px')
  await expect(nav.getByText('SQL editor', { exact: true })).toBeVisible()
  await expect(nav.getByText('Explore', { exact: true })).toBeVisible()
  await expect(nav.locator('footer')).toBeVisible()

  await context.close()
})
