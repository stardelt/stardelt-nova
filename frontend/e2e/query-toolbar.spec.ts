import { test, expect } from '@playwright/test'

test('Run is the first toolbar action, then Sample, then Schedule', async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 800 })
  await page.goto('/query')
  const run = await page.getByRole('button', { name: /^Run/ }).boundingBox()
  const sample = await page.getByRole('button', { name: 'Sample' }).boundingBox()
  const schedule = await page.getByRole('button', { name: /Schedule/ }).boundingBox()
  expect(run).not.toBeNull()
  expect(sample).not.toBeNull()
  expect(schedule).not.toBeNull()
  expect(run!.x).toBeLessThan(sample!.x)
  expect(sample!.x).toBeLessThan(schedule!.x)
})
