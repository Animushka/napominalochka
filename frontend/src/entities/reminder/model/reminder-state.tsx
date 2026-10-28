import { createContext, useCallback, useContext, useEffect, useState, type PropsWithChildren } from 'react';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { api, ApiError } from '@/shared/api';
import type { AppStateDTO, Lang, Recurrence, ReminderDTO, SnoozeDuration } from '@/shared/api/contracts';
import { useI18n } from '@/shared/i18n';
import { deviceTimezone, offsetString } from '@/shared/lib/date';
import { getTelegram } from '@/shared/lib/telegram';
import { createOwnedSession } from './owned-session';

export interface ReminderInput {
  description: string;
  at: string;
  recurrence: Recurrence;
  preserve_schedule?: true;
}
export interface PreferencesPatch {
  language?: Lang;
  nag_interval_minutes?: number;
  confirmation_check_minutes?: number;
}

type StateCommand =
  | { type: 'preferences'; patch: PreferencesPatch }
  | { type: 'timezone'; timezone: string }
  | { type: 'create'; input: ReminderInput }
  | { type: 'update'; id: number; input: ReminderInput }
  | { type: 'delete'; id: number }
  | { type: 'done'; id: number }
  | { type: 'snooze'; id: number; duration: SnoozeDuration };

interface CommandResult { alreadyCompleted?: boolean }
function currentIdentity() {
  const telegram = getTelegram();
  return { owner: telegram?.initDataUnsafe?.user?.id ?? 'anonymous', credential: telegram?.initData ?? '' };
}

export function isMissingReminder(error: unknown): boolean {
  return error instanceof ApiError && (
    error.status === 404 || error.key === 'errors.notFound' ||
    ['not_found', 'pending_not_found', 'reminder_not_found', 'already_completed'].includes(error.code ?? '')
  );
}

function useReminderStateModel(paused: boolean) {
  const client = useQueryClient();
  const [session] = useState(() => createOwnedSession(currentIdentity(), currentIdentity, () => new ApiError('errors.auth')));
  const owner = session.owner;
  const stateKey = ['reminders', 'state', owner] as const;
  const mutationKey = ['reminders', 'state-write', owner] as const;
  const { setLanguage } = useI18n();
  const mutation = useMutation({
    mutationKey,
    scope: { id: `reminder-state-writes:${owner}` },
    retry: false,
    mutationFn: async (command: StateCommand): Promise<CommandResult> => {
      // Every write cancels earlier reads before contacting the server. A second
      // cancellation before committing protects against a focus event in flight.
      session.assertCurrent();
      await client.cancelQueries({ queryKey: stateKey });
      session.assertCurrent();
      let nextState: AppStateDTO | undefined;
      let alreadyCompleted = false;
      switch (command.type) {
        case 'preferences':
          nextState = await session.request(() => api<AppStateDTO>('PATCH', '/api/preferences', command.patch));
          session.assertCurrent();
          break;
        case 'timezone':
          nextState = await session.request(() => api<AppStateDTO>('PUT', '/api/timezone', { timezone: command.timezone }));
          session.assertCurrent();
          break;
        case 'create': {
          const reminder = await session.request(() => api<ReminderDTO>('POST', '/api/reminders', command.input));
          session.assertCurrent();
          const previous = client.getQueryData<AppStateDTO>(stateKey);
          if (previous) nextState = { ...previous, reminders: [...previous.reminders, reminder].sort((a, b) => a.at.localeCompare(b.at)) };
          break;
        }
        case 'update': {
          const reminder = await session.request(() => api<ReminderDTO>('PUT', `/api/reminders/${command.id}`, command.input));
          session.assertCurrent();
          const previous = client.getQueryData<AppStateDTO>(stateKey);
          if (previous) nextState = { ...previous, reminders: previous.reminders.map((item) => item.id === command.id ? reminder : item).sort((a, b) => a.at.localeCompare(b.at)) };
          break;
        }
        case 'delete': {
          try {
            await session.request(() => api<null>('DELETE', `/api/reminders/${command.id}`));
            session.assertCurrent();
          } catch (error) {
            if (!isMissingReminder(error)) throw error;
            nextState = await session.request(() => api<AppStateDTO>('GET', '/api/state'));
            session.assertCurrent();
            break;
          }
          const previous = client.getQueryData<AppStateDTO>(stateKey);
          if (previous) nextState = { ...previous, reminders: previous.reminders.filter((item) => item.id !== command.id), pending: previous.pending.filter((item) => item.reminder_id !== command.id) };
          break;
        }
        case 'done':
        case 'snooze':
          try {
            nextState = await session.request(() => api<AppStateDTO>('POST', `/api/pending/${command.id}/${command.type}`, command.type === 'snooze' ? { duration: command.duration } : undefined));
            session.assertCurrent();
          } catch (error) {
            if (!isMissingReminder(error)) throw error;
            // A Telegram action may already have completed this occurrence.
            nextState = await session.request(() => api<AppStateDTO>('GET', '/api/state'));
            session.assertCurrent();
            alreadyCompleted = true;
          }
          break;
      }
      session.assertCurrent();
      await client.cancelQueries({ queryKey: stateKey });
      session.assertCurrent();
      if (nextState) client.setQueryData(stateKey, nextState);
      if (command.type === 'create' || command.type === 'update' || command.type === 'delete') {
        // Commit the known success immediately, then reconcile server-managed
        // schedules. A failed refresh must not turn a saved reminder into a retry.
        try {
          const fresh = await session.request(() => api<AppStateDTO>('GET', '/api/state'));
          session.assertCurrent();
          await client.cancelQueries({ queryKey: stateKey });
          session.assertCurrent();
          client.setQueryData(stateKey, fresh);
        } catch {
          // Keep a successful write after a transient refresh failure, but never
          // swallow an account switch and report success to the new account.
          session.assertCurrent();
        }
      }
      session.assertCurrent();
      return { alreadyCompleted };
    },
  });

  const query = useQuery({
    queryKey: stateKey,
    queryFn: async ({ signal }) => {
      let state = await session.request(() => api<AppStateDTO>('GET', '/api/state', undefined, signal));
      session.assertCurrent();
      if (!state.timezone && !signal.aborted) {
        state = await session.request(() => api<AppStateDTO>('PUT', '/api/timezone', { timezone: deviceTimezone() || offsetString() }, signal));
        session.assertCurrent();
      }
      session.assertCurrent();
      return state;
    },
    enabled: !mutation.isPending && !paused,
    staleTime: 3000,
    retry: false,
    refetchInterval: paused ? false : 30_000,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
  });

  useEffect(() => {
    if (session.isCurrent() && (query.data?.language === 'ru' || query.data?.language === 'en')) setLanguage(query.data.language);
  }, [session, query.data?.language, setLanguage]);

  const refresh = useCallback(async () => {
    session.assertCurrent();
    if (paused || document.hidden || client.isMutating({ mutationKey: ['reminders', 'state-write', owner] })) return;
    await client.invalidateQueries({ queryKey: ['reminders', 'state', owner] });
    session.assertCurrent();
  }, [client, owner, paused, session]);

  useEffect(() => {
    const onVisible = () => { if (!document.hidden) void refresh().catch(() => {}); };
    window.addEventListener('focus', onVisible);
    document.addEventListener('visibilitychange', onVisible);
    return () => {
      window.removeEventListener('focus', onVisible);
      document.removeEventListener('visibilitychange', onVisible);
    };
  }, [refresh]);

  const run = mutation.mutateAsync;
  return {
    state: query.data,
    isLoading: query.isPending,
    error: query.error,
    isMutating: mutation.isPending,
    refresh,
    updatePreferences: (patch: PreferencesPatch) => run({ type: 'preferences', patch }),
    setTimezone: (timezone: string) => run({ type: 'timezone', timezone }),
    createReminder: (input: ReminderInput) => run({ type: 'create', input }),
    updateReminder: (id: number, input: ReminderInput) => run({ type: 'update', id, input }),
    deleteReminder: (id: number) => run({ type: 'delete', id }),
    doneReminder: (id: number) => run({ type: 'done', id }),
    snoozeReminder: (id: number, duration: SnoozeDuration) => run({ type: 'snooze', id, duration }),
  };
}

type ReminderState = ReturnType<typeof useReminderStateModel>;
const ReminderStateContext = createContext<ReminderState | null>(null);

export function ReminderStateProvider({ children, paused = false }: PropsWithChildren<{ paused?: boolean }>) {
  const value = useReminderStateModel(paused);
  return <ReminderStateContext.Provider value={value}>{children}</ReminderStateContext.Provider>;
}
export function useReminderState() {
  const value = useContext(ReminderStateContext);
  if (!value) throw new Error('ReminderStateProvider is required');
  return value;
}
