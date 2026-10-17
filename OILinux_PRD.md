# OI Linux V1 技术实现规范

- 状态：需求冻结草案（Architecture Baseline）；核心文件共享机制尚须 PoC 验证
- 日期：2026-10-16
- 目标平台：Windows 10/11 x86-64
- 许可协议：GPL-3.0-or-later（项目自有代码及脚本）
- 发布渠道：Stable / Beta；GitHub Releases 与国内镜像

> **最高优先级原则**：Windows NTFS 共享工作目录是用户重要文件的长期存储位置；Alpine 虚拟机是可重建的 Linux 训练环境。用户不应把唯一一份重要源代码存放在 Linux 虚拟磁盘内。Linux 重置绝不能删除或重写 Windows 工作目录。
>
> **工程状态声明**：本规范是经过需求确认的实施设计，不表示 QEMU、SSHFS 或自定义 SFTP 功能已经完成真实环境验证。文件共享、权限和 ELF 执行属于发布前必须通过的阻断性测试。

## 1. 产品目标与非目标

### 1.1 V1 核心场景

1. 用户运行离线 Windows `.exe` 安装包，首次配置 Linux 数据目录及 Windows 工作目录。
2. 用户打开 OI Linux，在管理界面手动点击「启动 Linux」。
3. Rust 后端根据可用性选择 QEMU WHPX 或 TCG，启动真实 x86-64 Alpine Linux。
4. 用户打开 Windows 原生 VS Code，编辑本地 NTFS 工作目录中的 `.cpp` 文件。
5. 同一批文件出现在 Alpine 的 `/home/student/workspace`；用户在内置多标签终端执行 `g++ main.cpp -o main` 与 `./main`。
6. 编译产物依然保存在 Windows 文件夹；关闭 OI Linux 时正常关闭虚拟机。
7. 即使 CPU 不支持硬件虚拟化，也能通过 QEMU TCG 使用真实 Linux 环境，性能可以较低。

### 1.2 V1 非目标

- 不提供 Linux 桌面、X11/Wayland、Xfce、Code::Blocks；可作为后续版本。
- 不包含或重新分发 Windows VS Code；不依赖 VS Code Remote-SSH 插件。
- 不开发 VS Code 编译快捷键、`tasks.json` 自动化或 VS Code 扩展；编译只能由 Linux 终端触发。
- 不提供 Linux 系统救援串口、快照、备份与重要 Linux 数据保障。
- 不承诺与 CSP/NOIP 正式评测机完全一致；使用 Alpine 官方 GCC/G++ 与 musl。
- 不承诺共享目录具备 ext4 全量 POSIX 语义；不要求共享目录内符号链接、硬链接、套接字等特殊文件。
- 不支持 32 位 Windows、Windows 7/8.1、网络共享路径或非 NTFS 工作目录。
- 不使用自动遥测，不在后台主动检查更新。

## 2. 确定技术栈

| 模块 | 选型 | 备注 |
|---|---|---|
| Windows 应用 | Tauri 2 | 关闭 UI 时绑定 VM 生命周期，无独立守护进程 |
| 后端 | Rust | 进程管理、QMP、SSH/SFTP、数据存储、安装与更新 |
| 前端 | React + TypeScript + Vite | UI 状态管理与多语言 |
| 终端 | xterm.js + SSH PTY | 每个标签页独立 SSH 会话 |
| 语言 | zh-CN、en-US | 默认简体中文，i18next 等国际化机制 |
| 虚拟机 | QEMU x86_64 | WHPX 优先；失败自动回退 TCG |
| Linux | Alpine stable x86-64 | 在发布时固定版本；预装工具链 |
| 系统构建 | alpine-make-vm-image + 自定义脚本 | CI 自动构建、校验与启动测试 |
| 共享首选 | Windows Rust SFTP Server + Alpine SSHFS | 未通过 PoC 前不得锁死；允许替换 |
| 权限元数据 | SQLite + NTFS 文件身份 | 仅模拟核心执行权限语义 |
| 虚拟磁盘 | qcow2 基础镜像 + qcow2 overlay | 两者均位于同一 Linux 数据目录 |
| Windows 安装与更新 | Tauri NSIS + Tauri Updater | 离线 WebView2 安装组件；独立更新签名 |
| 自动化发布 | GitHub Actions | GitHub Releases + 国内镜像同构建产物 |

## 3. 整体架构与数据流

```text
Windows 10/11 x64
 ┌─────────────────────────────────────────────────────────────────────┐
 │ Windows VS Code  ──直接编辑──► D:\OIWorkspace\main.cpp             │
 │                                     │ NTFS                         │
 │ Tauri UI (React/xterm.js)           │                              │
 │   ├─ 管理界面/状态/设置             ▼                              │
 │   ├─ 多个 SSH PTY 标签页        Rust SFTP Server + SQLite mode DB │
 │   └─ Windows Terminal 入口             ▲                           │
 │                 │                       │ QEMU user networking       │
 │ Rust Host Core  │                       │（端口与访问路径需 PoC）    │
 │   ├─ QEMU/QMP/Job Object              │                           │
 │   ├─ SSH client (端口只绑定 127.0.0.1)│                           │
 │   ├─ 共享健康检查、故障恢复           │                           │
 │   ├─ 镜像与环境管理                   │                           │
 │   └─ 签名更新、日志                   │                           │
 │                     ┌─────────────────┴───────────────────────┐   │
 │                     │  QEMU (WHPX / TCG) Alpine Linux         │   │
 │                     │  SSHD + Bash + G++ + GDB + SSHFS       │   │
 │                     │  /home/student/workspace ◄─ SSHFS      │   │
 │                     │  $ g++ main.cpp -o main; ./main         │   │
 │                     └─────────────────────────────────────────┘   │
 └─────────────────────────────────────────────────────────────────────┘
```

**文件内容的唯一权威副本**：Windows 工作目录；Linux 挂载点不作持久化内容镜像。SSHFS 层只可保留不破坏跨系统一致性的缓存。Linux ext4 虚拟磁盘独立保存系统环境及非共享目录数据。

## 4. 目录与存储模型

### 4.1 路径

- Windows 软件：默认 `C:\Program Files\OI Linux\`（可配置）。
- Linux 数据目录：默认 `C:\ProgramData\OI Linux\`，首次初始化允许指定如 `D:\OI-Linux-Data\`；初始化后不提供有损/无损迁移功能。
- Windows 工作目录：默认 `C:\Users\Public\Documents\OIWorkspace\`；可在 Linux **停止**时修改，例如 `D:\OIWorkspace\`。
- Linux 挂载点固定：`/home/student/workspace`。
- 单机只有一个 Linux 环境，不根据 Windows 登录账户拆分虚拟机；Windows 文件 ACL 对已授权使用者开放必要权限，不对所有用户全盘授予写权限。

### 4.2 Linux 数据目录建议结构

```text
D:\OI-Linux-Data\
├── environment.json          # schema、发行版本、镜像标识、Hash、创建信息
├── base.qcow2               # 初始化时复制；此环境生命周期内保持不变
├── user.qcow2               # COW 可写层，持久保存 Linux 更改
├── config.json              # 资源与工作目录映射配置
├── ssh\                     # 双向连接身份与主机密钥固定信息（严格 ACL）
├── metadata.db              # 文件执行权限元数据
├── logs\                    # 7 天轮转及大小上限
└── state\                   # 单实例锁、异常退出及启动失败计数
```

`user.qcow2` 的 backing file 使用相对路径 `base.qcow2`，必须明确固定格式并检查完整依赖链。安装程序中自带的镜像只能用于**新环境初始化或重置**；软件升级不得替换现有数据目录的 `base.qcow2`。基础镜像与 overlay 必须成对保留。

### 4.3 单机部署与机房还原

- Windows 系统盘可能被机房还原，初始化向导允许把**Linux 数据目录和 Windows 工作目录同时放到不被还原的 NTFS 分区**。
- 重新安装后可选「关联已有 Linux 环境」，只读取校验文件、验证镜像依赖和 schema，不移动数据，不自动覆盖旧环境。
- 更换 Linux 数据目录只允许重新初始化；不得伪装为原有系统搬迁。

## 5. 首次初始化与身份体系

### 5.1 初始化向导

1. 检查 Windows x64、内存不少于 8 GiB、必要磁盘剩余空间、目录为本地 NTFS。
2. 选择「创建新环境」或「关联已有环境」。
3. 选择 Linux 数据目录、工作目录，并说明后者才是长期文件存放位置。
4. 选择默认资源：2 vCPU、1024 MiB RAM、最大 8 GiB 虚拟磁盘（qcow2 按需分配）。后续可修改 CPU、内存；磁盘扩容需独立操作和文件系统扩容。
5. 复制并校验基础镜像、建立 overlay、配置密钥和 `environment.json`。初始化操作必须可恢复，失败时不可删除用户已有工作目录。
6. 管理界面显示「未运行」，由用户手动启动 Linux。

### 5.2 Linux 用户与服务

- 用户 `student`，主目录 `/home/student`，默认 Bash；SSH 密钥认证，不允许 SSH 密码登录及 root 直接登录。
- 安装 `build-base`、`gdb`、`git`、`bash`、`coreutils`、`time`、`openssh`、`sshfs`、`fuse3`、`sudo` 等（软件包名以构建时 Alpine 分支实际仓库核对）。
- `student` 被授予 `NOPASSWD: ALL` 的 sudo 权限；这是教育用途的便利设置，不是安全隔离边界。
- SSHD 在 Linux 内部运行；无需安装单独 Guest Agent。可在初始化时安装一次性 OpenRC/Shell 配置脚本，不能留常驻自定义管理守护进程。

### 5.3 密钥与引导顺序（必须 PoC 验证）

- 每台电脑生成独立 SSH 连接密钥、SFTP 身份以及主机密钥固定信息，私钥仅保存在受 Windows ACL 保护的数据目录或对应 guest 系统目录。
- 第一次启动时用**仅引导期的只读 seed 配置磁盘**交付 SSH 公钥、Windows SFTP 地址/端口和信任锚；Alpine init 脚本读取后配置 authorized_keys/known_hosts。此 seed 盘不是 Guest Agent；不得将固定密钥打入公共 `base.qcow2`。
- Guest SSH 主机密钥需每机唯一，并在 Windows SSH 客户端校验。可优先研究初始化时预生成并安全注入/固定 host key；不得关闭主机密钥校验来掩盖引导问题。
- 对外部 Windows Terminal 提供受当前 Windows 用户 ACL 保护的 SSH 配置/私钥引用，不能暴露私钥到共享工作目录。
- 基础镜像不保存可复用的 SSH 主机私钥、密码或更新签名私钥。

## 6. QEMU 启动与生命周期

### 6.1 执行后端选择

- 自动尝试 WHPX 可用性测试；仅在真正可初始化时采用 WHPX。
- 未启用 WHPX、BIOS 禁止虚拟化、CPU 不支持或加速初始化异常，自动使用 TCG。
- 提供**用户主动触发**的「启用 Windows Hypervisor Platform」操作，提示提权和可能重启；拒绝后不影响 TCG。
- 8 GiB 主机内存最低要求；默认 1 GiB guest RAM、2 vCPU；不承诺旧 CPU 上编译速度达到 WHPX。

### 6.2 QEMU 配置原则（伪模板，实际端口动态生成）

```text
qemu-system-x86_64
  -accel whpx | -accel tcg          # 启动器选择后端并单独处理回退
  -m 1024 -smp 2
  -drive file=user.qcow2,format=qcow2,if=virtio
  -nic user,model=virtio-net-pci,hostfwd=tcp:127.0.0.1:<ssh_host_port>-:22
  -display none
  -qmp <Windows 上限于本机的受控 IPC 通道>
  ... <首次启动 seed 配置磁盘 / 必要引导选项>
```

Rust 使用结构化 argv，不通过 shell 拼接命令，处理带空格与中文的路径。为 QMP/日志/临时文件提供进程级 ACL。SSH hostfwd 自动选端口，绑定失败时重新分配并重启该次启动流程，不依赖固定 `2222`。

### 6.3 生命周期状态机

```text
UNINITIALIZED -> STOPPED -> STARTING -> RUNNING -> STOPPING -> STOPPED
                                   \-> START_ERROR
RUNNING + SHARED | SHARE_DEGRADED | SHARE_RECOVERING
```

- 打开应用仅显示管理页面，不自动启动 Linux。
- 点击启动后等待系统可访问、SSH 就绪；SSHFS 挂载成功与否**不决定**虚拟机是否正常运行。
- 启动成功后保持管理界面，不自动打开终端。
- 内置终端多标签，每标签独立 SSH PTY；外部 Windows Terminal 按需打开；未安装 Windows Terminal 时仍有内置终端。
- 通过界面停止或正常退出应用时，优先从 SSH 发送 poweroff 并等待；SSH 不可用则使用 QMP 关机尝试；超时后提示用户是否强制终止。
- 用户在 guest 执行 reboot 后可以手动重连终端/共享；执行 poweroff 后管理界面显示已停止。
- Tauri 主进程崩溃时，借助 Windows Job Object `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` 关闭关联 QEMU；不使用常驻守护进程，不保证崩溃时所有 guest 写入安全落盘。
- Windows 启动器只能在确认所有 QEMU 写入者退出后执行 qcow2 重置或替换。

### 6.4 故障与重置

- 记录连续启动失败次数，排除已被成功回退的 WHPX 故障以及单纯的文件共享失败。
- 建议连续 3 次真实启动失败后提示重置，但仅在用户明确确认后执行。
- 重置先检查 VM 停止与工作目录位置，准备新 overlay，原子切换，确认可用后清理旧 overlay；绝不能影响 Windows 工作目录、VS Code 文件和其他系统外部文件。
- 不提供救援串口、快照或备份；重置后 guest 内安装的软件和非共享文件会丢失。

## 7. 共享文件系统实现协议

### 7.1 强制功能

- Windows 文件夹与 `/home/student/workspace` 映射为同一批文件，而不是定时或编译时复制。
- 支持 Windows/Alpine 双向的普通文件和目录创建、读写、删除、重命名、覆盖。
- 允许 `g++ main.cpp -o main` 与 `./main` 在同一共享目录直接工作。
- `chmod +x`、`chmod -x` 对当前文件的执行访问产生真实影响，且重新启动后权限持久化。
- Windows 修改后，Linux **后续的重新打开读取**能够看到已提交的最新内容；对已打开且正在被另一方写入的同一文件，不承诺强一致快照。
- 大小写行为遵循 Windows 默认 NTFS 规则；源文件名大小写冲突明确记录/报错。
- SFTP 服务不能通过 Reparse Point、Junction、符号链接、绝对路径、路径穿越、Windows 特殊路径或 NTFS Alternate Data Streams 访问共享根目录之外的对象。

### 7.2 首选机制

- Host Rust SFTP 仅共享管理员/用户选择的单一 NTFS 工作根目录；**服务始终需要认证**。
- Guest SSHFS 以 `student` 用户挂载 `/home/student/workspace`。
- Windows→Guest SSH 使用 `127.0.0.1:<dynamic_ssh_port>`，通过 QEMU `hostfwd` 映射到 guest 22。
- Guest→Windows SFTP 优先尝试 QEMU user network 的 guest-visible host 地址 `10.0.2.2` 加一个随机主机端口；**须实测 Windows 上 127.0.0.1 绑定与 guest 访问映射的行为**。若不可可靠访问，改用 QEMU 支持的 `guestfwd` 显式转发或其他仅本机 IPC 桥接。不能为了连通而把 SFTP 默认开放到 `0.0.0.0`。
- 首轮 PoC 试用 `default_permissions`、缓存超时为零/短时、按需 `direct_io`；必须测 `execve`、动态链接 ELF、mmap、覆盖写入，不得因关闭缓存破坏可执行文件运行。
- 对于覆盖重命名，需要评估并尽可能实现 OpenSSH `posix-rename@openssh.com` 协议扩展与 NTFS 替换操作；因 Windows 文件占用导致失败时返回明确错误，不得静默丢数据。

### 7.3 权限元数据 SQLite

```sql
CREATE TABLE file_mode (
    volume_id     TEXT NOT NULL,
    file_id       BLOB NOT NULL,
    created_at_fs TEXT NOT NULL,
    mode          INTEGER NOT NULL,
    file_kind     TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    PRIMARY KEY (volume_id, file_id, created_at_fs)
);
```

实际文件身份以 Windows 文件句柄与文件系统 API 核验，不能仅信任客户端提交的路径或纯文本 File ID；NTFS 删除后 ID 可能被复用，创建时间与句柄校验用于减少误关联风险，并非绝对唯一性保证。

- SFTP `STAT`/`LSTAT`/`FSTAT` 返回模拟的 POSIX mode（普通文件、目录、合理 UID/GID 映射）；`SETSTAT`/`FSETSTAT` 支持执行位修改并事务性落库。
- Linux `chmod` 调整权限，重新挂载仍保留；新建文件、编译输出按 Linux 创建语义初始化，例如普通源代码默认非执行，G++ 创建的 ELF 具有执行权限；目录具有可遍历权限。
- 按文件实体跟随模式：在 NTFS 中重命名保留元数据；由编辑器以新文件替换旧文件时视为新的文件实体、重建默认模式。
- 默认 POSIX 访问控制不完整模拟；用 `default_permissions` 确保学生操作时 `chmod -x` 后直接执行被拒绝。特殊文件不支持。
- SQLite 损坏：停止共享挂载，保留 Windows 文件，提示手动修复与权限元数据重建；不得静默修复，不能删除工作目录。

### 7.4 缓存一致性与异常恢复

- 默认正确性优先，不启用无法保证外部 Windows 修改可见性的永久内容缓存。
- SSHFS/FUSE `direct_io` 是待测策略，不是默认已确定参数；核心门槛是**最新内容可见 + ELF 可执行 + chmod 准确**同时满足。
- Windows 编辑器采用临时文件 + rename 保存时，下一次 Linux 文件打开必须看到替换后内容；已有文件句柄的语义单独记录。
- 发生断线，Linux 仍可使用；共享状态置为 `SHARE_DEGRADED`。
- 自动恢复退避建议 1、2、4、8、16、30 秒/轮；始终提供「重新挂载」，失败不触发 VM 重置。
- 重试前检查原挂载是否仍占用/失效，避免双实例；在挂载成功前，禁止将文件悄悄写入同路径的 guest 本地空目录。可使用不可写占位挂载点、访问检查以及显式故障提示。
- 恢复成功需真实进行受控的读/写/权限测试，不能只检查 TCP 握手。

### 7.5 允许的替代方案

如果 SSHFS 无法稳定通过所有强制测试，允许改用其他协议或自定义 guest FUSE 客户端；必须保留 Windows NTFS 文件夹作为用户可直接用 VS Code 编辑的真实存储，且支持双向读写、即时可见、执行与权限持久化。**不得降低用户已确认功能要求，也不得用每次编译同步副本的方案伪装为实时共享。**

## 8. 安全边界与权限

1. SSH hostfwd **仅绑定** `127.0.0.1`，不对校园局域网开放；通过密钥认证。
2. Windows SFTP 服务只面向本机虚拟网络路径，严格认证与根目录限制；Guest 拥有免密码 sudo，必须**假定 guest 内程序不可信**。
3. 对每一次文件访问执行规范化、拒绝相对越界与 NTFS 重解析点，并采用基于 Windows 文件句柄的检查减少检查/使用竞态；禁止把路径字符串拼接后直接信任。
4. Linux root 并不等于 Windows 管理员，但 guest 可写入共享目录；其 `rm -rf` 等行为可能永久删除 Windows 工作目录数据。界面提示学生使用 Git 和自行备份源代码。
5. 秘钥、元数据 SQLite、日志、qcow2、更新签名公钥不能位于共享根目录；适当使用 Windows ACL 限制访问范围。
6. 不自动收集遥测、不记录终端命令内容或源代码、日志自动轮转约 7 天；导出日志需去除用户名、路径、密钥等敏感信息。
7. **GPL-3.0-or-later 仅适用本项目自有内容**；随包分发的 QEMU/GCC/Alpine/其他依赖分别遵守其原始许可证与相应源码供给义务。

## 9. Windows UI 与体验

- 主导航：管理页、终端、多语言设置、更新与关于、诊断。
- 管理页显示：VM 状态、WHPX/TCG、vCPU、内存、Linux 数据目录、Windows 工作目录、SSH 与共享状态；操作包括启动/停止、打开内置终端、打开 Windows Terminal、用 VS Code 打开工作目录、重试挂载、重置。
- Linux 启动后继续显示管理页，不自动跳转终端。
- xterm.js 多标签，无分屏；独立 SSH PTY、resize、UTF-8/中文、颜色、Ctrl+C、vim/gdb 交互。
- 没有 Windows Terminal：其按钮禁用并提示安装；内置终端正常。
- 没有 VS Code：不影响 Linux 功能；仅提示 Windows VS Code 缺失，用户自行安装。
- 界面简体中文/英文，默认中文；命令和 Linux 终端输出原样展示。
- 强制明确区分 `VM_ERROR` 与 `SHARE_ERROR`，后者不能阻止 Linux 使用。

## 10. 发布、更新与兼容策略

### 10.1 离线分发

- Windows x64 完整 `.exe` 安装包，含 Windows 主程序、QEMU、可构建验证过的 Alpine 基础镜像、SSH/SFTP 组件以及 WebView2 离线安装组件。
- Windows 安装完成后**无需联网**即可正常启动、共享及运行 Linux 编译器；联网只供 `apk`、Git 等网络功能与用户主动检查更新。
- V1 Windows 安装包可以没有 Authenticode 代码签名，应如实提示 Windows SmartScreen 风险；但更新包需 Tauri 签名，分发时同时发布 SHA-256。

### 10.2 更新渠道

- GitHub Releases 与国内镜像使用**同一份构建字节**及签名；Stable/Beta 都存在同一套应用渠道设置内，共用 Linux 环境。
- **只在用户点击「检查更新」时**联机检查；不自动检查、不自动下载。
- Beta→Stable **始终允许用户选择和安装**，按用户决定接受旧程序无法读取新版配置的风险；升级/降级不自动删除、重建 Linux 数据。Tauri Updater 默认只接受较新版本，须显式配置 `allowDowngrades: true`（并核对项目锁定的插件版本），或在 Rust 侧设定等效的版本比较器；不可只切换频道 endpoint 就声称已支持降级。若旧程序无法解析数据格式，应以只读错误方式退出相关功能，不做破坏性格式迁移。
- 应用更新不得替换既有 `base.qcow2`；现存 Alpine 系统软件完全由用户自行维护（`apk`）；新发布的镜像只用于新环境或重置。
- 更新前需先正常停止 QEMU，保证没有活动的 overlay 写入句柄。

### 10.3 构建与供应链

- 构建脚本固定 Alpine 分支、软件包清单、构建工具版本、依赖来源及校验值。
- Linux CI 用 `alpine-make-vm-image` 生成基础镜像，移除私钥及构建产物，加入自动启动/SSH/G++ 检查。
- Windows CI 从相同镜像构建独立测试环境；release 必须产出完整离线安装包、SHA-256、更新签名、第三方许可证清单、构建信息文件。
- QEMU Windows 包及其动态库遵循合法分发和 GPL 源码供给义务；保留完整依赖/许可证清单。

## 11. 测试矩阵及不可绕过的验收标准

### 11.1 测试环境

| 环境 | 必须测试 |
|---|---|
| Windows 10 x64，8 GiB，无 VT-x/AMD-V | TCG 启动、编译、重置 |
| Windows 11 x64，8 GiB，WHPX 可用 | WHPX 启动、编译、关机 |
| Windows 10/11，WHPX 初始化失败 | 自动回退 TCG |
| NTFS 默认大小写不敏感，Windows VS Code 反复保存 | 文件一致性、权限实体映射 |
| 无互联网、无 Windows Terminal、无 VS Code | 完整离线核心流程 |
| C 盘还原、D 盘数据持久 | 重新关联已有 Linux 数据 |

TCG 完整集成测试为 Windows CI 强制门槛；WHPX 真机测试为正式发布前的人工/专用 runner 强制门槛。CI 不应假定通用云端 runner 支持 WHPX。

### 11.2 P0 文件共享测试（任意一项失败均阻止发布）

1. Windows 创建 `main.cpp`，Alpine 立即读取；Windows 编辑器原子替换保存后下一次 Linux 读取到新内容。
2. Linux 创建、写入、删除、重命名文件后 Windows 端立即可见。
3. `g++ main.cpp -o main && ./main` 正常运行；再次覆盖编译同名 ELF 仍正常。
4. `chmod -x main` 后，普通 student 执行 `./main` 返回权限错误；`chmod +x main` 后恢复；VM 重启后权限保留。
5. Windows/Alpine 重命名文件后权限按实体继承；Windows 替换为新文件实体后执行权限按新建规则重置。
6. 直接测试从共享目录执行动态链接 ELF，覆盖 `execve`、私有/共享 `mmap` 的典型兼容行为（至少不影响常规竞赛程序执行）。
7. Windows 对文件加独占锁、杀死 SFTP 服务、断开共享、恢复服务：不得静默造成工作目录损坏；Linux 独立可用，恢复后读写验证成功。
8. SFTP 收到 `..`、绝对路径、Junction、符号链接、ADS、路径分隔符变种请求时，任何请求不得访问共享根之外的 host 内容。
9. 中文路径、空格、Unicode 文件名、大小写不同但 NTFS 冲突的名称：行为清晰稳定，不发生越界。
10. 文件共享服务随机端口，且默认无任何校园 LAN 可访问的入站 SSH/SFTP 监听。

### 11.3 P0 生命周期与数据安全

- 不支持 VT-x 仍能通过 TCG 启动真实 Alpine。
- 停止虚拟机后重启仍保留 Linux 软件/设置。
- 系统重置只影响 overlay 与 guest 文件，不删除 Windows 工作目录。
- 修改共享工作目录只允许在 Linux 停止时执行。
- 启动失败 3 次后仅建议重置，不自动清理。
- 崩溃时由 Job Object 清理 QEMU；重新启动具备 qcow2/ext4 失败恢复路径，容忍可能的数据丢失但不隐藏风险。
- 更新/切换 Beta/Stable 时不可静默替换 backing file 或清空环境。
- 离线安装无需联网下载 QEMU、WebView2、Alpine 或编译器。

### 11.4 性能测试（先建立基线，V1 不预设无法证明的秒数目标）

- 100、1000、5000 个源码/测试文件的 `ls`、`find` 与读取性能。
- 典型竞赛题单文件 C++17 编译和执行延迟：WHPX、TCG 分别采样。
- 共享目录编译与 guest 内 ext4 编译的性能差距；记录系统配置和数据规模。
- 关机/重启后的首次读取性能；长时间运行期间内存和后台 CPU。
- 8 GiB Windows 机器上的 RAM 余量与 WebView2/Tauri/SSHFS/QEMU 合计占用。

## 12. 实施里程碑

| 阶段 | 交付内容 | 完成定义 |
|---|---|---|
| M0：风险 PoC | Windows Rust SFTP + QEMU/Alpine/SSHFS + 权限 PoC | 第 11.2 节所有强制文件系统测试至少在代表性 Windows 环境通过；否则更换协议 |
| M1：可启动系统 | 自动化 Alpine 镜像、WHPX/TCG、SSH 引导、QMP、数据目录 | 离线首次初始化、启动、停止、重置、重连全流程通过 |
| M2：桌面交互 | Tauri/React/xterm.js、多标签、状态管理、I18n、配置页 | 常用 Linux 命令和 G++ 可通过内置终端使用 |
| M3：稳定性与发布 | 断线恢复、Job Object、日志、更新器、GitHub Actions、国内镜像 | TCG CI 与 WHPX 实机验收通过，交付完整离线 NSIS 安装包 |

**优先级约束**：不得在 M0 核心文件共享协议尚未通过测试时，宣称方案已具备生产可行性；可并行设计 UI，但不可把 UI 完成当作核心功能完成。

## 13. 建议仓库结构

```text
oi-linux/
├── LICENSE
├── README.md
├── docs/
│   ├── architecture.md
│   ├── protocol.md
│   ├── threat-model.md
│   ├── test-plan.md
│   └── decisions.md
├── src/                       # React/TypeScript + xterm.js
├── src-tauri/
│   ├── src/
│   │   ├── vm/                # QEMU, QMP, WHPX/TCG, Job Object
│   │   ├── ssh/               # Guest SSH client / PTY
│   │   ├── share/             # Rust SFTP, permissions, handles
│   │   ├── storage/           # qcow2, init, reset, config
│   │   ├── updater/           # Stable/Beta channel
│   │   └── logging/           # tracing and local diagnostics
│   └── tauri.conf.json
├── vm/
│   ├── packages.txt
│   ├── build.sh
│   ├── rootfs/
│   ├── scripts/
│   └── tests/
├── integration-tests/
│   ├── ntfs-share/
│   ├── chmod-exec/
│   ├── boot-reset/
│   └── upgrade-relink/
├── scripts/
└── .github/workflows/
    ├── linux-image.yml
    ├── windows-integration.yml
    └── release.yml
```

## 14. 决策追溯总表（1～70）

> 本表将此前会话的逐项产品选择折叠为可追溯索引。若详细设计与简表有差异，以用户后续的**最后确认**及本文正文的限制为准。

| 编号 | 决策摘要 |
|---|---|
| 1 | 面向包括学校机房在内的 Windows 电脑，允许管理员权限但不依赖日常提权 |
| 2 | 始终是真实 Linux；无硬件加速时必须使用 QEMU TCG |
| 3 | 仅 Windows 10/11 x64 |
| 4 | Windows 与 Linux 完全共享源码及编译产物，允许直接 `./main` |
| 5 | 内置终端 + Windows Terminal |
| 6 | V1 不做 Linux GUI 桌面 |
| 7 | 单个可配置的 Windows 共享根目录 |
| 8 | Linux 系统持久化 |
| 9 | 完整离线安装包 |
| 10 | Alpine 官方 GCC/G++ 满足日常训练 |
| 11 | 不捆绑 Windows VS Code |
| 12 | 每台 Windows 电脑一个 Linux 环境 |
| 13 | 标准 `.exe` 安装程序 |
| 14 | 最低 8 GiB 物理内存 |
| 15 | 正常关闭软件时正常关闭 Linux |
| 16 | 打开应用不自动启动 Linux，由用户点击启动 |
| 17 | 仅 Linux 终端编译，无 VS Code 任务集成 |
| 18 | 普通 `student` 用户、免密码 sudo |
| 19 | **最终为 Rust + Tauri + xterm.js**（覆盖曾短暂选择的 C#） |
| 20 | 支持在线和离线更新 |
| 21 | Alpine 系统软件由用户自行维护 |
| 22 | Alpine 默认可通过 NAT 访问互联网 |
| 23 | SSH 仅本机访问 |
| 24 | 仅系统重置、不提供快照和备份 |
| 25 | 工作目录仅正式支持本地 NTFS |
| 26 | 文件名遵循 Windows NTFS 默认大小写不敏感行为 |
| 27 | 工作目录只保证普通文件及目录，不要求链接等特殊文件 |
| 28 | Linux 启动成功后留在管理界面，不自动打开终端 |
| 29 | xterm.js 内置终端支持多标签、不分屏 |
| 30 | 中文与英语 UI |
| 31 | 用户手动检查在线更新 |
| 32 | React + TypeScript + Vite |
| 33 | 项目公开源码、免费分发 |
| 34 | **GPL-3.0-or-later** |
| 35 | GitHub Releases + 国内下载镜像 |
| 36 | 自动化构建 Alpine 镜像并支持 CI |
| 37 | `chmod ±x` 真正生效，执行权限持久化 |
| 38 | 共享失败不阻止 Linux 使用 |
| 39 | 文件共享自动尝试恢复 + 手动重试 |
| 40 | Linux 内部 reboot 后允许用户手动重连，不要求自动恢复所有会话 |
| 41 | 仅本地诊断日志，无遥测上传 |
| 42 | Tauri 崩溃时 QEMU 随之结束，无独立守护进程 |
| 43 | 无救援控制台；重要文件不应只存 Linux，损坏可重置 |
| 44 | 连续启动失败主动建议重置，但必须用户确认 |
| 45 | 可选引导一键启用 WHPX，拒绝后继续 TCG |
| 46 | 仅在 Linux 停止时切换工作目录 |
| 47 | 首次初始化可自定义 Linux 数据目录 |
| 48 | 不提供已有 Linux 数据目录搬迁功能 |
| 49 | 支持手动重新关联已有 Linux 数据目录 |
| 50 | 基础镜像与用户 overlay 一起存放于数据目录 |
| 51 | 简短首次初始化向导 |
| 52 | 可调整 CPU/内存/磁盘配置 |
| 53 | `rm` 遵循直接删除语义，不强制回收站 |
| 54 | SSH + Shell 脚本，无 Guest Agent 守护进程 |
| 55 | V1 Windows 安装包允许暂不进行 Authenticode 代码签名 |
| 56 | 支持 Stable/Beta 双渠道 |
| 57 | 允许更换文件共享技术，功能要求不降级 |
| 58 | 权限跟随 NTFS 文件实体 |
| 59 | 拒绝 SFTP 特殊路径/链接入口越界 |
| 60 | 启动时自动分配本机 SSH 端口 |
| 61 | Windows + Alpine 完整 CI 集成测试 |
| 62 | Stable/Beta 同一应用内切换，共用数据 |
| 63 | 优先日常信息学竞赛项目性能 |
| 64 | 软件自动生成 SSH/SFTP 密钥和认证配置 |
| 65 | 跨 Windows/Linux 文件内容一致性优先 |
| 66 | 仅要求核心可执行权限语义，不完整模拟 POSIX |
| 67 | 权限 SQLite 损坏时停止共享、手动修复 |
| 68 | **Stable/Beta 始终允许切换**，接受版本不兼容风险，不可自动清理数据 |
| 69 | 无 Windows Terminal 时内置终端仍可用 |
| 70 | TCG Windows CI 全面测试 + WHPX 真机发布验收 |

## 15. 实施前需验证的关键技术假设（不再要求用户选择）

1. **Windows guest→host 通道可达性**：QEMU user-mode 的 `10.0.2.2` 与 Host SFTP 仅本机监听是否兼容，必要时通过 `guestfwd` 转发；以实测为准。
2. **执行权限与缓存兼容**：`default_permissions`、FUSE 缓存策略、Windows 文件替换后的 inode/权限行为，以及动态 ELF `execve`、`mmap` 能否同时满足。
3. **SFTP 重命名与 Windows 文件锁**：`posix-rename@openssh.com` 模拟程度、覆盖替换失败时的原子性和用户可理解的错误处理。
4. **真实 PoC 性能**：老式 CPU 上 TCG 的 G++ 编译体验及 8 GiB 主机内存适配。
5. **Windows 与 QEMU 生命周期**：WHPX 初始化失败回退策略、Job Object 兼容性、QMP 超时控制。
6. **安装器依赖**：WebView2 `offlineInstaller` 正常离线运行，完整第三方分发许可及源代码提供策略。

这些是工程验证与实现问题，**不是新的产品需求问题**。如某假设失败，按已确认的第 57 项授权更换底层技术，但不可降低核心使用体验。

## 官方参考资料

- QEMU 网络与 `hostfwd` / `guestfwd`：https://www.qemu.org/docs/master/system/invocation.html
- QEMU 用户态网络： https://www.qemu.org/docs/master/system/devices/net.html
- Alpine 虚拟机镜像构建工具：https://github.com/alpinelinux/alpine-make-vm-image
- SSHFS 选项与权限说明：https://man7.org/linux/man-pages/man1/sshfs.1.html
- Linux FUSE I/O 模式：https://kernel.org/doc/html/latest/filesystems/fuse/fuse-io.html
- OpenSSH SFTP 协议扩展：https://github.com/openssh/openssh-portable/blob/master/PROTOCOL
- Tauri Windows 离线安装：https://v2.tauri.app/distribute/windows-installer/
- Tauri Updater（含降级支持）：https://v2.tauri.app/plugin/updater/
- 2026-09-20 的 Updater v2.12.0 降级配置变更：https://v2.tauri.app/release/updater/v2.12.0/
- Windows Job Objects：https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects

---

**项目进入下一阶段的唯一建议**：首先做 M0 文件共享 PoC，确认无硬件虚拟化时也能在真实 Alpine SSHFS 工作目录直接执行 G++ 产物，然后按 M1–M3 实施。至此已不存在必须继续向产品负责人提问才能开始开发的决定。
