import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AppConfig,
  AppState,
  EnvironmentInfo,
  InitEnvironmentRequest,
  InitProgress,
  LogFileInfo,
  PreflightReport,
  TerminalDataPayload,
  TerminalExitPayload,
  TerminalOpenResult,
  ThirdPartyLicense,
  UpdateChannel,
  UpdateInfo,
  UpdateProgress,
  WhpxResult,
} from "./types";

export const STATE_EVENT = "oi://state";
export const INIT_PROGRESS_EVENT = "oi://init-progress";
export const UPDATE_PROGRESS_EVENT = "oi://update-progress";

export function isTauri(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function terminalDataEvent(id: string): string {
  return `oi://terminal/${id}`;
}

function terminalExitEvent(id: string): string {
  return `oi://terminal/${id}/exit`;
}

export type TerminalChunk = string | Uint8Array;

/* ------------------------------------------------------------------ *
 * Development-only stand-in for the Rust backend. It exists so that
 * `vite build` and a plain-browser `vite preview` exercise the real UI
 * without Tauri. It is never used when `isTauri()` is true.
 * ------------------------------------------------------------------ */
const DEV_ENV: EnvironmentInfo = {
  initialized: true,
  data_dir: "D:\\OI-Linux-Data",
  workspace_dir: "C:\\Users\\Public\\Documents\\OIWorkspace",
  alpine_version: "3.21.0",
  schema: 1,
  created_at: "2026-01-01T00:00:00Z",
  base_sha256: "dev0dev0dev0dev0dev0dev0dev0dev0dev0dev0dev0dev0dev0dev0dev0dev0",
};

const DEV_CONFIG: AppConfig = {
  vcpu: 2,
  memory_mib: 1024,
  disk_gib: 8,
  language: "zh-CN",
  workspace_dir: DEV_ENV.workspace_dir,
  data_dir: DEV_ENV.data_dir,
  update_channel: "stable",
};

let dev: AppState = {
  vm: {
    status: "stopped",
    accelerator: null,
    pid: null,
    last_error: null,
    consecutive_start_failures: 0,
    uptime_secs: null,
  },
  share: {
    status: "not_mounted",
    mount_point: "/home/student/workspace",
    sftp_port: null,
    transport: null,
    last_error: null,
    last_check_unix: null,
  },
  ssh: { host_port: null, ready: false },
  environment: null,
  config: null,
  host: {
    qemu_path: null,
    qemu_version: null,
    whpx_available: false,
    whpx_enabled: false,
    windows_terminal_available: false,
    vscode_available: false,
    total_memory_mib: 8192,
    free_disk_mib: 40960,
    windows_build: "dev",
  },
};

function devSet(patch: Partial<AppState>): AppState {
  dev = { ...dev, ...patch };
  return dev;
}

function devInit(req: InitEnvironmentRequest): AppState {
  return devSet({
    environment: { ...DEV_ENV, data_dir: req.dataDir, workspace_dir: req.workspaceDir },
    config: {
      ...DEV_CONFIG,
      vcpu: req.vcpu,
      memory_mib: req.memoryMib,
      disk_gib: req.diskGib,
      workspace_dir: req.workspaceDir,
      data_dir: req.dataDir,
    },
  });
}

/* ------------------------------------------------------------------ */

export async function getState(): Promise<AppState> {
  if (!isTauri()) return dev;
  return invoke<AppState>("get_state");
}

export async function preflight(): Promise<PreflightReport> {
  if (!isTauri()) {
    return {
      ok: true,
      items: [
        {
          id: "windows_x64",
          label_key: "preflight.windows_x64",
          ok: true,
          detail: "dev",
          blocking: true,
        },
        {
          id: "whpx",
          label_key: "preflight.whpx",
          ok: false,
          detail: "dev",
          blocking: false,
        },
      ],
    };
  }
  return invoke<PreflightReport>("preflight");
}

export async function initEnvironment(req: InitEnvironmentRequest): Promise<AppState> {
  if (!isTauri()) return devInit(req);
  return invoke<AppState>("init_environment", { ...req });
}

export async function setWorkspaceDir(path: string): Promise<AppState> {
  if (!isTauri()) {
    return devSet({
      environment: dev.environment ? { ...dev.environment, workspace_dir: path } : null,
      config: dev.config ? { ...dev.config, workspace_dir: path } : null,
    });
  }
  return invoke<AppState>("set_workspace_dir", { path });
}

export async function setResources(vcpu: number, memoryMib: number): Promise<AppState> {
  if (!isTauri()) {
    return devSet({
      config: dev.config ? { ...dev.config, vcpu, memory_mib: memoryMib } : null,
    });
  }
  return invoke<AppState>("set_resources", { vcpu, memoryMib });
}

export async function resizeDisk(gib: number): Promise<AppState> {
  if (!isTauri()) {
    return devSet({ config: dev.config ? { ...dev.config, disk_gib: gib } : null });
  }
  return invoke<AppState>("resize_disk", { gib });
}

export async function setLanguage(language: AppConfig["language"]): Promise<AppState> {
  if (!isTauri()) {
    return devSet({ config: dev.config ? { ...dev.config, language } : null });
  }
  return invoke<AppState>("set_language", { language });
}

export async function startVm(): Promise<AppState> {
  if (!isTauri()) {
    if (!dev.environment && dev.config === null) {
      devSet({ environment: DEV_ENV, config: DEV_CONFIG });
    }
    return devSet({
      vm: { ...dev.vm, status: "running", accelerator: "tcg", pid: 4242, uptime_secs: 1 },
      ssh: { host_port: 2222, ready: true },
    });
  }
  return invoke<AppState>("start_vm");
}

export async function stopVm(force: boolean): Promise<AppState> {
  if (!isTauri()) {
    return devSet({
      vm: { ...dev.vm, status: "stopped", pid: null, uptime_secs: null },
      ssh: { host_port: null, ready: false },
      share: { ...dev.share, status: "not_mounted", transport: null, sftp_port: null },
    });
  }
  return invoke<AppState>("stop_vm", { force });
}

export async function resetEnvironment(): Promise<AppState> {
  if (!isTauri()) {
    return devSet({
      vm: { ...dev.vm, status: "stopped", pid: null, last_error: null, consecutive_start_failures: 0, uptime_secs: null },
      share: { ...dev.share, status: "not_mounted", transport: null, sftp_port: null, last_error: null },
      ssh: { host_port: null, ready: false },
    });
  }
  return invoke<AppState>("reset_environment");
}

export async function remountShare(): Promise<AppState> {
  if (!isTauri()) {
    return devSet({
      share: { ...dev.share, status: "mounted", transport: "direct", sftp_port: 34567 },
    });
  }
  return invoke<AppState>("remount_share");
}

export async function openWindowsTerminal(): Promise<void> {
  if (!isTauri()) return;
  return invoke<void>("open_windows_terminal");
}

export async function openVscodeWorkspace(): Promise<void> {
  if (!isTauri()) return;
  return invoke<void>("open_vscode_workspace");
}

export async function terminalOpen(cols: number, rows: number): Promise<TerminalOpenResult> {
  if (!isTauri()) return { id: `dev-${cols}x${rows}` };
  return invoke<TerminalOpenResult>("terminal_open", { cols, rows });
}

export async function terminalWrite(id: string, data: string): Promise<void> {
  if (!isTauri()) return;
  return invoke<void>("terminal_write", { id, data });
}

export async function terminalResize(
  id: string,
  cols: number,
  rows: number,
): Promise<void> {
  if (!isTauri()) return;
  return invoke<void>("terminal_resize", { id, cols, rows });
}

export async function terminalClose(id: string): Promise<void> {
  if (!isTauri()) return;
  return invoke<void>("terminal_close", { id });
}

export async function enableWhpx(): Promise<WhpxResult> {
  if (!isTauri()) {
    return { started: false, message: "dev: WHPX enablement requires the Tauri host." };
  }
  return invoke<WhpxResult>("enable_whpx");
}

export async function setUpdateChannel(channel: UpdateChannel): Promise<AppState> {
  if (!isTauri()) {
    return devSet({ config: dev.config ? { ...dev.config, update_channel: channel } : null });
  }
  return invoke<AppState>("set_update_channel", { channel });
}

export async function checkUpdates(): Promise<UpdateInfo | null> {
  if (!isTauri()) return null;
  return invoke<UpdateInfo | null>("check_updates");
}

export async function installUpdate(): Promise<void> {
  if (!isTauri()) return;
  return invoke<void>("install_update");
}

export async function listLogs(): Promise<LogFileInfo[]> {
  if (!isTauri()) return [];
  return invoke<LogFileInfo[]>("list_logs");
}

export async function exportLogs(destDir: string): Promise<string[]> {
  if (!isTauri()) return [];
  return invoke<string[]>("export_logs", { destDir });
}

export async function thirdPartyLicenses(): Promise<ThirdPartyLicense[]> {
  if (!isTauri()) return [];
  return invoke<ThirdPartyLicense[]>("third_party_licenses");
}

export async function onState(handler: (state: AppState) => void): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<AppState>(STATE_EVENT, (event) => handler(event.payload));
}

export async function onInitProgress(
  handler: (progress: InitProgress) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<InitProgress>(INIT_PROGRESS_EVENT, (event) => handler(event.payload));
}

export async function onUpdateProgress(
  handler: (progress: UpdateProgress) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<UpdateProgress>(UPDATE_PROGRESS_EVENT, (event) => handler(event.payload));
}

export async function onTerminalData(
  id: string,
  handler: (chunk: TerminalChunk) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<TerminalDataPayload>(terminalDataEvent(id), (event) => {
    const payload = event.payload;
    handler(payload.encoding === "base64" ? base64ToBytes(payload.data) : payload.data);
  });
}

export async function onTerminalExit(
  id: string,
  handler: (payload: TerminalExitPayload) => void,
): Promise<UnlistenFn> {
  if (!isTauri()) return () => undefined;
  return listen<TerminalExitPayload>(terminalExitEvent(id), (event) => handler(event.payload));
}

function base64ToBytes(value: string): Uint8Array {
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}
