import { expect, type Page, type Route } from '@playwright/test'
import type { AppStateDTO, Scenario, YandexStatus } from '../src/shared/api/contracts'

type Reply = { status?: number; body?: unknown; abort?: boolean; settled?: () => void }
type Override = () => Reply | Promise<Reply>
export interface ApiCall { method: string; path: string; body: any; authorization: string | undefined }
const clone = <T>(value: T): T => structuredClone(value)

export function stateFixture(): AppStateDTO {
  return {
    language: 'en', nag_interval_minutes: 10, confirmation_check_minutes: 20,
    timezone: 'Europe/Moscow', timezone_id: 'Europe/Moscow', now: '2026-10-28T12:00:00',
    reminders: [{ id: 11, description: 'Water the plants', at: '2026-10-29T09:00:00', recurrence: 'daily', when: '', relative: '', repeat: '' }],
    pending: [{ id: 21, reminder_id: 11, description: 'Read a chapter', next_at: '2026-10-28T12:10:00', next_when: '', relative: '' }],
  }
}

export class MockApp {
  state = stateFixture()
  yandex: YandexStatus = { available: true, connected: false, enabled: false, requires_reconnect: false, scenario_id: null, scenario_name: null, last_error: null }
  scenarios: Scenario[] = [
    { id: 'speaker-a', name: 'Living room reminder', is_active: true },
    { id: 'speaker-b', name: 'Office reminder', is_active: true },
    { id: 'inactive', name: 'Inactive scenario', is_active: false },
  ]
  calls: ApiCall[] = []
  unexpected: string[] = []
  pageErrors: string[] = []
  private overrides = new Map<string, Override[]>()

  once(method: string, path: string, override: Override) {
    const key = `${method} ${path}`
    this.overrides.set(key, [...this.overrides.get(key) || [], override])
  }

  hold(method: string, path: string, body: unknown) {
    let release!: () => void
    let entered!: () => void
    let finished!: () => void
    const gate = new Promise<void>((resolve) => { release = resolve })
    const started = new Promise<void>((resolve) => { entered = resolve })
    const delivered = new Promise<void>((resolve) => { finished = resolve })
    const snapshot = clone(body)
    this.once(method, path, async () => { entered(); await gate; return { body: snapshot, settled: finished } })
    return { release, started, delivered }
  }

  matching(method: string, path: string) { return this.calls.filter((call) => call.method === method && call.path === path) }

  async install(page: Page, options: { native?: boolean; dark?: boolean; authenticated?: boolean } = {}) {
    page.on('pageerror', (error) => this.pageErrors.push(error.message))
    await page.addInitScript(({ native, dark, authenticated }) => {
      const mainHandlers = new Set<() => void>()
      const backHandlers = new Set<() => void>()
      const events = new Map<string, Set<() => void>>()
      const bridge = {
        mainHandlers, backHandlers, mainVisible: false, backVisible: false,
        opened: [] as string[], confirmations: [] as string[], params: {} as Record<string, unknown>,
        main: () => [...mainHandlers].forEach((handler) => handler()),
        back: () => [...backHandlers].forEach((handler) => handler()),
        event: (name: string) => events.get(name)?.forEach((handler) => handler()),
      }
      const tg = {
        initData: authenticated ? 'fixture-account-101' : '',
        initDataUnsafe: { user: { id: 101, language_code: 'en', first_name: 'Fixture' } },
        colorScheme: dark ? 'dark' : 'light', themeParams: {},
        ready() {}, expand() {}, setHeaderColor() {}, setBackgroundColor() {},
        HapticFeedback: { selectionChanged() {}, notificationOccurred() {} },
        onEvent(name: string, handler: () => void) { if (!events.has(name)) events.set(name, new Set()); events.get(name)!.add(handler) },
        offEvent(name: string, handler: () => void) { events.get(name)?.delete(handler) },
        openLink(url: string) { bridge.opened.push(url) },
        isVersionAtLeast() { return true },
        showPopup(params: { message: string }, callback: (button: string) => void) { bridge.confirmations.push(params.message); callback('confirm') },
        BackButton: {
          onClick(handler: () => void) { backHandlers.add(handler) }, offClick(handler: () => void) { backHandlers.delete(handler) },
          show() { bridge.backVisible = true }, hide() { bridge.backVisible = false },
        },
        ...(native ? { MainButton: {
          onClick(handler: () => void) { mainHandlers.add(handler) }, offClick(handler: () => void) { mainHandlers.delete(handler) },
          setParams(params: Record<string, unknown>) { bridge.params = params; bridge.mainVisible = Boolean(params.is_visible) },
          show() { bridge.mainVisible = true }, hide() { bridge.mainVisible = false }, showProgress() {}, hideProgress() {},
        } } : {}),
      }
      Object.assign(window, { Telegram: { WebApp: tg }, __bridge: bridge })
    }, { native: options.native ?? false, dark: options.dark ?? false, authenticated: options.authenticated ?? true })

    await page.route('**/*', async (route) => {
      const url = new URL(route.request().url())
      if (url.origin !== 'http://127.0.0.1:4173') {
        // The Telegram bridge is synthetic. No external script, account or provider is contacted.
        await route.fulfill({ status: 200, contentType: 'application/javascript', body: '' })
        return
      }
      if (!url.pathname.startsWith('/api/')) { await route.continue(); return }
      const request = route.request()
      const networkSettled = request.response().then((response) => response?.finished()).catch(() => undefined)
      const call = { method: request.method(), path: url.pathname, body: request.postDataJSON(), authorization: request.headers().authorization }
      this.calls.push(call)
      const key = `${call.method} ${call.path}`
      const override = this.overrides.get(key)?.shift()
      const reply = override ? await override() : this.respond(call)
      try {
        if (reply.abort) await route.abort('failed')
        else await this.fulfill(route, reply)
      } catch (error) {
        // Canceled reads are intentionally released after a write/account change.
        if (!String(error).includes('Target page, context or browser has been closed') && !String(error).includes('Invalid InterceptionId')) throw error
      } finally {
        if (reply.settled) { await networkSettled; reply.settled() }
      }
    })
  }

  private async fulfill(route: Route, reply: Reply) {
    await route.fulfill({ status: reply.status ?? 200, contentType: 'application/json', body: JSON.stringify(reply.body ?? {}) })
  }

  private respond({ method, path, body }: ApiCall): Reply {
    if (method === 'GET' && path === '/api/state') return { body: clone(this.state) }
    if (method === 'PATCH' && path === '/api/preferences') { this.state = { ...this.state, ...body }; return { body: clone(this.state) } }
    if (method === 'PUT' && path === '/api/timezone') { this.state.timezone = body.timezone; this.state.timezone_id = body.timezone; return { body: clone(this.state) } }
    if (method === 'POST' && /^\/api\/pending\/\d+\/done$/.test(path)) {
      this.state.pending = this.state.pending.filter((item) => item.id !== Number(path.split('/')[3])); return { body: clone(this.state) }
    }
    if (method === 'POST' && /^\/api\/pending\/\d+\/snooze$/.test(path)) {
      const pending = this.state.pending.find((item) => item.id === Number(path.split('/')[3]))!
      pending.next_at = body.duration === '10m' ? '2026-10-28T12:20:00' : body.duration === '1h' ? '2026-10-28T13:00:00' : '2026-10-29T09:00:00'
      return { body: clone(this.state) }
    }
    if (method === 'POST' && path === '/api/reminders') {
      const reminder = { ...body, id: 99, when: '', relative: '', repeat: '' }
      this.state.reminders.push(reminder); return { body: reminder }
    }
    if (method === 'PUT' && /^\/api\/reminders\/\d+$/.test(path)) {
      const id = Number(path.split('/')[3]); const previous = this.state.reminders.find((item) => item.id === id)!
      const reminder = { ...previous, ...body, ...(body.preserve_schedule ? { at: previous.at, recurrence: previous.recurrence } : {}) }
      this.state.reminders = this.state.reminders.map((item) => item.id === id ? reminder : item); return { body: reminder }
    }
    if (method === 'GET' && path === '/api/yandex') return { body: clone(this.yandex) }
    if (method === 'GET' && path === '/api/yandex/scenarios') return { body: { scenarios: clone(this.scenarios) } }
    if (method === 'POST' && path === '/api/yandex/connect') return { body: { url: 'https://oauth.yandex.ru/authorize?state=fixture-only' } }
    if (method === 'PUT' && path === '/api/yandex') {
      this.yandex = { ...this.yandex, ...body, scenario_name: this.scenarios.find((scenario) => scenario.id === body.scenario_id)?.name ?? null }
      return { body: clone(this.yandex) }
    }
    if (method === 'POST' && path === '/api/yandex/test') return { body: { ok: true } }
    if (method === 'DELETE' && path === '/api/yandex') {
      this.yandex = { ...this.yandex, connected: false, enabled: false, requires_reconnect: false, scenario_id: null, scenario_name: null }
      return { body: {} }
    }
    this.unexpected.push(`${method} ${path}`)
    return { status: 500, body: { code: 'unexpected_fixture_request' } }
  }

  async open(page: Page) {
    await page.goto('/')
    await expect(page.getByRole('heading', { name: 'Your plans, all here' })).toBeVisible()
  }

  async assertClean() { expect(this.unexpected).toEqual([]); expect(this.pageErrors).toEqual([]) }
}

export const aliceAccordion = (page: Page) => page.getByRole('button', { name: /^Alice / })
export const preferencesAccordion = (page: Page) => page.getByRole('button', { name: /^How to remind me / })
export async function openAlice(page: Page) { const button = aliceAccordion(page); await expect(button).toBeVisible(); if (await button.getAttribute('aria-expanded') !== 'true') await button.click() }
export async function chooseScenario(page: Page, name = 'Living room reminder') {
  await page.getByRole('button', { name: /Reminder scenario/ }).click()
  await page.getByRole('option', { name, exact: true }).click()
  await page.getByRole('button', { name: 'Next: test it', exact: true }).click()
  await expect(page.getByRole('button', { name: 'Test on your speaker', exact: true })).toBeEnabled()
}
