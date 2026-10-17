import { useState } from "react";
import { useTranslation } from "react-i18next";
import {
  openVscodeWorkspace,
  openWindowsTerminal,
  remountShare,
  startVm,
  stopVm,
} from "../api";
import { formatMiB, formatTimestamp, formatUnixSeconds } from "../format";
import { useAppState } from "../state/AppStore";
import { ResetDialog } from "../components/ResetDialog";
import {
  Button,
  Card,
  DefList,
  DefRow,
  Hint,
  Mono,
  PathValue,
  ShareStateBadge,
  VmStateBadge,
} from "../components/ui";

export function ManagePage({
  onOpenTerminal,
  onOpenWizard,
}: {
  onOpenTerminal: () => void;
  onOpenWizard: () => void;
}) {
  const { t, i18n } = useTranslation();
  const app = useAppState();
  const [resetOpen, setResetOpen] = useState(false);
  const state = app.state;

  if (!state) {
    return (
      <section className="page">
        <p className="hint">{t("common.loading")}</p>
      </section>
    );
  }

  const { vm, share, ssh, environment, config, host } = state;
  const locale = i18n.language;
  const initialized = environment?.initialized ?? false;
  const running = vm.status === "running";
  const stopping = vm.status === "stopping";
  const anyBusy = app.busy !== null;
  const canStart = initialized && (vm.status === "stopped" || vm.status === "start_error");
  const canStop = running || vm.status === "starting";
  const canRemount = initialized && running;
  const canReset = initialized && vm.status === "stopped";

  const uptime = (() => {
    const secs = vm.uptime_secs;
    if (secs === null) return "—";
    if (secs < 60) return `${secs} ${t("common.seconds")}`;
    if (secs < 3600) return `${Math.floor(secs / 60)} ${t("common.minutes")}`;
    return `${Math.floor(secs / 3600)} ${t("common.hours")} ${Math.floor(
      (secs % 3600) / 60,
    )} ${t("common.minutes")}`;
  })();

  const startHint = !initialized
    ? t("manage.hint.notInitialized")
    : vm.status === "starting"
      ? t("manage.hint.startingHint")
      : vm.status === "stopping"
        ? t("manage.hint.stoppingHint")
        : t("manage.hint.startReady");

  return (
    <section className="page">
      <header className="pageHeader">
        <div className="pageTitleBlock">
          <h1>{t("manage.title")}</h1>
          <p className="pageDescription">{t("manage.description")}</p>
        </div>
        <div className="pageActions">
          <VmStateBadge status={vm.status} />
          <ShareStateBadge status={share.status} />
        </div>
      </header>

      <Card title={t("manage.vmHeading")}>
        <DefList>
          <DefRow label={t("manage.field.status")}>
            <VmStateBadge status={vm.status} />
          </DefRow>
          <DefRow label={t("manage.field.accelerator")}>
            {vm.accelerator === "whpx"
              ? t("state.accel.whpx")
              : vm.accelerator === "tcg"
                ? t("state.accel.tcg")
                : t("state.accel.undetermined")}
          </DefRow>
          <DefRow label={t("manage.field.vcpu")}>{config ? config.vcpu : "—"}</DefRow>
          <DefRow label={t("manage.field.memory")}>
            {config ? formatMiB(config.memory_mib) : "—"}
          </DefRow>
          <DefRow label={t("manage.field.disk")}>
            {config ? `${config.disk_gib} GiB` : "—"}
          </DefRow>
          <DefRow label={t("manage.field.pid")}>
            {vm.pid === null ? t("manage.pidNone") : <Mono>{vm.pid}</Mono>}
          </DefRow>
          <DefRow label={t("manage.field.uptime")}>{vm.uptime_secs === null ? "—" : uptime}</DefRow>
          {vm.last_error ? (
            <DefRow label={t("manage.lastErrorLabel")}>
              <span className="inlineError">
                <span className="bannerCode">VM_ERROR</span>
                {vm.last_error}
              </span>
            </DefRow>
          ) : null}
        </DefList>
      </Card>

      <Card title={t("manage.pathsHeading")}>
        <DefList>
          <DefRow label={t("manage.field.dataDir")}>
            <PathValue value={environment?.data_dir ?? config?.data_dir} />
          </DefRow>
          <DefRow label={t("manage.field.workspaceDir")}>
            <PathValue value={environment?.workspace_dir ?? config?.workspace_dir} />
          </DefRow>
          <DefRow label={t("manage.field.alpine")}>
            {environment ? environment.alpine_version : "—"}
          </DefRow>
          <DefRow label={t("manage.field.schema")}>
            {environment ? environment.schema : "—"}
          </DefRow>
          <DefRow label={t("manage.field.createdAt")}>
            {environment ? formatTimestamp(environment.created_at, locale) : "—"}
          </DefRow>
          <DefRow label={t("manage.field.baseSha")}>
            <Mono title={environment?.base_sha256 ?? ""}>{environment?.base_sha256 ?? "—"}</Mono>
          </DefRow>
        </DefList>
        <Hint tone="info">{t("manage.workspaceNote")}</Hint>
      </Card>

      <Card title={t("manage.shareHeading")}>
        <DefList>
          <DefRow label={t("manage.field.sshStatus")}>
            <span className={ssh.ready ? "stateText stateText--ok" : "stateText"}>
              {ssh.ready ? t("state.ssh.ready") : t("state.ssh.pending")}
            </span>
          </DefRow>
          <DefRow label={t("manage.field.sshPort")}>
            {ssh.host_port === null ? "—" : <Mono>{ssh.host_port}</Mono>}
          </DefRow>
          <DefRow label={t("manage.field.shareStatus")}>
            <ShareStateBadge status={share.status} />
          </DefRow>
          <DefRow label={t("manage.field.mountPoint")}>
            <Mono>{share.mount_point}</Mono>
          </DefRow>
          <DefRow label={t("manage.field.sftpPort")}>
            {share.sftp_port === null ? "—" : <Mono>{share.sftp_port}</Mono>}
          </DefRow>
          <DefRow label={t("manage.field.transport")}>
            {share.transport === "direct"
              ? t("transport.direct")
              : share.transport === "reverse_tunnel"
                ? t("transport.reverse_tunnel")
                : "—"}
          </DefRow>
          <DefRow label={t("manage.field.lastCheck")}>
            {formatUnixSeconds(share.last_check_unix, locale)}
          </DefRow>
        </DefList>
        {share.last_error && share.status !== "error" ? (
          <Hint tone="warn">
            <span className="bannerCode">SHARE_ERROR</span> {share.last_error}
          </Hint>
        ) : null}
        <Hint tone={share.status === "error" ? "warn" : "info"}>
          {t("manage.hint.shareIndependent")}
        </Hint>
      </Card>

      {!initialized ? (
        <Card title={t("manage.notInitializedTitle")}>
          <p className="hint">{t("manage.notInitializedBody")}</p>
          <Button variant="primary" onClick={onOpenWizard}>
            {t("manage.action.openWizard")}
          </Button>
        </Card>
      ) : null}

      <Card title={t("manage.actionsHeading")}>
        <div className="actionGrid">
          <Button
            variant="primary"
            onClick={() => void app.runVm("start", startVm)}
            disabled={!canStart || anyBusy}
          >
            {app.busy === "start" ? t("common.running") : t("manage.action.start")}
          </Button>
          <Button
            onClick={() => void app.runVm("stop", () => stopVm(false))}
            disabled={!canStop || anyBusy}
          >
            {app.busy === "stop" ? t("common.running") : t("manage.action.stop")}
          </Button>
          {stopping ? (
            <Button
              variant="danger"
              onClick={() => void app.runVm("forceStop", () => stopVm(true))}
              disabled={anyBusy}
            >
              {app.busy === "forceStop" ? t("common.running") : t("manage.action.forceStop")}
            </Button>
          ) : null}
          <Button onClick={onOpenTerminal} disabled={!running}>
            {t("manage.action.openTerminal")}
          </Button>
          <Button
            onClick={() => void app.runOther("windows-terminal", openWindowsTerminal)}
            disabled={!host.windows_terminal_available || !running || anyBusy}
          >
            {t("manage.action.openWindowsTerminal")}
          </Button>
          <Button
            onClick={() => void app.runOther("vscode", openVscodeWorkspace)}
            disabled={!host.vscode_available || anyBusy}
          >
            {t("manage.action.openVscode")}
          </Button>
          <Button
            onClick={() => void app.runShare("remount", remountShare)}
            disabled={!canRemount || anyBusy}
          >
            {app.busy === "remount" ? t("common.running") : t("manage.action.remount")}
          </Button>
          <Button
            variant="danger"
            onClick={() => setResetOpen(true)}
            disabled={!canReset || anyBusy}
          >
            {t("manage.action.reset")}
          </Button>
        </div>

        <div className="hintStack">
          <Hint tone={canStart ? "info" : "warn"}>{startHint}</Hint>
          {!running ? <Hint tone="info">{t("manage.hint.needRunning")}</Hint> : null}
          {!host.windows_terminal_available ? (
            <Hint tone="warn">{t("manage.hint.wtMissing")}</Hint>
          ) : null}
          {!host.vscode_available ? <Hint tone="info">{t("manage.hint.vscodeMissing")}</Hint> : null}
          {initialized && !running ? (
            <Hint tone="info">{t("manage.hint.remountNeedRunning")}</Hint>
          ) : null}
          {initialized && !canReset ? (
            <Hint tone="warn">{t("manage.hint.resetOnlyWhenIdle")}</Hint>
          ) : null}
        </div>
      </Card>

      <ResetDialog open={resetOpen} onClose={() => setResetOpen(false)} />
    </section>
  );
}
