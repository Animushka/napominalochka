import { useRef, useState } from 'react';
import { CalendarDays, Check, Copy, RefreshCw } from 'lucide-react';
import { api, errorKey } from '@/shared/api';
import { useI18n } from '@/shared/i18n';
import { confirmAction, haptic } from '@/shared/lib/telegram';
import { Button, Input, Label, Section } from '@/shared/ui';

interface CalendarLink { url: string; webcal: string }

export function Calendar() {
  const { t } = useI18n();
  // Subscription credentials are kept only in this mounted feature; never in
  // localStorage, the shared query cache, telemetry or browser console output.
  const [link, setLink] = useState<CalendarLink | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [status, setStatus] = useState<{ key: string; error?: boolean } | null>(null);
  const locked = useRef(false);
  const inputRef = useRef<HTMLInputElement>(null);

  async function load(reset = false) {
    if (locked.current) return;
    locked.current = true;
    setBusy(true);
    setStatus(null);
    try {
      if (reset && !await confirmAction(t('calendar.resetConfirm'), t('calendar.reset'))) return;
      const next = await api<CalendarLink>(reset ? 'POST' : 'GET', reset ? '/api/calendar/reset' : '/api/calendar');
      setLink(next);
      setCopied(false);
      if (reset) setStatus({ key: 'calendar.resetDone' });
    } catch (error) {
      setStatus({ key: errorKey(error), error: true });
    } finally {
      locked.current = false;
      setBusy(false);
    }
  }

  async function copy() {
    if (!link || busy) return;
    try {
      await navigator.clipboard.writeText(link.webcal);
      setCopied(true);
      setStatus({ key: 'calendar.copied' });
      haptic('success');
    } catch {
      inputRef.current?.focus();
      inputRef.current?.select();
      setStatus({ key: 'calendar.copyManual' });
    }
  }

  return <Section title={t('calendar.title')} description={t('calendar.subtitle')} icon={<CalendarDays aria-hidden="true" />}>
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">{t('calendar.description')}</p>
      <p className="text-sm text-muted-foreground">{t('calendar.instructions')}</p>
      {!link ? <Button type="button" variant="outline" isDisabled={busy} onPress={() => void load()}><CalendarDays aria-hidden="true" />{t(busy ? 'status.loading' : 'calendar.link')}</Button> : <>
        <div className="space-y-2">
          <Label htmlFor="calendar-url">{t('calendar.link')}</Label>
          <Input ref={inputRef} id="calendar-url" type="text" value={link.webcal} readOnly spellCheck={false} autoComplete="off" />
        </div>
        <div className="flex flex-wrap gap-2">
          <Button type="button" variant="secondary" isDisabled={busy} onPress={() => void copy()}>{copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}{t(copied ? 'calendar.copied' : 'calendar.copy')}</Button>
          <Button type="button" variant="outline" isDisabled={busy} onPress={() => void load(true)}><RefreshCw aria-hidden="true" />{t('calendar.reset')}</Button>
        </div>
        <p className="text-xs text-muted-foreground">{t('calendar.privacy')}</p>
      </>}
      {status && <p role="status" aria-live="polite" className={status.error ? 'text-sm text-destructive' : 'text-sm text-muted-foreground'}>{t(status.key)}</p>}
    </div>
  </Section>;
}
