import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getState, onState } from "../api";
import { applyLanguage } from "../i18n";
import { toErrorMessage } from "../format";
import type { AppState } from "../types";

export type ErrorKind = "vm" | "share" | "generic";

export interface ActionError {
  kind: ErrorKind;
  message: string;
}

type GuardResult<T> = { ok: true; value: T } | { ok: false };

export interface AppContextValue {
  state: AppState | null;
  loadError: string | null;
  actionError: ActionError | null;
  busy: string | null;
  setState: (next: AppState) => void;
  dismissActionError: () => void;
  runVm: (key: string, fn: () => Promise<AppState>) => Promise<AppState | null>;
  runShare: (key: string, fn: () => Promise<AppState>) => Promise<AppState | null>;
  runOther: (key: string, fn: () => Promise<unknown>) => Promise<boolean>;
}

const AppContext = createContext<AppContextValue | null>(null);

export function AppProvider({ children }: { children: ReactNode }) {
  const [state, setStateValue] = useState<AppState | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<ActionError | null>(null);
  const [busy, setBusy] = useState<string | null>(null);

  useEffect(() => {
    const holder: { stop: UnlistenFn | null } = { stop: null };
    let cancelled = false;
    void (async () => {
      holder.stop = await onState((next) => {
        if (!cancelled) setStateValue(next);
      });
      try {
        const initial = await getState();
        if (!cancelled) setStateValue(initial);
      } catch (error) {
        if (!cancelled) setLoadError(toErrorMessage(error));
      }
    })();
    return () => {
      cancelled = true;
      if (holder.stop) holder.stop();
    };
  }, []);

  const language = state?.config?.language;
  useEffect(() => {
    if (language) void applyLanguage(language);
  }, [language]);

  const setState = useCallback((next: AppState) => {
    setStateValue(next);
  }, []);

  const dismissActionError = useCallback(() => {
    setActionError(null);
  }, []);

  const guard = useCallback(
    async <T,>(key: string, kind: ErrorKind, fn: () => Promise<T>): Promise<GuardResult<T>> => {
      setBusy(key);
      setActionError(null);
      try {
        const value = await fn();
        return { ok: true, value };
      } catch (error) {
        setActionError({ kind, message: toErrorMessage(error) });
        return { ok: false };
      } finally {
        setBusy(null);
      }
    },
    [],
  );

  const runVm = useCallback(
    async (key: string, fn: () => Promise<AppState>): Promise<AppState | null> => {
      const result = await guard(key, "vm", fn);
      if (!result.ok) return null;
      setStateValue(result.value);
      return result.value;
    },
    [guard],
  );

  const runShare = useCallback(
    async (key: string, fn: () => Promise<AppState>): Promise<AppState | null> => {
      const result = await guard(key, "share", fn);
      if (!result.ok) return null;
      setStateValue(result.value);
      return result.value;
    },
    [guard],
  );

  const runOther = useCallback(
    async (key: string, fn: () => Promise<unknown>): Promise<boolean> => {
      const result = await guard(key, "generic", fn);
      return result.ok;
    },
    [guard],
  );

  const value = useMemo<AppContextValue>(
    () => ({
      state,
      loadError,
      actionError,
      busy,
      setState,
      dismissActionError,
      runVm,
      runShare,
      runOther,
    }),
    [
      state,
      loadError,
      actionError,
      busy,
      setState,
      dismissActionError,
      runVm,
      runShare,
      runOther,
    ],
  );

  return <AppContext.Provider value={value}>{children}</AppContext.Provider>;
}

export function useAppState(): AppContextValue {
  const context = useContext(AppContext);
  if (!context) {
    throw new Error("useAppState must be used inside AppProvider");
  }
  return context;
}
