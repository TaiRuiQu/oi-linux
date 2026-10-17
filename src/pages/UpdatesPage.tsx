import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import type { UnlistenFn } from "@tauri-apps/api/event";
import {
  checkUpdates,
  installUpdate,
  onUpdateProgress,
  setUpdateChannel,
  thirdPartyLicenses,
} from "../api";
import { formatBytes, formatTimestamp } from "../format";
import { useAppState } from "../state/AppStore";
import { Badge, Button, Card, DefList, DefRow, Hint, Mono } from "../components/ui";
import type { ThirdPartyLicense, UpdateInfo, UpdateProgress } from "../types";

export function UpdatesPage() {
  const { t, i18n } = useTranslation();
  const app = useAppState();
  const state = app.state;
  const channel = state?.config?.update_channel ?? "stable";

  const [info, setInfo] = useState<UpdateInfo | null>(null);
  const [checked, setChecked] = useState(false);
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const [licenses, setLicenses] = useState<ThirdPartyLicense[] | null>(null);
  const [licensesError, setLicensesError] = useState<string | null>(null);

  useEffect(() => {
    const holder = { cancelled: false };
    void (async () => {
      try {
        const list = await thirdPartyLicenses();
        if (!holder.cancelled) setLicenses(list);
      } catch {
        if (!holder.cancelled) setLicensesError(t("updates.licenses.loadFailed"));
      }
    })();
    return () => {
      holder.cancelled = true;
    };
  }, [t]);

  const runCheck = async () => {
    setProgress(null);
    const ok = await app.runOther("check", async () => {
      const result = await checkUpdates();
      setInfo(result);
      setChecked(true);
    });
    if (!ok) setChecked(false);
  };

  const runInstall = async () => {
    setProgress(null);
    const holder: { stop: UnlistenFn | null } = { stop: null };
    holder.stop = await onUpdateProgress((payload) => setProgress(payload));
    try {
      await app.runOther("install", installUpdate);
    } finally {
      if (holder.stop) holder.stop();
    }
  };

  const changeChannel = (next: "stable" | "beta") => {
    void app.runVm("channel", () => setUpdateChannel(next));
  };

  const percent =
    progress && progress.total && progress.total > 0
      ? Math.max(0, Math.min(100, Math.round((progress.downloaded / progress.total) * 100)))
      : null;

  return (
    <section className="page">
      <header className="pageHeader">
        <div className="pageTitleBlock">
          <h1>{t("updates.title")}</h1>
          <p className="pageDescription">{t("updates.description")}</p>
        </div>
      </header>

      <Card title={t("updates.channel.heading")}>
        <fieldset className="fieldset">
          <legend className="srOnly">{t("updates.channel.label")}</legend>
          <div className="radioRow">
            <label className={`choice${channel === "stable" ? " choice--selected" : ""}`}>
              <input
                type="radio"
                name="update-channel"
                checked={channel === "stable"}
                onChange={() => changeChannel("stable")}
              />
              <span className="choiceTitle">{t("updates.channel.stable")}</span>
            </label>
            <label className={`choice${channel === "beta" ? " choice--selected" : ""}`}>
              <input
                type="radio"
                name="update-channel"
                checked={channel === "beta"}
                onChange={() => changeChannel("beta")}
              />
              <span className="choiceTitle">{t("updates.channel.beta")}</span>
            </label>
          </div>
        </fieldset>
        <Hint tone="info">{t("updates.channel.note")}</Hint>
      </Card>

      <Card title={t("updates.check.heading")}>
        <div className="actionRow">
          <Button variant="primary" onClick={() => void runCheck()} disabled={app.busy === "check"}>
            {app.busy === "check" ? t("updates.check.checking") : t("updates.check.button")}
          </Button>
        </div>
        <Hint tone="warn">{t("updates.check.neverAutomatic")}</Hint>
        {checked && info === null ? (
          <Hint tone="info">{t("updates.check.upToDate")}</Hint>
        ) : null}

        {info ? (
          <>
            <DefList>
              <DefRow label={t("updates.info.version")}>
                <Mono>{info.version}</Mono>
                {info.downgrade ? (
                  <span className="mutedNote"> · {t("updates.info.downgrade")}</span>
                ) : null}
              </DefRow>
              <DefRow label={t("updates.info.channel")}>
                <Badge tone={info.channel === "beta" ? "amber" : "blue"}>
                  {info.channel === "beta"
                    ? t("updates.channel.beta")
                    : t("updates.channel.stable")}
                </Badge>
              </DefRow>
              <DefRow label={t("updates.info.published")}>
                {formatTimestamp(info.published_at, i18n.language)}
              </DefRow>
              <DefRow label={t("updates.info.sha")}>
                <Mono title={info.sha256 ?? ""}>{info.sha256 ?? "—"}</Mono>
              </DefRow>
              <DefRow label={t("updates.info.signature")}>
                <Mono title={info.signature ?? ""}>{info.signature ?? "—"}</Mono>
              </DefRow>
            </DefList>
            <div className="notesBlock">
              <h3>{t("updates.info.notes")}</h3>
              <pre className="notesBody">{info.notes}</pre>
            </div>
            <Hint tone="warn">{t("updates.info.installNote")}</Hint>
            <div className="actionRow">
              <Button
                variant="primary"
                onClick={() => void runInstall()}
                disabled={app.busy === "install"}
              >
                {app.busy === "install" ? t("updates.info.installing") : t("updates.info.install")}
              </Button>
            </div>
            {progress ? (
              <div className="progressBlock">
                <p className="progressLabel">{t("updates.info.progress")}</p>
                {percent === null ? (
                  <p className="progressMeta">
                    {formatBytes(progress.downloaded)} · {t("updates.info.progressUnknown")}
                  </p>
                ) : (
                  <>
                    <div className="progressTrack">
                      <div className="progressBar" style={{ width: `${percent}%` }} />
                    </div>
                    <p className="progressMeta">
                      {formatBytes(progress.downloaded)} / {formatBytes(progress.total ?? 0)} ·{" "}
                      {percent}%
                    </p>
                  </>
                )}
              </div>
            ) : null}
          </>
        ) : null}
      </Card>

      <Card title={t("updates.signer.heading")}>
        <Hint tone="warn">{t("updates.signer.installerUnsigned")}</Hint>
        <Hint tone="info">{t("updates.signer.updatesSigned")}</Hint>
      </Card>

      <Card title={t("updates.licenses.heading")} description={t("updates.licenses.description")}>
        {licensesError ? <Hint tone="danger">{licensesError}</Hint> : null}
        {licenses === null && !licensesError ? <p className="hint">{t("common.loading")}</p> : null}
        {licenses && licenses.length === 0 ? (
          <p className="hint">{t("updates.licenses.empty")}</p>
        ) : null}
        {licenses && licenses.length > 0 ? (
          <table className="dataTable">
            <thead>
              <tr>
                <th scope="col">{t("updates.licenses.name")}</th>
                <th scope="col">{t("updates.licenses.version")}</th>
                <th scope="col">{t("updates.licenses.license")}</th>
                <th scope="col">{t("updates.licenses.url")}</th>
              </tr>
            </thead>
            <tbody>
              {licenses.map((license) => (
                <tr key={`${license.name}-${license.version}`}>
                  <td>{license.name}</td>
                  <td>
                    <Mono>{license.version}</Mono>
                  </td>
                  <td>{license.license}</td>
                  <td>
                    <span className="linkText">{license.url}</span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : null}
      </Card>
    </section>
  );
}
