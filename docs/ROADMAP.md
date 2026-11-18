# Possible improvements

A list of ideas for future work, grouped by area. **★ = recommended next steps.**

## Recommended next steps

1. ★ **Live updates via the engine event stream** (`/events`) instead of polling every few
   seconds – instant UI updates, less CPU, and the basis for notifications.
2. ★ **Notifications** – Windows toast when a container crashes, becomes unhealthy or keeps
   restarting; optionally from the tray even when the window is hidden.
3. ★ **Container detail page** – inspect (env, mounts, ports, labels, command), CPU/memory
   history graph, logs and console in one place, *Recreate with changes* (Docker cannot modify
   a container, so this would recreate it with edited ports / volumes / env).
4. ★ **Volume backup & restore** – export a volume to a `.tar` on Windows and import it back,
   show the size of each volume.
5. ★ **Securing the API** – the TCP API on `127.0.0.1:2375` has no authentication, so any
   local process can control the engine. Options: TLS with generated certificates, or a
   named-pipe relay (`npipe://`) instead of TCP.

## Containers, images, compose

- Logs: search with highlighting, timestamps toggle, save to file, merged live logs of a whole
  compose stack.
- Embedded console (terminal inside the app) as an alternative to Windows Terminal.
- File browser inside a container (`docker cp` up/download).
- Images: build from a Dockerfile (folder picker), tag & push, registry login, layer history,
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

- Start with Windows (autostart entry) – together with *Start hidden in the tray* and engine
  autostart this makes a "background service".
- Tray submenu with running containers (start / stop / logs).
- Keyboard shortcuts (Ctrl+F filter, F5 refresh, Ctrl+1…8 pages), light theme, high contrast.
- Czech plural forms (1 / 2–4 / 5+), more languages.
- Self-update / release pipeline: GitHub Actions build, signed installer (MSI or winget).

## Code quality

- Split `worker.rs` (large) into modules per area; typed errors instead of `String`.
- Async HTTP / event handling instead of a thread per action.
- CI: `cargo clippy`, `cargo test`, release builds on tag.
- Automated UI tests (egui_kittest) for key dialogs.

## Not yet verified in real use

- Interactive `docker run -it` / `exec -it` through the shim and the app.
- Restoring containers after a real engine restart.
- Engine installation on Fedora / openSUSE / Arch WSL distributions (package helpers are
  tested in containers only).
- Add distribution with *Use for the app* and engine installation (end-to-end).
