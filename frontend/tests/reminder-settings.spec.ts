import { expect, test } from '@playwright/test'
import { MockApp } from './fixtures'

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
