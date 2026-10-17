# Bundled resources

Everything in this directory is copied into the installed application by the Tauri bundler
(`resources/**/*` in `tauri.conf.json`). The installer must be complete and offline-capable, so
the following artefacts are staged here before an installer build:

| Path | Content | Staged by |
|---|---|---|
| `qemu/` | QEMU for Windows x86-64 (`qemu-system-x86_64.exe`, its DLLs, `share/edk2-*-code.fd`) | `scripts/stage-resources.ps1` |
| `images/base.qcow2` | The verified Alpine base image, built from `vm/` | `scripts/stage-resources.ps1` |
| `images/base.qcow2.sha256` | SHA-256 of the base image, pinned by the installer | `scripts/stage-resources.ps1` |
| `licenses/third-party.json` | Third-party licence inventory required by the distribution obligations | `scripts/stage-resources.ps1` |
| `webview2/` | WebView2 offline installer (the Tauri `offlineInstaller` mode expects it in the bundle) | installer jobs |

Development builds do not need any of this: `locate_qemu` falls back to a system QEMU
installation and the base image is discovered in the development checkout (`vm/out/`) or through
`OI_LINUX_BASE_IMAGE`. `scripts/stage-resources.ps1` is what turns a checkout into a release
bundle.
