import { useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { initEnvironment, onInitProgress, preflight } from "../api";
import { toErrorMessage, formatMiB } from "../format";
import { useAppState } from "../state/AppStore";
import { Badge, Button, DefList, DefRow, Hint, NumberInput } from "./ui";
import type { InitProgress, PreflightReport } from "../types";

const DEFAULT_DATA_DIR = "D:\\OI-Linux-Data";
const DEFAULT_WORKSPACE_DIR = "C:\\Users\\Public\\Documents\\OIWorkspace";

type Phase = "idle" | "running" | "done" | "failed";

export function FirstRunWizard({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation();
  const { state, setState } = useAppState();

  const [step, setStep] = useState(0);
  const [checkSeq, setCheckSeq] = useState(0);
  const [report, setReport] = useState<PreflightReport | null>(null);
  const [reportError, setReportError] = useState<string | null>(null);
  const [relink, setRelink] = useState(false);
  const [dataDir, setDataDir] = useState(state?.environment?.data_dir ?? DEFAULT_DATA_DIR);
  const [workspaceDir, setWorkspaceDir] = useState(
    state?.config?.workspace_dir ?? DEFAULT_WORKSPACE_DIR,
  );
  const [vcpu, setVcpu] = useState(2);
  const [memory, setMemory] = useState(1024);
  const [disk, setDisk] = useState(8);
  const [phase, setPhase] = useState<Phase>("idle");
  const [progress, setProgress] = useState<InitProgress | null>(null);
  const [initError, setInitError] = useState<string | null>(null);

  useEffect(() => {
    if (step !== 0) return;
    const holder = { cancelled: false };
    setReportError(null);
    void (async () => {
      try {
        const next = await preflight();
        if (!holder.cancelled) setReport(next);
      } catch (cause) {
        if (!holder.cancelled) setReportError(toErrorMessage(cause));
      }
    })();
    return () => {
      holder.cancelled = true;
    };
  }, [step, checkSeq]);

  const blocked = useMemo(
    () => (report ? report.items.some((item) => item.blocking && !item.ok) : true),
    [report],
  );

  const pathsValid =
    dataDir.trim().length > 0 &&
    workspaceDir.trim().length > 0 &&
    dataDir.trim().toLowerCase() !== workspaceDir.trim().toLowerCase();

  const resourcesValid = vcpu >= 1 && vcpu <= 16 && memory >= 256 && memory <= 16384;

  const canAdvance = (() => {
    if (step === 0) return report !== null && !blocked;
    if (step === 2) return pathsValid;
    if (step === 3) return resourcesValid;
    return true;
  })();

  const startInitialization = async () => {
    setPhase("running");
    setInitError(null);
    setProgress(null);
    const holder: { stop: UnlistenFn | null } = { stop: null };
    holder.stop = await onInitProgress((payload) => setProgress(payload));
    try {
      const next = await initEnvironment({
        dataDir: dataDir.trim(),
        workspaceDir: workspaceDir.trim(),
        vcpu,
        memoryMib: memory,
        diskGib: disk,
        relink,
      });
      setState(next);
      setPhase("done");
    } catch (cause) {
      setInitError(toErrorMessage(cause));
      setPhase("failed");
    } finally {
      if (holder.stop) holder.stop();
    }
  };

  const percent = progress ? Math.max(0, Math.min(100, Math.round(progress.percent))) : 0;

  return (
    <div className="wizardBackdrop">
      <div className="wizard" role="dialog" aria-modal="true" aria-label={t("wizard.title")}>
        <header className="wizardHeader">
          <div>
            <h1>{t("wizard.title")}</h1>
            <p className="pageDescription">{t("wizard.subtitle")}</p>
          </div>
          <span className="wizardStepCount">
            {t("wizard.stepOf", { current: step + 1, total: 5 })}
          </span>
        </header>

        <ol className="wizardSteps">
          {(
            ["preflight", "mode", "paths", "resources", "initialize"] as const
          ).map((key, index) => (
            <li
              key={key}
              className={`wizardStep${index === step ? " wizardStep--active" : ""}${
                index < step ? " wizardStep--done" : ""
              }`}
            >
              <span className="wizardStepIndex">{index + 1}</span>
              <span>{t(`wizard.step.${key}`)}</span>
            </li>
          ))}
        </ol>

        <div className="wizardBody">
          {step === 0 ? (
            <>
              <h2>{t("wizard.preflight.title")}</h2>
              <p className="cardDescription">{t("wizard.preflight.subtitle")}</p>
              {reportError ? (
                <Hint tone="danger">{reportError}</Hint>
              ) : null}
              {report === null && !reportError ? (
                <p className="hint">{t("common.loading")}</p>
              ) : null}
              {report && report.items.length === 0 ? (
                <p className="hint">{t("wizard.preflight.empty")}</p>
              ) : null}
              {report && report.items.length > 0 ? (
                <ul className="preflightList">
                  {report.items.map((item) => (
                    <li key={item.id} className="preflightItem">
                      <Badge tone={item.ok ? "green" : item.blocking ? "red" : "amber"}>
                        {item.ok ? t("wizard.preflight.ok") : t("wizard.preflight.failed")}
                      </Badge>
                      <span className="preflightLabel">{t(item.label_key)}</span>
                      <span className="preflightKind">
                        {item.blocking
                          ? t("wizard.preflight.blocking")
                          : t("wizard.preflight.optional")}
                      </span>
                      <span className="preflightDetail">{item.detail}</span>
                    </li>
                  ))}
                </ul>
              ) : null}
              {report ? (
                <Hint tone={report.ok && !blocked ? "info" : "warn"}>
                  {report.ok && !blocked
                    ? t("wizard.preflight.reportOk")
                    : t("wizard.preflight.reportFailed")}
                </Hint>
              ) : null}
              <Button onClick={() => setCheckSeq((value) => value + 1)}>
                {t("wizard.preflight.rerun")}
              </Button>
            </>
          ) : null}

          {step === 1 ? (
            <>
              <h2>{t("wizard.mode.title")}</h2>
              <p className="cardDescription">{t("wizard.mode.subtitle")}</p>
              <div className="choiceGrid">
                <label className={`choice${relink ? "" : " choice--selected"}`}>
                  <input
                    type="radio"
                    name="wizard-mode"
                    checked={!relink}
                    onChange={() => setRelink(false)}
                  />
                  <span className="choiceTitle">{t("wizard.mode.createTitle")}</span>
                  <span className="choiceBody">{t("wizard.mode.createBody")}</span>
                </label>
                <label className={`choice${relink ? " choice--selected" : ""}`}>
                  <input
                    type="radio"
                    name="wizard-mode"
                    checked={relink}
                    onChange={() => setRelink(true)}
                  />
                  <span className="choiceTitle">{t("wizard.mode.relinkTitle")}</span>
                  <span className="choiceBody">{t("wizard.mode.relinkBody")}</span>
                </label>
              </div>
            </>
          ) : null}

          {step === 2 ? (
            <>
              <h2>{t("wizard.paths.title")}</h2>
              <p className="cardDescription">{t("wizard.paths.subtitle")}</p>
              <div className="field">
                <label htmlFor="wizard-data-dir">{t("wizard.paths.dataDir")}</label>
                <input
                  id="wizard-data-dir"
                  className="textInput"
                  type="text"
                  spellCheck={false}
                  value={dataDir}
                  onChange={(event) => setDataDir(event.target.value)}
                />
                <p className="fieldHint">{t("wizard.paths.dataDirHint")}</p>
              </div>
              <div className="field">
                <label htmlFor="wizard-workspace-dir">{t("wizard.paths.workspaceDir")}</label>
                <input
                  id="wizard-workspace-dir"
                  className="textInput"
                  type="text"
                  spellCheck={false}
                  value={workspaceDir}
                  onChange={(event) => setWorkspaceDir(event.target.value)}
                />
                <p className="fieldHint">{t("wizard.paths.workspaceDirHint")}</p>
              </div>
              <Hint tone="warn">{t("wizard.paths.workspacePrimaryNote")}</Hint>
              {!pathsValid ? <Hint tone="danger">{t("wizard.paths.required")}</Hint> : null}
            </>
          ) : null}

          {step === 3 ? (
            <>
              <h2>{t("wizard.resources.title")}</h2>
              <p className="cardDescription">{t("wizard.resources.subtitle")}</p>
              <div className="fieldRow">
                <div className="field">
                  <label htmlFor="wizard-vcpu">{t("wizard.resources.vcpu")}</label>
                  <NumberInput
                    id="wizard-vcpu"
                    value={vcpu}
                    onChange={setVcpu}
                    min={1}
                    max={16}
                  />
                </div>
                <div className="field">
                  <label htmlFor="wizard-memory">{t("wizard.resources.memory")}</label>
                  <NumberInput
                    id="wizard-memory"
                    value={memory}
                    onChange={setMemory}
                    min={256}
                    max={16384}
                    step={128}
                  />
                </div>
                <div className="field">
                  <label htmlFor="wizard-disk">{t("wizard.resources.disk")}</label>
                  <NumberInput id="wizard-disk" value={disk} onChange={setDisk} min={8} max={512} />
                </div>
              </div>
              <p className="fieldHint">{t("wizard.resources.defaults")}</p>
              <Hint tone="warn">{t("wizard.resources.diskNote")}</Hint>
            </>
          ) : null}

          {step === 4 ? (
            <>
              <h2>{t("wizard.init.title")}</h2>
              <p className="cardDescription">{t("wizard.init.subtitle")}</p>
              <DefList>
                <DefRow label={t("wizard.paths.dataDir")}>{dataDir}</DefRow>
                <DefRow label={t("wizard.paths.workspaceDir")}>{workspaceDir}</DefRow>
                <DefRow label={t("wizard.resources.vcpu")}>{vcpu}</DefRow>
                <DefRow label={t("wizard.resources.memory")}>{formatMiB(memory)}</DefRow>
                <DefRow label={t("wizard.resources.disk")}>{disk} GiB</DefRow>
                <DefRow label={t("wizard.mode.title")}>
                  {relink ? t("wizard.mode.relinkTitle") : t("wizard.mode.createTitle")}
                </DefRow>
              </DefList>

              {phase === "idle" ? (
                <Button variant="primary" onClick={() => void startInitialization()}>
                  {t("wizard.init.start")}
                </Button>
              ) : null}

              {phase === "running" ? (
                <div className="progressBlock">
                  <p className="progressLabel">{t("wizard.init.running")}</p>
                  <div className="progressTrack">
                    <div className="progressBar" style={{ width: `${percent}%` }} />
                  </div>
                  <p className="progressMeta">
                    {t("wizard.init.currentStep")}: {progress ? t(progress.step_key) : "—"}
                    {progress && progress.detail ? ` · ${progress.detail}` : ""}
                  </p>
                  <p className="progressMeta">{t("wizard.init.percent", { value: percent })}</p>
                </div>
              ) : null}

              {phase === "done" ? (
                <Hint tone="info">
                  <strong>{t("wizard.init.success")}</strong>
                  <br />
                  {t("wizard.init.resultNote")}
                </Hint>
              ) : null}

              {phase === "failed" ? (
                <>
                  <Hint tone="danger">
                    <strong>{t("wizard.init.failed")}</strong>
                    <br />
                    {initError ?? t("error.noDetail")}
                    <br />
                    {t("wizard.init.retryNote")}
                  </Hint>
                  <Button onClick={() => setPhase("idle")}>{t("common.retry")}</Button>
                </>
              ) : null}
            </>
          ) : null}
        </div>

        <footer className="wizardFooter">
          <Button onClick={onClose} disabled={phase === "running"}>
            {t("common.close")}
          </Button>
          <div className="wizardFooterRight">
            <Button
              onClick={() => setStep((value) => Math.max(0, value - 1))}
              disabled={step === 0 || phase === "running"}
            >
              {t("common.back")}
            </Button>
            <Button
              variant="primary"
              onClick={() => setStep((value) => Math.min(4, value + 1))}
              disabled={!canAdvance || step === 4}
            >
              {t("common.next")}
            </Button>
            {phase === "done" ? (
              <Button variant="primary" onClick={onClose}>
                {t("common.finish")}
              </Button>
            ) : null}
          </div>
        </footer>
      </div>
    </div>
  );
}
