import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { exportLogs, listLogs } from "../api";
import { formatBytes, formatUnixSeconds, toErrorMessage } from "../format";
import { useAppState } from "../state/AppStore";
import { Modal } from "../components/Modal";
import { ResetDialog } from "../components/ResetDialog";
import { Badge, Button, Card, DefList, DefRow, Hint, Mono, TextInput } from "../components/ui";
import type { LogFileInfo } from "../types";

export function DiagnosticsPage() {
  const { t, i18n } = useTranslation();
  const app = useAppState();
  const state = app.state;

  const [logs, setLogs] = useState<LogFileInfo[] | null>(null);
  const [logsError, setLogsError] = useState<string | null>(null);
  const [destDir, setDestDir] = useState("");
  const [exportOpen, setExportOpen] = useState(false);
  const [exported, setExported] = useState<string[] | null>(null);
  const [resetOpen, setResetOpen] = useState(false);

  const loadLogs = useCallback(async () => {
    setLogsError(null);
    try {
      const list = await listLogs();
      setLogs(list);
    } catch (cause) {
      setLogsError(toErrorMessage(cause));
    }
  }, []);

  useEffect(() => {
    void loadLogs();
  }, [loadLogs]);

  const failures = state?.vm.consecutive_start_failures ?? 0;
  const suggestReset = failures >= 3;

  const runExport = async () => {
    if (destDir.trim().length === 0) return;
    const ok = await app.runOther("export", async () => {
      const written = await exportLogs(destDir.trim());
      setExported(written);
    });
    if (ok) setExportOpen(false);
  };

  return (
    <section className="page">
      <header className="pageHeader">
        <div className="pageTitleBlock">
          <h1>{t("diagnostics.title")}</h1>
          <p className="pageDescription">{t("diagnostics.description")}</p>
        </div>
        <div className="pageActions">
          <Button onClick={() => void loadLogs()}>{t("common.refresh")}</Button>
        </div>
      </header>

      <Card title={t("diagnostics.failures.heading")}>
        <DefList>
          <DefRow label={t("diagnostics.failures.count")}>
            <Badge tone={suggestReset ? "red" : failures > 0 ? "amber" : "green"}>
              {t("diagnostics.failures.times", { count: failures })}
            </Badge>
          </DefRow>
        </DefList>
        {suggestReset ? (
          <>
            <Hint tone="warn">{t("diagnostics.failures.suggest")}</Hint>
            <Hint tone="info">{t("diagnostics.failures.note")}</Hint>
            <div className="actionRow">
              <Button variant="danger" onClick={() => setResetOpen(true)}>
                {t("diagnostics.failures.resetAction")}
              </Button>
            </div>
          </>
        ) : (
          <Hint tone="info">{t("diagnostics.failures.ok")}</Hint>
        )}
      </Card>

      <Card title={t("diagnostics.logs.heading")}>
        {logsError ? (
          <Hint tone="danger">
            {t("diagnostics.logs.loadFailed")} {logsError}
          </Hint>
        ) : null}
        {logs === null && !logsError ? <p className="hint">{t("common.loading")}</p> : null}
        {logs && logs.length === 0 ? <p className="hint">{t("diagnostics.logs.empty")}</p> : null}
        {logs && logs.length > 0 ? (
          <>
            <table className="dataTable">
              <thead>
                <tr>
                  <th scope="col">{t("diagnostics.logs.name")}</th>
                  <th scope="col">{t("diagnostics.logs.size")}</th>
                  <th scope="col">{t("diagnostics.logs.modified")}</th>
                  <th scope="col">{t("diagnostics.logs.path")}</th>
                </tr>
              </thead>
              <tbody>
                {logs.map((log) => (
                  <tr key={log.path}>
                    <td>{log.name}</td>
                    <td>
                      <Mono>{formatBytes(log.size_bytes)}</Mono>
                    </td>
                    <td>{formatUnixSeconds(log.modified_unix, i18n.language)}</td>
                    <td>
                      <span className="pathValue" title={log.path}>
                        {log.path}
                      </span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
            <p className="hint">{t("diagnostics.logs.total", { count: logs.length })}</p>
          </>
        ) : null}
      </Card>

      <Card title={t("diagnostics.export.heading")}>
        <Hint tone="warn">{t("diagnostics.export.privacyNote")}</Hint>
        <div className="actionRow">
          <Button
            variant="primary"
            onClick={() => {
              setExported(null);
              setExportOpen(true);
            }}
          >
            {t("diagnostics.export.button")}
          </Button>
        </div>
        {exported ? (
          <div className="exportResult">
            <h3>{t("diagnostics.export.result")}</h3>
            {exported.length === 0 ? (
              <p className="hint">{t("diagnostics.export.empty")}</p>
            ) : (
              <ul className="fileList">
                {exported.map((file) => (
                  <li key={file}>
                    <Mono>{file}</Mono>
                  </li>
                ))}
              </ul>
            )}
          </div>
        ) : null}
      </Card>

      {exportOpen ? (
        <Modal
          title={t("diagnostics.export.button")}
          onClose={() => setExportOpen(false)}
          footer={
            <>
              <Button onClick={() => setExportOpen(false)}>{t("common.cancel")}</Button>
              <Button
                variant="primary"
                onClick={() => void runExport()}
                disabled={destDir.trim().length === 0 || app.busy === "export"}
              >
                {app.busy === "export" ? t("diagnostics.export.busy") : t("diagnostics.export.action")}
              </Button>
            </>
          }
        >
          <p className="warningBody">{t("diagnostics.export.privacyNote")}</p>
          <div className="field">
            <label htmlFor="export-dest">{t("diagnostics.export.destLabel")}</label>
            <TextInput id="export-dest" value={destDir} onChange={setDestDir} />
            <p className="fieldHint">{t("diagnostics.export.destHint")}</p>
          </div>
          {destDir.trim().length === 0 ? (
            <Hint tone="danger">{t("diagnostics.export.required")}</Hint>
          ) : null}
        </Modal>
      ) : null}

      <ResetDialog open={resetOpen} onClose={() => setResetOpen(false)} />
    </section>
  );
}
