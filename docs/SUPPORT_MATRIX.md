# Grok Muse Platform Support Matrix

This matrix describes implementation targets for the v0.6.1 preview. CI runs on macOS, Ubuntu and Windows. Native certification remains outstanding. The distributed installer covers Apple Silicon only. See [release readiness](release-readiness.md).

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

Host CLI invocation and version probes are disabled. Isolated CLI-agent execution and real-account certification remain outstanding. The following historical model examples and pricing require provider verification before use.

| Provider | Method | Supported Models | Availability Checks | Auth Flow | Cost Metering |
|---|---|---|---|---|---|
| **Meta AI (Llama)** | Cloud API | `muse-spark`, `muse-wonder` | Ping endpoint / API key presence | API Key in OS Keychain | Bounded meter ($0.80-$3.20/M tokens) |
| **xAI Grok** | Cloud API | `grok-4`, `grok-vision-beta` | Ping endpoint / API key presence | API Key in OS Keychain | Bounded meter ($3.00-$15.00/M tokens) |
| **Anthropic Claude** | CLI Driver | `claude-3-5-sonnet`, `claude-3-opus` | Host probes disabled | `claude login` / `ANTHROPIC_API_KEY` | Labeled honestly: 0.0 USD (unmetered CLI) |
| **OpenAI Codex** | CLI Driver | `codex`, `gpt-4o` | Host probes disabled | `codex login` / `OPENAI_API_KEY` | Labeled honestly: 0.0 USD (unmetered CLI) |
| **xAI Grok CLI** | CLI Driver | `grok` | Host probes disabled | `grok auth` / `XAI_API_KEY` | Labeled honestly: 0.0 USD (unmetered CLI) |
| **Offline Candle VLM** | Local Engine | `Qwen2-VL-2B-Instruct`, `SmolVLM2` | Installed weights registry check | None (offline open weights) | Free (local compute) |

---

## 4. Workflows

1. **General Assistance & Research**: Source-backed answers, file manipulation, multi-bot channels, reviewable deliverables.
2. **Coding**: Isolated Linux workspace VMs, Git diff generation, patch review, sandboxed test execution.
3. **Desktop Automation**: Scoped application control, interactive approval cards, observed UI outcomes, "Take the Wheel" takeover.
