export type Lang = "ru" | "en";
export type Recurrence = "once" | "daily" | "weekdays" | "weekly" | "monthly" | "yearly";
export type SnoozeDuration = "10m" | "1h" | "tomorrow";

export interface ReminderDTO {
  id: number;
  description: string;
  at: string;
  recurrence: Recurrence;
  when: string;
  relative: string;
  repeat: string;
}

export interface PendingDTO {
  id: number;
  reminder_id: number | null;
  description: string;
  next_at: string;
  next_when: string;
  relative: string;
}

export interface AppStateDTO {
  language: Lang;
  nag_interval_minutes: number;
  confirmation_check_minutes: number;
  timezone: string | null;
  timezone_id: string | null;
  now: string | null;
  reminders: ReminderDTO[];
  pending: PendingDTO[];
}

export interface YandexStatus {
  available: boolean;
  connected: boolean;
  enabled: boolean;
  requires_reconnect: boolean;
  scenario_id: string | null;
  scenario_name: string | null;
  last_error: string | null;
}

export interface Scenario {
  id: string;
  name: string;
  is_active: boolean;
}
