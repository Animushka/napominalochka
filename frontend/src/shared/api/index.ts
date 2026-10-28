import { getTelegram } from "@/shared/lib/telegram";

export class ApiError extends Error {
  key: string;
  code?: string;
  status?: number;

  constructor(key: string, code?: string, status?: number) {
    super(key);
    this.name = "ApiError";
    this.key = key;
    this.code = code;
    this.status = status;
  }
}

export const errorKeys: Readonly<Record<string, string>> = {
  unauthorized: "errors.auth", authentication_required: "errors.auth", invalid_auth: "errors.auth", auth_expired: "errors.auth",
  not_found: "errors.notFound", reminder_not_found: "errors.notFound",
  invalid_request: "errors.invalidRequest", bad_request: "errors.invalidRequest", invalid_recurrence: "errors.invalidRequest",
  invalid_datetime: "errors.dateRequired", invalid_date: "errors.dateRequired", date_required: "errors.dateRequired",
  past_time: "errors.dateInPast", time_in_past: "errors.dateInPast", date_in_past: "errors.dateInPast", past_datetime: "errors.dateInPast",
  empty_description: "errors.descriptionRequired", description_required: "errors.descriptionRequired", description_too_long: "errors.descriptionTooLong",
  invalid_timezone: "errors.timezone", timezone_required: "errors.timezone",
  invalid_preferences: "errors.preferences", calendar_disabled: "errors.calendarDisabled", rate_limited: "errors.rateLimited",
  yandex_not_connected: "yandex.errors.notConnected", yandex_reconnect_required: "yandex.errors.reconnect",
  yandex_scenario_missing: "yandex.errors.scenarioMissing", yandex_rate_limited: "yandex.errors.rateLimited",
  yandex_unavailable: "yandex.errors.unavailable", yandex_invalid_state: "yandex.errors.invalidState",
  yandex_invalid_selection: "yandex.errors.invalidSelection", yandex_internal: "yandex.errors.generic",
  yandex_delivery_uncertain: "yandex.errors.deliveryUncertain",
};

const legacyErrors: Readonly<Record<string, string>> = {
  "Please pick a date and time.": "errors.dateRequired",
  "The calendar feed is not enabled.": "errors.calendarDisabled",
  "Please enter a description.": "errors.descriptionRequired",
  "The time must be in the future.": "errors.dateInPast",
};

export async function api<T>(method: string, path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
  const controller = new AbortController();
  let timedOut = false;
  const abort = () => controller.abort(signal?.reason);
  if (signal?.aborted) abort();
  else signal?.addEventListener("abort", abort, { once: true });
  const timeout = window.setTimeout(() => {
    timedOut = true;
    controller.abort();
  }, 15_000);

  try {
    const response = await fetch(path, {
      method,
      credentials: "same-origin",
      cache: "no-store",
      signal: controller.signal,
      headers: {
        Authorization: `tma ${getTelegram()?.initData || ""}`,
        ...(body !== undefined ? { "Content-Type": "application/json" } : {}),
      },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (response.status === 204) return null as T;
    const data: unknown = await response.json().catch(() => ({}));
    if (!response.ok) {
      const payload = data && typeof data === "object" ? data as Record<string, unknown> : {};
      const code = typeof payload.code === "string" ? payload.code : undefined;
      const legacy = typeof payload.error === "string" ? payload.error : "";
      const key = (code && errorKeys[code]) || legacyErrors[legacy]
        || ([401, 403].includes(response.status) ? "errors.auth" : response.status === 404 ? "errors.notFound"
          : response.status === 429 ? "errors.rateLimited" : response.status === 400 ? "errors.invalidRequest" : "errors.generic");
      throw new ApiError(key, code, response.status);
    }
    return data as T;
  } catch (error) {
    if (error instanceof ApiError) throw error;
    if (signal?.aborted && !timedOut) throw signal.reason ?? new DOMException("Aborted", "AbortError");
    throw new ApiError("errors.network");
  } finally {
    window.clearTimeout(timeout);
    signal?.removeEventListener("abort", abort);
  }
}

export const errorKey = (error: unknown): string => error instanceof ApiError ? error.key : "errors.generic";
export type * from "./contracts";
