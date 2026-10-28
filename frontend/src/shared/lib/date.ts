import type { Translate } from "@/shared/i18n";
import ru from "@/shared/i18n/messages/ru.json";
import en from "@/shared/i18n/messages/en.json";

const pad = (value: number) => String(value).padStart(2, "0");

// Dates here are UTC containers for the selected timezone's wall-clock values.
// Using device-local getters would shift reminders when the device zone differs.
export const parseLocal = (value: string): Date => new Date(`${value.slice(0, 16)}:00Z`);
export const formatLocal = (date: Date): string => `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}T${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}`;
export const addMinutes = (date: Date, minutes: number): Date => new Date(date.getTime() + minutes * 60_000);

export function atTime(date: Date, hours: number, minutes: number, days = 0): Date {
  const result = new Date(date.getTime());
  result.setUTCDate(result.getUTCDate() + days);
  result.setUTCHours(hours, minutes, 0, 0);
  return result;
}

export function formatWhen(value: string, nowValue: string, t: Translate, locale: string): string {
  const date = parseLocal(value);
  const now = parseLocal(nowValue);
  const time = new Intl.DateTimeFormat(locale, { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone: "UTC" }).format(date);
  const day = value.slice(0, 10);
  if (day === formatLocal(now).slice(0, 10)) return t("date.today", { time });
  if (day === formatLocal(atTime(now, 0, 0, 1)).slice(0, 10)) return t("date.tomorrow", { time });
  if (day === formatLocal(atTime(now, 0, 0, -1)).slice(0, 10)) return t("date.yesterday", { time });
  const label = new Intl.DateTimeFormat(locale, { day: "numeric", month: "short", ...(date.getUTCFullYear() !== now.getUTCFullYear() ? { year: "numeric" } : {}), timeZone: "UTC" }).format(date);
  return t("date.at", { date: label, time });
}

export function formatRelative(value: string, nowValue: string, locale: string): string {
  const minutes = Math.round((parseLocal(value).getTime() - parseLocal(nowValue).getTime()) / 60_000);
  const messages = locale.startsWith("ru") ? ru : en;
  if (minutes < 0) return messages["date.overdue"];
  if (minutes === 0) return messages["date.now"];
  const unit = minutes < 60 ? "minute" : minutes < 1440 ? "hour" : "day";
  const amount = unit === "minute" ? minutes : unit === "hour" ? Math.round(minutes / 60) : Math.round(minutes / 1440);
  return new Intl.RelativeTimeFormat(locale, { numeric: "always" }).format(amount, unit);
}

export function quickChoices(nowValue: string | Date): { labelKey: string; value: string }[] {
  const now = typeof nowValue === "string" ? parseLocal(nowValue) : nowValue;
  const evening = atTime(now, 20, 0);
  const lateEvening = now > addMinutes(evening, -30);
  const daysToMonday = ((8 - now.getUTCDay()) % 7) || 7;
  return [
    { labelKey: "quick.15min", value: formatLocal(addMinutes(now, 15)) },
    { labelKey: "quick.hour", value: formatLocal(addMinutes(now, 60)) },
    { labelKey: lateEvening ? "quick.tomorrowEvening" : "quick.evening", value: formatLocal(lateEvening ? atTime(now, 20, 0, 1) : evening) },
    { labelKey: "quick.tomorrow", value: formatLocal(atTime(now, 9, 0, 1)) },
    { labelKey: "quick.monday", value: formatLocal(atTime(now, 9, 0, daysToMonday)) },
  ];
}

export const deviceTimezone = (): string => Intl.DateTimeFormat().resolvedOptions().timeZone;

export function offsetString(): string {
  const minutes = -new Date().getTimezoneOffset();
  return `${minutes < 0 ? "-" : "+"}${pad(Math.floor(Math.abs(minutes) / 60))}:${pad(Math.abs(minutes) % 60)}`;
}
