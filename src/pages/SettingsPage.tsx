import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { enableWhpx, resizeDisk, setLanguage, setResources, setWorkspaceDir } from "../api";
import { formatMiB } from "../format";
import { applyLanguage, isLanguage } from "../i18n";
import { useAppState } from "../state/AppStore";
import { Modal } from "../components/Modal";
import {
  Button,
  Card,
  CheckRow,
  DefList,
  DefRow,
  Hint,
  NumberInput,
  TextInput,
} from "../components/ui";

export function SettingsPage() {
  const { t, i18n } = useTranslation();
  const app = useAppState();
  const state = app.state;

  const config = state?.config ?? null;
  const host = state?.host ?? null;
  const vmStatus = state?.vm.status ?? "uninitialized";
  const stopped = vmStatus === "stopped";
  const anyBusy = app.busy !== null;

  const [vcpu, setVcpu] = useState(config?.vcpu ?? 2);
  const [memory, setMemory] = useState(config?.memory_mib ?? 1024);
  const [workspace, setWorkspace] = useState(config?.workspace_dir ?? "");
  const [diskTarget, setDiskTarget] = useState(config?.disk_gib ?? 8);
  const [diskAck, setDiskAck] = useState(false);
  const [diskModalOpen, setDiskModalOpen] = useState(false);
  const [whpxStarted, setWhpxStarted] = useState<string | null>(null);

  useEffect(() => {
    if (config) {
      setVcpu(config.vcpu);
      setMemory(config.memory_mib);
    }
  }, [config?.vcpu, config?.memory_mib]);

  useEffect(() => {
    if (config) setWorkspace(config.workspace_dir);
  }, [config?.workspace_dir]);

  useEffect(() => {
    if (config) setDiskTarget(config.disk_gib);
  }, [config?.disk_gib]);

  if (!state || !config) {
    return (
      <section className="page">
        <p className="hint">{t("common.loading")}</p>
      </section>
    );
  }

  const activeLanguage = isLanguage(i18n.language) ? i18n.language : "zh-CN";
  const resourcesValid = vcpu >= 1 && vcpu <= 16 && memory >= 256 && memory <= 16384;

  const changeLanguage = async (next: "zh-CN" | "en-US") => {
    const ok = await app.runOther("language", async () => {
      await setLanguage(next);
      await applyLanguage(next);
    });
    if (!ok) await applyLanguage(activeLanguage);
  };

  const saveResources = () => {
    void app.runVm("resources", () => setResources(vcpu, memory));
  };

  const saveWorkspace = () => {
    if (workspace.trim().length === 0) return;
    void app.runVm("workspace", () => setWorkspaceDir(workspace.trim()));
  };

  const confirmResize = async () => {
    const ok = await app.runVm("disk", () => resizeDisk(diskTarget));
    if (ok) {
      setDiskModalOpen(false);
      setDiskAck(false);
    }
  };

  const runWhpx = async () => {
    setWhpxStarted(null);
    const ok = await app.runOther("whpx", async () => {
      const result = await enableWhpx();
      setWhpxStarted(
        `${result.started ? t("settings.whpx.started") : t("settings.whpx.notStarted")} — ${result.message}`,
      );
    });
    if (!ok) setWhpxStarted(t("settings.whpx.enableFailed"));
  };

  return (
    <section className="page">
      <header className="pageHeader">
        <div className="pageTitleBlock">
          <h1>{t("settings.title")}</h1>
          <p className="pageDescription">{t("settings.description")}</p>
        </div>
      </header>

      <Card title={t("settings.language.heading")}>
        <fieldset className="fieldset">
          <legend className="srOnly">{t("settings.language.label")}</legend>
          <div className="radioRow">
            <label className={`choice${activeLanguage === "zh-CN" ? " choice--selected" : ""}`}>
              <input
                type="radio"
                name="ui-language"
                checked={activeLanguage === "zh-CN"}
                onChange={() => void changeLanguage("zh-CN")}
              />
              <span className="choiceTitle">{t("settings.language.zh")}</span>
            </label>
            <label className={`choice${activeLanguage === "en-US" ? " choice--selected" : ""}`}>
              <input
                type="radio"
                name="ui-language"
                checked={activeLanguage === "en-US"}
                onChange={() => void changeLanguage("en-US")}
              />
              <span className="choiceTitle">{t("settings.language.en")}</span>
            </label>
          </div>
        </fieldset>
        <Hint tone="info">{t("settings.language.note")}</Hint>
      </Card>

      <Card title={t("settings.resources.heading")}>
        <div className="fieldRow">
          <div className="field">
            <label htmlFor="settings-vcpu">{t("settings.resources.vcpu")}</label>
            <NumberInput
              id="settings-vcpu"
              value={vcpu}
              onChange={setVcpu}
              min={1}
              max={16}
              disabled={!stopped}
            />
          </div>
          <div className="field">
            <label htmlFor="settings-memory">{t("settings.resources.memory")}</label>
            <NumberInput
              id="settings-memory"
              value={memory}
              onChange={setMemory}
              min={256}
              max={16384}
              step={128}
              disabled={!stopped}
            />
          </div>
        </div>
        <DefList>
          <DefRow label={t("settings.resources.current")}>
            {config.vcpu} vCPU · {formatMiB(config.memory_mib)}
          </DefRow>
        </DefList>
        {!stopped ? <Hint tone="warn">{t("settings.resources.stoppedOnly")}</Hint> : null}
        {!resourcesValid ? <Hint tone="danger">{t("settings.resources.invalidVcpu")}</Hint> : null}
        <div className="actionRow">
          <Button
            variant="primary"
            onClick={saveResources}
            disabled={!stopped || !resourcesValid || anyBusy}
          >
            {app.busy === "resources" ? t("common.running") : t("settings.resources.apply")}
          </Button>
        </div>
      </Card>

      <Card title={t("settings.disk.heading")}>
        <DefList>
          <DefRow label={t("settings.disk.current")}>
            {config.disk_gib} GiB
            <span className="mutedNote"> · {t("settings.disk.growOnly")}</span>
          </DefRow>
        </DefList>
        <Hint tone="warn">{t("settings.disk.warning")}</Hint>
        <div className="actionRow">
          <Button onClick={() => setDiskModalOpen(true)} disabled={!stopped || anyBusy}>
            {t("settings.disk.expand")}
          </Button>
        </div>
        {!stopped ? <Hint tone="info">{t("settings.disk.stoppedOnly")}</Hint> : null}
      </Card>

      <Card title={t("settings.workspace.heading")}>
        <div className="field">
          <label htmlFor="settings-workspace">{t("settings.workspace.label")}</label>
          <TextInput
            id="settings-workspace"
            value={workspace}
            onChange={setWorkspace}
            disabled={!stopped}
          />
        </div>
        <Hint tone="info">{t("settings.workspace.note")}</Hint>
        {!stopped ? <Hint tone="warn">{t("settings.workspace.stoppedOnly")}</Hint> : null}
        <div className="actionRow">
          <Button
            variant="primary"
            onClick={saveWorkspace}
            disabled={!stopped || workspace.trim().length === 0 || anyBusy}
          >
            {app.busy === "workspace" ? t("common.running") : t("settings.workspace.apply")}
          </Button>
        </div>
      </Card>

      <Card title={t("settings.whpx.heading")}>
        <DefList>
          <DefRow label={t("settings.whpx.available")}>
            {host?.whpx_available
              ? t("settings.whpx.available_yes")
              : t("settings.whpx.available_no")}
          </DefRow>
          <DefRow label={t("settings.whpx.enabled")}>
            {host?.whpx_enabled
              ? t("settings.whpx.enabled_yes")
              : t("settings.whpx.enabled_no")}
          </DefRow>
        </DefList>
        <Hint tone="warn">{t("settings.whpx.elevationNote")}</Hint>
        <Hint tone="info">{t("settings.whpx.tcgNote")}</Hint>
        <div className="actionRow">
          <Button onClick={() => void runWhpx()} disabled={anyBusy || host?.whpx_enabled === true}>
            {app.busy === "whpx" ? t("common.running") : t("settings.whpx.enable")}
          </Button>
        </div>
        {whpxStarted ? <Hint tone="info">{whpxStarted}</Hint> : null}
      </Card>

      {diskModalOpen ? (
        <Modal
          title={t("settings.disk.expand")}
          onClose={() => setDiskModalOpen(false)}
          footer={
            <>
              <Button onClick={() => setDiskModalOpen(false)}>{t("common.cancel")}</Button>
              <Button
                variant="primary"
                onClick={() => void confirmResize()}
                disabled={!diskAck || diskTarget <= config.disk_gib || anyBusy}
              >
                {app.busy === "disk" ? t("settings.disk.busy") : t("settings.disk.confirm")}
              </Button>
            </>
          }
        >
          <p className="warningBody">{t("settings.disk.warning")}</p>
          <p className="safeBody">{t("settings.disk.growOnly")}</p>
          <div className="field">
            <label htmlFor="disk-target">{t("settings.disk.target")}</label>
            <NumberInput
              id="disk-target"
              value={diskTarget}
              onChange={setDiskTarget}
              min={config.disk_gib}
              max={512}
            />
          </div>
          <CheckRow checked={diskAck} onChange={setDiskAck}>
            {t("settings.disk.ack")}
          </CheckRow>
        </Modal>
      ) : null}
    </section>
  );
}
