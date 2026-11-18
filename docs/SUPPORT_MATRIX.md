# DT Platform Support Matrix

This matrix describes target coverage for **DT v1.0.1** (`galaxycoils/DT`). Source builds, unit/contract tests, and Apple Silicon DMG packaging pass locally; CI builds all 6 desktop installers (`DT-1.0.1-macos-aarch64.dmg`, `DT-1.0.1-macos-x86_64.dmg`, `DT-1.0.1-windows-x86_64-setup.exe`, `DT-1.0.1-windows-aarch64-setup.exe`, `dt_1.0.1_amd64.deb`, `dt_1.0.1_arm64.deb`). Full 8-target native certification remains gated on platform signing identities, physical non-Mac hardware, live provider accounts, and the 7-day soak. See [coverage inventory](release/coverage-inventory.md) and [release readiness](release-readiness.md).

---

## 1. Operating System & Architecture Support

| Operating System | Architecture | Support Tier | Workspace VM Hypervisor | Local VLM Acceleration | Status |
|---|---|---|---|---|---|
| **macOS 14+ (Sonoma, Sequoia)** | Apple Silicon (`aarch64`) | Preview / certification pending | HVF (Hypervisor.framework) | Metal (MPS) | Native evidence pending |
| **macOS 14+ (Sonoma, Sequoia)** | Intel (`x86_64`) | Preview / certification pending | HVF (Hypervisor.framework) | CPU / Accelerate | Native evidence pending |
| **Linux (Ubuntu 24.04 LTS)** | AMD64 (`x86_64`) | Preview / certification pending | KVM (`/dev/kvm`) | CUDA / Vulkan / CPU | Native evidence pending |
| **Windows 11 (23H2+)** | AMD64 (`x86_64`) | Preview / certification pending | WHPX (Windows Hypervisor Platform) | DirectML / CUDA | Native evidence pending |
| Other Linux Distributions | `x86_64` / `aarch64` | Tier 2 (Experimental) | KVM | CPU | Experimental / evidence pending |

---

## 2. Native Desktop Integration & Subsystem Matrix

| Subsystem | macOS Implementation | Linux Implementation | Windows Implementation |
|---|---|---|---|
| **Screen Capture** | ScreenCaptureKit / CGDisplayStream | Desktop Portal / PipeWire | Windows Graphics Capture (WGC) |
| **Accessibility & Inspection** | `AXUIElement` / Accessibility APIs | AT-SPI2 D-Bus (`org.a11y.Bus`) | Microsoft UI Automation (UIA) |
| **Input Simulation** | `CGEvent` synthesis | `uinput` / `ydotool` / Portal | `SendInput` / Win32 Synthesized Input |
| **Clipboard** | `NSPasteboard` | `wl-clipboard` / `xclip` | Win32 OLE / Clipboard API |
| **Notifications** | `NSUserNotificationCenter` / UNNotification | `org.freedesktop.Notifications` | Windows Toast Notifications |
| **Credential Storage** | macOS Keychain (`security-framework`) | Secret Service API / libsecret | Windows Credential Manager (`keyring`) |
| **IPC Daemon Transport** | Unix Domain Sockets (`sockaddr_un`) | Unix Domain Sockets (XDG runtime) | Named Pipes (`\\.\pipe\...`) |

---

## 3. Provider implementation status

Provider setup, model discovery, and text verification share the daemon-owned transport. Real account certification is pending. See [provider validation](provider-validation.md) for each provider's auth method, catalog source, and observed test coverage. Host CLI probes remain disabled; isolated CLI-agent certification is outstanding.

The distributed provider UI includes ChatGPT plan access, OpenAI API keys, Meta, xAI, Kilo, OpenRouter, Nous Portal, and custom/local OpenAI-compatible services. Model availability comes from the connected provider. Cost figures are estimates, and advertised capabilities are not execution certification.

---

## 4. Workflows

1. **General Assistance & Research**: Source-backed answers, file manipulation, multi-bot channels, reviewable deliverables.
2. **Coding**: Isolated Linux workspace VMs, Git diff generation, patch review, sandboxed test execution.
3. **Desktop Automation**: Scoped application control, interactive approval cards, observed UI outcomes, "Take the Wheel" takeover.
