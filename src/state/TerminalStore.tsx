import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";

export interface TerminalTab {
  key: string;
  index: number;
  exited: boolean;
  exitCode: number | null;
}

export interface TerminalContextValue {
  tabs: TerminalTab[];
  activeKey: string | null;
  setActiveKey: (key: string) => void;
  openTab: () => void;
  closeTab: (key: string) => void;
  markExited: (key: string, code: number | null) => void;
}

const TerminalContext = createContext<TerminalContextValue | null>(null);

export function TerminalProvider({ children }: { children: ReactNode }) {
  const [tabs, setTabs] = useState<TerminalTab[]>([]);
  const [activeKey, setActiveKeyState] = useState<string | null>(null);
  const counter = useRef(0);

  const openTab = useCallback(() => {
    counter.current += 1;
    const key = `term-${counter.current}`;
    setTabs((previous) => [
      ...previous,
      { key, index: counter.current, exited: false, exitCode: null },
    ]);
    setActiveKeyState(key);
  }, []);

  const closeTab = useCallback((key: string) => {
    setTabs((previous) => {
      const next = previous.filter((tab) => tab.key !== key);
      setActiveKeyState((current) => {
        if (current !== key) return current;
        if (next.length === 0) return null;
        const last = next[next.length - 1];
        return last ? last.key : null;
      });
      return next;
    });
  }, []);

  const setActiveKey = useCallback((key: string) => {
    setActiveKeyState(key);
  }, []);

  const markExited = useCallback((key: string, code: number | null) => {
    setTabs((previous) =>
      previous.map((tab) => (tab.key === key ? { ...tab, exited: true, exitCode: code } : tab)),
    );
  }, []);

  const value = useMemo<TerminalContextValue>(
    () => ({ tabs, activeKey, setActiveKey, openTab, closeTab, markExited }),
    [tabs, activeKey, setActiveKey, openTab, closeTab, markExited],
  );

  return <TerminalContext.Provider value={value}>{children}</TerminalContext.Provider>;
}

export function useTerminalTabs(): TerminalContextValue {
  const context = useContext(TerminalContext);
  if (!context) {
    throw new Error("useTerminalTabs must be used inside TerminalProvider");
  }
  return context;
}
