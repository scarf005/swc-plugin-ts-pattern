import { expect, test } from '@playwright/test'

test('compares SWC plugin and plain JS columns after textarea edits', async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', (error) => errors.push(error.message))
  page.on('console', (message) => {
    if (message.type() === 'error') errors.push(message.text())
  })

  const appModule = await page.request.get('/src/App.tsx')
  await expect(appModule).toBeOK()
  const appCode = await appModule.text()
  await expect(appCode).toContain('export default App')
  await expect(appCode).not.toContain('ts-pattern-as-is')
  await expect(appCode).not.toContain('ts-pattern-swc-compiled.js')

  const swcRunnerModule = await page.request.get('/src/runners/ts-pattern-swc.tsx')
  await expect(swcRunnerModule).toBeOK()
  const swcRunnerCode = await swcRunnerModule.text()
  await expect(swcRunnerCode).toContain('const _tsPatternData = result.data')
  await expect(swcRunnerCode).toContain('switch (_tsPatternData.type)')
  await expect(swcRunnerCode).not.toContain('match(result).with')

  await page.goto('/', { waitUntil: 'networkidle' })

  await expect(page.getByRole('link', { name: 'GitHub' })).toHaveAttribute('href', 'https://github.com/scarf005/swc-plugin-ts-pattern')
  await expect(page.getByRole('link', { name: 'Playground' })).toHaveAttribute('href', 'https://ts-pattern-swc-plugin.pages.dev/')
  await expect(page.locator('.layout')).toHaveCSS('grid-template-columns', /.+ .+/)

  await expect(page.getByLabel('Operations')).toHaveValue('100,000')
  await expect(page.getByRole('status')).toContainText('Ready')
  await page.getByRole('button', { name: 'Run' }).click()

  await expect(page.getByRole('heading', { name: 'ts-pattern AS-IS' })).not.toBeVisible()
  await expect(page.getByRole('heading', { name: 'ts-pattern with swc-plugin' })).toBeVisible()
  await expect(page.getByRole('heading', { name: 'plain JS with nested switch' })).toBeVisible()
  await expect(page.getByRole('textbox', { name: 'source ts-pattern module compiled by swc-plugin' })).toContainText('export const renderWithTsPatternSwc')
  await expect(page.getByRole('textbox', { name: 'source ts-pattern module compiled by swc-plugin' })).toContainText('const html = match(result)')
  await expect(page.getByRole('textbox', { name: 'source plain switch module code' })).toContainText('export const renderWithPlainSwitch')
  await expect(page.getByRole('textbox', { name: 'source plain switch module code' })).toContainText('switch (result.type)')
  await expect(page.getByRole('textbox', { name: 'source plain switch module code' })).toContainText('const data = result.data')
  await expect(page.getByRole('textbox', { name: 'source plain switch module code' })).toContainText('switch (data.type)')
  await expect(page.locator('.token-keyword')).not.toHaveCount(0)
  await expect(page.locator('.token-jsx')).not.toHaveCount(0)
  await expect(page.locator('.json-comment')).not.toHaveCount(0)
  await expect(page.locator('.json-key')).not.toHaveCount(0)
  await expect(page.locator('.json-string')).not.toHaveCount(0)

  const comparison = page.getByLabel('benchmark comparison')
  await expect(comparison.getByRole('textbox', { name: /Result/ })).toHaveCount(2)
  await expect(page.getByRole('textbox', { name: 'ts-pattern with swc-plugin Result' })).toContainText('Oups! An error occured')
  await expect(page.getByRole('textbox', { name: 'plain JS with nested switch Result' })).toContainText('<img src=')
  await expect(page.getByRole('textbox', { name: 'ts-pattern with swc-plugin Result' })).toHaveCSS('white-space', 'pre-wrap')
  await expect(page.getByRole('textbox', { name: 'ts-pattern with swc-plugin Result' })).toHaveJSProperty('scrollTop', 0)
  await expect(comparison.locator('.json-input.readonly').first()).toHaveJSProperty('clientHeight', await comparison.locator('.json-input.readonly').first().evaluate((element) => element.scrollHeight))
  await expect(comparison.getByText(/\d+\.\d% (?:faster|slower) than plain switch/)).not.toHaveCount(0)
  await expect(comparison.getByText(/ops\/s \(\d+\.\d% (?:faster|slower) than plain switch\)/)).not.toHaveCount(0)

  await page.getByLabel('Operations').fill('12,345')
  await page.getByLabel('Input').fill(`[
    // JSONC input
    { "type": "ok", "data": { "type": "text", "content": "Edited text" } },
    { "type": "error", "error": { "message": "Still hidden by snippet" } },
  ]`)
  await page.getByRole('button', { name: 'Run' }).click()

  await expect(page.getByRole('status')).toContainText('Parsed 2 records')
  await expect(page.getByRole('status')).toContainText('12,345 operations')
  await expect(page.getByRole('textbox', { name: 'ts-pattern with swc-plugin Result' })).toContainText('Oups! An error occured')
  await expect(page.getByRole('textbox', { name: 'plain JS with nested switch Result' })).toContainText('Edited text')
  await expect(comparison.getByText('Throughput')).toHaveCount(2)
  expect(errors).toEqual([])
})
