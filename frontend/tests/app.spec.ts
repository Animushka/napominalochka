import { expect, test, type Locator } from '@playwright/test'
import { MockApp, aliceAccordion, chooseScenario, openAlice, preferencesAccordion } from './fixtures'

test('Done removes only the pending occurrence and retains the recurring reminder', async ({ page }) => {
  const app = new MockApp(); await app.install(page); await app.open(page)
  await page.getByRole('button', { name: 'Mark as done: Read a chapter' }).click()
  await expect(page.getByRole('heading', { name: 'Read a chapter' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Edit reminder: Water the plants' })).toBeVisible()
  expect(app.matching('POST', '/api/pending/21/done')).toHaveLength(1)
  expect(app.matching('DELETE', '/api/reminders/11')).toHaveLength(0)
  await app.assertClean()
})

for (const [duration, label, next] of [
  ['10m', '10 minutes', '2026-10-28T12:20:00'],
  ['1h', '1 hour', '2026-10-28T13:00:00'],
  ['tomorrow', 'Tomorrow at 09:00', '2026-10-29T09:00:00'],
] as const) {
  test(`Snooze ${duration} updates the existing pending occurrence`, async ({ page }) => {
    const app = new MockApp(); await app.install(page); await app.open(page)
    await page.getByRole('button', { name: 'Snooze', exact: true }).click()
    await page.getByRole('button', { name: label, exact: true }).click()
    await expect(page.getByRole('status')).toContainText('Snoozed.')
    expect(app.matching('POST', '/api/pending/21/snooze').map((call) => call.body)).toEqual([{ duration }])
    expect(app.state.pending[0].next_at).toBe(next)
    await expect(page.getByRole('heading', { name: 'Read a chapter' })).toBeVisible()
    expect(app.matching('POST', '/api/reminders')).toHaveLength(0)
    await app.assertClean()
  })
}

test('Create and description-only edit preserve the original recurring schedule', async ({ page }) => {
  const app = new MockApp(); await app.install(page); await app.open(page)
  await page.getByRole('button', { name: 'New reminder', exact: true }).click()
  await page.getByRole('textbox', { name: "What should we remind you about?" }).fill('Book train tickets')
  await page.getByRole('button', { name: 'In an hour', exact: true }).click()
  await page.getByRole('button', { name: 'Save reminder', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Edit reminder: Book train tickets' })).toBeVisible()
  expect(app.matching('POST', '/api/reminders').map((call) => call.body)).toEqual([{ description: 'Book train tickets', at: '2026-10-28T13:00', recurrence: 'once' }])

  await page.getByRole('button', { name: 'Edit reminder: Water the plants' }).click()
  // The recurring schedule advances on the server while the editor holds its original snapshot.
  app.state.reminders.find((reminder) => reminder.id === 11)!.at = '2026-10-30T09:00:00'
  await page.getByRole('textbox', { name: "What should we remind you about?" }).fill('Water all the plants')
  await page.evaluate(() => window.dispatchEvent(new Event('focus')))
  await expect(page.getByRole('textbox', { name: "What should we remind you about?" })).toHaveValue('Water all the plants')
  await page.getByRole('button', { name: 'Save changes', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Edit reminder: Water all the plants' })).toBeVisible()
  expect(app.matching('PUT', '/api/reminders/11').map((call) => call.body)).toEqual([{ description: 'Water all the plants', at: '2026-10-29T09:00', recurrence: 'daily', preserve_schedule: true }])
  expect(app.state.reminders.find((reminder) => reminder.id === 11)!.at).toBe('2026-10-30T09:00:00')
  await app.assertClean()
})

test('A stale HTTP state read cannot resurrect a completed occurrence', async ({ page }) => {
  const app = new MockApp(); await app.install(page); await app.open(page)
  const held = app.hold('GET', '/api/state', app.state)
  await page.evaluate(() => window.dispatchEvent(new Event('focus')))
  await held.started
  await page.getByRole('button', { name: 'Mark as done: Read a chapter' }).click()
  await expect(page.getByRole('heading', { name: 'Read a chapter' })).toHaveCount(0)
  held.release(); await held.delivered
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))))
  await expect(page.getByRole('heading', { name: 'Read a chapter' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Edit reminder: Water the plants' })).toBeVisible()
  await app.assertClean()
})

test('Preferences use the custom Select and persist interval and follow-up changes', async ({ page }) => {
  const app = new MockApp(); await app.install(page); await app.open(page)
  await preferencesAccordion(page).click()
  const select = page.getByRole('button', { name: /Repeat until I tap/ })
  await select.click()
  await expect(page.getByRole('listbox')).toBeVisible()
  await page.getByRole('option', { name: 'Every 30 minutes', exact: true }).click()
  await expect(select).toContainText('Every 30 minutes')
  await expect(page.getByRole('status')).toContainText('Settings saved')
  // React Aria exposes a visually hidden native input; keyboard targets its role,
  // while pointer interaction targets the visible associated label.
  const followup = page.getByRole('switch', { name: 'A follow-up check' })
  await followup.focus(); await followup.press('Space')
  await expect(followup).not.toBeChecked()
  await page.getByText('A follow-up check', { exact: true }).click()
  await expect(followup).toBeChecked()
  expect(app.matching('PATCH', '/api/preferences').map((call) => call.body)).toEqual([{ nag_interval_minutes: 30 }, { confirmation_check_minutes: 0 }, { confirmation_check_minutes: 20 }])
  await app.assertClean()
})

test('RU/EN round trip and Telegram dark theme render in the production bundle', async ({ page }) => {
  const app = new MockApp(); await app.install(page, { dark: true }); await app.open(page)
  await expect(page.locator('html')).toHaveClass(/dark/)
  await page.getByRole('button', { name: 'Russian', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Всё важное — здесь' })).toBeVisible()
  await expect(page.locator('html')).toHaveAttribute('lang', 'ru')
  await page.getByRole('button', { name: 'Английский', exact: true }).click()
  await expect(page.getByRole('heading', { name: 'Your plans, all here' })).toBeVisible()
  await expect(page.locator('html')).toHaveAttribute('lang', 'en')
  await page.evaluate(() => { const w = window as any; w.Telegram.WebApp.colorScheme = 'light'; w.__bridge.event('themeChanged') })
  await expect(page.locator('html')).not.toHaveClass(/dark/)
  await app.assertClean()
})

test('Native MainButton and Back retain one handler and remove it on editor exit', async ({ page }) => {
  const app = new MockApp(); await app.install(page, { native: true }); await app.open(page)
  const counts = () => page.evaluate(() => { const b = (window as any).__bridge; return [b.mainHandlers.size, b.backHandlers.size] })
  await page.getByRole('button', { name: 'New reminder', exact: true }).click()
  await expect.poll(counts).toEqual([1, 1])
  await expect(page.getByRole('button', { name: 'Save reminder', exact: true })).toHaveCount(0)
  const description = page.getByRole('textbox', { name: "What should we remind you about?" })
  await description.fill('First draft'); await description.fill('Final draft')
  await expect.poll(counts).toEqual([1, 1])
  await page.evaluate(() => { (window as any).__bridge.main(); (window as any).__bridge.main() })
  await expect(page.getByRole('button', { name: 'Edit reminder: Final draft' })).toBeVisible()
  expect(app.matching('POST', '/api/reminders')).toHaveLength(1)
  await expect.poll(counts).toEqual([0, 0])
  await page.getByRole('button', { name: 'New reminder', exact: true }).click()
  await expect.poll(counts).toEqual([1, 1])
  await page.evaluate(() => (window as any).__bridge.back())
  await expect(page.getByRole('heading', { name: 'Your plans, all here' })).toBeVisible()
  await expect.poll(counts).toEqual([0, 0])
  await app.assertClean()
})

test('Native Back dismisses the editor Select before leaving its unsaved draft', async ({ page }) => {
  const app = new MockApp(); await app.install(page, { native: true }); await app.open(page)
  const bridgeState = () => page.evaluate(() => {
    const b = (window as any).__bridge
    return { main: b.mainHandlers.size, back: b.backHandlers.size, mainVisible: b.mainVisible, backVisible: b.backVisible }
  })
  await page.getByRole('button', { name: 'Edit reminder: Water the plants' }).click()
  const description = page.getByRole('textbox', { name: 'What should we remind you about?' })
  await description.fill('Keep this unsaved draft')
  const recurrence = page.getByRole('button', { name: /Repeat/ }).and(page.locator('[data-slot="select-trigger"]'))
  await recurrence.click()
  await expect(page.getByRole('listbox')).toBeVisible()
  await expect.poll(bridgeState).toEqual({ main: 1, back: 1, mainVisible: true, backVisible: true })
  await page.evaluate(() => (window as any).__bridge.back())
  await expect(page.getByRole('listbox')).toHaveCount(0)
  await expect(description).toHaveValue('Keep this unsaved draft')
  await expect(page.getByRole('heading', { name: 'Edit reminder', exact: true })).toBeVisible()
  await expect.poll(bridgeState).toEqual({ main: 1, back: 1, mainVisible: true, backVisible: true })
  await page.evaluate(() => (window as any).__bridge.back())
  await expect(page.getByRole('heading', { name: 'Your plans, all here' })).toBeVisible()
  await expect.poll(bridgeState).toEqual({ main: 0, back: 0, mainVisible: false, backVisible: false })
  expect(app.matching('PUT', '/api/reminders/11')).toHaveLength(0)
  await app.assertClean()
})

test('Native Back is shown only while a settings Select is open', async ({ page }) => {
  const app = new MockApp(); await app.install(page, { native: true }); await app.open(page)
  const bridgeState = () => page.evaluate(() => {
    const b = (window as any).__bridge
    return { main: b.mainHandlers.size, back: b.backHandlers.size, mainVisible: b.mainVisible, backVisible: b.backVisible }
  })
  await expect.poll(bridgeState).toEqual({ main: 0, back: 0, mainVisible: false, backVisible: false })
  await preferencesAccordion(page).click()
  const interval = page.getByRole('button', { name: /Repeat until I tap/ })
  await interval.click()
  await expect(page.getByRole('listbox')).toBeVisible()
  await expect.poll(bridgeState).toEqual({ main: 0, back: 1, mainVisible: false, backVisible: true })
  await page.evaluate(() => (window as any).__bridge.back())
  await expect(page.getByRole('listbox')).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'Your plans, all here' })).toBeVisible()
  await expect(preferencesAccordion(page)).toHaveAttribute('aria-expanded', 'true')
  await expect.poll(bridgeState).toEqual({ main: 0, back: 0, mainVisible: false, backVisible: false })
  expect(app.matching('PATCH', '/api/preferences')).toHaveLength(0)
  await interval.click()
  await page.getByRole('option', { name: 'Every 30 minutes', exact: true }).click()
  await expect(interval).toContainText('Every 30 minutes')
  await expect.poll(bridgeState).toEqual({ main: 0, back: 0, mainVisible: false, backVisible: false })
  await app.assertClean()
})

test('Native Back removes editor and overlay registrations when the account changes', async ({ page }) => {
  const app = new MockApp(); await app.install(page, { native: true }); await app.open(page)
  const bridgeState = () => page.evaluate(() => {
    const b = (window as any).__bridge
    return { main: b.mainHandlers.size, back: b.backHandlers.size, mainVisible: b.mainVisible, backVisible: b.backVisible }
  })
  await page.getByRole('button', { name: 'Edit reminder: Water the plants' }).click()
  await page.getByRole('button', { name: /Repeat/ }).and(page.locator('[data-slot="select-trigger"]')).click()
  await expect(page.getByRole('listbox')).toBeVisible()
  await expect.poll(bridgeState).toEqual({ main: 1, back: 1, mainVisible: true, backVisible: true })
  app.state.pending = []
  app.state.reminders = [{ ...app.state.reminders[0], id: 202, description: 'Second account plan' }]
  await page.evaluate(() => {
    const tg = (window as any).Telegram.WebApp
    tg.initData = 'fixture-account-202'; tg.initDataUnsafe.user.id = 202
    window.dispatchEvent(new Event('focus'))
  })
  await expect(page.getByRole('button', { name: 'Edit reminder: Second account plan' })).toBeVisible()
  await expect(page.getByRole('listbox')).toHaveCount(0)
  await expect.poll(bridgeState).toEqual({ main: 0, back: 0, mainVisible: false, backVisible: false })
  await page.getByRole('button', { name: 'Edit reminder: Second account plan' }).click()
  await expect.poll(bridgeState).toEqual({ main: 1, back: 1, mainVisible: true, backVisible: true })
  await page.evaluate(() => (window as any).__bridge.back())
  await expect(page.getByRole('heading', { name: 'Your plans, all here' })).toBeVisible()
  await expect.poll(bridgeState).toEqual({ main: 0, back: 0, mainVisible: false, backVisible: false })
  await app.assertClean()
})

test('Alice connects, saves audio off, tests, and enables only after explicit heard confirmation', async ({ page }) => {
  const app = new MockApp(); await app.install(page); await app.open(page); await openAlice(page)
  await expect(page.getByText('Yandex requests permission to view and control your entire smart home.', { exact: false })).toBeVisible()
  await page.getByRole('button', { name: 'Connect Yandex', exact: true }).click()
  await expect.poll(() => page.evaluate(() => (window as any).__bridge.opened)).toEqual(['https://oauth.yandex.ru/authorize?state=fixture-only'])
  await expect(page.getByText('Finish signing in to Yandex and confirm the connection in Telegram.', { exact: false })).toBeVisible()
  app.yandex.connected = true
  await page.getByRole('button', { name: 'Refresh connection', exact: true }).click()
  await chooseScenario(page)
  expect(app.matching('PUT', '/api/yandex').map((call) => call.body)).toEqual([{ scenario_id: 'speaker-a', enabled: false }])
  await page.getByRole('button', { name: 'Test on your speaker', exact: true }).click()
  await expect(page.getByText('Did the phrase play on the right speaker?', { exact: true })).toBeVisible()
  expect(app.yandex.enabled).toBe(false)
  expect(app.matching('PUT', '/api/yandex').filter((call) => call.body.enabled)).toHaveLength(0)
  await page.getByRole('button', { name: 'Yes, it works — turn on', exact: true }).click()
  await expect(aliceAccordion(page)).toContainText('Spoken reminders are on')
  await expect(page.getByRole('button', { name: /Test again in \d+s/ })).toBeDisabled()
  await page.getByRole('button', { name: 'Turn spoken reminders off', exact: true }).click()
  await expect(aliceAccordion(page)).toContainText('spoken reminders off')
  await page.getByRole('button', { name: 'Turn spoken reminders on', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Yes, it works — turn on', exact: true })).toHaveCount(0)
  expect(app.matching('PUT', '/api/yandex').filter((call) => call.body.enabled)).toHaveLength(1)
  await page.getByRole('button', { name: 'Disconnect Yandex', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Connect Yandex', exact: true })).toBeVisible()
  expect(app.matching('DELETE', '/api/yandex')).toHaveLength(1)
  await expect.poll(() => page.evaluate(() => (window as any).__bridge.confirmations.length)).toBe(1)
  await app.assertClean()
})

test('Alice preserves an unfinished choice on focus and a not-heard answer requires another manual test', async ({ page }) => {
  const app = new MockApp(); app.yandex.connected = true
  await app.install(page); await page.clock.install(); await app.open(page); await openAlice(page)
  const select = page.getByRole('button', { name: /Reminder scenario/ })
  await select.click(); await page.getByRole('option', { name: 'Office reminder', exact: true }).click()
  const baseline = app.matching('GET', '/api/yandex').length
  await page.clock.fastForward(3100)
  await page.evaluate(() => { window.dispatchEvent(new Event('focus')); window.dispatchEvent(new Event('focus')); document.dispatchEvent(new Event('visibilitychange')) })
  await expect.poll(() => app.matching('GET', '/api/yandex').length).toBe(baseline + 1)
  await expect(select).toContainText('Office reminder')
  await expect(page.getByRole('button', { name: 'Next: test it', exact: true })).toBeEnabled()
  await page.getByRole('button', { name: 'Next: test it', exact: true }).click()
  await page.getByRole('button', { name: 'Test on your speaker', exact: true }).click()
  await page.getByRole('button', { name: 'I did not hear it', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Yes, it works — turn on', exact: true })).toHaveCount(0)
  await expect(page.getByRole('button', { name: /Test again in \d+s/ })).toBeDisabled()
  await page.clock.fastForward(61_000)
  await expect(page.getByRole('button', { name: 'Test on your speaker', exact: true })).toBeEnabled()
  expect(app.matching('POST', '/api/yandex/test')).toHaveLength(1)
  expect(app.yandex.enabled).toBe(false)
  await page.getByRole('button', { name: 'Test on your speaker', exact: true }).click()
  await page.getByRole('button', { name: 'Yes, it works — turn on', exact: true }).click()
  await expect(aliceAccordion(page)).toContainText('Spoken reminders are on')
  expect(app.matching('POST', '/api/yandex/test')).toHaveLength(2)
  await app.assertClean()
})

test('Alice refuses OAuth destinations outside the exact credential-free HTTPS host', async ({ page }) => {
  const app = new MockApp(); await app.install(page); await app.open(page); await openAlice(page)
  for (const url of ['https://oauth.yandex.ru.evil.example/authorize', 'http://oauth.yandex.ru/authorize', 'https://fixture-user@oauth.yandex.ru/authorize', 'https://oauth.yandex.ru:444/authorize']) {
    app.once('POST', '/api/yandex/connect', () => ({ body: { url } }))
    await page.getByRole('button', { name: 'Connect Yandex', exact: true }).click()
    await expect(page.getByText('Could not open the Yandex connection.', { exact: false })).toBeVisible()
    await expect(page.getByRole('button', { name: 'Connect Yandex', exact: true })).toBeEnabled()
  }
  expect(await page.evaluate(() => (window as any).__bridge.opened)).toEqual([])
  expect(app.matching('POST', '/api/yandex/connect')).toHaveLength(4)
  await app.assertClean()
})

for (const statusReadFails of [false, true]) {
  test(`Alice failed delivery never retries; reconciliation ${statusReadFails ? 'shows uncertainty when unavailable' : 'shows server disabled/reconnect'}`, async ({ page }) => {
    const app = new MockApp()
    Object.assign(app.yandex, { connected: true, enabled: true, scenario_id: 'speaker-a', scenario_name: 'Living room reminder' })
    await app.install(page); await page.clock.install(); await app.open(page); await openAlice(page)
    const baseline = app.matching('GET', '/api/yandex').length
    app.once('POST', '/api/yandex/test', () => {
      Object.assign(app.yandex, { enabled: false, requires_reconnect: true, last_error: 'yandex_reconnect_required' })
      if (statusReadFails) app.once('GET', '/api/yandex', () => ({ abort: true }))
      return { abort: true }
    })
    await page.getByRole('button', { name: 'Test on your speaker', exact: true }).click()
    await expect(page.getByText('Yandex did not confirm the run.', { exact: false })).toBeVisible()
    await expect(aliceAccordion(page)).not.toContainText('Spoken reminders are on')
    await expect(aliceAccordion(page)).toContainText(statusReadFails ? 'Yandex could not be reached' : 'Please reconnect your account')
    expect(app.matching('GET', '/api/yandex')).toHaveLength(baseline + 1)
    await page.clock.fastForward(61_000)
    expect(app.matching('POST', '/api/yandex/test')).toHaveLength(1)
    expect(app.matching('PUT', '/api/yandex').filter((call) => call.body.enabled)).toHaveLength(0)
    await app.assertClean()
  })
}

test('Alice scenarios failure performs one bounded status reread', async ({ page }) => {
  const app = new MockApp(); app.yandex.connected = true
  app.once('GET', '/api/yandex/scenarios', () => ({ status: 503, body: { code: 'yandex_unavailable' } }))
  await app.install(page); await app.open(page)
  await expect.poll(() => app.matching('GET', '/api/yandex').length).toBe(2)
  expect(app.matching('GET', '/api/yandex/scenarios')).toHaveLength(1)
  await app.assertClean()
})

test('Missing saved and inactive scenarios are visible but cannot be selected or enabled', async ({ page }) => {
  const app = new MockApp()
  Object.assign(app.yandex, { connected: true, scenario_id: 'missing', scenario_name: 'Old unavailable speaker' })
  await app.install(page); await app.open(page); await openAlice(page)
  await expect(page.getByRole('button', { name: 'Turn spoken reminders on', exact: true })).toBeDisabled()
  await expect(page.getByText('This scenario is no longer available.', { exact: false })).toBeVisible()
  await page.getByRole('button', { name: 'Change scenario', exact: true }).click()
  await page.getByRole('button', { name: /Reminder scenario/ }).click()
  await expect(page.getByRole('option', { name: 'Inactive scenario · inactive', exact: true })).toHaveAttribute('aria-disabled', 'true')
  await expect(page.getByRole('option', { name: 'Saved scenario is unavailable', exact: true })).toHaveAttribute('aria-disabled', 'true')
  await page.getByRole('option', { name: 'Office reminder', exact: true }).click()
  await page.getByRole('button', { name: 'Next: test it', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Test on your speaker', exact: true })).toBeEnabled()
  expect(app.matching('PUT', '/api/yandex').map((call) => call.body)).toEqual([{ scenario_id: 'speaker-b', enabled: false }])
  await app.assertClean()
})

test('Switching accounts ignores old state and Alice responses', async ({ page }) => {
  const app = new MockApp(); Object.assign(app.yandex, { connected: true, enabled: true, scenario_id: 'speaker-a', scenario_name: 'Living room reminder' })
  await app.install(page); await app.open(page); await openAlice(page)
  const oldAlice = app.hold('GET', '/api/yandex', app.yandex)
  const oldState = app.hold('GET', '/api/state', app.state)
  await page.getByRole('button', { name: 'Refresh connection', exact: true }).click(); await oldAlice.started
  await page.evaluate(() => window.dispatchEvent(new Event('focus'))); await oldState.started
  app.state.pending = []; app.state.reminders = [{ ...app.state.reminders[0], id: 202, description: 'Second account plan' }]
  Object.assign(app.yandex, { connected: false, enabled: false, scenario_id: null, scenario_name: null })
  await page.evaluate(() => {
    const tg = (window as any).Telegram.WebApp; tg.initData = 'fixture-account-202'; tg.initDataUnsafe.user.id = 202
    window.dispatchEvent(new Event('focus'))
  })
  await expect(page.getByRole('button', { name: 'Edit reminder: Second account plan' })).toBeVisible()
  oldAlice.release(); oldState.release(); await Promise.all([oldAlice.delivered, oldState.delivered])
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))))
  await openAlice(page)
  await expect(page.getByRole('button', { name: 'Connect Yandex', exact: true })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Edit reminder: Water the plants' })).toHaveCount(0)
  await expect(page.getByRole('heading', { name: 'Read a chapter' })).toHaveCount(0)
  await expect(aliceAccordion(page)).not.toContainText('Spoken reminders are on')
  expect(app.calls.some((call) => call.path === '/api/yandex' && call.authorization === 'tma fixture-account-202')).toBe(true)
  await app.assertClean()
})

async function centeredChevron(trigger: Locator, selector: string) {
  await expect(trigger).toBeVisible()
  const result = await trigger.evaluate((element, svgSelector) => {
    const outer = element.getBoundingClientRect()
    const icons = [...element.querySelectorAll(svgSelector)].filter((icon) => {
      const rect = icon.getBoundingClientRect(); return rect.width > 0 && rect.height > 0 && getComputedStyle(icon).visibility !== 'hidden'
    })
    return { count: icons.length, delta: icons.map((icon) => {
      const rect = icon.getBoundingClientRect(); return Math.abs(rect.y + rect.height / 2 - outer.y - outer.height / 2)
    }) }
  }, selector)
  expect(result.count, 'Exactly one visible SVG chevron in the trigger').toBe(1)
  expect(result.delta[0], 'Chevron and trigger vertical centers differ by at most 2 CSS pixels').toBeLessThanOrEqual(2)
}

for (const width of [320, 390, 620]) {
  test(`Stock Select and Accordion SVG chevrons are centered at ${width}px`, async ({ page }) => {
    await page.setViewportSize({ width, height: 900 })
    const app = new MockApp(); app.yandex.connected = true
    await app.install(page); await app.open(page)
    const accordionButtons = page.getByRole('button').filter({ has: page.locator('svg[data-slot="accordion-trigger-icon"]') })
    await expect(accordionButtons).toHaveCount(4)
    for (const button of await accordionButtons.all()) await centeredChevron(button, 'svg[data-slot="accordion-trigger-icon"]')
    await preferencesAccordion(page).click()
    await centeredChevron(preferencesAccordion(page), 'svg[data-slot="accordion-trigger-icon"]')
    const interval = page.getByRole('button', { name: /Repeat until I tap/ })
    await centeredChevron(interval, 'svg')
    await interval.click(); await expect(page.getByRole('listbox')).toBeVisible(); await page.keyboard.press('Escape')
    await openAlice(page)
    await centeredChevron(aliceAccordion(page), 'svg[data-slot="accordion-trigger-icon"]')
    await centeredChevron(page.getByRole('button', { name: /Reminder scenario/ }), 'svg')
    expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true)
    await app.assertClean()
  })
}

test('Unauthenticated launch makes no application API requests', async ({ page }) => {
  const app = new MockApp(); await app.install(page, { authenticated: false }); await page.goto('/')
  await expect(page.getByRole('link', { name: /Open the bot in Telegram/ })).toBeVisible()
  expect(app.calls).toEqual([])
  await app.assertClean()
})
