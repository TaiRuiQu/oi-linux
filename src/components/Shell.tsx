import { useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { isTauri } from "../api";
import { useAppState } from "../state/AppStore";
import { useTerminalTabs } from "../state/TerminalStore";
import { DiagnosticsPage } from "../pages/DiagnosticsPage";
import { ManagePage } from "../pages/ManagePage";
import { SettingsPage } from "../pages/SettingsPage";
import { UpdatesPage } from "../pages/UpdatesPage";
import { ErrorBanners } from "./ErrorBanners";
import { FirstRunWizard } from "./FirstRunWizard";
import { TerminalPanel } from "./TerminalPanel";
import { ShareStateBadge, VmStateBadge } from "./ui";

export type PageId = "manage" | "terminal" | "settings" | "updates" | "diagnostics";

const NAV: readonly { id: PageId; labelKey: string }[] = [
  { id: "manage", labelKey: "nav.manage" },
  { id: "terminal", labelKey: "nav.terminal" },
  { id: "settings", labelKey: "nav.settings" },
  { id: "updates", labelKey: "nav.updates" },
  { id: "diagnostics", labelKey: "nav.diagnostics" },
];

export function Shell() {
  const { t } = useTranslation();
  const { state } = useAppState();
  const { tabs, openTab } = useTerminalTabs();
  const [page, setPage] = useState<PageId>("manage");
  const [wizardOpen, setWizardOpen] = useState(false);
  const autoOpened = useRef(false);

  const initialized = state?.environment?.initialized ?? false;

  useEffect(() => {
    if (state === null || autoOpened.current) return;
    autoOpened.current = true;
    if (!initialized) setWizardOpen(true);
  }, [state, initialized]);

  const openTerminalPage = () => {
    setPage("terminal");
    if (tabs.length === 0) openTab();
  };

  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <span className="brandName">{t("app.name")}</span>
          <span className="brandTagline">{t("app.tagline")}</span>
        </div>
        <nav className="nav" aria-label={t("nav.aria")}>
          {NAV.map((item) => (
            <button
              key={item.id}
              type="button"
              className={`navItem${page === item.id ? " navItem--active" : ""}`}
              aria-current={page === item.id ? "page" : undefined}
              onClick={() => setPage(item.id)}
            >
              {t(item.labelKey)}
            </button>
          ))}
        </nav>
        {state ? (
          <div className="sidebarStatus">
            <VmStateBadge status={state.vm.status} />
            <ShareStateBadge status={state.share.status} />
          </div>
        ) : null}
      </aside>

      <main className="main">
        {!isTauri() ? <p className="devStrip">{t("app.browserNotice")}</p> : null}
        <ErrorBanners />
        <div className="pageStack">
          {page === "manage" ? (
            <ManagePage
              onOpenTerminal={openTerminalPage}
              onOpenWizard={() => setWizardOpen(true)}
            />
          ) : null}
          {page === "settings" ? <SettingsPage /> : null}
          {page === "updates" ? <UpdatesPage /> : null}
          {page === "diagnostics" ? <DiagnosticsPage /> : null}
          <TerminalPanel active={page === "terminal"} />
        </div>
        {wizardOpen ? <FirstRunWizard onClose={() => setWizardOpen(false)} /> : null}
      </main>
    </div>
  );
}
