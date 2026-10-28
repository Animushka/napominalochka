import { Check, ChevronDown, CircleHelp, FlaskConical, Link2, RefreshCw, ShieldAlert, Volume2 } from 'lucide-react'
import { errorKeys } from '@/shared/api'
import { useI18n } from '@/shared/i18n'
import { Alert, AlertDescription } from '@/shared/ui/alert'
import { AppSelect } from '@/shared/ui/app-select'
import { Button } from '@/shared/ui/button'
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/shared/ui/collapsible'
import { Section } from '@/shared/ui/section'
import { useAlice } from './use-alice'

export interface AlicePanelProps {
  ready: boolean
  accountKey?: string | number
}

export function AlicePanel({ ready, accountKey = 'current' }: AlicePanelProps) {
  const { t } = useI18n()
  const { state, controller, remaining } = useAlice(ready, accountKey)
  const status = state.status
  if (!ready || status?.available !== true) return null

  const connected = status.connected && !status.requires_reconnect
  const stage = connected ? state.stage : 'connect'
  const locked = state.loading || state.busy !== null
  const selected = state.scenarios?.find((scenario) => scenario.id === state.draftId && scenario.is_active)
  const scenarioName = selected?.name || status.scenario_name || t('yandex.savedScenario')
  const awaitingConfirmation = !!state.heardId && state.heardId === status.scenario_id && state.heardId === state.draftId
  const testDisabled = locked || state.statusUncertain || remaining > 0 || !selected || !connected
    || state.draftId !== status.scenario_id
  const lastError = status.requires_reconnect ? 'yandex.errors.reconnect'
    : status.last_error ? errorKeys[status.last_error] || 'yandex.errors.delivery' : null
  const summary = state.statusUncertain ? 'yandex.errors.unavailable'
    : status.requires_reconnect ? 'yandex.summaryReconnect'
      : status.enabled && connected ? 'yandex.summaryEnabled'
        : connected ? 'yandex.summaryConnected' : 'yandex.subtitle'
  const missingScenario = connected && state.scenarios !== null && !!state.draftId && !selected
  const options = (state.scenarios || []).map((scenario) => ({
    value: scenario.id,
    label: scenario.is_active ? scenario.name : t('yandex.inactiveScenario', { name: scenario.name }),
    disabled: !scenario.is_active,
  }))
  if (state.draftId && !options.some((option) => option.value === state.draftId)) {
    options.push({ value: state.draftId, label: state.scenarios ? t('yandex.savedScenarioMissing')
      : status.scenario_name || t('yandex.savedScenario'), disabled: true })
  }
  const testLabel = t(state.busy === 'test' ? 'yandex.testing' : remaining ? 'yandex.testCooldown' : 'yandex.test', { seconds: remaining })
  const steps = [
    { name: 'connect', label: 'yandex.progressConnect', Icon: Link2 },
    { name: 'choose', label: 'yandex.progressChoose', Icon: Check },
    { name: 'test', label: 'yandex.progressTest', Icon: FlaskConical },
  ] as const

  return (
    <Section title={t('yandex.title')} description={t(summary)} icon={<Volume2 aria-hidden="true" className="size-5" />} onOpenChange={controller.onOpenChange}>
      <div className="space-y-5" aria-busy={locked}>
        {stage !== 'manage' && (
          <ol className="grid grid-cols-3 gap-2" aria-label={t('yandex.setup')}>
            {steps.map(({ name, label, Icon }) => (
              <li key={name} aria-current={stage === name ? 'step' : undefined}
                className={`flex min-w-0 items-center justify-center gap-1.5 rounded-lg px-2 py-2.5 text-xs font-medium ${stage === name ? 'bg-primary text-primary-foreground' : 'bg-muted text-muted-foreground'}`}>
                <Icon className="size-3.5 shrink-0" aria-hidden="true" />
                <span className="truncate">{t(label)}</span>
              </li>
            ))}
          </ol>
        )}

        {stage === 'connect' && (
          <div className="space-y-4">
            <p className="text-sm text-muted-foreground">{t('yandex.description')}</p>
            <Alert>
              <ShieldAlert aria-hidden="true" />
              <AlertDescription id="alice-permissions">{t('yandex.permissions')}</AlertDescription>
            </Alert>
            <Button className="w-full" isDisabled={locked} onPress={() => void controller.connect()} aria-describedby="alice-permissions alice-return-help">
              <Link2 aria-hidden="true" />
              {t(state.busy === 'connect' ? 'yandex.connecting' : state.oauthUrl ? 'yandex.continue' : status.requires_reconnect ? 'yandex.reconnect' : 'yandex.connect')}
            </Button>
            <p id="alice-return-help" className="text-sm text-muted-foreground">{t('yandex.returnHelp')}</p>
          </div>
        )}

        {stage === 'choose' && (
          <div className="space-y-4">
            <div className="space-y-2">
              <label htmlFor="alice-scenario" className="text-sm font-medium">{t('yandex.scenario')}</label>
              <AppSelect id="alice-scenario" ariaLabel={t('yandex.scenario')} value={state.draftId}
                onValueChange={controller.select} options={options} placeholder={t('yandex.chooseScenario')}
                disabled={locked || !state.scenarios?.length} aria-describedby="alice-scenario-help alice-actions-warning" />
              <p id="alice-scenario-help" className="text-sm text-muted-foreground">
                {t(state.loading ? 'yandex.loading' : state.scenarios === null ? 'yandex.loadScenarios'
                  : !state.scenarios.some((scenario) => scenario.is_active) ? 'yandex.noScenarios' : 'yandex.scenarioHelp')}
              </p>
            </div>
            <Alert><ShieldAlert aria-hidden="true" /><AlertDescription id="alice-actions-warning">{t('yandex.actionsWarning')}</AlertDescription></Alert>
            <Button className="w-full" isDisabled={locked || state.statusUncertain || !selected}
              onPress={() => void controller.choose()} aria-describedby="alice-actions-warning">
              {t(state.busy === 'selection' ? 'action.saving' : 'yandex.next')}
            </Button>
            {!!status.scenario_id && <Button variant="ghost" className="w-full" isDisabled={locked} onPress={controller.cancelChange}>{t('action.cancel')}</Button>}
          </div>
        )}

        {(stage === 'test' || stage === 'manage') && (
          <div className="space-y-4">
            <div className="flex min-w-0 items-center gap-3 rounded-xl bg-muted p-4">
              <Volume2 className="size-5 shrink-0 text-muted-foreground" aria-hidden="true" />
              <div className="min-w-0 space-y-1">
                <p className="text-xs text-muted-foreground">{t('yandex.selected')}</p>
                <p className="break-words text-sm font-semibold">{stage === 'manage' ? status.scenario_name || scenarioName : scenarioName}</p>
              </div>
            </div>
            <p className="text-sm leading-relaxed text-muted-foreground">{t('yandex.behavior')}</p>
            <Alert><ShieldAlert aria-hidden="true" /><AlertDescription id="alice-test-help">{t('yandex.testHelp')}</AlertDescription></Alert>

            {stage === 'test' && (awaitingConfirmation ? (
              <div className="space-y-3 rounded-xl border border-primary/20 bg-primary/5 p-4">
                <p className="text-sm font-medium">{t('yandex.heardPrompt')}</p>
                <Button className="h-auto min-h-10 w-full whitespace-normal" isDisabled={locked || state.statusUncertain || !selected} onPress={() => void controller.enable()}>
                  <Check className="shrink-0" aria-hidden="true" />{t(state.busy === 'enable' ? 'action.saving' : 'yandex.confirm')}
                </Button>
                <Button variant="ghost" className="w-full" isDisabled={locked} onPress={controller.notHeard}>{t('yandex.notHeard')}</Button>
              </div>
            ) : (
              <Button className="w-full" isDisabled={testDisabled} onPress={() => void controller.test()} aria-describedby="alice-test-help">
                <FlaskConical aria-hidden="true" />{testLabel}
              </Button>
            ))}

            {stage === 'manage' && (
              <>
                <Button className="h-auto min-h-10 w-full whitespace-normal" variant={status.enabled && !state.statusUncertain ? 'secondary' : 'default'}
                  isDisabled={locked || (!state.statusUncertain && !status.enabled && !selected)} onPress={() => void controller.toggle()}>
                  <Volume2 className="shrink-0" aria-hidden="true" />
                  {t(state.statusUncertain ? 'yandex.refresh' : state.busy === 'disable' ? 'action.saving' : status.enabled ? 'yandex.disable' : 'yandex.enable')}
                </Button>
                <Button variant="outline" className="w-full" isDisabled={testDisabled} onPress={() => void controller.test()} aria-describedby="alice-test-help">
                  <FlaskConical aria-hidden="true" />{testLabel}
                </Button>
              </>
            )}
            <Button variant="ghost" className="w-full" isDisabled={locked} onPress={controller.change}>{t(stage === 'manage' ? 'yandex.changeShort' : 'yandex.change')}</Button>
          </div>
        )}

        {(lastError || missingScenario) && (
          <Alert variant="destructive"><AlertDescription role="status">{t(lastError || 'yandex.errors.scenarioMissing')}</AlertDescription></Alert>
        )}
        {(state.loading || state.feedback) && (
          <p role="status" aria-live="polite" className={`text-sm leading-relaxed ${state.feedbackError && !state.loading ? 'text-destructive' : 'text-muted-foreground'}`}>
            {t(state.loading ? 'yandex.loading' : state.feedback || '')}
          </p>
        )}

        <Collapsible>
          <CollapsibleTrigger className="flex w-full items-center gap-2 rounded-md py-2 text-left text-sm font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring">
            <CircleHelp className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
            <span className="flex-1">{t('yandex.guide')}</span>
            <ChevronDown className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
          </CollapsibleTrigger>
          <CollapsibleContent className="space-y-3 pt-3 text-sm leading-relaxed text-muted-foreground">
            <p>{t('yandex.step1')}</p>
            <blockquote className="rounded-lg bg-muted px-4 py-3 font-medium text-foreground">{t('yandex.phrase')}</blockquote>
            <p>{t('yandex.fixedPhrase')}</p>
            <div className="flex flex-wrap gap-x-5 gap-y-3">
              <a className="text-primary underline-offset-4 hover:underline" href="/privacy" target="_blank" rel="noopener noreferrer">{t('yandex.privacy')}</a>
              <a className="text-primary underline-offset-4 hover:underline" href="https://id.yandex.ru/personal/data-access" target="_blank" rel="noopener noreferrer">{t('yandex.revoke')}</a>
            </div>
          </CollapsibleContent>
        </Collapsible>

        <div className="flex flex-wrap items-center justify-between gap-2 border-t pt-4">
          <Button size="sm" variant="ghost" isDisabled={locked} onPress={() => void controller.refresh(true)}>
            <RefreshCw className={state.loading ? 'animate-spin' : ''} aria-hidden="true" />{t('yandex.refresh')}
          </Button>
          {(status.connected || status.requires_reconnect) && (
            <Button size="sm" variant="ghost" className="text-muted-foreground" isDisabled={locked}
              onPress={() => void controller.disconnect(t('yandex.disconnectConfirm'), t('yandex.disconnect'))}>
              {t('yandex.disconnect')}
            </Button>
          )}
        </div>
      </div>
    </Section>
  )
}
