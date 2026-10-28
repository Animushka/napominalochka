import { useEffect, useRef } from "react";

interface TelegramMainButton {
  setParams: (params: { text?: string; is_visible?: boolean; is_active?: boolean }) => unknown;
  onClick: (handler: () => void) => unknown;
  offClick: (handler: () => void) => unknown;
  show?: () => unknown;
  hide?: () => unknown;
  showProgress?: (leaveActive?: boolean) => unknown;
  hideProgress?: () => unknown;
}

interface TelegramBackButton {
  onClick: (handler: () => void) => unknown;
  offClick: (handler: () => void) => unknown;
  show: () => unknown;
  hide: () => unknown;
}

export interface TelegramWebApp {
  initData?: string;
  initDataUnsafe?: { user?: { id?: number; language_code?: string; first_name?: string } };
  colorScheme?: "light" | "dark";
  themeParams?: { bg_color?: string; secondary_bg_color?: string };
  MainButton?: TelegramMainButton;
  BackButton?: TelegramBackButton;
  HapticFeedback?: {
    selectionChanged?: () => unknown;
    notificationOccurred?: (type: "error" | "success" | "warning") => unknown;
    impactOccurred?: (style: "light" | "medium" | "heavy" | "rigid" | "soft") => unknown;
  };
  ready?: () => void;
  expand?: () => void;
  close?: () => void;
  onEvent?: (name: string, handler: () => void) => void;
  offEvent?: (name: string, handler: () => void) => void;
  setHeaderColor?: (color: string) => void;
  setBackgroundColor?: (color: string) => void;
  isVersionAtLeast?: (version: string) => boolean;
  showPopup?: (params: { message: string; buttons: { id: string; type: "cancel" | "destructive"; text?: string }[] }, callback: (buttonId: string) => void) => void;
  showConfirm?: (message: string, callback: (confirmed: boolean) => void) => void;
  openLink?: (url: string, options?: { try_instant_view?: boolean; try_browser?: string }) => void;
  openTelegramLink?: (url: string) => void;
}

declare global {
  interface Window { Telegram?: { WebApp?: TelegramWebApp } }
}

export const getTelegram = (): TelegramWebApp | undefined => window.Telegram?.WebApp;

function telegramCall(action: () => unknown): void {
  try { action(); } catch { /* Optional methods differ across Telegram client versions. */ }
}

interface BackHandler {
  callback: () => void;
  priority: number;
}
interface BackCoordinator {
  handlers: BackHandler[];
  dispatch: () => void;
}
const backCoordinators = new WeakMap<TelegramBackButton, BackCoordinator>();

function registerBackHandler(back: TelegramBackButton, callback: () => void, priority: number): () => void {
  let coordinator = backCoordinators.get(back);
  if (!coordinator) {
    const handlers: BackHandler[] = [];
    coordinator = {
      handlers,
      dispatch: () => {
        // Only the highest-priority, most recently registered layer consumes Back.
        const top = handlers.reduce<BackHandler | undefined>((previous, handler) =>
          !previous || handler.priority >= previous.priority ? handler : previous, undefined);
        top?.callback();
      },
    };
    backCoordinators.set(back, coordinator);
    const dispatch = coordinator.dispatch;
    telegramCall(() => back.onClick(dispatch));
  }
  const handler = { callback, priority };
  coordinator.handlers.push(handler);
  telegramCall(() => back.show());
  let registered = true;
  return () => {
    if (!registered) return;
    registered = false;
    const index = coordinator.handlers.indexOf(handler);
    if (index !== -1) coordinator.handlers.splice(index, 1);
    if (coordinator.handlers.length) telegramCall(() => back.show());
    else {
      telegramCall(() => back.offClick(coordinator.dispatch));
      telegramCall(() => back.hide());
      backCoordinators.delete(back);
    }
  };
}

/** Shares one SDK listener between the current screen and any open overlays. */
export function useTelegramBack(onBack: (() => void) | undefined, {
  enabled = true,
  priority = "screen",
}: { enabled?: boolean; priority?: "screen" | "overlay" } = {}): void {
  const back = getTelegram()?.BackButton;
  const current = useRef(onBack);
  current.current = onBack;
  const hasHandler = Boolean(onBack);
  useEffect(() => {
    if (!enabled || !hasHandler || typeof back?.onClick !== "function" || typeof back?.offClick !== "function") return;
    return registerBackHandler(back, () => current.current?.(), priority === "overlay" ? 1 : 0);
  }, [back, enabled, hasHandler, priority]);
}

export function haptic(type: "selection" | "error" | "success" | "warning" = "selection"): void {
  telegramCall(() => type === "selection"
    ? getTelegram()?.HapticFeedback?.selectionChanged?.()
    : getTelegram()?.HapticFeedback?.notificationOccurred?.(type));
}

export function confirmAction(message: string, confirmLabel: string): Promise<boolean> {
  const tg = getTelegram();
  return new Promise((resolve) => {
    try {
      if (tg?.showPopup && tg.isVersionAtLeast?.("6.2")) {
        tg.showPopup({ message, buttons: [{ id: "cancel", type: "cancel" }, { id: "confirm", type: "destructive", text: confirmLabel }] }, (id) => resolve(id === "confirm"));
      } else if (tg?.showConfirm) tg.showConfirm(message, resolve);
      else resolve(window.confirm(message));
    } catch { resolve(window.confirm(message)); }
  });
}

export function syncTelegramTheme(): void {
  const tg = getTelegram();
  const dark = tg?.initData
    ? tg.colorScheme === "dark"
    : window.matchMedia("(prefers-color-scheme: dark)").matches;
  document.documentElement.classList.toggle("dark", dark);
  const background = tg?.themeParams?.bg_color;
  if (background) {
    document.querySelector('meta[name="theme-color"]')?.setAttribute("content", background);
    telegramCall(() => tg?.setHeaderColor?.(background));
    telegramCall(() => tg?.setBackgroundColor?.(background));
  }
}

/** Initializes the bridge once per mount and returns a StrictMode-safe cleanup. */
export function initializeTelegram(): () => void {
  const tg = getTelegram();
  const media = window.matchMedia("(prefers-color-scheme: dark)");
  syncTelegramTheme();
  telegramCall(() => tg?.ready?.());
  telegramCall(() => tg?.expand?.());
  telegramCall(() => tg?.onEvent?.("themeChanged", syncTelegramTheme));
  media.addEventListener("change", syncTelegramTheme);
  return () => {
    telegramCall(() => tg?.offEvent?.("themeChanged", syncTelegramTheme));
    media.removeEventListener("change", syncTelegramTheme);
  };
}

export function useTelegram(): void {
  useEffect(initializeTelegram, []);
}

export interface TelegramButtonOptions {
  onSave?: () => void;
  onBack?: () => void;
  saveLabel: string;
  canSave?: boolean;
  busy?: boolean;
}

export function useTelegramButtons({ onSave, onBack, saveLabel, canSave = true, busy = false }: TelegramButtonOptions): { hasNativeSave: boolean } {
  const tg = getTelegram();
  const main = tg?.MainButton;
  const hasNativeSave = Boolean(tg?.initData && typeof main?.setParams === "function" && typeof main?.onClick === "function" && typeof main?.offClick === "function");
  const current = useRef({ onSave, onBack, canSave, busy });
  current.current = { onSave, onBack, canSave, busy };
  useTelegramBack(() => { if (!current.current.busy) current.current.onBack?.(); }, { enabled: Boolean(onBack) });

  useEffect(() => {
    if (!hasNativeSave || !main) return;
    const save = () => {
      if (current.current.canSave && !current.current.busy) current.current.onSave?.();
    };
    telegramCall(() => main.onClick(save));
    return () => {
      telegramCall(() => main.offClick(save));
      telegramCall(() => main.hide?.());
      telegramCall(() => main.hideProgress?.());
    };
  }, [hasNativeSave, main]);

  useEffect(() => {
    if (hasNativeSave) telegramCall(() => main?.setParams({ text: saveLabel, is_visible: Boolean(onSave), is_active: canSave && !busy }));
    telegramCall(() => busy ? main?.showProgress?.() : main?.hideProgress?.());
  }, [main, hasNativeSave, onSave, saveLabel, canSave, busy]);

  return { hasNativeSave };
}
