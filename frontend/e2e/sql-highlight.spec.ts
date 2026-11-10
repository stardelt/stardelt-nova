import { test, expect } from '@playwright/test'

test('the editor colours keywords, strings and comments while the textarea keeps the text', async ({ page }) => {
  await page.goto('/query')
  const editor = page.getByLabel('SQL query')
  await editor.fill("select 'x' -- note")
  const overlay = page.getByTestId('sql-highlight')
  await expect(overlay).toHaveAttribute('aria-hidden', 'true')
  await expect(overlay.locator('[data-token="keyword"]')).toHaveText(['select'])
  await expect(overlay.locator('[data-token="string"]')).toHaveText(["'x'"])
  await expect(overlay.locator('[data-token="comment"]')).toHaveText(['-- note'])
  await expect(editor).toHaveValue("select 'x' -- note")
  // Typing keeps both layers in step.
  await editor.fill('drop table t;')
  await expect(overlay.locator('[data-token="keyword"]')).toHaveText(['drop', 'table'])
})
