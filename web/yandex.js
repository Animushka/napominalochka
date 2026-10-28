// Alice owns its connection flow. The reminder screen supplies only shared services.
export function createYandexController({ api, t, tg, errorKey, errorKeys, confirmAction, haptic }) {
  const $ = (id) => document.getElementById(id);
  const model = { status: null, statusUncertain: false, scenarios: null, loading: false, busy: null, stage: null, draftId: "", heardId: null, feedback: null, feedbackError: false, oauthUrl: null, refreshedAt: 0, cooldownUntil: 0 };
  let cooldownTimer;
  const locked = () => Boolean(model.loading || model.busy);
  const selected = () => model.scenarios?.find((scenario) => scenario.id === model.draftId && scenario.is_active);
  const feedback = (key, isError = false) => { model.feedback = key; model.feedbackError = isError; };
  const connected = () => model.status?.connected && !model.status.requires_reconnect;
  function setStatus(status, reset = false) {
    const changed = model.status?.connected !== status.connected || model.status?.scenario_id !== status.scenario_id || (model.status?.requires_reconnect && !status.requires_reconnect);
    model.status = status;
    model.statusUncertain = false;
    if (reset || changed || model.stage === null) {
      model.draftId = typeof status.scenario_id === "string" ? status.scenario_id : "";
      model.stage = model.draftId ? "manage" : "choose";
      model.heardId = null;
    }
    if (!connected()) model.scenarios = null;
    else model.oauthUrl = null;
    if (changed) feedback(null);
  }
  function handleError(error) {
    feedback(errorKey(error), true);
    if (["yandex_reconnect_required", "yandex_not_connected"].includes(error.code) && model.status) {
      model.status.requires_reconnect = true;
      model.heardId = null;
    }
  }
  async function reconcileAfterFailure() {
    const previous = { stage: model.stage, draftId: model.draftId, feedback: model.feedback, feedbackError: model.feedbackError };
    model.heardId = null;
    model.statusUncertain = true;
    try {
      // Provider failures may disable the connection on the server. Read status once; never retry the action.
      setStatus(await api("GET", "/api/yandex"));
    } catch { /* Keep the original error and show unknown status until a later explicit refresh succeeds. */ }
    model.stage = previous.stage;
    model.draftId = previous.draftId;
    model.heardId = null;
    feedback(previous.feedback, previous.feedbackError);
  }
  function renderTestButton() {
    const remaining = Math.max(0, Math.ceil((model.cooldownUntil - Date.now()) / 1000));
    for (const id of ["yandex-test", "yandex-manage-test"]) {
      $(id).disabled = locked() || model.statusUncertain || Boolean(remaining) || !selected() || !connected();
      $(id).textContent = t(model.busy === "test" ? "yandex.testing" : remaining ? "yandex.testCooldown" : "yandex.test", { seconds: remaining });
    }
    window.clearTimeout(cooldownTimer);
    if (remaining) cooldownTimer = window.setTimeout(renderTestButton, 1000);
  }
  function render() {
    const status = model.status;
    $("yandex-panel").hidden = status?.available !== true;
    if (!status?.available) return;
    const isConnected = connected();
    const stage = isConnected ? model.stage : "connect";
    const isLocked = locked();
    $("yandex-summary").textContent = t(model.statusUncertain ? "yandex.errors.unavailable" : status.requires_reconnect ? "yandex.summaryReconnect" : status.enabled && isConnected ? "yandex.summaryEnabled" : isConnected ? "yandex.summaryConnected" : "yandex.subtitle");
    $("yandex-progress").hidden = stage === "manage";
    for (const step of $("yandex-progress").children) {
      if (step.dataset.step === stage) step.setAttribute("aria-current", "step");
      else step.removeAttribute("aria-current");
    }
    for (const [id, name] of [["connect", "connect"], ["choose", "choose"], ["test", "test"], ["manage", "manage"]]) $(`yandex-${id}-area`).hidden = stage !== name;
    $("yandex-connect").disabled = isLocked;
    $("yandex-connect").textContent = t(model.busy === "connect" ? "yandex.connecting" : model.oauthUrl ? "yandex.continue" : status.requires_reconnect ? "yandex.reconnect" : "yandex.connect");
    $("yandex-refresh").disabled = isLocked;
    $("yandex-disconnect").hidden = !status.connected && !status.requires_reconnect;
    $("yandex-disconnect").disabled = isLocked;
    const select = $("yandex-scenario");
    const placeholder = document.createElement("option");
    placeholder.value = "";
    placeholder.textContent = t("yandex.chooseScenario");
    select.replaceChildren(placeholder);
    for (const scenario of model.scenarios || []) {
      const option = document.createElement("option");
      option.value = scenario.id;
      option.textContent = scenario.is_active ? scenario.name : t("yandex.inactiveScenario", { name: scenario.name });
      option.disabled = !scenario.is_active;
      select.append(option);
    }
    if (model.draftId && !model.scenarios?.some((scenario) => scenario.id === model.draftId)) {
      const option = document.createElement("option");
      option.value = model.draftId;
      option.textContent = model.scenarios ? t("yandex.savedScenarioMissing") : status.scenario_name || t("yandex.savedScenario");
      option.disabled = true;
      select.append(option);
    }
    select.value = model.draftId;
    select.disabled = isLocked || !model.scenarios?.length;
    $("yandex-scenario-help").textContent = t(model.loading ? "yandex.loading" : model.scenarios === null ? "yandex.loadScenarios" : !model.scenarios.some((scenario) => scenario.is_active) ? "yandex.noScenarios" : "yandex.scenarioHelp");
    $("yandex-next").disabled = isLocked || model.statusUncertain || !selected();
    $("yandex-next").textContent = t(model.busy === "selection" ? "action.saving" : "yandex.next");
    $("yandex-cancel-change").hidden = !status.scenario_id;
    $("yandex-cancel-change").disabled = isLocked;
    const scenarioName = selected()?.name || status.scenario_name || t("yandex.savedScenario");
    $("yandex-test-scenario").textContent = scenarioName;
    $("yandex-current-scenario").textContent = status.scenario_name || scenarioName;
    const awaitingConfirmation = Boolean(model.heardId && model.heardId === status.scenario_id);
    $("yandex-heard-area").hidden = !awaitingConfirmation;
    $("yandex-test").hidden = awaitingConfirmation;
    $("yandex-confirm").disabled = isLocked || model.statusUncertain || !awaitingConfirmation;
    $("yandex-confirm").textContent = t(model.busy === "enable" ? "action.saving" : "yandex.confirm");
    $("yandex-not-heard").disabled = isLocked;
    $("yandex-toggle").disabled = isLocked || (!model.statusUncertain && !status.enabled && !selected());
    $("yandex-toggle").textContent = t(model.statusUncertain ? "yandex.refresh" : model.busy === "disable" ? "action.saving" : status.enabled ? "yandex.disable" : "yandex.enable");
    for (const id of ["yandex-change", "yandex-back-choose"]) $(id).disabled = isLocked;
    renderTestButton();
    const lastError = status.requires_reconnect ? "yandex.errors.reconnect" : status.last_error ? errorKeys[status.last_error] || "yandex.errors.delivery" : null;
    $("yandex-last-error").hidden = !lastError;
    $("yandex-last-error").textContent = lastError ? t(lastError) : "";
    $("yandex-status").hidden = !model.loading && !model.feedback;
    $("yandex-status").classList.toggle("is-error", model.feedbackError);
    $("yandex-status").textContent = model.loading ? t("yandex.loading") : model.feedback ? t(model.feedback) : "";
  }
  async function refresh({ force = false } = {}) {
    if (locked() || (!force && Date.now() - model.refreshedAt < 3000)) return;
    model.loading = true;
    if (force) { model.oauthUrl = null; feedback(null); }
    render();
    let fetchingScenarios = false;
    try {
      setStatus(await api("GET", "/api/yandex"));
      if (model.status.available && connected() && ($("yandex-panel").open || force)) {
        fetchingScenarios = true;
        const data = await api("GET", "/api/yandex/scenarios");
        model.scenarios = Array.isArray(data.scenarios) ? data.scenarios.filter((scenario) => typeof scenario.id === "string" && typeof scenario.name === "string" && typeof scenario.is_active === "boolean") : [];
      }
    } catch (error) {
      handleError(error);
      if (fetchingScenarios) {
        model.scenarios = null;
        await reconcileAfterFailure();
      } else model.statusUncertain = true;
    }
    finally { model.loading = false; model.refreshedAt = Date.now(); render(); }
  }
  function openOAuth(url) {
    const target = new URL(url);
    if (target.protocol !== "https:" || target.hostname !== "oauth.yandex.ru" || target.username || target.password || (target.port && target.port !== "443")) throw new Error("oauth");
    if (typeof tg?.openLink === "function") {
      try { tg.openLink(target.href); return; } catch { /* Older clients can use a browser link. */ }
    }
    window.open(target.href, "_blank", "noopener,noreferrer");
  }
  async function connect() {
    if (locked()) return;
    if (model.oauthUrl) {
      try { openOAuth(model.oauthUrl); } catch { feedback("yandex.errors.connect", true); render(); }
      return;
    }
    model.busy = "connect";
    feedback(null);
    render();
    try {
      const data = await api("POST", "/api/yandex/connect");
      try { openOAuth(data.url); } catch { feedback("yandex.errors.connect", true); return; }
      model.oauthUrl = data.url;
      feedback("yandex.awaitingReturn");
    } catch (error) { handleError(error); }
    finally { model.busy = null; render(); }
  }
  async function choose() {
    if (locked() || model.statusUncertain || !selected()) return;
    model.busy = "selection";
    feedback(null);
    render();
    try {
      // Saving a selection always leaves audio off until the user hears and confirms a test.
      setStatus(await api("PUT", "/api/yandex", { scenario_id: model.draftId, enabled: false }), true);
      model.stage = "test";
      haptic();
    } catch (error) { handleError(error); }
    finally { model.busy = null; render(); }
  }
  async function test() {
    if (locked() || model.statusUncertain || !selected() || model.draftId !== model.status?.scenario_id || !connected() || Date.now() < model.cooldownUntil) return;
    model.busy = "test";
    model.heardId = null;
    feedback(null);
    render();
    try {
      await api("POST", "/api/yandex/test");
      model.cooldownUntil = Date.now() + 60000;
      // HTTP acceptance does not prove that the user heard the speaker.
      if (!model.status.enabled) { model.heardId = model.status.scenario_id; model.stage = "test"; }
      feedback("yandex.testSent");
      model.status.last_error = null;
      haptic("success");
    } catch (error) {
      // Never retry automatically, including when the response may have been lost.
      model.cooldownUntil = Date.now() + 60000;
      if (errorKey(error) === "errors.network") feedback("yandex.errors.deliveryUncertain", true);
      else handleError(error);
      await reconcileAfterFailure();
    } finally { model.busy = null; render(); }
  }
  async function enable() {
    if (locked() || model.statusUncertain || !model.heardId || model.heardId !== model.status?.scenario_id || !connected()) return;
    model.busy = "enable";
    feedback(null);
    render();
    try {
      setStatus(await api("PUT", "/api/yandex", { scenario_id: model.heardId, enabled: true }), true);
      feedback("yandex.enabledSaved");
      haptic("success");
    } catch (error) { handleError(error); }
    finally { model.busy = null; render(); }
  }
  async function toggle() {
    if (locked()) return;
    if (model.statusUncertain) { await refresh({ force: true }); return; }
    if (!model.status?.enabled) { model.stage = "test"; model.heardId = null; feedback(null); render(); return; }
    model.busy = "disable";
    feedback(null);
    render();
    try {
      setStatus(await api("PUT", "/api/yandex", { scenario_id: model.status.scenario_id, enabled: false }), true);
      feedback("yandex.disabledSaved");
    } catch (error) { handleError(error); }
    finally { model.busy = null; render(); }
  }
  function change() {
    if (locked()) return;
    model.stage = "choose";
    model.heardId = null;
    feedback(null);
    render();
    if (model.scenarios === null) void refresh({ force: true });
  }
  async function disconnect() {
    if (locked()) return;
    model.busy = "confirmDisconnect";
    render();
    try {
      if (!await confirmAction("yandex.disconnectConfirm", "yandex.disconnect")) return;
      model.busy = "disconnect";
      render();
      await api("DELETE", "/api/yandex");
      setStatus({ available: true, connected: false, enabled: false, requires_reconnect: false, scenario_id: null, scenario_name: null, last_error: null }, true);
      model.oauthUrl = null;
      model.cooldownUntil = 0;
      feedback("yandex.disconnected");
    } catch (error) { handleError(error); }
    finally { model.busy = null; render(); }
  }
  $("yandex-panel").addEventListener("toggle", () => { if ($("yandex-panel").open && model.scenarios === null) void refresh({ force: true }); });
  $("yandex-connect").addEventListener("click", connect);
  $("yandex-refresh").addEventListener("click", () => { void refresh({ force: true }); });
  $("yandex-scenario").addEventListener("change", () => { model.draftId = $("yandex-scenario").value; model.heardId = null; feedback(null); render(); });
  $("yandex-next").addEventListener("click", choose);
  $("yandex-test").addEventListener("click", test);
  $("yandex-manage-test").addEventListener("click", test);
  $("yandex-confirm").addEventListener("click", enable);
  $("yandex-not-heard").addEventListener("click", () => { model.heardId = null; feedback("yandex.checkSpeaker"); render(); });
  $("yandex-toggle").addEventListener("click", toggle);
  $("yandex-change").addEventListener("click", change);
  $("yandex-back-choose").addEventListener("click", change);
  $("yandex-cancel-change").addEventListener("click", () => { model.draftId = model.status?.scenario_id || ""; model.stage = "manage"; feedback(null); render(); });
  $("yandex-disconnect").addEventListener("click", disconnect);
  return { refresh, render };
}
