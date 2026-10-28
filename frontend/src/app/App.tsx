import { Component, useEffect, useState, type ReactNode } from 'react';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { Bell, ExternalLink, RefreshCw } from 'lucide-react';
import { I18nProvider, useI18n } from '@/shared/i18n';
import { getTelegram, useTelegram } from '@/shared/lib/telegram';
import { Button, Card, CardContent, CardHeader, CardTitle, CardDescription, LinkButton } from '@/shared/ui';
import { RemindersPage } from '@/pages/reminders';

function identity() {
  const tg = getTelegram();
  return { auth: tg?.initData || '', account: String(tg?.initDataUnsafe?.user?.id ?? 'unknown') };
}
function LaunchScreen({ failed = false }: { failed?: boolean }) {
  const { t } = useI18n();
  return <main className="mx-auto flex min-h-dvh max-w-md items-center px-5">
    <Card className="w-full">
      <CardHeader><Bell className="mb-3 size-8" aria-hidden="true" /><CardTitle>{t(failed ? 'status.unavailable' : 'brand.name')}</CardTitle><CardDescription>{t(failed ? 'errors.generic' : 'errors.telegramRequired')}</CardDescription></CardHeader>
      <CardContent className="space-y-3">
        {failed && <Button className="w-full" onPress={() => window.location.reload()}><RefreshCw aria-hidden="true" />{t('action.retry')}</Button>}
        <LinkButton className="w-full" href="https://t.me/napominalozhka_bot"><ExternalLink aria-hidden="true" />{t('action.openBot')}</LinkButton>
      </CardContent>
    </Card>
  </main>;
}
class ScreenBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  render() { return this.state.failed ? <LaunchScreen failed /> : this.props.children; }
}
function Session({ accountKey, authenticated }: { accountKey: string; authenticated: boolean }) {
  const [client] = useState(() => new QueryClient({ defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false }, mutations: { retry: false } } }));
  useTelegram();
  useEffect(() => () => { client.clear(); }, [client]);
  return <I18nProvider><ScreenBoundary><QueryClientProvider client={client}>
    {authenticated ? <RemindersPage accountKey={accountKey} /> : <LaunchScreen />}
  </QueryClientProvider></ScreenBoundary></I18nProvider>;
}
export default function App() {
  const [session, setSession] = useState(() => ({ ...identity(), revision: 0 }));
  useEffect(() => {
    const update = () => {
      if (document.hidden) return;
      const next = identity();
      setSession(previous => previous.auth === next.auth && previous.account === next.account ? previous : { ...next, revision: previous.revision + 1 });
    };
    window.addEventListener('focus', update); document.addEventListener('visibilitychange', update);
    return () => { window.removeEventListener('focus', update); document.removeEventListener('visibilitychange', update); };
  }, []);
  const key = `${session.account}:${session.revision}`;
  return <Session key={key} accountKey={key} authenticated={Boolean(session.auth)} />;
}
