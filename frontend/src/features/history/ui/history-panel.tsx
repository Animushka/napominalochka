import { useRef, useState } from 'react';
import { Check, History, RotateCcw } from 'lucide-react';
import { useReminderState } from '@/entities/reminder';
import { errorKey } from '@/shared/api';
import { useI18n } from '@/shared/i18n';
import { formatWhen } from '@/shared/lib/date';
import { haptic } from '@/shared/lib/telegram';
import { Alert, AlertDescription, Button, Section, Spinner } from '@/shared/ui';

export function HistoryPanel() {
  const { t, locale } = useI18n();
  const {
    state, history, historyOpen, setHistoryOpen, historyLoading, historyError,
    historyHasMore, historyLoadingMore, loadMoreHistory, retryHistory, undoCompletion, isMutating,
  } = useReminderState();
  const [feedback, setFeedback] = useState<{ key: string; error?: boolean } | null>(null);
  const [busyId, setBusyId] = useState<number | null>(null);
  const locked = useRef(false);
  const when = (at: string) => formatWhen(at, state?.now || at, t, locale);

  async function undo(id: number) {
    if (isMutating || locked.current) return;
    locked.current = true;
    setBusyId(id);
    setFeedback(null);
    try {
      await undoCompletion(id);
      setFeedback({ key: 'history.restored' });
      haptic('success');
    } catch (error) {
      setFeedback({ key: errorKey(error), error: true });
      haptic('error');
    } finally {
      locked.current = false;
      setBusyId(null);
    }
  }

  async function readMore(retry = false) {
    try {
      if (retry) await retryHistory();
      else await loadMoreHistory();
    } catch (error) {
      setFeedback({ key: errorKey(error), error: true });
    }
  }

  return <Section id="completion-history" title={t('history.title')} description={t('history.subtitle')}
    icon={<History aria-hidden="true" />} open={historyOpen} onOpenChange={setHistoryOpen}>
    <div className="space-y-4" aria-busy={historyLoading || historyLoadingMore || busyId !== null}>
      {feedback && <p role="status" aria-live="polite" className={feedback.error ? 'text-sm text-destructive' : 'text-sm text-muted-foreground'}>{t(feedback.key)}</p>}
      {historyError && <Alert variant="destructive"><AlertDescription>{t(errorKey(historyError))}</AlertDescription>
        <Button variant="outline" size="sm" isDisabled={isMutating} onPress={() => void readMore(true)}>{t('action.retry')}</Button>
      </Alert>}
      {historyLoading ? <p role="status" className="flex items-center gap-2 text-sm text-muted-foreground"><Spinner className="size-4" />{t('history.loading')}</p>
        : history.length === 0 && !historyError ? <p className="text-sm leading-relaxed text-muted-foreground">{t('history.empty')}</p> : null}
      {history.length > 0 && <ol className="divide-y" aria-label={t('history.entries')}>
        {history.map((item) => <li key={item.id} data-history-id={item.id} className="space-y-3 py-4 first:pt-0 last:pb-0">
          <div className="flex gap-3">
            {item.undone_at ? <RotateCcw className="mt-1 size-4 shrink-0 text-muted-foreground" aria-hidden="true" />
              : <Check className="mt-1 size-4 shrink-0 text-primary" aria-hidden="true" />}
            <div className="min-w-0 space-y-1">
              <h3 className="break-words font-medium">{item.description}</h3>
              <p className="text-sm text-muted-foreground">{t('history.completedAt')} <time dateTime={item.completed_at}>{when(item.completed_at)}</time></p>
              {item.scheduled_at && <p className="text-xs text-muted-foreground">{t('history.scheduledAt')} <time dateTime={item.scheduled_at}>{when(item.scheduled_at)}</time></p>}
              {item.undone_at && <p className="text-sm text-muted-foreground">{t('history.undoneAt')} <time dateTime={item.undone_at}>{when(item.undone_at)}</time></p>}
            </div>
          </div>
          {item.can_undo && !item.undone_at && <Button variant="outline" size="sm" className="min-h-10" isDisabled={isMutating || busyId !== null}
            aria-label={t('history.undoLabel', { description: item.description })} onPress={() => void undo(item.id)}>
            <RotateCcw aria-hidden="true" />{t(busyId === item.id ? 'action.saving' : 'history.undo')}
          </Button>}
          {!item.can_undo && !item.undone_at && <p className="text-xs text-muted-foreground">{t('history.cannotUndo')}</p>}
        </li>)}
      </ol>}
      {historyHasMore && <Button variant="outline" className="w-full" isDisabled={historyLoadingMore || isMutating}
        onPress={() => void readMore()}>{t(historyLoadingMore ? 'history.loading' : 'history.more')}</Button>}
      {history.length > 0 && <p className="text-xs leading-relaxed text-muted-foreground">{t('history.undoHelp')}</p>}
    </div>
  </Section>;
}
