# WLS Desktop

A native Windows desktop app (Rust + egui, a single EXE, no web UI) for managing a
**container engine – Docker or Podman – running inside a WSL distribution**, without
Docker Desktop and without the Docker CLI for Windows.

> The user interface is available in Czech and English (Settings → Language).

## Features

| Page | What it does |
|---|---|
| **Overview** (Přehled) | engine status, container / image / volume counts, CPU and RAM available to WSL, disk usage with a *Clean up* menu (prune), live CPU, memory, network and disk stats per running container (with the image it runs from) |
| **Containers** (Kontejnery) | grouped by compose project; start, stop, restart, pause, remove; colored state badges (running, stopped, error with exit code, health); live logs (filter, copy); console in Windows Terminal; *Run container* dialog |
| **Images** | search Docker Hub (dialog with tag picker, size and update date), pull by exact name, remove, prune, run a container from an image, which containers use an image |
| **Volumes** | create, remove, prune, which containers use a volume, browse its content (as root), *run a container with this volume* |
| **Networks** (Sítě) | create (bridge, ipvlan, macvlan), remove, prune, subnets, attached containers |
| **Compose** | stacks detected from container labels: up, stop, start, restart, pull, build, ps, logs, down; edit the compose file in Notepad; start a stack from any folder (Windows or Linux path) |
| **Files** (Soubory) | file browser for the distribution: navigation, bookmarks, new folder or file, rename and move, delete, chmod, built-in text editor (Ctrl+S), upload from Windows and download to Windows, *As root* mode |
| **WSL engine** | start, stop and restart the engine, put the distribution to sleep, terminal; distribution list (start, terminate, default, *use for the engine*); *Install & setup* section: Docker / Podman switch, install, API setup, uninstall, `docker` command in PATH, `DOCKER_HOST`; editor for `/etc/docker/daemon.json` (Docker only) |
| **Settings** (Nastavení) | language (Czech / English), distribution (+ *Add distribution* wizard), API port, refresh interval, keep-alive, engine autostart, restoring containers that were running last time, tray behaviour, `docker` command in PATH, global `.wslconfig` (memory, CPU, swap, `vmIdleTimeout`, `networkingMode`) |

Less frequent and destructive actions (prune, pause all, …) live in the **⋯** menu next to
the filter field, which is pinned to the right on every page.

### Run container dialog

- Image, name, ports, environment variables, volumes, network, restart policy, command.
- **Shared volumes:** pick an existing volume, set the path inside the container and
  *read-only*, and click *Add*. One named volume can be mounted into any number of containers.
- **Interactive (-it)** opens the container shell in Windows Terminal (removed on exit).
- **sleep infinity** keeps images without their own service (alpine, ubuntu, debian, …) running.
- A few seconds after start the app checks the container state and reports immediately
  exited containers including the exit code.

### Add distribution

*Settings → Add distribution…* (also on the WSL engine page) installs a new WSL distribution
in the background:

1. pick a distribution from `wsl --list --online`, optionally a custom instance name;
2. `wsl --install <distro> --name <name> --no-launch` (no interactive first start);
3. preparation as root: systemd enabled in `/etc/wsl.conf`, a default user with passwordless
   `sudo` (optional password), `[user] default=…`;
4. restart of the distribution and a systemd check;
5. optionally install Docker or Podman and switch the app to the new distribution
   (only one distribution can expose the API – all WSL distributions share the network).

Engine installation supports apt (Debian, Ubuntu, Kali), dnf (Fedora, Alma, Oracle, Rocky),
zypper (openSUSE, SLES) and pacman (Arch). Progress is shown in an output window.

### Restoring containers

With *Settings → Restart the containers that were running last time* enabled, the app keeps
track of running containers (per distribution and engine, in
`%APPDATA%\WLS_Desktop\last_running.json`) and starts them again once the engine comes up –
e.g. after a WSL restart or a reboot. Stopping the engine, putting the distribution to sleep or
shutting WSL down from the app does not overwrite the list. A restart policy such as
`unless-stopped` on the container is the engine-native alternative.

### Tray

A tray icon with a status dot (green = engine running) and a tooltip. Left click shows or
hides the window, right click opens a menu: status, open, start / stop / restart the engine,
sleep the distribution, pause and resume containers, terminal, files in Explorer,
*close / minimize to tray* toggles and Quit. By default both close and minimize only hide the
window (configurable in Settings). The app runs as a single instance – starting it again just
shows the running window. Windows 11 puts icons of new apps into the hidden part of the tray
(^ arrow); pin it via *Settings → Personalization → Taskbar → Other system tray icons*.

### Docker / Podman engine

The engine is selected per distribution on the *WSL engine* page.

- **Install & setup:** installs the engine via apt (Debian / Ubuntu). If systemd is not running,
  it enables it in `/etc/wsl.conf` and restarts the distribution. Then it sets up the TCP API and
  verifies it responds. Progress is shown in an output window.
- **Uninstall:** disables the services, removes the API setup and the packages (apt purge).
  Optionally deletes the data as well (`/var/lib/docker…` or `/var/lib/containers`).
- **Podman** runs as the rootful service `wls-podman-api`
  (`podman system service tcp://127.0.0.1:<port>`). The app talks to it through the same
  Docker-compatible API. CLI commands (running a container, compose, console, logs, the shim)
  are translated to `podman --url tcp://127.0.0.1:<port> …`, so they see the same containers.
  Short image names are expanded to `docker.io/library/…`. `docker-compose` is installed as the
  compose provider. Setting up one engine disables the other so they don't fight over the port.

### `docker` command for Windows (shim)

The project also builds a small `docker.exe` (`src/shim/docker.rs`) that forwards commands to
the engine inside WSL: `docker ps` on Windows runs as
`wsl.exe -d <distro> --cd <current folder> -e docker ps` (or `podman --url … ps` for Podman).
The distribution and engine come from the WLS Desktop settings. Windows paths in arguments are
translated (`-v D:\data:/data`, `--mount source=C:\x`, `-f C:\p\compose.yml`, `.\data`,
`\\wsl.localhost\Debian\…`) and the exit code of the command is returned. When named
`docker-compose.exe` it behaves like `docker compose`.

Install it from *WSL engine → Install & setup* (or *Settings*): it is copied to
`%LOCALAPPDATA%\WLS_Desktop\bin` and that folder is added to the user PATH (no admin rights).

To behave as close to the Docker CLI as possible:

- **Environment variables from Windows** are forwarded via `WSLENV` – only what the command
  needs: `-e VAR` / `--env VAR` without a value, variables used in compose files
  (`${VAR}`, `$VAR`), plus anything listed in `WLS_FORWARD_ENV=A,B`. `DOCKER_HOST`, `PATH`
  and similar are never forwarded.
- **Paths in BuildKit options** are translated too: `--secret src=…`, `--output dest=…`,
  `--build-context x=…`, `--cache-from` / `--cache-to`, `--ssh`, `save -o …`. Values of `-e`,
  `--label`, `--build-arg` and `--filter` are left untouched. Network paths (`\\server\share`)
  that WSL cannot see produce a warning.
- **`DOCKER_HOST` for tools** that talk to the API directly instead of calling the CLI
  (VS Code Dev Containers, Testcontainers, SDKs): the *WSL engine* page can set the user
  variable `DOCKER_HOST=tcp://127.0.0.1:<port>`.

Debugging: `WLS_SHIM_DEBUG=1` prints the executed command; `WLS_ENGINE=podman|docker`,
`WLS_PORT` and `WLS_DISTRO` override the settings.

Remaining differences from the Docker CLI for Windows: `docker login`, `config.json` and
contexts live inside WSL; every call has the overhead of starting `wsl.exe` (~0.1–0.3 s);
Podman has no buildx, swarm or contexts.

Consoles and log tails open in **Windows Terminal** (`wt.exe`), falling back to a classic
console window when it is not available.

## How it works

- **Engine API over TCP:** besides its socket, the engine listens on `tcp://127.0.0.1:2375`.
  For Docker this is a systemd override
  `/etc/systemd/system/docker.service.d/10-wls-desktop-tcp.conf`, for Podman the unit
  `/etc/systemd/system/wls-podman-api.service`. WSL forwards the port to `localhost` on Windows
  automatically. The API has no authentication and is reachable only locally.
- **WSL operations** (starting / stopping the distribution, `systemctl`, compose, file
  operations) run via `wsl.exe -d <distro> [-u root] -e …`.
- **Switching distributions:** pick another distribution in Settings (or *Use for the engine*
  on the WSL engine page) and run *Install & setup* (or just *Set up API*) there once.
  Requirements: systemd (`[boot] systemd=true` in `/etc/wsl.conf`, set up automatically by
  *Install & setup*) and apt for automatic installation.
- **Keep-alive:** WSL stops an idle distribution – together with the engine. The app therefore
  runs a hidden `wsl.exe … flock … sleep infinity` that runs at most once per distribution and
  keeps running after the app is closed, so the engine stays up. **Sleep** stops it and shuts
  the distribution down.

The app configuration is stored in `%APPDATA%\WLS_Desktop\config.json`.

## Building

Requirements: Rust (MSVC toolchain) and Visual Studio Build Tools with C++ (including the
Windows SDK – `rc.exe` is used to embed the icon).

```bash
cargo build --release
```

Output: `target\release\wls-desktop.exe` (~8 MB, no runtime dependencies) and
`target\release\docker.exe` (the shim).

## Tests

```bash
cargo test
```

Integration tests against a real WSL distribution and engine (distribution from `WLS_DISTRO`,
default `Ubuntu`; the Podman test expects an API on `WLS_PODMAN_PORT`, default 2376):

```bash
cargo test -- --ignored --test-threads=1
```

For debugging and screenshots: `WLS_START_PAGE` selects the start page (`containers`, `images`,
`volumes`, `networks`, `compose`, `files`, `engine`, `settings`), `WLS_DEBUG_DIALOG=1` shows a
sample confirmation dialog, `WLS_DEBUG_HUB=<image>` opens the Docker Hub dialog,
`WLS_DEBUG_EXPAND=1` expands the engine setup section.

## UI conventions

- Icons come from the [Phosphor](https://phosphoricons.com/) font (`egui-phosphor`) and are
  written directly into text, e.g. `"\u{E4A6} Smazat"`.
- Tables: `ui::table()` / `ui::table_in()` with columns `W::Px` (fixed width), `W::Fr` (share
  of the remaining width) and `W::PxOpt` / `W::FrOpt` (less important columns hidden in narrow
  windows). An icon column uses `icons_w(n)`.
- Texts are bilingual via `t!("česky", "English")` and `tf!("… {x}", "… {x}")` (`src/i18n.rs`);
  the language is switched at runtime.
- All controls share the height `theme::CONTROL_H`.
- Toolbars: `ui::toolbar()`, or `ui::toolbar_lr()` with the right part pinned (filter
  `ui::filter_field()`, menu `ui::more_menu()`); inputs `ui::field()` / `ui::area()`; dialogs
  `ui::modal()` + `ui::dialog_buttons()`.

## Project structure

```
src/
  main.rs          entry point, window, single instance
  app.rs           app state, navigation, dialogs, toasts
  worker.rs        background thread: refresh and actions
  docker.rs        Docker Engine API client (ureq), Docker Hub search and tags
  engine.rs        Docker / Podman: setup, install and uninstall scripts, CLI
  wsl.rs           wsl.exe: distributions, commands, terminal, keep-alive
  fs.rs            file operations inside the distribution
  wslconfig.rs     editing %USERPROFILE%\.wslconfig
  pathmap.rs       Windows -> WSL path translation (shim)
  shim_install.rs  installing the shim into PATH, DOCKER_HOST
  shim/            docker.exe shim (docker.rs, envfwd.rs)
  tray.rs          tray icon, menu, hiding the window
  i18n.rs          Czech / English texts (t!, tf!)
  theme.rs         colors, style
  icon_art.rs      icon drawing (shared with build.rs for the .ico in the EXE)
  ui/              individual pages
build.rs           generates the .ico and embeds it into the EXE (winresource)
```

The UI layout is inspired by the *WSL Container Desktop* project (GPLv3). Only the look was
taken over; the code is written from scratch.
