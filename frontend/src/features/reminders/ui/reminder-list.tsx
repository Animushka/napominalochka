import { useRef, useState } from 'react';
import { AlarmClock, CalendarClock, Check, ChevronRight, Clock3, Repeat2 } from 'lucide-react';
import type { AppStateDTO, ReminderDTO, SnoozeDuration } from '@/shared/api/contracts';
import { errorKey } from '@/shared/api';
import { useI18n } from '@/shared/i18n';
import { formatRelative, formatWhen } from '@/shared/lib/date';
import { haptic } from '@/shared/lib/telegram';
import { Badge, Button, Card, CardContent } from '@/shared/ui';
import { useReminderState } from '@/entities/reminder';

interface ReminderListProps {
  state: AppStateDTO;
  onEdit: (reminder: ReminderDTO) => void;
}

export function ReminderList({ state, onEdit }: ReminderListProps) {
  const { t, locale } = useI18n();
  const { doneReminder, snoozeReminder, isMutating } = useReminderState();
  const [snoozeId, setSnoozeId] = useState<number | null>(null);
  const locked = useRef(false);
  const [busyId, setBusyId] = useState<number | null>(null);
  const [status, setStatus] = useState<{ key: string; error?: boolean } | null>(null);
  const now = state.now || '';
  const pending = state.pending ?? [];
  const when = (at: string) => formatWhen(at, now, t, locale);

  async function act(id: number, duration?: SnoozeDuration) {
    if (isMutating || locked.current) return;
    locked.current = true;
    setBusyId(id);
    setStatus(null);
    try {
      const result = duration ? await snoozeReminder(id, duration) : await doneReminder(id);
      setSnoozeId(null);
      setStatus({ key: result.alreadyCompleted ? 'pending.alreadyCompleted' : duration ? 'pending.snoozed' : 'pending.completed' });
      haptic('success');
    } catch (error) {
      setStatus({ key: errorKey(error), error: true });
      haptic('error');
    } finally {
      locked.current = false;
      setBusyId(null);
    }
  }

  return <div className="space-y-6">
    {status && <p role="status" aria-live="polite" className={status.error ? 'text-sm text-destructive' : 'text-sm text-muted-foreground'}>{t(status.key)}</p>}
    {pending.length > 0 && <section aria-labelledby="pending-title" className="space-y-3">
      <div className="flex items-center gap-2">
        <h2 id="pending-title" className="text-lg font-semibold">{t('pending.title')}</h2>
        <Badge variant="secondary">{new Intl.NumberFormat(locale).format(pending.length)}</Badge>
      </div>
      {pending.map((item) => <Card key={item.id} data-pending-id={item.id} className="py-0">
        <CardContent className="space-y-4 p-5">
          <div className="flex gap-3">
            <AlarmClock className="mt-1 size-5 shrink-0 text-muted-foreground" aria-hidden="true" />
            <div className="min-w-0 space-y-1">
              <h3 className="break-words font-medium">{item.description}</h3>
              <p className="text-sm text-muted-foreground">{t('pending.nextMessage')} {' '}
                {item.next_at ? <time dateTime={item.next_at}>{when(item.next_at)}</time> : item.next_when || t('date.now')}
              </p>
            </div>
          </div>
          <div className="flex flex-wrap gap-2">
            <Button size="lg" className="min-h-11" isDisabled={isMutating || busyId !== null} aria-label={t('pending.doneLabel', { description: item.description })} onPress={() => void act(item.id)}>
              <Check aria-hidden="true" />{t(busyId === item.id ? 'pending.saving' : 'pending.done')}
            </Button>
            <Button size="lg" className="min-h-11" variant="outline" isDisabled={isMutating || busyId !== null} aria-expanded={snoozeId === item.id} aria-controls={`snooze-${item.id}`} onPress={() => setSnoozeId((previous) => previous === item.id ? null : item.id)}>
              <Clock3 aria-hidden="true" />{t('pending.snooze')}
            </Button>
          </div>
          {snoozeId === item.id && <div id={`snooze-${item.id}`} role="group" aria-label={t('pending.snooze')} className="flex flex-wrap gap-2">
            {(['10m', '1h', 'tomorrow'] as const).map((duration) => <Button key={duration} type="button" size="lg" className="min-h-11" variant="secondary" isDisabled={isMutating || busyId !== null} onPress={() => void act(item.id, duration)}>{t(`pending.${duration}`)}</Button>)}
          </div>}
        </CardContent>
      </Card>)}
    </section>}
    <section aria-labelledby="scheduled-title" className="space-y-3">
      <div className="flex items-center gap-2">
        <h2 id="scheduled-title" className="text-lg font-semibold">{t('home.reminders')}</h2>
        <Badge variant="secondary">{new Intl.NumberFormat(locale).format(state.reminders.length)}</Badge>
      </div>
      {state.reminders.length === 0 ? <Card>
        <CardContent className="flex flex-col items-center gap-3 py-10 text-center">
          <CalendarClock className="size-8 text-muted-foreground" aria-hidden="true" />
          <h3 className="font-medium">{t(pending.length ? 'empty.scheduledTitle' : 'empty.title')}</h3>
          <p className="max-w-sm text-sm text-muted-foreground">{t(pending.length ? 'empty.scheduledDescription' : 'empty.description')}</p>
        </CardContent>
      </Card> : <Card className="gap-0 overflow-hidden py-0">
        {state.reminders.map((reminder) => <Button key={reminder.id} type="button" variant="ghost" className="h-auto w-full justify-start gap-3 rounded-none border-b p-4 text-left whitespace-normal last:border-b-0" aria-label={t('reminder.edit', { description: reminder.description })} onPress={() => onEdit(reminder)} data-reminder-id={reminder.id}>
          {reminder.recurrence === 'once' ? <Clock3 className="size-5 shrink-0 text-muted-foreground" aria-hidden="true" /> : <Repeat2 className="size-5 shrink-0 text-muted-foreground" aria-hidden="true" />}
          <span className="min-w-0 flex-1 space-y-1">
            <span className="block break-words font-medium">{reminder.description}</span>
            <span className="block text-xs font-normal text-muted-foreground"><time dateTime={reminder.at}>{when(reminder.at)}</time>{' · '}{reminder.at < now ? t('date.overdue') : reminder.at === now ? t('date.now') : formatRelative(reminder.at, now, locale)}</span>
            {reminder.recurrence !== 'once' && <span className="block text-xs font-normal text-muted-foreground">{reminder.repeat || t(`repeat.${reminder.recurrence}`)}</span>}
          </span>
          <ChevronRight className="size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
        </Button>)}
      </Card>}
    </section>
  </div>;
}
