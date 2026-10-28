import { useEffect, useState } from 'react';
import { Bell, CheckCircle2, Clock3, Heart, Info, Plus, RefreshCw } from 'lucide-react';
import type { Lang, ReminderDTO } from '@/shared/api/contracts';
import { errorKey } from '@/shared/api';
import { useI18n } from '@/shared/i18n';
import { haptic } from '@/shared/lib/telegram';
import { Alert, AlertDescription, Avatar, AvatarFallback, AvatarImage, Button, Card, CardContent, Section, Spinner } from '@/shared/ui';
import { ReminderEditor, ReminderList, ReminderStateProvider, useReminderState } from '@/features/reminders';
import { Preferences } from '@/features/preferences';
import { Calendar } from '@/features/calendar';
import { AlicePanel } from '@/features/alice';

export function RemindersPage({ accountKey }: { accountKey: string }) {
  const [editing, setEditing] = useState<ReminderDTO | null | undefined>(undefined);
  return <ReminderStateProvider paused={editing !== undefined}>
    <Screen accountKey={accountKey} editing={editing} setEditing={setEditing} />
  </ReminderStateProvider>;
}
function Screen({ accountKey, editing, setEditing }: {
  accountKey: string; editing: ReminderDTO | null | undefined;
  setEditing: (reminder: ReminderDTO | null | undefined) => void;
}) {
  const { t, language } = useI18n();
  const { state, isLoading, error, refresh, updatePreferences, isMutating } = useReminderState();
  const [feedback, setFeedback] = useState<{ key: string; error?: boolean } | null>(null);
  useEffect(() => {
    if (!feedback) return;
    const timer = setTimeout(() => setFeedback(null), 4500);
    return () => clearTimeout(timer);
  }, [feedback]);
  const open = (reminder: ReminderDTO | null) => { setEditing(reminder ? { ...reminder } : null); setFeedback(null); window.scrollTo({ top: 0 }); haptic(); };
  const close = () => { setEditing(undefined); window.scrollTo({ top: 0 }); };
  async function changeLanguage(next: Lang) {
    if (isMutating || next === language) return;
    try { await updatePreferences({ language: next }); setFeedback({ key: 'settings.languageSaved' }); }
    catch (cause) { setFeedback({ key: errorKey(cause), error: true }); }
  }
  return <main className="mx-auto min-h-dvh w-full max-w-xl space-y-5 px-4 pt-5 pb-8 sm:px-6">
    <header className="flex items-center justify-between gap-3">
      <a className="flex min-w-0 items-center gap-2 font-semibold tracking-tight" href="https://t.me/napominalozhka_bot" aria-label={t('brand.telegram')}><Bell className="size-5 shrink-0" aria-hidden="true" /><span className="truncate">{t('brand.name')}</span></a>
      <div role="group" aria-label={t('language.label')} className="flex shrink-0 gap-1">
        {(['ru', 'en'] as const).map(lang => <Button key={lang} size="sm" variant={language === lang ? 'secondary' : 'ghost'} aria-pressed={language === lang} aria-label={t(`language.${lang}`)} isDisabled={!state || isMutating} onPress={() => void changeLanguage(lang)}>{t(`language.${lang}Short`)}</Button>)}
      </div>
    </header>
    {!state ? <Card><CardContent className="flex min-h-64 flex-col items-center justify-center gap-4 p-6 text-center">
      {isLoading ? <><Spinner className="size-7" /><p role="status">{t('status.loading')}</p></> : <><p role="alert">{t(errorKey(error))}</p><Button onPress={() => void refresh()}><RefreshCw aria-hidden="true" />{t('action.retry')}</Button></>}
    </CardContent></Card> : editing !== undefined ? <ReminderEditor key={editing?.id ?? 'new'} reminder={editing} onClose={close} onSaved={key => setFeedback({ key })} /> : <>
      <Card className="py-0"><CardContent className="flex items-center justify-between gap-4 py-4">
        <h1 className="text-xl font-semibold tracking-tight sm:text-2xl">{t('home.compactTitle')}</h1>
        <Avatar className="size-16"><AvatarImage src="/avatar.jpg" alt="" /><AvatarFallback><Bell className="size-10" /></AvatarFallback></Avatar>
      </CardContent></Card>
      <Button size="lg" className="min-h-11 w-full" onPress={() => open(null)}><Plus aria-hidden="true" />{t('action.add')}</Button>
      {error && <Alert variant="destructive"><AlertDescription>{t(errorKey(error))}</AlertDescription></Alert>}
      <ReminderList state={state} onEdit={open} />
      <p className="flex items-center justify-center gap-1.5 text-xs text-muted-foreground"><Clock3 className="size-3.5" aria-hidden="true" />{t('timezone.label')} {state.timezone}</p>
      <div className="space-y-3">
        <Preferences state={state} />
        <AlicePanel ready accountKey={accountKey} />
        <Calendar />
        <Section title={t('about.title')} description={t('about.subtitle')} icon={<Info aria-hidden="true" />}><p className="text-sm text-muted-foreground">{t('about.description')}</p></Section>
      </div>
      <footer className="flex items-center justify-center gap-2 py-3 text-sm text-muted-foreground"><Heart className="size-4" aria-hidden="true" />{t('home.footer')}</footer>
    </>}
    {feedback && <Alert role="status" variant={feedback.error ? 'destructive' : 'default'} className="fixed inset-x-4 bottom-4 z-50 mx-auto max-w-sm shadow-lg"><CheckCircle2 aria-hidden="true" /><AlertDescription>{t(feedback.key)}</AlertDescription></Alert>}
  </main>;
}
