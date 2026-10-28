import { useRef, useState } from 'react';
import { Globe2, Settings2 } from 'lucide-react';
import type { AppStateDTO } from '@/shared/api/contracts';
import { errorKey } from '@/shared/api';
import { useI18n } from '@/shared/i18n';
import { deviceTimezone } from '@/shared/lib/date';
import { haptic } from '@/shared/lib/telegram';
import { AppSelect, Button, Label, Section, Switch } from '@/shared/ui';
import { useReminderState, type PreferencesPatch } from '@/entities/reminder';

export function Preferences({ state }: { state: AppStateDTO }) {
  const { t } = useI18n();
  const { updatePreferences, setTimezone, isMutating } = useReminderState();
  const [status, setStatus] = useState<{ key: string; error?: boolean } | null>(null);
  const [busy, setBusy] = useState(false);
  const locked = useRef(false);
  const device = deviceTimezone();

  async function save(patch: PreferencesPatch | string) {
    if (locked.current || isMutating) return;
    locked.current = true;
    setBusy(true);
    setStatus({ key: 'settings.saving' });
    try {
      if (typeof patch === 'string') await setTimezone(patch);
      else await updatePreferences(patch);
      setStatus({ key: typeof patch === 'string' ? 'timezone.saved' : 'settings.saved' });
      haptic('success');
    } catch (error) {
      setStatus({ key: errorKey(error), error: true });
      haptic('error');
    } finally {
      locked.current = false;
      setBusy(false);
    }
  }

  return <Section title={t('settings.title')} description={t('settings.subtitle')} icon={<Settings2 aria-hidden="true" />}>
    <div className="space-y-6">
      <div className="space-y-2">
        <Label htmlFor="nag-interval">{t('settings.interval')}</Label>
        <AppSelect id="nag-interval" ariaLabel={t('settings.interval')} value={String(state.nag_interval_minutes)} disabled={busy || isMutating} onValueChange={(value) => void save({ nag_interval_minutes: Number(value) })} options={[1, 5, 10, 20, 30, 60].map((value) => ({ value: String(value), label: t(`interval.${value}`) }))} />
        <p className="text-sm text-muted-foreground">{t('settings.intervalHelp')}</p>
      </div>
      <div className="flex items-start justify-between gap-4">
        <div className="space-y-2">
          <Label htmlFor="confirmation-check">{t('settings.check')}</Label>
          <p id="confirmation-check-help" className="text-sm text-muted-foreground">{t('settings.checkHelp')}</p>
        </div>
        <Switch id="confirmation-check" aria-label={t('settings.check')} aria-describedby="confirmation-check-help" isSelected={state.confirmation_check_minutes === 20} isDisabled={busy || isMutating} onChange={(checked) => void save({ confirmation_check_minutes: checked ? 20 : 0 })} />
      </div>
      {device && device !== state.timezone_id && <div className="space-y-2 border-t pt-4">
        <p className="text-sm text-muted-foreground">{t('timezone.mismatch', { device, timezone: state.timezone || '' })}</p>
        <Button type="button" variant="outline" size="sm" isDisabled={busy || isMutating} onPress={() => void save(device)}><Globe2 aria-hidden="true" />{t('timezone.use', { timezone: device })}</Button>
      </div>}
      {status && <p role="status" aria-live="polite" className={status.error ? 'text-sm text-destructive' : 'text-sm text-muted-foreground'}>{t(status.key)}</p>}
    </div>
  </Section>;
}
