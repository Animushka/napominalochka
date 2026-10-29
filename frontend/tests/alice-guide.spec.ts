import { expect, test } from '@playwright/test'
import { MockApp } from './fixtures'

test.describe('Nested Alice scenario guide', () => {
  test.use({ hasTouch: true })

  for (const language of ['en', 'ru'] as const) {
    test(`opening the ${language} guide keeps the Alice panel expanded`, async ({ page }, testInfo) => {
      const app = new MockApp()
      app.state.language = language
      Object.assign(app.yandex, {
        connected: true, enabled: true,
        scenario_id: 'speaker-a', scenario_name: 'Living room reminder',
      })
      await app.install(page, { dark: true })
      await page.goto('/')

      const parent = page.getByRole('button', { name: language === 'ru' ? /^Алиса / : /^Alice / })
      await parent.click()
      await expect(parent).toHaveAttribute('aria-expanded', 'true')
      const guide = page.getByRole('button', {
        name: language === 'ru' ? 'Как подготовить сценарий' : 'Prepare a scenario', exact: true,
      })
      const phrase = page.getByText(language === 'ru'
        ? 'У тебя новое напоминание. Загляни в Telegram.'
        : 'You have a new reminder. Take a look in Telegram.', { exact: true })

      await expect(guide).toHaveAttribute('aria-expanded', 'false')
      await guide.tap()
      await expect(parent).toHaveAttribute('aria-expanded', 'true')
      await expect(guide).toHaveAttribute('aria-expanded', 'true')
      await expect(phrase).toBeVisible()
      await page.screenshot({ path: testInfo.outputPath('alice-guide-open.png') })

      await guide.tap()
      await expect(parent).toHaveAttribute('aria-expanded', 'true')
      await expect(guide).toHaveAttribute('aria-expanded', 'false')
      await expect(phrase).toBeHidden()

      await guide.focus()
      await page.keyboard.press('Enter')
      await expect(parent).toHaveAttribute('aria-expanded', 'true')
      await expect(guide).toHaveAttribute('aria-expanded', 'true')
      await expect(phrase).toBeVisible()
      await page.keyboard.press('Space')
      await expect(parent).toHaveAttribute('aria-expanded', 'true')
      await expect(guide).toHaveAttribute('aria-expanded', 'false')

      // The boundary isolates the guide without preventing the parent from closing.
      await parent.click()
      await expect(parent).toHaveAttribute('aria-expanded', 'false')
      await parent.click()
      await expect(parent).toHaveAttribute('aria-expanded', 'true')
      await expect(guide).toBeVisible()
      expect(app.calls.filter(({ method }) => method !== 'GET')).toEqual([])
      await app.assertClean()
    })
  }
})
