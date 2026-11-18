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

## Recommended next steps

1. ★ **TLS by default** for new installations and a named-pipe relay (`npipe://`, user-only
   ACL) as an alternative for tools that cannot use client certificates (findings F-01 / F-02).
2. ★ **Remaining security findings** – compose labels trust (F-05), env via `--env-file`
   instead of argv (F-11), Docker install without `curl | sh` (F-13) – see [SECURITY.md](SECURITY.md).
3. ★ **Recreate**: keep all networks (with aliases and static IPs) and anonymous volumes.
4. ★ Typed errors also for WSL commands (`wsl.rs`), not only the API client.
5. ★ Shim fast path for more commands (`inspect`, `logs` without `-f`, image / volume filters).

## Containers, images, compose

- Logs: search with highlighting, timestamps toggle, save to file, merged live logs of a whole
  compose stack.
- Embedded console (terminal inside the app) as an alternative to Windows Terminal.
- File browser inside a container (`docker cp` up/download).
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
- Translation of more path-bearing flags; support for Docker contexts.

## App

- Keyboard shortcuts (Ctrl+F filter, F5 refresh, Ctrl+1…8 pages), light theme, high contrast.
- Czech plural forms (1 / 2–4 / 5+), more languages.
- Self-update / release pipeline: GitHub Actions build, signed installer (MSI or winget).

## Code quality

- Typed errors for WSL commands; a full async runtime is not planned – the app uses an event
  stream thread, a worker pool and a shared keep-alive HTTP agent instead.
- CI: `cargo clippy`, `cargo test`, release builds on tag.
- Automated UI tests (egui_kittest) for key dialogs.

## Not yet verified in real use

- Interactive `docker run -it` / `exec -it` through the shim and the app.
- Restoring containers after a real engine restart.
- Engine installation on Fedora / openSUSE / Arch WSL distributions (package helpers are
  tested in containers only).
- Add distribution with *Use for the app* and engine installation (end-to-end).
- TLS mode with the Docker engine (only the Podman path and the proxy were tested live).
- Notifications for OOM and restart loops on a real workload.
