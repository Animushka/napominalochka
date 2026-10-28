import { detectLanguage, getLanguage, getLocale, loadDictionaries, setLanguage, supportedLanguage, t } from "/assets/i18n.js";
import { createYandexController } from "/assets/yandex.js";

const tg = window.Telegram?.WebApp;
const $ = (id) => document.getElementById(id);
const REPEATS = ["once", "daily", "weekdays", "weekly", "monthly", "yearly"];
const INTERVALS = [1, 5, 10, 20, 30, 60];
let state = null;
let editing = null;
let loadedSchedule = null;
let repeat = "once";
let view = "loading";
let saving = false;
let preferencesSaving = false;
let fatalKey = "errors.generic";
let formErrorKey = null;
let toastTimer;
let refreshing = false;
let refreshedAt = 0;
let stateGeneration = 0;
let pendingBusy = null;
let snoozeId = null;
const nativeSave = Boolean(tg?.MainButton?.setParams);

const pad = (value) => String(value).padStart(2, "0");
// These Dates represent wall-clock values in the user's selected timezone, not the device's zone.
const parse = (value) => new Date(`${value.slice(0, 16)}:00Z`);
const fmt = (date) => `${date.getUTCFullYear()}-${pad(date.getUTCMonth() + 1)}-${pad(date.getUTCDate())}T${pad(date.getUTCHours())}:${pad(date.getUTCMinutes())}`;
const addMinutes = (date, minutes) => new Date(date.getTime() + minutes * 60000);
function atTime(date, hours, minutes, days = 0) {
  const result = new Date(date.getTime());
  result.setUTCDate(result.getUTCDate() + days);
  result.setUTCHours(hours, minutes, 0, 0);
  return result;
}
function node(tag, className, text) {
  const element = document.createElement(tag);
  if (className) element.className = className;
  if (text !== undefined) element.textContent = text;
  return element;
}
function telegramCall(callback) { try { callback(); } catch { /* Older Telegram clients can omit optional capabilities. */ } }
function haptic(type = "selection") {
  telegramCall(() => type === "selection" ? tg?.HapticFeedback?.selectionChanged() : tg?.HapticFeedback?.notificationOccurred(type));
}

class ApiError extends Error {
  constructor(key, code) { super(key); this.key = key; this.code = code; }
}
const ERROR_KEYS = {
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
// Compatibility with older servers; never expose arbitrary server text or internal errors.
const LEGACY_ERRORS = {
  "Please pick a date and time.": "errors.dateRequired",
  "The calendar feed is not enabled.": "errors.calendarDisabled",
  "Please enter a description.": "errors.descriptionRequired",
  "The time must be in the future.": "errors.dateInPast",
};
async function api(method, path, body) {
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), 15000);
  try {
    const response = await fetch(path, {
      method, credentials: "same-origin", cache: "no-store", signal: controller.signal,
      headers: { Authorization: `tma ${tg?.initData || ""}`, ...(body !== undefined ? { "Content-Type": "application/json" } : {}) },
      body: body !== undefined ? JSON.stringify(body) : undefined,
    });
    if (response.status === 204) return null;
    const data = await response.json().catch(() => ({}));
    if (!response.ok) {
      const key = ERROR_KEYS[data.code] || LEGACY_ERRORS[data.error]
        || ([401, 403].includes(response.status) ? "errors.auth" : response.status === 404 ? "errors.notFound"
          : response.status === 429 ? "errors.rateLimited" : response.status === 400 ? "errors.invalidRequest" : "errors.generic");
      throw new ApiError(key, data.code);
    }
    return data;
  } catch (error) {
    if (error instanceof ApiError) throw error;
    throw new ApiError("errors.network");
  } finally { window.clearTimeout(timeout); }
}
const errorKey = (error) => error instanceof ApiError ? error.key : "errors.generic";
const yandex = createYandexController({ api, t, tg, errorKey, errorKeys: ERROR_KEYS, confirmAction, haptic });

function syncNativeButtons() {
  telegramCall(() => {
    if (view === "form-view") {
      tg?.BackButton?.show();
      tg?.MainButton?.setParams({ text: t(editing ? "action.saveChanges" : "action.save"), is_visible: true, is_active: !saving, color: "#f1a18a", text_color: "#7e3827" });
    } else {
      tg?.BackButton?.hide();
      tg?.MainButton?.hide();
    }
  });
  $("save").hidden = nativeSave;
  $("save").disabled = saving;
  $("save").textContent = t(saving ? "action.saving" : editing ? "action.saveChanges" : "action.save");
}
function show(nextView) {
  view = nextView;
  for (const id of ["loading", "fatal", "list-view", "form-view"]) $(id).hidden = id !== nextView;
  syncNativeButtons();
}
function fatal(key) {
  fatalKey = key;
  $("fatal-message").textContent = t(key);
  $("retry").hidden = key === "errors.telegramRequired" || key === "errors.auth";
  show("fatal");
}
function toast(key) {
  window.clearTimeout(toastTimer);
  $("toast").textContent = t(key);
  $("toast").hidden = false;
  toastTimer = window.setTimeout(() => { $("toast").hidden = true; }, 3400);
}
function formError(key) {
  formErrorKey = key;
  $("form-error").textContent = t(key);
  $("form-error").hidden = false;
  $("description").setAttribute("aria-invalid", String(key.startsWith("errors.description")));
  if (key.startsWith("errors.description")) $("description").focus();
  haptic("error");
}
function clearFormError() {
  formErrorKey = null;
  $("form-error").hidden = true;
  $("description").removeAttribute("aria-invalid");
}
function applyLanguage(language) {
  setLanguage(language);
  if (state) {
    renderList();
    if (view === "form-view") renderFormControls();
    renderPreferences();
  }
  yandex.render();
  if (view === "fatal") $("fatal-message").textContent = t(fatalKey);
  if (formErrorKey) $("form-error").textContent = t(formErrorKey);
  $("toast").hidden = true;
  syncNativeButtons();
}

function formatWhen(value) {
  const date = parse(value);
  const time = new Intl.DateTimeFormat(getLocale(), { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone: "UTC" }).format(date);
  const day = value.slice(0, 10);
  const now = parse(state.now);
  if (day === fmt(now).slice(0, 10)) return t("date.today", { time });
  if (day === fmt(atTime(now, 0, 0, 1)).slice(0, 10)) return t("date.tomorrow", { time });
  if (day === fmt(atTime(now, 0, 0, -1)).slice(0, 10)) return t("date.yesterday", { time });
  const label = new Intl.DateTimeFormat(getLocale(), { day: "numeric", month: "short", ...(date.getUTCFullYear() !== now.getUTCFullYear() ? { year: "numeric" } : {}), timeZone: "UTC" }).format(date);
  return t("date.at", { date: label, time });
}
function formatRelative(value) {
  const minutes = Math.round((parse(value) - parse(state.now)) / 60000);
  if (minutes < 0) return t("date.overdue");
  if (minutes === 0) return t("date.now");
  const unit = minutes < 60 ? "minute" : minutes < 1440 ? "hour" : "day";
  const amount = unit === "minute" ? minutes : unit === "hour" ? Math.round(minutes / 60) : Math.round(minutes / 1440);
  return new Intl.RelativeTimeFormat(getLocale(), { numeric: "always" }).format(amount, unit);
}
function renderList() {
  const focusedReminder = document.activeElement?.dataset.reminderId;
  $("tz-name").textContent = state.timezone || "";
  $("form-tz-name").textContent = state.timezone || "";
  const compact = Boolean(state.reminders.length || state.pending?.length);
  $("home-hero").classList.toggle("hero-compact", compact);
  $("home-title").dataset.i18n = compact ? "home.compactTitle" : "home.title";
  $("home-title").textContent = t($("home-title").dataset.i18n);
  renderPending();
  $("reminder-count").textContent = new Intl.NumberFormat(getLocale()).format(state.reminders.length);
  const list = $("list");
  list.replaceChildren();
  if (!state.reminders.length) {
    const empty = node("div", "empty-state");
    const illustration = node("span", "empty-illustration", "✧");
    illustration.setAttribute("aria-hidden", "true");
    empty.append(illustration, node("p", "empty-title", t(state.pending?.length ? "empty.scheduledTitle" : "empty.title")), node("p", "empty-description", t(state.pending?.length ? "empty.scheduledDescription" : "empty.description")));
    list.append(empty);
  }
  for (const reminder of state.reminders) {
    const item = node("button", "reminder-item");
    item.type = "button";
    item.dataset.reminderId = String(reminder.id);
    item.dataset.overdue = String(reminder.at < state.now);
    item.setAttribute("aria-label", t("reminder.edit", { description: reminder.description }));
    const marker = node("span", "reminder-marker", reminder.recurrence === "once" ? "◷" : "↻");
    marker.setAttribute("aria-hidden", "true");
    const content = node("div", "reminder-content");
    const title = node("div", "reminder-title", reminder.description);
    const meta = node("div", "reminder-meta");
    const when = node("time", "", formatWhen(reminder.at));
    when.dateTime = reminder.at;
    meta.append(when, document.createTextNode(` · ${formatRelative(reminder.at)}`));
    const recurrence = REPEATS.includes(reminder.recurrence) ? reminder.recurrence : "once";
    if (recurrence !== "once") meta.append(node("div", "reminder-recurrence", `↻ ${reminder.repeat || t(`repeat.${recurrence}`)}`));
    content.append(title, meta);
    const arrow = node("span", "reminder-arrow", "›");
    arrow.setAttribute("aria-hidden", "true");
    item.append(marker, content, arrow);
    item.addEventListener("click", () => openForm(reminder));
    list.append(item);
  }
  renderTimezoneBanner();
  if (focusedReminder) Array.from(list.children).find((item) => item.dataset.reminderId === focusedReminder)?.focus({ preventScroll: true });
}
function renderPending() {
  const pending = Array.isArray(state.pending) ? state.pending : [];
  const focusKey = document.activeElement?.dataset.pendingFocus;
  $("pending-section").hidden = !pending.length;
  $("pending-count").textContent = new Intl.NumberFormat(getLocale()).format(pending.length);
  const list = $("pending-list");
  list.replaceChildren();
  if (!pending.some((item) => item.id === snoozeId)) snoozeId = null;
  for (const pendingItem of pending) {
    const item = node("article", "pending-item");
    item.dataset.pendingId = String(pendingItem.id);
    const title = node("h3", "reminder-title", pendingItem.description);
    const meta = node("p", "reminder-meta");
    meta.append(document.createTextNode(t("pending.nextMessage") + " "));
    const next = node("time", "", pendingItem.next_at ? formatWhen(pendingItem.next_at) : pendingItem.next_when || t("date.now"));
    if (pendingItem.next_at) next.dateTime = pendingItem.next_at;
    meta.append(next);
    const actions = node("div", "button-row pending-actions");
    const done = node("button", "button button-secondary", t(pendingBusy === pendingItem.id ? "pending.saving" : "pending.done"));
    done.type = "button";
    done.dataset.pendingFocus = `done-${pendingItem.id}`;
    done.disabled = pendingBusy !== null;
    done.setAttribute("aria-label", t("pending.doneLabel", { description: pendingItem.description }));
    done.addEventListener("click", () => { void actOnPending(pendingItem.id, "done"); });
    const snooze = node("button", "button button-quiet", t("pending.snooze"));
    snooze.type = "button";
    snooze.dataset.pendingFocus = `snooze-${pendingItem.id}`;
    snooze.disabled = pendingBusy !== null;
    snooze.setAttribute("aria-expanded", String(snoozeId === pendingItem.id));
    snooze.setAttribute("aria-controls", `snooze-${pendingItem.id}`);
    snooze.addEventListener("click", () => { snoozeId = snoozeId === pendingItem.id ? null : pendingItem.id; renderPending(); });
    actions.append(done, snooze);
    item.append(title, meta, actions);
    const choices = node("div", "chips snooze-choices");
    choices.id = `snooze-${pendingItem.id}`;
    choices.hidden = snoozeId !== pendingItem.id;
    choices.setAttribute("role", "group");
    choices.setAttribute("aria-label", t("pending.snooze"));
    for (const duration of ["10m", "1h", "tomorrow"]) {
      const choice = node("button", "chip", t(`pending.${duration}`));
      choice.type = "button";
      choice.disabled = pendingBusy !== null;
      choice.dataset.pendingFocus = `${duration}-${pendingItem.id}`;
      choice.addEventListener("click", () => { void actOnPending(pendingItem.id, "snooze", duration); });
      choices.append(choice);
    }
    item.append(choices);
    list.append(item);
  }
  if (focusKey) Array.from(list.querySelectorAll("[data-pending-focus]")).find((button) => button.dataset.pendingFocus === focusKey)?.focus({ preventScroll: true });
}
async function actOnPending(id, action, duration) {
  if (pendingBusy !== null) return;
  pendingBusy = id;
  stateGeneration++;
  renderPending();
  try {
    state = await api("POST", `/api/pending/${id}/${action}`, action === "snooze" ? { duration } : undefined);
    snoozeId = null;
    renderList();
    haptic("success");
    toast(action === "done" ? "pending.completed" : "pending.snoozed");
  } catch (error) {
    if (["not_found", "pending_not_found", "already_completed"].includes(error.code) || errorKey(error) === "errors.notFound") {
      try { state = await api("GET", "/api/state"); renderList(); toast("pending.alreadyCompleted"); }
      catch (refreshError) { toast(errorKey(refreshError)); }
    } else toast(errorKey(error));
  } finally { pendingBusy = null; renderPending(); }
}
// A background refresh updates only the list; form values and open settings keep their state.
async function refreshVisibleState({ force = false } = {}) {
  if (!state || document.hidden || view !== "list-view" || refreshing || saving || preferencesSaving || pendingBusy !== null || (!force && Date.now() - refreshedAt < 3000)) return;
  refreshing = true;
  const generation = stateGeneration;
  try {
    const fresh = await api("GET", "/api/state");
    if (generation !== stateGeneration) return;
    state = fresh;
    renderList();
  } catch { /* Keep the last usable list; mutations still surface their own errors. */ }
  finally { refreshing = false; refreshedAt = Date.now(); }
}
function deviceTimezone() { return Intl.DateTimeFormat().resolvedOptions().timeZone; }
function renderTimezoneBanner() {
  const device = deviceTimezone();
  const banner = $("tz-banner");
  banner.replaceChildren();
  banner.hidden = !device || device === state.timezone_id;
  if (banner.hidden) return;
  const text = node("p", "", t("timezone.mismatch", { device, timezone: state.timezone }));
  const button = node("button", "text-link", t("timezone.use", { timezone: device }));
  button.type = "button";
  button.addEventListener("click", async () => {
    button.disabled = true;
    stateGeneration++;
    try {
      state = await api("PUT", "/api/timezone", { timezone: device });
      renderList();
      toast("timezone.saved");
    } catch (error) { toast(errorKey(error)); button.disabled = false; }
  });
  banner.append(text, button);
}
function quickChoices() {
  const now = parse(state.now);
  const evening = atTime(now, 20, 0);
  const lateEvening = now > addMinutes(evening, -30);
  const daysToMonday = ((8 - now.getUTCDay()) % 7) || 7;
  return [
    ["quick.15min", addMinutes(now, 15)], ["quick.hour", addMinutes(now, 60)],
    [lateEvening ? "quick.tomorrowEvening" : "quick.evening", lateEvening ? atTime(now, 20, 0, 1) : evening],
    ["quick.tomorrow", atTime(now, 9, 0, 1)], ["quick.monday", atTime(now, 9, 0, daysToMonday)],
  ];
}
function setWhen(value) {
  const [date, time] = value.split("T");
  $("date").value = date;
  $("time").value = time;
  highlightQuick();
}
function highlightQuick() {
  const value = `${$("date").value}T${$("time").value}`;
  for (const chip of $("quick").children) chip.setAttribute("aria-pressed", String(chip.dataset.value === value));
}
function setRepeat(value) {
  repeat = REPEATS.includes(value) ? value : "once";
  $("repeat-summary").textContent = t(`repeat.${repeat}`);
  for (const chip of $("repeat").children) chip.setAttribute("aria-pressed", String(chip.dataset.value === repeat));
}
function renderFormControls() {
  $("form-title").textContent = t(editing ? "form.edit" : "form.new");
  $("form-title").dataset.i18n = editing ? "form.edit" : "form.new";
  $("quick").replaceChildren();
  for (const [key, date] of quickChoices()) {
    const chip = node("button", "chip", t(key));
    chip.type = "button";
    chip.dataset.value = fmt(date);
    chip.addEventListener("click", () => { setWhen(chip.dataset.value); clearFormError(); haptic(); });
    $("quick").append(chip);
  }
  $("repeat").replaceChildren();
  for (const value of REPEATS) {
    const chip = node("button", "chip", t(`repeat.${value}`));
    chip.type = "button";
    chip.dataset.value = value;
    chip.addEventListener("click", () => { setRepeat(value); haptic(); });
    $("repeat").append(chip);
  }
  highlightQuick();
  setRepeat(repeat);
}
function openForm(reminder) {
  editing = reminder;
  repeat = reminder?.recurrence || "once";
  $("repeat-panel").open = repeat !== "once";
  $("description").value = reminder?.description || "";
  $("delete").hidden = !reminder;
  $("date").min = state.now.slice(0, 10);
  clearFormError();
  renderFormControls();
  setWhen(reminder?.at || fmt(atTime(parse(state.now), 9, 0, 1)));
  loadedSchedule = reminder ? { at: `${$("date").value}T${$("time").value}`, recurrence: repeat } : null;
  show("form-view");
  window.scrollTo({ top: 0 });
}
function backToList() {
  if (saving) return;
  editing = null;
  renderList();
  show("list-view");
  void refreshVisibleState({ force: true });
  window.scrollTo({ top: 0 });
}
async function save(event) {
  event?.preventDefault();
  if (saving || view !== "form-view") return;
  const body = { description: $("description").value.trim(), at: `${$("date").value}T${$("time").value}`, recurrence: repeat };
  if (!body.description) return formError("errors.descriptionRequired");
  if (Array.from(body.description).length > 1000) return formError("errors.descriptionTooLong");
  if (!$("date").value || !$("time").value) return formError("errors.dateRequired");
  // A description edit must not rewind a recurring schedule advanced while this form was open.
  if (editing && body.at === loadedSchedule?.at && body.recurrence === loadedSchedule.recurrence) body.preserve_schedule = true;
  clearFormError();
  saving = true;
  stateGeneration++;
  syncNativeButtons();
  telegramCall(() => tg?.MainButton?.showProgress());
  $("delete").disabled = true;
  try {
    if (editing) await api("PUT", `/api/reminders/${editing.id}`, body);
    else await api("POST", "/api/reminders", body);
    haptic("success");
    await reload();
    toast("reminder.saved");
  } catch (error) { formError(errorKey(error)); }
  finally {
    saving = false;
    $("delete").disabled = false;
    telegramCall(() => tg?.MainButton?.hideProgress());
    syncNativeButtons();
  }
}
function confirmAction(key, confirmKey = key === "reminder.deleteConfirm" ? "action.delete" : "calendar.reset") {
  return new Promise((resolve) => {
    if (tg?.showPopup && tg.isVersionAtLeast?.("6.2")) {
      tg.showPopup({ message: t(key), buttons: [{ id: "cancel", type: "cancel", text: t("action.cancel") }, { id: "confirm", type: "destructive", text: t(confirmKey) }] }, (id) => resolve(id === "confirm"));
    } else if (tg?.showConfirm) tg.showConfirm(t(key), resolve);
    else resolve(window.confirm(t(key)));
  });
}
async function deleteReminder() {
  if (!editing || saving || !await confirmAction("reminder.deleteConfirm")) return;
  saving = true;
  stateGeneration++;
  $("delete").disabled = true;
  syncNativeButtons();
  try {
    await api("DELETE", `/api/reminders/${editing.id}`);
    await reload();
    haptic("success");
    toast("reminder.deleted");
  } catch (error) { formError(errorKey(error)); }
  finally { saving = false; $("delete").disabled = false; syncNativeButtons(); }
}
function offsetString() {
  const minutes = -new Date().getTimezoneOffset();
  return `${minutes < 0 ? "-" : "+"}${pad(Math.floor(Math.abs(minutes) / 60))}:${pad(Math.abs(minutes) % 60)}`;
}
function renderPreferences() {
  $("nag-interval").value = String(INTERVALS.includes(state.nag_interval_minutes) ? state.nag_interval_minutes : 10);
  $("confirmation-check").checked = state.confirmation_check_minutes === 20;
}
function lockPreferences(locked) {
  preferencesSaving = locked;
  $("nag-interval").disabled = locked;
  $("confirmation-check").disabled = locked;
  for (const button of document.querySelectorAll("[data-language]")) button.disabled = locked || !state;
}
async function updatePreferences(patch) {
  if (preferencesSaving || !state) return;
  lockPreferences(true);
  stateGeneration++;
  $("preferences-status").classList.remove("is-error");
  $("preferences-status").textContent = t("settings.saving");
  const previousLanguage = getLanguage();
  if (patch.language) applyLanguage(patch.language);
  try {
    state = await api("PATCH", "/api/preferences", patch);
    if (supportedLanguage(state.language)) applyLanguage(state.language);
    renderPreferences();
    $("preferences-status").textContent = t("settings.saved");
    if (patch.language) toast("settings.languageSaved");
    haptic("success");
  } catch (error) {
    if (patch.language) applyLanguage(previousLanguage);
    renderPreferences();
    $("preferences-status").classList.add("is-error");
    $("preferences-status").textContent = t(errorKey(error));
    toast(errorKey(error));
  } finally { lockPreferences(false); }
}
async function reload() {
  stateGeneration++;
  state = await api("GET", "/api/state");
  refreshedAt = Date.now();
  if (!state.timezone) state = await api("PUT", "/api/timezone", { timezone: deviceTimezone() || offsetString() });
  editing = null;
  if (supportedLanguage(state.language)) applyLanguage(state.language);
  else applyLanguage(getLanguage());
  renderList();
  renderPreferences();
  lockPreferences(false);
  show("list-view");
  void yandex.refresh();
}
async function showCalendar(reset = false) {
  const button = $(reset ? "calendar-reset" : "calendar-toggle");
  button.disabled = true;
  try {
    const link = await api(reset ? "POST" : "GET", reset ? "/api/calendar/reset" : "/api/calendar");
    $("calendar-url").value = link.webcal;
    $("calendar").hidden = false;
    $("calendar-toggle").setAttribute("aria-expanded", "true");
    $("calendar-copy").textContent = t("calendar.copy");
    if (reset) toast("calendar.resetDone");
  } catch (error) { toast(errorKey(error)); }
  finally { button.disabled = false; }
}
function syncTheme() {
  const dark = tg?.colorScheme === "dark" || (!tg && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  const color = getComputedStyle(document.documentElement).getPropertyValue("--bg").trim();
  document.querySelector('meta[name="theme-color"]').content = color;
  telegramCall(() => tg?.setHeaderColor?.(color));
  telegramCall(() => tg?.setBackgroundColor?.(color));
}

$("avatar").addEventListener("error", () => { $("avatar").hidden = true; $("avatar-fallback").hidden = false; });
if ($("avatar").complete && !$("avatar").naturalWidth) { $("avatar").hidden = true; $("avatar-fallback").hidden = false; }
for (const button of document.querySelectorAll("[data-language]")) {
  button.disabled = true;
  button.addEventListener("click", () => { if (button.dataset.language !== getLanguage()) updatePreferences({ language: button.dataset.language }); });
}
$("date").addEventListener("change", () => { highlightQuick(); clearFormError(); });
$("time").addEventListener("change", () => { highlightQuick(); clearFormError(); });
$("description").addEventListener("input", clearFormError);
$("add").addEventListener("click", () => openForm(null));
$("form-back").addEventListener("click", backToList);
$("reminder-form").addEventListener("submit", save);
$("delete").addEventListener("click", deleteReminder);
$("nag-interval").addEventListener("change", () => updatePreferences({ nag_interval_minutes: Number($("nag-interval").value) }));
$("confirmation-check").addEventListener("change", () => updatePreferences({ confirmation_check_minutes: $("confirmation-check").checked ? 20 : 0 }));
function refreshOnReturn() {
  if (document.hidden || !state) return;
  void refreshVisibleState({ force: true });
  void yandex.refresh();
}
window.addEventListener("focus", refreshOnReturn);
document.addEventListener("visibilitychange", refreshOnReturn);
window.setInterval(() => { void refreshVisibleState(); }, 30000);
$("calendar-toggle").addEventListener("click", () => {
  if ($("calendar").hidden) showCalendar();
  else { $("calendar").hidden = true; $("calendar-toggle").setAttribute("aria-expanded", "false"); }
});
$("calendar-copy").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText($("calendar-url").value);
    $("calendar-copy").textContent = t("calendar.copied");
    haptic("success");
  } catch {
    $("calendar-url").focus();
    $("calendar-url").select();
    $("calendar-url").setSelectionRange(0, $("calendar-url").value.length);
    toast("calendar.copyManual");
  }
});
$("calendar-reset").addEventListener("click", async () => { if (await confirmAction("calendar.resetConfirm")) showCalendar(true); });
$("retry").addEventListener("click", () => { show("loading"); reload().catch((error) => fatal(errorKey(error))); });

async function start() {
  syncTheme();
  const initialLanguage = detectLanguage(tg?.initDataUnsafe?.user?.language_code);
  const dictionariesReady = loadDictionaries(initialLanguage);
  document.documentElement.lang = initialLanguage;
  $("loading").querySelector("p").textContent = t("status.loading");
  try { await dictionariesReady; }
  catch {
    setLanguage(initialLanguage);
    $("fatal-message").textContent = t("errors.locales");
    $("retry").addEventListener("click", (event) => { event.stopImmediatePropagation(); window.location.reload(); }, { capture: true });
    show("fatal");
    return;
  }
  if (!tg?.initData) return fatal("errors.telegramRequired");
  telegramCall(() => tg.ready());
  telegramCall(() => tg.expand());
  telegramCall(() => tg.MainButton?.onClick(save));
  telegramCall(() => tg.BackButton?.onClick(backToList));
  telegramCall(() => tg.onEvent?.("themeChanged", syncTheme));
  telegramCall(() => tg.onEvent?.("activated", refreshOnReturn));
  await reload().catch((error) => fatal(errorKey(error)));
}
start();
