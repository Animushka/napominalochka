import { expect, test } from '@playwright/test'
import { MockApp } from './fixtures'

const historyButton = (page: import('@playwright/test').Page) => page.getByRole('button', { name: /^Completion history / })

const completion = (id: number) => ({ id, description: `Completed item ${id}`, scheduled_at: '2026-10-28T09:00:00',
  completed_at: '2026-10-28T09:12:00', undone_at: null, can_undo: true })

test('Done records history lazily and undo restores the occurrence without changing its schedule', async ({ page }) => {
  const app = new MockApp(); await app.install(page); await app.open(page)
  expect(app.matching('GET', '/api/history')).toHaveLength(0)
  await page.getByRole('button', { name: 'Mark as done: Read a chapter' }).click()
  await expect(page.getByRole('heading', { name: 'Read a chapter' })).toHaveCount(0)
  expect(app.matching('GET', '/api/history')).toHaveLength(0)
  await page.getByRole('button', { name: 'View or undo completion', exact: true }).click()
  await expect(historyButton(page)).toHaveAttribute('aria-expanded', 'true')
  const entry = page.locator('[data-history-id="1"]')
  await expect(entry).toContainText('Read a chapter')
  await expect(entry).toContainText('Marked done:')
  await entry.getByRole('button', { name: 'Undo completion: Read a chapter' }).click()
  await expect(page.locator('[data-pending-id="1001"]')).toContainText('Read a chapter')
  await expect(entry).toContainText('Mark undone:')
  await expect(entry.getByRole('button', { name: 'Undo completion: Read a chapter' })).toHaveCount(0)
  await expect(page.getByRole('button', { name: 'Edit reminder: Water the plants' })).toBeVisible()
  expect(app.matching('POST', '/api/history/1/undo')).toHaveLength(1)
  expect(app.matching('POST', '/api/yandex/test')).toHaveLength(0)
  expect(app.state.reminders[0].at).toBe('2026-10-29T09:00:00')
  await app.assertClean()
})

test('History loads cursor pages and a stale undo failure refreshes availability', async ({ page }) => {
  const app = new MockApp(); app.history = Array.from({ length: 25 }, (_, index) => completion(25 - index))
  await app.install(page); await app.open(page); await historyButton(page).click()
  await expect(page.locator('[data-history-id]')).toHaveCount(20)
  await page.getByRole('button', { name: 'Show more', exact: true }).click()
  await expect(page.locator('[data-history-id]')).toHaveCount(25)
  await expect(page.getByRole('button', { name: 'Show more', exact: true })).toHaveCount(0)
  expect(app.matching('GET', '/api/history').some((call) => call.query?.includes('before=6'))).toBe(true)
  app.once('POST', '/api/history/25/undo', () => {
    app.history[0].can_undo = false
    return { status: 409, body: { code: 'completion_unavailable' } }
  })
  await page.getByRole('button', { name: 'Undo completion: Completed item 25', exact: true }).click()
  await expect(page.getByRole('status')).toContainText('This completion can no longer be undone.')
  await expect(page.locator('[data-history-id="25"]').getByRole('button')).toHaveCount(0)
  expect(app.state.pending).toHaveLength(1)
  await app.assertClean()
})

test('History reads and delayed undo responses stay with their originating Telegram account', async ({ page }) => {
  const app = new MockApp(); app.history = [completion(1)]
  await app.install(page); await app.open(page); await historyButton(page).click()
  const held = app.hold('POST', '/api/history/1/undo', { ...app.state, pending: [{ ...app.state.pending[0], description: 'Old account result' }] })
  await page.getByRole('button', { name: 'Undo completion: Completed item 1' }).click(); await held.started
  app.state.pending = []; app.history = [completion(202)]
  app.state.reminders = [{ ...app.state.reminders[0], id: 202, description: 'Second account plan' }]
  await page.evaluate(() => {
    const tg = (window as any).Telegram.WebApp
    tg.initData = 'fixture-account-202'; tg.initDataUnsafe.user.id = 202
    window.dispatchEvent(new Event('focus'))
  })
  await expect(page.getByRole('button', { name: 'Edit reminder: Second account plan' })).toBeVisible()
  held.release(); await held.delivered
  await expect(page.getByRole('heading', { name: 'Old account result' })).toHaveCount(0)
  await expect(page.locator('[data-history-id="1"]')).toHaveCount(0)
  await historyButton(page).click()
  await expect(page.locator('[data-history-id="202"]')).toContainText('Completed item 202')
  await app.assertClean()
})

test('Individual settings save atomically and resetting defaults preserves the schedule', async ({ page }) => {
  const app = new MockApp(); await app.install(page); await app.open(page)
  await page.getByRole('button', { name: 'Edit reminder: Water the plants' }).click()
  await page.getByRole('button', { name: /Settings for this reminder/ }).click()
  await page.getByRole('button', { name: /Repeat this reminder until Done/ }).click()
  await page.getByRole('option', { name: 'Every 5 minutes', exact: true }).click()
  await page.getByRole('button', { name: /Follow-up for this reminder/ }).click()
  await page.getByRole('option', { name: 'No follow-up question', exact: true }).click()
  const voice = page.getByRole('switch', { name: 'Speak this reminder with Alice' })
  await voice.focus(); await voice.press('Space')
  await page.getByRole('button', { name: 'Save changes', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Edit reminder: Water the plants' })).toBeVisible()
  expect(app.matching('PUT', '/api/reminders/11').map((call) => call.body.settings)).toEqual([
    { nag_interval_minutes: 5, confirmation_check_minutes: 0, voice_enabled: false },
  ])
  expect(app.state.nag_interval_minutes).toBe(10)
  expect(app.state.confirmation_check_minutes).toBe(20)
  expect(app.matching('PATCH', '/api/preferences')).toHaveLength(0)
  await page.getByRole('button', { name: 'Edit reminder: Water the plants' }).click()
  await page.getByRole('button', { name: /Settings for this reminder/ }).click()
  await page.getByRole('button', { name: /Repeat this reminder until Done/ }).click()
  await page.getByRole('option', { name: 'Use general setting: Every 10 minutes', exact: true }).click()
  await page.getByRole('button', { name: /Follow-up for this reminder/ }).click()
  await page.getByRole('option', { name: 'Use general setting: Ask again after 20 minutes', exact: true }).click()
  await voice.focus(); await voice.press('Space')
  await page.getByRole('button', { name: 'Save changes', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Edit reminder: Water the plants' })).toBeVisible()
  expect(app.matching('PUT', '/api/reminders/11')[1].body).toMatchObject({
    at: '2026-10-29T09:00', recurrence: 'daily', preserve_schedule: true,
    settings: { nag_interval_minutes: null, confirmation_check_minutes: null, voice_enabled: true },
  })
  await app.assertClean()
})
