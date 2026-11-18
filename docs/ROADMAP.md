# Possible improvements

A list of ideas for future work, grouped by area. **★ = recommended next steps.**

## Done

- Live updates via the engine event stream (`/events`), polling only as a fallback.
- Windows notifications: crash, out of memory, unhealthy, restart loop.
- Container detail: inspect, CPU / memory history, logs, actions, *Recreate with changes*.
- Securing the API: mutual TLS proxy (opt-in) – see README *API security* and
  [SECURITY.md](SECURITY.md).
- Start with Windows; `worker.rs` split into modules; build from a Dockerfile.

## Recommended next steps

1. ★ **Fixes from the security review** – see [SECURITY.md](SECURITY.md).
2. ★ **Volume backup & restore** – export a volume to a `.tar` on Windows and import it back,
   show the size of each volume.
3. ★ **TLS by default** for new installations, certificate rotation (*Regenerate certificates*),
   and a named-pipe relay as an alternative for tools that cannot use client certificates.
4. ★ **Recreate**: carry over entrypoint, labels, resource limits and all networks.
5. ★ **Cancel a running build**, build output with BuildKit progress.

## Containers, images, compose

- Logs: search with highlighting, timestamps toggle, save to file, merged live logs of a whole
  compose stack.
- Embedded console (terminal inside the app) as an alternative to Windows Terminal.
- File browser inside a container (`docker cp` up/download).
- Images: tag & push, registry login, layer history,
  "newer version available" check for running images.
- Compose: built-in YAML editor with validation, new stack from a template (Postgres, Redis,
  WordPress, …), scaling services, editing the `.env` file.
- Networks: connect / disconnect a container.
- Templates / favourites for frequently run containers.

## WSL and distributions

- Remove a distribution (`wsl --unregister`) with a confirmation, export / import
  (`wsl --export` / `--import`) as a backup, move a distribution to another drive.
- Compact the virtual disk (`.vhdx`) to give space back to Windows.
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
- Faster simple commands (`ps`, `images`) by calling the API directly instead of starting
  `wsl.exe`.
- Translation of more path-bearing flags; support for Docker contexts.

## App

- Tray submenu with running containers (start / stop / logs).
- Keyboard shortcuts (Ctrl+F filter, F5 refresh, Ctrl+1…8 pages), light theme, high contrast.
- Czech plural forms (1 / 2–4 / 5+), more languages.
- Self-update / release pipeline: GitHub Actions build, signed installer (MSI or winget).

## Code quality

- Typed errors instead of `String`.
- Async HTTP / event handling instead of a thread per action.
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
