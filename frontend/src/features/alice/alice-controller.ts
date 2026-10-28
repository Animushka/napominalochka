import { api, errorKey } from '@/shared/api'
import type { Scenario, YandexStatus } from '@/shared/api/contracts'
import { confirmAction, getTelegram, haptic } from '@/shared/lib/telegram'

type Stage = 'choose' | 'test' | 'manage'
type Busy = 'connect' | 'selection' | 'test' | 'enable' | 'disable' | 'confirmDisconnect' | 'disconnect'

export interface AliceState {
  status: YandexStatus | null
  statusUncertain: boolean
  scenarios: Scenario[] | null
  loading: boolean
  busy: Busy | null
  stage: Stage | null
  draftId: string
  heardId: string | null
  feedback: string | null
  feedbackError: boolean
  oauthUrl: string | null
  refreshedAt: number
  cooldownUntil: number
}

const initialState = (): AliceState => ({
  status: null, statusUncertain: false, scenarios: null, loading: false, busy: null,
  stage: null, draftId: '', heardId: null, feedback: null, feedbackError: false,
  oauthUrl: null, refreshedAt: 0, cooldownUntil: 0,
})

export function oauthTarget(url: string): URL {
  const target = new URL(url)
  if (target.protocol !== 'https:' || target.hostname !== 'oauth.yandex.ru'
    || target.username || target.password || (target.port && target.port !== '443')) {
    throw new Error('Invalid OAuth destination')
  }
  return target
}

function openOAuth(url: string) {
  const target = oauthTarget(url)
  const telegram = getTelegram()
  if (typeof telegram?.openLink === 'function') {
    try { telegram.openLink(target.href); return } catch { /* Older clients use the browser. */ }
  }
  window.open(target.href, '_blank', 'noopener,noreferrer')
}

/** Owns only Alice endpoints. Every operation belongs to one active account lifecycle. */
export class AliceController {
  private state = initialState()
  private listeners = new Set<() => void>()
  private requests = new Set<AbortController>()
  private active = false
  private generation = 0
  private open = false

  getSnapshot = () => this.state
  subscribe = (listener: () => void) => {
    this.listeners.add(listener)
    return () => { this.listeners.delete(listener) }
  }

  private update(patch: Partial<AliceState>) {
    this.state = { ...this.state, ...patch }
    this.listeners.forEach((listener) => listener())
  }

  activate = () => {
    this.active = true
    void this.refresh(true)
  }

  deactivate = () => {
    this.active = false
    this.generation += 1
    this.requests.forEach((request) => request.abort())
    this.requests.clear()
    // Never leave an accepted test or stale account data behind on logout.
    const cooldownUntil = this.state.cooldownUntil
    this.update({ ...initialState(), cooldownUntil })
  }

  private operation() {
    const generation = this.generation
    const controller = new AbortController()
    this.requests.add(controller)
    return {
      signal: controller.signal,
      current: () => this.active && this.generation === generation && !controller.signal.aborted,
      finish: () => { this.requests.delete(controller) },
    }
  }

  private get locked() { return !this.active || this.state.loading || this.state.busy !== null }
  private get connected() { return !!this.state.status?.connected && !this.state.status.requires_reconnect }
  private get selected() { return this.state.scenarios?.find((scenario) => scenario.id === this.state.draftId && scenario.is_active) }
  private feedback(key: string | null, feedbackError = false) { this.update({ feedback: key, feedbackError }) }

  private setStatus(status: YandexStatus, reset = false) {
    const previous = this.state.status
    const changed = previous?.connected !== status.connected || previous?.scenario_id !== status.scenario_id
      || (previous?.requires_reconnect && !status.requires_reconnect)
    const patch: Partial<AliceState> = { status, statusUncertain: false }
    if (reset || changed || this.state.stage === null) {
      patch.draftId = typeof status.scenario_id === 'string' ? status.scenario_id : ''
      patch.stage = patch.draftId ? 'manage' : 'choose'
      patch.heardId = null
    }
    if (!status.connected || status.requires_reconnect) {
      patch.scenarios = null
      patch.heardId = null
    } else patch.oauthUrl = null
    if (changed) { patch.feedback = null; patch.feedbackError = false }
    this.update(patch)
  }

  private handleError(error: unknown) {
    this.feedback(errorKey(error), true)
    const code = typeof error === 'object' && error !== null && 'code' in error ? error.code : null
    if ((code === 'yandex_reconnect_required' || code === 'yandex_not_connected') && this.state.status) {
      this.update({ status: { ...this.state.status, requires_reconnect: true }, heardId: null })
    }
  }

  private async reconcileAfterFailure(operation: ReturnType<AliceController['operation']>) {
    const { stage, draftId, feedback, feedbackError } = this.state
    this.update({ heardId: null, statusUncertain: true })
    try {
      // A single status read resolves server-side disabling; it never repeats a provider action.
      const status = await api<YandexStatus>('GET', '/api/yandex', undefined, operation.signal)
      if (!operation.current()) return
      this.setStatus(status)
    } catch { /* Preserve uncertainty and the original failure until an explicit refresh succeeds. */ }
    if (operation.current()) this.update({ stage, draftId, heardId: null, feedback, feedbackError })
  }

  refresh = async (force = false) => {
    if (this.locked || (!force && Date.now() - this.state.refreshedAt < 3000)) return
    const operation = this.operation()
    this.update({ loading: true, ...(force ? { oauthUrl: null, feedback: null, feedbackError: false } : {}) })
    let fetchingScenarios = false
    try {
      const status = await api<YandexStatus>('GET', '/api/yandex', undefined, operation.signal)
      if (!operation.current()) return
      this.setStatus(status)
      if (status.available && this.connected && (this.open || force)) {
        fetchingScenarios = true
        const data = await api<{ scenarios: Scenario[] }>('GET', '/api/yandex/scenarios', undefined, operation.signal)
        if (!operation.current()) return
        const scenarios = Array.isArray(data.scenarios)
          ? data.scenarios.filter((scenario) => scenario && typeof scenario.id === 'string'
            && typeof scenario.name === 'string' && typeof scenario.is_active === 'boolean') : []
        this.update({ scenarios })
      }
    } catch (error) {
      if (!operation.current()) return
      this.handleError(error)
      if (fetchingScenarios) {
        this.update({ scenarios: null })
        await this.reconcileAfterFailure(operation)
      } else this.update({ statusUncertain: true, heardId: null })
    } finally {
      if (operation.current()) this.update({ loading: false, refreshedAt: Date.now() })
      operation.finish()
    }
  }

  onOpenChange = (open: boolean) => {
    this.open = open
    if (open && this.state.scenarios === null) void this.refresh(true)
  }

  connect = async () => {
    if (this.locked) return
    if (this.state.oauthUrl) {
      try { openOAuth(this.state.oauthUrl) } catch { this.feedback('yandex.errors.connect', true) }
      return
    }
    const operation = this.operation()
    this.update({ busy: 'connect', feedback: null, feedbackError: false })
    try {
      const data = await api<{ url: string }>('POST', '/api/yandex/connect', undefined, operation.signal)
      if (!operation.current()) return
      try { openOAuth(data.url) } catch { this.feedback('yandex.errors.connect', true); return }
      this.update({ oauthUrl: data.url })
      this.feedback('yandex.awaitingReturn')
    } catch (error) {
      if (operation.current()) this.handleError(error)
    } finally {
      if (operation.current()) this.update({ busy: null })
      operation.finish()
    }
  }

  select = (draftId: string) => {
    if (this.locked) return
    this.update({ draftId, heardId: null, feedback: null, feedbackError: false })
  }

  choose = async () => {
    if (this.locked || this.state.statusUncertain || !this.connected || !this.selected) return
    const operation = this.operation()
    const scenarioId = this.state.draftId
    this.update({ busy: 'selection', heardId: null, feedback: null, feedbackError: false })
    try {
      const status = await api<YandexStatus>('PUT', '/api/yandex', { scenario_id: scenarioId, enabled: false }, operation.signal)
      if (!operation.current()) return
      this.setStatus(status, true)
      this.update({ stage: 'test' })
      haptic()
    } catch (error) {
      if (!operation.current()) return
      this.handleError(error)
      await this.reconcileAfterFailure(operation)
    } finally {
      if (operation.current()) this.update({ busy: null })
      operation.finish()
    }
  }

  test = async () => {
    if (this.locked || this.state.statusUncertain || !this.selected || !this.connected
      || this.state.draftId !== this.state.status?.scenario_id || Date.now() < this.state.cooldownUntil) return
    const operation = this.operation()
    const scenarioId = this.state.draftId
    this.update({ busy: 'test', heardId: null, feedback: null, feedbackError: false, cooldownUntil: Date.now() + 60_000 })
    try {
      await api<{ ok: boolean }>('POST', '/api/yandex/test', undefined, operation.signal)
      if (!operation.current()) return
      this.update({ cooldownUntil: Date.now() + 60_000 })
      // Provider acceptance only unlocks the question. Only the user's answer can enable audio.
      if (!this.state.status?.enabled) this.update({ heardId: scenarioId, stage: 'test' })
      if (this.state.status) this.update({ status: { ...this.state.status, last_error: null } })
      this.feedback('yandex.testSent')
      haptic('success')
    } catch (error) {
      if (!operation.current()) return
      this.update({ cooldownUntil: Date.now() + 60_000 })
      if (errorKey(error) === 'errors.network') this.feedback('yandex.errors.deliveryUncertain', true)
      else this.handleError(error)
      await this.reconcileAfterFailure(operation)
    } finally {
      if (operation.current()) this.update({ busy: null })
      operation.finish()
    }
  }

  enable = async () => {
    if (this.locked || this.state.statusUncertain || !this.state.heardId || !this.selected
      || this.state.heardId !== this.state.status?.scenario_id || this.state.heardId !== this.state.draftId || !this.connected) return
    const operation = this.operation()
    const scenarioId = this.state.heardId
    this.update({ busy: 'enable', feedback: null, feedbackError: false })
    try {
      const status = await api<YandexStatus>('PUT', '/api/yandex', { scenario_id: scenarioId, enabled: true }, operation.signal)
      if (!operation.current()) return
      this.setStatus(status, true)
      this.feedback('yandex.enabledSaved')
      haptic('success')
    } catch (error) {
      if (!operation.current()) return
      this.handleError(error)
      await this.reconcileAfterFailure(operation)
    } finally {
      if (operation.current()) this.update({ busy: null })
      operation.finish()
    }
  }

  notHeard = () => {
    if (this.locked) return
    this.update({ heardId: null })
    this.feedback('yandex.checkSpeaker')
  }

  toggle = async () => {
    if (this.locked) return
    if (this.state.statusUncertain) { await this.refresh(true); return }
    if (!this.state.status?.enabled) {
      if (!this.selected || !this.connected) return
      this.update({ stage: 'test', heardId: null, feedback: null, feedbackError: false })
      return
    }
    const operation = this.operation()
    this.update({ busy: 'disable', feedback: null, feedbackError: false, heardId: null })
    try {
      // Disabling is a local server setting and does not execute the selected scenario.
      const status = await api<YandexStatus>('PUT', '/api/yandex', { scenario_id: this.state.status.scenario_id, enabled: false }, operation.signal)
      if (!operation.current()) return
      this.setStatus(status, true)
      this.feedback('yandex.disabledSaved')
    } catch (error) {
      if (!operation.current()) return
      this.handleError(error)
      await this.reconcileAfterFailure(operation)
    } finally {
      if (operation.current()) this.update({ busy: null })
      operation.finish()
    }
  }

  change = () => {
    if (this.locked) return
    this.update({ stage: 'choose', heardId: null, feedback: null, feedbackError: false })
    if (this.state.scenarios === null) void this.refresh(true)
  }

  cancelChange = () => {
    if (this.locked) return
    this.update({ draftId: this.state.status?.scenario_id || '', stage: 'manage', heardId: null, feedback: null, feedbackError: false })
  }

  disconnect = async (message: string, confirmLabel: string) => {
    if (this.locked) return
    const operation = this.operation()
    this.update({ busy: 'confirmDisconnect' })
    try {
      const confirmed = await confirmAction(message, confirmLabel)
      if (!operation.current() || !confirmed) return
      this.update({ busy: 'disconnect' })
      await api('DELETE', '/api/yandex', undefined, operation.signal)
      if (!operation.current()) return
      this.setStatus({ available: true, connected: false, enabled: false, requires_reconnect: false,
        scenario_id: null, scenario_name: null, last_error: null }, true)
      this.update({ oauthUrl: null, cooldownUntil: 0 })
      this.feedback('yandex.disconnected')
    } catch (error) {
      if (!operation.current()) return
      this.handleError(error)
      await this.reconcileAfterFailure(operation)
    } finally {
      if (operation.current()) this.update({ busy: null })
      operation.finish()
    }
  }
}
