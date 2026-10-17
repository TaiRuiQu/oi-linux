export type VmStatus =
  | "uninitialized"
  | "stopped"
  | "starting"
  | "running"
  | "stopping"
  | "start_error";

export type ShareStatus =
  | "not_mounted"
  | "mounting"
  | "mounted"
  | "degraded"
  | "recovering"
  | "error";

export type Accelerator = "whpx" | "tcg";

export interface VmState {
  status: VmStatus;
  accelerator: Accelerator | null;
  pid: number | null;
  last_error: string | null;
  consecutive_start_failures: number;
  uptime_secs: number | null;
}

export interface ShareState {
  status: ShareStatus;
  mount_point: string;
  sftp_port: number | null;
  transport: "direct" | "reverse_tunnel" | null;
  last_error: string | null;
  last_check_unix: number | null;
}

export interface SshState {
  host_port: number | null;
  ready: boolean;
}

export interface EnvironmentInfo {
  initialized: boolean;
  data_dir: string;
  workspace_dir: string;
  alpine_version: string;
  schema: number;
  created_at: string;
  base_sha256: string;
}

export interface AppConfig {
  vcpu: number;
  memory_mib: number;
  disk_gib: number;
  language: "zh-CN" | "en-US";
  workspace_dir: string;
  data_dir: string;
  update_channel: "stable" | "beta";
}

export interface HostInfo {
  qemu_path: string | null;
  qemu_version: string | null;
  whpx_available: boolean;
  whpx_enabled: boolean;
  windows_terminal_available: boolean;
  vscode_available: boolean;
  total_memory_mib: number;
  free_disk_mib: number;
  windows_build: string;
}

export interface PreflightItem {
  id: string;
  label_key: string;
  ok: boolean;
  detail: string;
  blocking: boolean;
}

export interface PreflightReport {
  ok: boolean;
  items: PreflightItem[];
}

export interface AppState {
  vm: VmState;
  share: ShareState;
  ssh: SshState;
  environment: EnvironmentInfo | null;
  config: AppConfig | null;
  host: HostInfo;
}

export interface LogFileInfo {
  name: string;
  path: string;
  size_bytes: number;
  modified_unix: number;
}

export interface UpdateInfo {
  channel: "stable" | "beta";
  version: string;
  notes: string;
  published_at: string;
  sha256: string | null;
  download_url: string;
  downgrade: boolean;
  signature: string | null;
}

export interface InitEnvironmentRequest {
  dataDir: string;
  workspaceDir: string;
  vcpu: number;
  memoryMib: number;
  diskGib: number;
  relink: boolean;
}

export interface TerminalOpenResult {
  id: string;
}

export interface TerminalDataPayload {
  id: string;
  data: string;
  encoding?: "utf8" | "base64";
}

export interface TerminalExitPayload {
  id: string;
  code: number | null;
}

export interface InitProgress {
  step_key: string;
  detail: string;
  percent: number;
}

export interface UpdateProgress {
  downloaded: number;
  total: number | null;
}

export interface WhpxResult {
  started: boolean;
  message: string;
}

export interface ThirdPartyLicense {
  name: string;
  version: string;
  license: string;
  url: string;
}

export type Language = AppConfig["language"];
export type UpdateChannel = AppConfig["update_channel"];
