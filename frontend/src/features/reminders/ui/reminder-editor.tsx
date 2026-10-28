import { useRef, useState, type FormEvent } from 'react';
import { ArrowLeft, ChevronDown, ChevronUp, Repeat2, Save, Trash2 } from 'lucide-react';
import type { Recurrence, ReminderDTO } from '@/shared/api/contracts';
import { errorKey } from '@/shared/api';
import { useI18n } from '@/shared/i18n';
import { atTime, formatLocal, parseLocal, quickChoices } from '@/shared/lib/date';
import { confirmAction, haptic, useTelegramButtons } from '@/shared/lib/telegram';
import { AppSelect, Button, Card, CardContent, Input, Label, Textarea } from '@/shared/ui';
import { isMissingReminder, useReminderState, type ReminderInput } from '@/entities/reminder';

const recurrences: Recurrence[] = ['once', 'daily', 'weekdays', 'weekly', 'monthly', 'yearly'];
interface ReminderEditorProps {
  reminder?: ReminderDTO | null;
  onClose: () => void;
  onSaved?: (messageKey: string) => void;
}

export function ReminderEditor({ reminder, onClose, onSaved }: ReminderEditorProps) {
  const { t } = useI18n();
  const { state, isMutating, createReminder, updateReminder, deleteReminder, refresh } = useReminderState();
  // Capture once on mount. Polling and a recurring reminder advancing must never
  // replace a person's in-progress draft or its original schedule comparison.
  const [snapshot] = useState(() => ({
    reminder: reminder ? { ...reminder, at: reminder.at.slice(0, 16) } : null,
    now: state?.now || '',
    timezone: state?.timezone || '',
  }));
  const [description, setDescription] = useState(snapshot.reminder?.description || '');
  const [at, setAt] = useState(() => snapshot.reminder?.at || formatLocal(atTime(parseLocal(snapshot.now), 9, 0, 1)));
  const [recurrence, setRecurrence] = useState<Recurrence>(snapshot.reminder?.recurrence || 'once');
  const [repeatOpen, setRepeatOpen] = useState(recurrence !== 'once');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const locked = useRef(false);
  const descriptionRef = useRef<HTMLTextAreaElement>(null);
  const [date = '', time = ''] = at.split('T');

  function showError(key: string) {
    setError(key);
    haptic('error');
    if (key.startsWith('errors.description')) descriptionRef.current?.focus();
  }
  function back() { if (!locked.current && !isMutating) onClose(); }

  async function save() {
    if (locked.current || isMutating) return;
    const input: ReminderInput = { description: description.trim(), at, recurrence };
    if (!input.description) return showError('errors.descriptionRequired');
    if (Array.from(input.description).length > 1000) return showError('errors.descriptionTooLong');
    if (!date || !time || !Number.isFinite(parseLocal(at).getTime())) return showError('errors.dateRequired');
    if (snapshot.reminder && at === snapshot.reminder.at && recurrence === snapshot.reminder.recurrence) input.preserve_schedule = true;
    locked.current = true;
    setBusy(true);
    setError(null);
    try {
      if (snapshot.reminder) await updateReminder(snapshot.reminder.id, input);
      else await createReminder(input);
      haptic('success');
      onSaved?.('reminder.saved');
      onClose();
    } catch (cause) {
      showError(errorKey(cause));
    } finally {
      locked.current = false;
      setBusy(false);
    }
  }

  async function remove() {
    if (!snapshot.reminder || locked.current || isMutating) return;
    // Lock while the confirmation is visible to avoid stacking native popups.
    locked.current = true;
    setBusy(true);
    try {
      if (!await confirmAction(t('reminder.deleteConfirm'), t('action.delete'))) return;
      try {
        await deleteReminder(snapshot.reminder.id);
      } catch (cause) {
        if (!isMissingReminder(cause)) throw cause;
        await refresh();
      }
      haptic('success');
      onSaved?.('reminder.deleted');
      onClose();
    } catch (cause) {
      showError(errorKey(cause));
    } finally {
      locked.current = false;
      setBusy(false);
    }
  }

  const { hasNativeSave } = useTelegramButtons({
    onSave: () => { void save(); },
    onBack: back,
    saveLabel: t(snapshot.reminder ? 'action.saveChanges' : 'action.save'),
    canSave: !busy && !isMutating,
    busy,
  });
  const onSubmit = (event: FormEvent) => { event.preventDefault(); void save(); };

  return <section aria-labelledby="form-title" className="space-y-6">
    <Button type="button" variant="ghost" onPress={back} isDisabled={busy || isMutating}><ArrowLeft aria-hidden="true" />{t('action.back')}</Button>
    <div className="space-y-2">
      <p className="text-sm text-muted-foreground">{t('form.eyebrow')}</p>
      <h1 id="form-title" className="text-2xl font-semibold tracking-tight">{t(snapshot.reminder ? 'form.edit' : 'form.new')}</h1>
    </div>
    <form onSubmit={onSubmit} noValidate className="space-y-6">
      <Card className="py-0">
        <CardContent className="space-y-3 p-5">
          <Label htmlFor="description">{t('form.description')}</Label>
          <Textarea ref={descriptionRef} id="description" rows={2} value={description} placeholder={t('form.placeholder')} disabled={busy} aria-describedby="description-help" aria-invalid={error?.startsWith('errors.description') || undefined} onChange={(event) => { setDescription(event.target.value); setError(null); }} />
          <p id="description-help" className="text-sm text-muted-foreground">{t('form.descriptionHelp')}</p>
        </CardContent>
      </Card>
      <Card className="py-0">
        <CardContent className="space-y-4 p-5">
          <fieldset className="space-y-4" disabled={busy}>
            <legend className="font-medium">{t('form.when')}</legend>
            <div className="flex flex-wrap gap-2">
              {quickChoices(snapshot.now).map((choice) => <Button key={choice.labelKey} type="button" size="sm" variant={at === choice.value ? 'secondary' : 'outline'} aria-pressed={at === choice.value} onPress={() => { setAt(choice.value); setError(null); haptic(); }}>{t(choice.labelKey)}</Button>)}
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div className="min-w-0 space-y-2"><Label htmlFor="date">{t('form.date')}</Label><Input className="min-h-11" id="date" type="date" value={date} min={snapshot.now.slice(0, 10)} required onChange={(event) => { setAt(`${event.target.value}T${time}`); setError(null); }} /></div>
              <div className="min-w-0 space-y-2"><Label htmlFor="time">{t('form.time')}</Label><Input className="min-h-11" id="time" type="time" value={time} required onChange={(event) => { setAt(`${date}T${event.target.value}`); setError(null); }} /></div>
            </div>
            <p className="text-xs text-muted-foreground">{t('timezone.label')} {snapshot.timezone}</p>
          </fieldset>
        </CardContent>
      </Card>
      <Card className="py-0">
        <CardContent className="space-y-3 p-5">
          <Button type="button" variant="ghost" className="w-full justify-between" isDisabled={busy} aria-expanded={repeatOpen} aria-controls="repeat-controls" onPress={() => setRepeatOpen((previous) => !previous)}>
            <span className="flex items-center gap-2"><Repeat2 aria-hidden="true" />{t('form.repeat')}</span>
            <span className="flex items-center gap-2 text-muted-foreground">{t(`repeat.${recurrence}`)}{repeatOpen ? <ChevronUp aria-hidden="true" /> : <ChevronDown aria-hidden="true" />}</span>
          </Button>
          {repeatOpen && <div id="repeat-controls"><AppSelect id="repeat" ariaLabel={t('form.repeat')} value={recurrence} disabled={busy} onValueChange={(value) => { setRecurrence(value as Recurrence); setError(null); haptic(); }} options={recurrences.map((value) => ({ value, label: t(`repeat.${value}`) }))} /></div>}
        </CardContent>
      </Card>
      {error && <p role="alert" className="text-sm text-destructive">{t(error)}</p>}
      {!hasNativeSave && <Button type="submit" className="w-full" isDisabled={busy || isMutating}><Save aria-hidden="true" />{t(busy ? 'action.saving' : snapshot.reminder ? 'action.saveChanges' : 'action.save')}</Button>}
      {snapshot.reminder && <Button type="button" variant="destructive" className="w-full" isDisabled={busy || isMutating} onPress={() => void remove()}><Trash2 aria-hidden="true" />{t('action.delete')}</Button>}
    </form>
  </section>;
}
