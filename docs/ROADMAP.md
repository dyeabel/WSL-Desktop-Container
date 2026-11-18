# Possible improvements

A list of ideas for future work, grouped by area. **★ = recommended next steps.**

## Done

- Live updates via the engine event stream (`/events`), polling only as a fallback.
- Windows notifications: crash, out of memory, unhealthy, restart loop.
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

## Recommended next steps

1. ★ **Code signing** of the EXEs and the ZIP (Authenticode) and a winget manifest.
2. ★ **Self-update**: check GitHub releases and offer the new portable ZIP.
3. ★ **Embedded terminal** inside the app instead of Windows Terminal (exec / console).
4. ★ Logs: follow containers that start later in a merged stack view automatically.
5. ★ Docker engine end-to-end tests (TLS mode, notifications) – only Podman is tested live.

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
