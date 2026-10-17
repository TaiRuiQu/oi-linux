import { useCallback, useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Terminal, type ITheme } from "@xterm/xterm";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  isTauri,
  onTerminalData,
  onTerminalExit,
  terminalClose,
  terminalOpen,
  terminalResize,
  terminalWrite,
} from "../api";
import { toErrorMessage } from "../format";
import { useAppState } from "../state/AppStore";
import { useTerminalTabs } from "../state/TerminalStore";
import { Button } from "./ui";

const TERM_THEME: ITheme = {
  background: "#12141a",
  foreground: "#e3e7ee",
  cursor: "#8fb6ff",
  cursorAccent: "#12141a",
  selectionBackground: "#2b3b5e",
  black: "#1b1e24",
  red: "#f2777a",
  green: "#9ece6a",
  yellow: "#e0af68",
  blue: "#7aa2f7",
  magenta: "#bb9af7",
  cyan: "#7dcfff",
  white: "#c0caf5",
  brightBlack: "#565f89",
  brightRed: "#ff9dae",
  brightGreen: "#b9f27c",
  brightYellow: "#ffd08a",
  brightBlue: "#9ab8ff",
  brightMagenta: "#d3b9ff",
  brightCyan: "#a3e5ff",
  brightWhite: "#ffffff",
};

const MIN_SIZE_PX = 40;

function TerminalPane({
  tabKey,
  active,
  exited,
  exitCode,
  onExit,
}: {
  tabKey: string;
  active: boolean;
  exited: boolean;
  exitCode: number | null;
  onExit: (key: string, code: number | null) => void;
}) {
  const { t } = useTranslation();
  const hostRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitNowRef = useRef<() => void>(() => undefined);
  const ptyRef = useRef<string | null>(null);
  const onExitRef = useRef(onExit);

  useEffect(() => {
    onExitRef.current = onExit;
  }, [onExit]);

  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;

    const term = new Terminal({
      fontFamily: '"Cascadia Mono", Consolas, "Liberation Mono", Menlo, monospace',
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 4000,
      allowProposedApi: false,
      theme: TERM_THEME,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.loadAddon(new WebLinksAddon());
    term.open(host);
    termRef.current = term;

    const fitNow = () => {
      if (host.clientWidth < MIN_SIZE_PX || host.clientHeight < MIN_SIZE_PX) return;
      try {
        fit.fit();
      } catch {
        return;
      }
      const id = ptyRef.current;
      if (id) void terminalResize(id, term.cols, term.rows);
    };
    fitNowRef.current = fitNow;

    const holder: { data: UnlistenFn | null; exit: UnlistenFn | null; stopped: boolean } = {
      data: null,
      exit: null,
      stopped: false,
    };

    const start = async () => {
      fitNow();
      const cols = Math.max(term.cols, 2);
      const rows = Math.max(term.rows, 2);
      let id: string;
      try {
        const opened = await terminalOpen(cols, rows);
        id = opened.id;
      } catch (cause) {
        term.writeln("");
        term.writeln(t("terminal.openFailed", { message: toErrorMessage(cause) }));
        return;
      }
      if (holder.stopped) {
        void terminalClose(id);
        return;
      }
      ptyRef.current = id;
      holder.data = await onTerminalData(id, (chunk) => term.write(chunk));
      holder.exit = await onTerminalExit(id, (payload) => {
        onExitRef.current(tabKey, payload.code);
      });
      if (!isTauri()) {
        term.writeln(t("terminal.devNotice"));
      }
      if (host.clientWidth >= MIN_SIZE_PX) term.focus();
    };
    void start();

    const dataSubscription = term.onData((data) => {
      const id = ptyRef.current;
      if (id) void terminalWrite(id, data);
    });

    const observer = new ResizeObserver(() => fitNow());
    observer.observe(host);

    return () => {
      holder.stopped = true;
      observer.disconnect();
      dataSubscription.dispose();
      if (holder.data) holder.data();
      if (holder.exit) holder.exit();
      const id = ptyRef.current;
      ptyRef.current = null;
      if (id) void terminalClose(id);
      term.dispose();
      termRef.current = null;
      fitNowRef.current = () => undefined;
    };
  }, []);

  useEffect(() => {
    if (!active) return;
    const frame = window.requestAnimationFrame(() => {
      fitNowRef.current();
      termRef.current?.focus();
    });
    return () => window.cancelAnimationFrame(frame);
  }, [active]);

  return (
    <div className={`termPane${active ? " termPane--active" : ""}`}>
      <div className="termHost" ref={hostRef} />
      {exited ? (
        <div className="termExitBar">
          {exitCode === null
            ? t("terminal.exitedUnknown")
            : t("terminal.exited", { code: exitCode })}
        </div>
      ) : null}
    </div>
  );
}

export function TerminalPanel({ active }: { active: boolean }) {
  const { t } = useTranslation();
  const { state } = useAppState();
  const { tabs, activeKey, setActiveKey, openTab, closeTab, markExited } = useTerminalTabs();

  const handleExit = useCallback(
    (key: string, code: number | null) => {
      markExited(key, code);
    },
    [markExited],
  );

  const running = state?.vm.status === "running";

  return (
    <section className={`page page--terminal${active ? "" : " isHidden"}`}>
      <header className="pageHeader">
        <div className="pageTitleBlock">
          <h1>{t("terminal.title")}</h1>
          <p className="pageDescription">{t("terminal.description")}</p>
        </div>
        <div className="pageActions">
          <Button variant="primary" onClick={openTab}>
            {t("terminal.newTab")}
          </Button>
        </div>
      </header>

      {!running ? <p className="hint hint--warn">{t("terminal.needsRunning")}</p> : null}

      <div className="tabBar" role="tablist" aria-label={t("terminal.tabs")}>
        {tabs.map((tab) => {
          const name = t("terminal.tabName", { index: tab.index });
          const selected = tab.key === activeKey;
          return (
            <div key={tab.key} className={`tab${selected ? " tab--active" : ""}`}>
              <button
                type="button"
                role="tab"
                aria-selected={selected}
                tabIndex={selected ? 0 : -1}
                className="tabLabel"
                onClick={() => setActiveKey(tab.key)}
              >
                <span>{name}</span>
                {tab.exited ? <span className="tabFlag">{t("terminal.exitedShort")}</span> : null}
              </button>
              <button
                type="button"
                className="tabClose"
                aria-label={t("terminal.closeTab", { name })}
                onClick={() => closeTab(tab.key)}
              >
                ×
              </button>
            </div>
          );
        })}
      </div>

      <div className="termStack">
        {tabs.length === 0 ? (
          <div className="emptyState">
            <h2>{t("terminal.emptyTitle")}</h2>
            <p>{t("terminal.emptyBody")}</p>
            <Button variant="primary" onClick={openTab}>
              {t("terminal.openFirst")}
            </Button>
          </div>
        ) : (
          tabs.map((tab) => (
            <TerminalPane
              key={tab.key}
              tabKey={tab.key}
              active={tab.key === activeKey}
              exited={tab.exited}
              exitCode={tab.exitCode}
              onExit={handleExit}
            />
          ))
        )}
      </div>
    </section>
  );
}
