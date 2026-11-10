import { test, expect, type Route } from '@playwright/test'

// stardelt-nova#714: the account menu's Sign out never submitted. Closing the menu set state,
// React removed the form during the click, and the browser never ran the form's default action.
// The menu must not unmount itself before the POST has left the browser.

const json = (route: Route, body: unknown, status = 200) =>
  route.fulfill({ status, contentType: 'application/json', body: JSON.stringify(body) })

test('Sign out posts the form to /api/auth/logout', async ({ page }) => {
  let posted = false
  await page.route('**/api/**', (route) => {
    const path = new URL(route.request().url()).pathname
    if (path === '/api/auth/logout') {
      posted = route.request().method() === 'POST'
      return route.fulfill({ status: 200, contentType: 'text/plain', body: 'signed out' })
    }
    if (path === '/api/me')
      return json(route, { user: 'alice', auth_mode: 'oidc', demo: false, warehouse: 'warehouse', is_admin: false, groups: ['analysts'] })
    if (path === '/api/settings') return json(route, {})
    return json(route, [])
  })
  await page.goto('/')
  await page.getByRole('button', { name: /Account menu/i }).click()
  await page.getByRole('menuitem', { name: /Sign out/ }).click()
  // Fails before the fix: the click closed the menu and the form never submitted.
  await expect.poll(() => posted).toBe(true)
})
