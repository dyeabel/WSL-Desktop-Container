# Possible improvements

A list of ideas for future work, grouped by area. **★ = recommended next steps.**

## Done

- Host abstraction, step 1: all `wsl.exe` calls behind the `Host` trait, own HTTP/1.1 client instead of `ureq`
  in the Docker API client; measured (docs/bench/2026-11-21-host-abstraction-after.md) within the performance limits.
- Host abstraction, step 2 (Linux): Linux host verified (Linux build, tests and live tests in the Podman container).
- `wslcd` command.
- Performance and memory optimizations (8 items); measurements in docs/bench/2026-11-22-step2-and-optimizations.md.
- Linux host (step 2): the app runs on Linux against a Docker or rootless Podman socket (WSL
  features hidden), XDG paths, `notify-send`, XDG autostart, tar.gz package and a `linux` CI job
  with the release attachment; see README *Linux*.
- Live updates via the engine event stream (`/events`), polling only as a fallback.
- Windows notifications: crash, out of memory, unhealthy, restart loop; Events page with the
  history and details, routine exits (SIGTERM, `--rm`, Testcontainers) only recorded, merged
  toasts, per-type settings and muting.
- Shim with Podman: health in `docker ps`, Docker fields in `events --format`, real buildx
  builders over the Podman API.
- Container detail: inspect, CPU / memory history, logs, actions, *Recreate with changes*.
- Securing the API: mutual TLS proxy (opt-in) – see README *API security* and
  [SECURITY.md](SECURITY.md).
- Start with Windows; `worker.rs` split into modules; build from a Dockerfile (with cancel).
- Volume backup / restore / clone and sizes; compose templates, editor, validation, scaling.
- Remove (with confirmation) / export / import distributions, disk compacting.
- `docker` shim fast path via the API; tray submenu with containers.
- Recreate through the API keeping all settings; most findings from the security review fixed.
- Typed API errors, shared HTTP agent with keep-alive, worker thread pool.
- TLS by default for new installations, named-pipe relay for tools; security findings F-05,
  F-11, F-13 fixed; typed WSL errors.
- Recreate keeps all networks and anonymous volumes; shim: more path flags, Docker contexts,
  fast path for `inspect`, `logs` and filters.
- Light theme, sortable tables, portable ZIP and production build (`scripts/release.ps1`, GitHub
  Actions release).
- Logs: search with highlighting, timestamps, save, merged live logs of a compose stack;
  file browser inside a container.
- Docker end-to-end test in a fresh WSL distribution; code signing support (`scripts/sign.ps1`,
  Azure Artifact Signing in CI) and winget manifest generation (`scripts/winget.ps1`).
- Merged stack logs attach containers that start later; manual CI workflow `e2e` for the Docker
  end-to-end test on a self-hosted runner.
- Memory overview (VmmemWSL vs. Linux vs. containers, history, freeing markers), WSL memory
  freeing (manual and automatic).
- Network overview: VM / container / stack traffic, published ports with Windows state, distro
  listeners, network map; ports in / out per container (nsenter + ss).
- `localhost` (IPv6 `::1`) relay for published ports.
- Traffic per connection (ss -ti), connection history, port conflicts with other WSL
  distributions; memory / disk threshold alerts with banner and Windows notifications.
- Resource history across restarts (`history/` ring files: 1-minute samples for 48 h,
  15-minute min / avg / max for 35 days; gaps while the app was off), 1 h / 24 h / 7 d / 30 d
  ranges, weekly summary.
- Per-container memory / CPU / PIDs limits from the UI (live `update`, no restart), limits in the
  performance table with a near-limit badge.
- Kubernetes lab: k3s in the engine, environments (template FE + API + DB, YAML folder), flow
  map from conntrack, rules → NetworkPolicy with connection tests, portable kubeconfig.
- WSLCD Lite: Alpine distribution without systemd (POSIX `systemctl` replacement), Docker or
  Podman with TLS, API ~1–2 s after a cold start.
- Memory cap for containers: shared cgroup v2 cap (MemTotal − reserve) for Docker and Podman in the primary WSL distribution, OOMScoreAdjust=-900 for the engine, cap and usage in Settings and the Memory overview.

## Recommended next steps

1. ★ **Publish**: make the releases public, choose a license, get an Azure Artifact Signing
   account (or SignPath for OSS) and submit the winget manifest to `microsoft/winget-pkgs`.
2. ★ **Self-update**: check GitHub releases and offer the new portable ZIP.
3. ★ **Embedded terminal** inside the app instead of Windows Terminal (exec / console).
4. ★ **Limits in the container list** – optional column / tooltip with the limit, and limits
   in the *Run* / *Recreate* forms.
5. ★ **History export** – CSV export of the stored history and container CPU sampled also
   outside the Overview.

## Containers, images, compose

- Embedded console (terminal inside the app) as an alternative to Windows Terminal.
- Images: tag & push, registry login, layer history,
  "newer version available" check for running images.
- Networks: connect / disconnect a container.
- Templates / favourites for frequently run containers.

## WSL and distributions

- Move a distribution to another drive.
- More `.wslconfig` options: `autoMemoryReclaim`, `sparseVhd`, `dnsTunneling`, `firewall`.
- Overview of all distributions and their engines; quick switch from the tray.
- Add distribution: optional password-protected `sudo` instead of passwordless, custom install
  location (`--location`), import of a custom rootfs (e.g. Alpine).

## Engine

- Rootless Docker / rootless Podman as an option.
- Docker → Podman migration (move images and volumes).
- `daemon.json` / `containers.conf` editor with a form for common options (DNS, registry
  mirrors, log rotation, data-root).

## `docker` shim

- Share `docker login` credentials between Windows and WSL.

## App

- Keyboard shortcuts (Ctrl+F filter, F5 refresh, Ctrl+1…8 pages), high contrast.
- Czech plural forms (1 / 2–4 / 5+), more languages.

## Code quality

- Typed errors for WSL commands; a full async runtime is not planned – the app uses an event
  stream thread, a worker pool and a shared keep-alive HTTP agent instead.
- Automated UI tests (egui_kittest) for key dialogs.

## Not yet verified in real use

- Interactive `docker run -it` / `exec -it` through the shim and the app.
- Restoring containers after a real engine restart.
- Engine installation on Fedora / openSUSE / Arch WSL distributions (package helpers are
  tested in containers only).
- Add distribution with *Use for the app* and engine installation (end-to-end).
- TLS mode with the Docker engine (only the Podman path and the proxy were tested live).
- Notifications for OOM and restart loops on a real workload.
