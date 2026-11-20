# WSL Container Desktop – Security Review

Independent review of the source at `main` (commit `1a3b55e`; `df36249` landed during the
review and only touches `src/ui/images.rs` layout). The source was read directly. Live checks
used only read-only commands against the `Debian` distro on the review machine (Podman engine,
plain-HTTP mode). No source, engine, service, registry or configuration was changed.

## 1. Summary

WSL Container Desktop is careful in the small things. Commands are built as argv vectors, shell scripts
take values through `env VAR=…` and `"$1"`, file operations use `--`, toasts escape XML and the
TLS material is sound (P-256, correct EKU and SAN, `umask 077`, a dedicated rustls root store).
The weak point is the trust boundary around the engine API. By default it is an
unauthenticated, rootful Docker/Podman API on `127.0.0.1:2375`. Any local Windows account, any
process in any WSL distro (including `nobody`) and, through DNS rebinding, a web page can reach
it. Root in WSL means code execution as the Windows user through interop. The second theme is
Windows Terminal: untrusted strings, such as directory names and compose labels that an image
can set, reach `wt.exe`. `wt.exe` treats `;` as a command separator, and the `cmd.exe /c start`
fallback treats `&` as a command separator. The opt-in TLS mode is well built, but the client
key lands in a folder that every Linux uid can read through `/mnt/c`.

## 2. Threat model

**Assets**
- Root in the WSL distro: the engine is rootful, and the `docker`/`wslcd-api` groups are root-equivalent.
- The Windows user account. WSL interop is enabled (`/proc/sys/fs/binfmt_misc/WSLInterop` = enabled),
  so any Linux uid can start Windows programs as the Windows user. Verified: `wsl -u nobody -e whoami.exe`
  returns the Windows account.
- Container data, volumes and bind-mounted Windows folders under `/mnt/c`.
- Secrets: container environment variables, build args, registry credentials sent by
  DOCKER_HOST clients (`X-Registry-Auth`) and the TLS client key and CA.

**Attackers and what they can reach**

| Attacker | Reach | Notes |
|---|---|---|
| Other local Windows users | `127.0.0.1:2375` on the Windows loopback (wslrelay). They can also bind the port themselves while the engine is down. | Loopback TCP is not per-user. |
| Other processes of the same Windows user | Everything. `wsl.exe -u root` needs no password. | Out of scope for the app. TLS cannot help here. |
| Other WSL distros, non-root uids, host-network containers | `127.0.0.1:2375`, because all WSL2 distros share one network namespace, and `/mnt/c/...` | Verified: `nobody` gets `OK` from `/_ping` and can read `%APPDATA%\WLS_Desktop`. |
| Bridge-network containers | Not the loopback API. | They do reach bind mounts, which they can use to plant file names. |
| Malicious web pages | Simple POSTs (CSRF). DNS rebinding gives full access, because the API accepts any `Host` header (verified). | Mitigated only by browser Private/Local Network Access features. |
| Malicious images, compose files or Dockerfiles | Container labels (`com.docker.compose.project.*`), file names written to bind mounts, `${VAR}` references in compose files | Labels steer UI actions (F-03, F-05). |
| Malicious file names in the distro | Files page: terminal, Explorer, root-mode operations | F-03, F-10 |
| Malicious Docker Hub response | Search and tag JSON | Parsed into typed fields, displayed and URL-encoded. Low risk. |

## 3. Findings

| ID | Severity | Title | Location | Status |
|---|---|---|---|---|
| F-01 | High (Critical on multi-user hosts) | Default engine API is unauthenticated rootful TCP on loopback | `src/engine.rs:161`, `src/engine.rs:193`, `src/shim_install.rs:46-53` | Confirmed |
| F-02 | High | Browser CSRF and DNS rebinding against the plain API | `src/engine.rs:161`, `src/engine.rs:193` | Confirmed (precondition) / Suspected (browser exploit) |
| F-03 | High | Command injection into `wt.exe` through `;` in directory names and compose labels | `src/wsl.rs:283-286`, `src/worker/containers.rs:105-108`, `src/ui/compose.rs:21,124`, `src/ui/files.rs:173` | Suspected |
| F-04 | Medium | `cmd.exe /c start` fallback passes unquoted arguments, so `&` and `\|` inject | `src/wsl.rs:289-291` | Confirmed |
| F-05 | Medium | Compose stack metadata (working dir, config files) is trusted from container labels | `src/ui/compose.rs:14-30,80-125`, `src/ui/files.rs:115-120`, `src/worker/mod.rs:461-470` | Confirmed (data flow) / Suspected (impact) |
| F-06 | Medium | TLS client key stored where every Linux uid in every distro can read it | `src/worker/engine_ops.rs:43-55`, `src/docker.rs:410-412` | Confirmed |
| F-07 | Medium | Recreate silently drops security settings (User, caps, seccomp/AppArmor, read-only, limits) | `src/inspect.rs:323-335`, `src/worker/detail.rs:133-209` | Confirmed |
| F-08 | Low | TLS lifecycle: no renewal or expiry check, CA key kept, socat `fork` without a child limit | `src/engine.rs:238-262` | Confirmed |
| F-09 | Low | Silent fallback to plain HTTP when the TLS config fails to load | `src/app.rs:138-143`, `src/docker.rs:475-481` | Confirmed |
| F-10 | Low | Root-mode file operations follow symlinks (write, chmod, upload) | `src/fs.rs:118-127,154-156`, `src/worker/fs_ops.rs:30-37` | Confirmed |
| F-11 | Low | Secrets on command lines and in output windows | `src/worker/containers.rs:22`, `src/worker/spec.rs:43-45`, `src/worker/build.rs:42`, `src/inspect.rs:103-106` | Confirmed |
| F-12 | Low | Add distribution: `chpasswd` line injection, passwordless sudo always on | `src/worker/distro.rs:42-43`, `src/engine.rs:351-353` | Confirmed |
| F-13 | Low | `curl https://get.docker.com \| sh` as root, no pinning; `cargo audit` not run | `src/engine.rs:321` | Confirmed |
| F-14 | Low | Single instance by `FindWindowW` title is spoofable and racy | `src/tray.rs:83-87,128-136` | Confirmed |
| F-15 | Low | Unbounded allocation and panics on malformed API data | `src/docker.rs:691-692`, `src/docker.rs:324-327`, `src/docker.rs:713-714` | Confirmed |
| F-16 | Info | Shim forwards any Windows env var named in a compose file | `src/shim/envfwd.rs:101-126` | Confirmed |
| F-17 | Info | Shim maps `\\wsl.localhost\<other distro>\…` onto the configured distro | `src/pathmap.rs:29-33` | Confirmed |
| F-18 | Info | `ps_quote` ignores Unicode single quotes; PowerShell is used for registry writes | `src/shim_install.rs:95-97` | Confirmed |
| F-19 | Info | Keep-alive lock in world-writable `/tmp` | `src/wsl.rs:270` | Confirmed |

## 4. Findings in detail

### F-01: Default engine API is unauthenticated rootful TCP on loopback (High)

**Description.** With `tls = false` (the default, `src/config.rs:51`), Docker is started with
`-H tcp://127.0.0.1:$PORT` (`src/engine.rs:161`). Podman runs
`podman system service --time=0 tcp://127.0.0.1:$PORT` as root (`src/engine.rs:193`, unit
written at `:197-210`). The README documents the API as "reachable only locally", but on this
machine it is reachable by:
- any Windows process on the loopback, verified with `Invoke-WebRequest http://127.0.0.1:2375/_ping` returning `OK`;
- any Linux uid in the distro, verified with `wsl -u nobody … curl http://127.0.0.1:2375/_ping` returning `OK`;
- any other WSL2 distro, which shares the network namespace;
- any container started with `--network host`.

The API is root-equivalent: `POST /containers/create` with `"Binds":["/:/host"]` and
`"Privileged":true`. Root in the distro reaches the Windows account through interop and `/mnt/c`.
`set_docker_host` also points every DOCKER_HOST-aware tool at this endpoint (`src/shim_install.rs:46-53`).
The review machine currently has `DOCKER_HOST=tcp://127.0.0.1:2375` as a user variable.

**Exploit scenarios.**
1. A second interactive or RDP user on the same PC runs `curl -X POST 127.0.0.1:2375/containers/create …`
   and starts a privileged container. That gives root in Debian, and then `/mnt/c/Windows/System32/cmd.exe`
   runs as the victim.
2. A compromised service or CI job in another distro, or a `--network host` container, does the same.
3. Port squatting. While the distro is stopped, any local process or user binds `127.0.0.1:2375` on Windows.
   The app then reads fake snapshots from it, which feed F-03 and F-05 with attacker labels.
   DOCKER_HOST clients (Testcontainers, VS Code, SDKs) send it `X-Registry-Auth` registry credentials on pull.

**Fix.**
- Make the TLS mode the default for new setups. Offer to migrate existing ones at startup: run `set_tls(true)` behind a one-time prompt.
- Show a persistent warning badge while plain mode is active.
- Longer term, drop TCP entirely. Serve a Windows named pipe (`\\.\pipe\wls_docker_engine`) whose DACL grants
  only the current user SID, and relay each connection with `wsl.exe -u root -e socat STDIO UNIX-CONNECT:/run/podman/podman.sock`.
  This mirrors Docker Desktop's `npipe://` and removes the network and DNS-rebinding exposure.
  Set `DOCKER_HOST=npipe:////./pipe/wls_docker_engine` for tools.

### F-02: Browser CSRF and DNS rebinding against the plain API (High)

**Description.** Neither engine checks `Host`. A request with `Host: attacker.example` returned
the full `/version` JSON. `OPTIONS` returns `405` with no CORS headers, so non-simple requests are
blocked by browsers. CORS-"simple" POSTs without a body are not blocked:
- `/images/create?fromImage=…`
- `/containers/{name}/start|stop|kill|restart`
- `/build?remote=https://…` (Docker)

DNS rebinding (attacker domain re-resolving to 127.0.0.1) bypasses CORS entirely and gives
read/write access, which leads to F-01 impact.

**Exploit scenario.** The user visits a page that pulls a malicious image and kills containers
through simple POSTs. Alternatively, the page rebinds to 127.0.0.1 and creates and starts a
privileged container.

The engine's `Content-Type` handling for `/containers/create` could not be probed: a
non-destructive probe was blocked by the reviewer's permission policy. If Podman parses a
`text/plain` JSON body, plain CSRF alone is enough for root. Browser Local/Private Network
Access protections (recent Chromium) reduce but do not remove this, and other browsers differ.

**Fix.** Same as F-01: TLS with client certificates, or a named pipe. Browsers cannot complete
the mTLS handshake without the key. As an interim measure in plain mode, put a filtering proxy in
front of the engine that rejects any request whose `Host` is not `127.0.0.1:<port>` or
`localhost:<port>`, or that carries an `Origin` header. socat cannot do this; a few lines of Rust
or nginx can.

### F-03: Command injection into `wt.exe` through `;` (High, Suspected)

**Description.** `open_terminal` runs `wt.exe -w 0 nt --title <title> wsl.exe <args…>`
(`src/wsl.rs:285`). Windows Terminal splits its command line into sub-commands on `;`, and only
`\;` is literal. For `Term::Dir(dir)` both the title and the `--cd` value are the raw directory
string (`src/worker/containers.rs:106`). The directory comes from two places:
- the Files page's current directory (`src/ui/files.rs:173`), which can be any name in the distro;
- the compose label `com.docker.compose.project.working_dir` (`src/ui/compose.rs:21,124`).
  An image can set this label with `LABEL`, and it is copied to every container started from the image.

**Exploit scenario.** A container writes the directory `/mnt/c/Users/u/proj/x;calc.exe` through
a bind mount, or an image sets `working_dir="/tmp;powershell -enc …"`. The user clicks
*Terminal in this folder* or *Terminal in stack folder*. `wt` runs `calc.exe` or `powershell` as
a new tab: Windows code execution from container-controlled data. The interactive Run dialog has
the same issue with `;` in the command or env field (`src/worker/containers.rs:17-19`). That is
the user's own input, but a common `sh -c 'a; b'` breaks.

This is Suspected because wt's splitting of `;` inside a single argv element was not executed on
this machine. It is documented wt behaviour.

**Fix.**
- Escape every argument passed after `nt`: `a.replace(';', "\\;")`.
- Strip control characters and `;` from `--title`.
- Better, pass untrusted values through the environment instead of argv:
  `wsl.exe -d D -e sh -c 'cd -- "$WSLCD_DIR" && exec "${SHELL:-sh}"'`
  with `WSLCD_DIR` set on the `Command` and `WSLENV=WSLCD_DIR/u`. The command line is then constant.

### F-04: `cmd.exe /c start` fallback injection (Medium)

**Description.** When `wt.exe` cannot be spawned, the app runs
`Command::new("cmd.exe").args(["/c","start", title, "wsl.exe"]).args(wsl_args)`
(`src/wsl.rs:289-291`). Rust quotes an argument only when it contains a space, a tab or `"`. It
applies no cmd.exe escaping, because the BatBadBut fix covers only `.bat`/`.cmd` targets. So `&`,
`|`, `^` and `%VAR%` in any argument are interpreted by cmd. `start` also takes an unquoted
first token as the program, not the title.

**Exploit scenario.** On a machine without Windows Terminal, a directory or compose label
`/tmp&calc` runs `calc` when the user opens a terminal there.

**Fix.** Drop `cmd.exe`. Spawn `wsl.exe` directly with `creation_flags(CREATE_NEW_CONSOLE /* 0x10 */)`
and set the console title from inside the session if needed.

### F-05: Compose stack metadata trusted from container labels (Medium)

**Description.** Stacks, their directory and their files come only from container labels
(`src/ui/compose.rs:14-30`). Any image or `docker run --label` can create a fake stack. The values
are then used as follows:
- `--cd` for `compose up/pull/build` (`src/worker/containers.rs:57`);
- Notepad on each `config_files` entry (`src/ui/compose.rs:117`);
- `explorer.exe <UNC path>` (`src/ui/compose.rs:121`, `src/worker/mod.rs:466-470`);
- the terminal (F-03);
- Files bookmarks (`src/ui/files.rs:115-120`).

`explorer.exe` given a file path opens it with its file association.

**Exploit scenario.** An image labels itself with
`com.docker.compose.project.working_dir=/home/u/Downloads/payload.hta`. *Open folder in Explorer*
then opens the HTA. A zone prompt may appear for `\\wsl.localhost`; this was not tested.
Alternatively, *Build & up* runs an attacker-chosen compose project directory with a privileged
service definition.

**Fix.**
- Before acting, run `test -d -- "$dir"` in the distro and refuse otherwise.
- Pass a directory to Explorer only after that check, and never a file.
- Show the resolved directory in the confirmation for `up`/`build`.
- Mark stacks whose labels are inherited from the image as untrusted. To detect this, compare the container labels with `/images/{id}/json` `Config.Labels`.

### F-06: TLS client key readable by every Linux uid in every distro (Medium)

**Description.** `fetch_client_certs` copies `ca.pem`, `cert.pem` and `key.pem` (unencrypted) to
`%APPDATA%\WLS_Desktop\tls` with `std::fs::write` and inherited ACLs
(`src/worker/engine_ops.rs:43-55`). On Windows that limits access to the user, SYSTEM and
Administrators. Inside WSL, however, `/mnt/c` is a 9p drvfs mount with `uid=1000` and no
`metadata` option. On this machine, `/mnt/c/Users/…/AppData/Roaming/WLS_Desktop` shows as
`drwxrwxrwx`, and `nobody` could read `config.json`. The key would be equally readable. The TLS
mode exists to stop other distros and non-root uids from reaching the API, and exactly those
callers can read the key and connect.

The same interop finding means any Linux uid can already start Windows programs as the user.
Real isolation also needs interop restricted.

**Exploit scenario.** A `nobody`-level process in another distro reads `key.pem`, `cert.pem` and
`ca.pem` from `/mnt/c/...` and calls `https://127.0.0.1:2376` with them. It gets root.

**Fix.**
- Do not keep the key as a plain file. Protect it with DPAPI (`CryptProtectData`, user scope),
  decrypt it into memory for `rustls`, and store only `key.pem.dpapi`.
- Write the PEM only when the user explicitly enables DOCKER_CERT_PATH for external tools, and warn about it.
- Document, and offer in setup, `[automount] options="metadata,umask=077"` (or `fmask=077,dmask=077`) in `/etc/wsl.conf`.
- Document, and offer, `[interop] enabled=false` for users who want the uid boundary to mean something.

### F-07: Recreate silently drops security settings (Medium)

**Description.** *Recreate with changes* rebuilds the container through `docker run` from a
`RunSpec` (`src/inspect.rs:323-335`), then force-removes the original
(`src/worker/detail.rs:188`). `RunSpec` carries only image, name, ports, env, volumes, network,
restart and command. `Config.User` is parsed (`src/inspect.rs:173`) but never used. The following
settings are all lost: `CapDrop`/`CapAdd`, `SecurityOpt` (`no-new-privileges`, seccomp and
AppArmor profiles), `ReadonlyRootfs`, `Privileged`, `Devices`, `Memory`/`PidsLimit`, `Tmpfs`,
`Healthcheck`, labels, `Entrypoint` (warned about only in the UI) and log options. Anonymous
volumes of the old container are orphaned, because removal has no `v=1` and the new container
gets fresh ones.

**Exploit scenario.** A service deliberately run as `--user 1000 --cap-drop ALL --read-only`
comes back as root with default capabilities and a writable rootfs after a routine port change.
Nothing tells the user.

**Fix.** Recreate through the API instead: take the inspected `Config` and `HostConfig`, apply
the edited fields, and `POST /containers/create`. Alternatively, refuse or warn when any of the
fields above differ from their defaults. Re-attach anonymous volumes by name.

### F-08: TLS lifecycle gaps (Low)

**Description.** Certificates are generated only when files are missing, or when `REGEN=1`
(`src/engine.rs:238`). `REGEN` is never set anywhere in `src/`. The server and client
certificates expire after 825 days (`:247,:250`). Nothing checks expiry, rotates or revokes them,
so the API simply stops working one day. `ca-key.pem` stays in `/etc/wsl-container-desktop/tls`
indefinitely. socat runs with `fork` and no `max-children` (`:262`), so any local process can
open unlimited TLS handshakes or children.

**Fix.**
- In `tls_proxy`, run `openssl x509 -checkend 2592000 -noout -in $T/server.pem || REGEN=1`. Do the same for `cert.pem`.
- Add a *Regenerate certificates* action that sets `REGEN=1`.
- Delete `ca-key.pem` after issuing, or move it to `/root` with mode 0400.
- Add `max-children=32`, which needs socat 1.7.4 or later.

### F-09: Silent fallback to plain HTTP when TLS config fails (Low)

**Description.** At startup, an error from `configure_tls(true)` is only printed with `eprintln!`
(`src/app.rs:138-143`). That output is invisible in a `windows_subsystem` build. The global
`TLS` slot stays `None`, so `Docker::with_tls` picks `http://` (`src/docker.rs:480`) even though
`cfg.tls` is true. socat rejects the connection, so this is not a downgrade against the real
engine. It does make the app talk plaintext to whatever owns the port: a squatter while the
engine is down.

**Fix.** Keep a "TLS required" flag. When `cfg.tls` is true and no client config is loaded,
return an error from every request instead of building an HTTP agent, and surface it in the UI.

### F-10: Root-mode file operations follow symlinks (Low)

**Description.** In *Work as root* mode:
- `write_text` runs `cat > "$1"` (`src/fs.rs:155`), which follows a symlink at the destination.
- `chmod -- mode path` (`src/fs.rs:125-126`) changes the symlink target.
- Upload is `cp -r -- src… dest` (`src/fs.rs:118-122`, `src/worker/fs_ops.rs:30-37`), which writes through an existing destination symlink.
- Reading and then saving in the editor is a TOCTOU window.

**Exploit scenario.** A container with a bind mount on `/srv/app` replaces `/srv/app/config.yml`
with a symlink to `/etc/sudoers.d/x` or `/etc/shadow`. An admin then edits it, chmods it or
uploads a file of that name as root.

**Fix.**
- Write to `mktemp "$dir/.wls.XXXXXX"` and then `mv -fT` over the target; rename replaces the link, not its target.
- Refuse `chmod` when `[ -L "$p" ]`.
- Use `cp -r --remove-destination --no-dereference`.
- Show a "symlink to …" warning before any root operation on a link.

### F-11: Secrets on command lines and in output windows (Low)

**Description.** Container env values become `-e KEY=VALUE` argv for `wsl.exe` and
`docker/podman run` (`src/worker/spec.rs:43-45`, `src/worker/containers.rs:22`). Recreate
re-sends every user env var this way. Inside WSL any uid can read `/proc/*/cmdline`. The
recreate form shows env values unmasked in a text area (`src/ui/detail.rs:593`). Build output
echoes `--build-arg` values (`src/worker/build.rs:42`). Masking in the detail view relies on
`is_secret` (`src/inspect.rs:103-106`), which misses names such as `AUTH`, `CREDENTIAL`,
`PRIVATE`, `DSN` and `DATABASE_URL`.

**Fix.**
- Pass env through `--env-file /dev/stdin`, or through a mode-0600 temp file deleted after `run`.
- Mask values in the recreate form and in the echoed build command.
- Widen the secret-name heuristic.

### F-12: Add distribution – `chpasswd` line injection, passwordless sudo (Low)

**Description.** The password is passed on stdin as `"{user}:{password}\n"`
(`src/worker/distro.rs:43`), which correctly keeps it off argv. It is not validated: a `\n` in it
starts a new `chpasswd` record, for example `x\nroot:y` sets root's password. Only the user's own
input reaches this, so impact is low, but it is a correctness and safety bug. `DISTRO_PREPARE`
always installs `NOPASSWD:ALL` (`src/engine.rs:352`), and engine install adds uid 1000 to
`docker` (`src/engine.rs:323-324`). Both are root-equivalent for any process of that user.
The username is validated in the UI (`src/ui/settings.rs:26-30`), which keeps the
`/etc/sudoers.d/wls-$U` write safe.

**Fix.**
- Reject `\n`, `\r` and `:` in passwords.
- Make passwordless sudo a checkbox, off when a password is given.
- Clear `spec.password` after use, for example by overwriting it with a `zeroize`-style fill.

### F-13: Supply chain (Low)

**Description.** On apt and dnf systems the engine is installed with `curl -fsSL https://get.docker.com | sh` as root
(`src/engine.rs:321`). It trusts that host's TLS and content at run time, with no pinning and no review step.
Other packages come from the distro's signed repositories (`socat`, `openssl`, `podman`, `docker-compose`, `sudo`, `curl`).
`qualify_image` only prefixes `docker.io/` (`src/engine.rs:362-376`). Pulls are tag-based, not digest-pinned.

Direct dependencies from `cargo tree --depth 1` are all current major lines:
- eframe 0.31.1
- egui-phosphor 0.9.0
- raw-window-handle 0.6.2
- rfd 0.15.4
- rustls 0.23.46 (ring 0.17.14)
- rustls-pki-types 1.16.0
- serde 1.0.229
- serde_json 1.0.154
- tauri-winrt-notification 0.8.1
- tray-icon 0.26.1
- ureq 2.12.1 (webpki-roots 0.26)
- windows-sys 0.59.0
- winresource 0.1.34 (build)

**`cargo audit` is not installed on this machine and was not run.** Advisory status is unverified.

**Fix.**
- Use Docker's documented apt/dnf repository setup, with a `signed-by` key whose fingerprint is
  pinned in the script, instead of `curl | sh`.
- At minimum, download the script to a file, show its SHA-256 in the output and run it from there.
- Add `cargo audit` and `cargo deny` to CI.

### F-14: Single instance through `FindWindowW` title (Low)

**Description.** `activate_existing` calls `FindWindowW(NULL, "WSL Container Desktop")` and, if any
top-level window has that exact title, shows it and exits (`src/tray.rs:83-87,128-136`,
`src/main.rs:28`). Any process, or simply an Explorer window on a folder named `WSL Container Desktop`,
blocks the app from starting and gets `SetForegroundWindow`. Two simultaneous launches both pass
the check.

**Fix.**
- Use a named mutex, `CreateMutexW(… "Local\\WLS.Desktop-<user SID>")`, for the single-instance decision.
- Find the existing window by a unique window class, and confirm its owning process image path
  with `GetWindowThreadProcessId` and `QueryFullProcessImageNameW` before showing it.

### F-15: Unbounded allocation and panics on malformed API data (Low)

**Description.**
- `stream_logs` allocates `vec![0u8; size]` with `size` taken from the 8-byte frame header (up to 4 GiB) (`src/docker.rs:691-692`). An allocation failure aborts the process.
- `short_id` slices bytes `&id[..12]` (`src/docker.rs:324-327`) and panics on a non-ASCII id.
- `stream_events` uses `lines()` with no line-length cap (`src/docker.rs:713-714`).
- Every UI action spawns a thread (`src/worker/mod.rs:241-244`).
- `pull` reads the whole progress stream through `into_string`, which ureq caps at 10 MB, so a very large pull reports an error.

These are reachable from a squatting fake API (F-01, scenario 3) or a misbehaving engine.

**Fix.**
- Cap the frame size (for example 1 MiB) and read larger frames in chunks.
- Use `id.get(..12).unwrap_or(id)` or `chars().take(12)`.
- Wrap the events reader in `take()` per line.
- Stream pull progress instead of buffering it.

### F-16: Shim forwards Windows env vars named in compose files (Info)

**Description.** For `docker compose`, the shim scans compose files in the cwd (or given with
`-f`) for `$VAR` and `${VAR}`, comments included, and forwards every match that exists in
Windows through `WSLENV` (`src/shim/envfwd.rs:101-126`). The block list (`:14-18`) covers only
infrastructure variables. This matches what native Docker on Windows does, but a cloned
repository whose compose file references `${GITHUB_TOKEN}` or `${AWS_SECRET_ACCESS_KEY}` will pull
those into containers.

**Fix.** Document the behaviour. Optionally, print the forwarded names to stderr by default (not
only with `WSLCD_SHIM_DEBUG`) and ignore `#` comments.

### F-17: UNC path for another distro mapped onto the configured distro (Info)

**Description.** `to_wsl_path` strips `\\wsl.localhost\<any>\` and returns `/<rest>`
(`src/pathmap.rs:29-33`). `-v \\wsl.localhost\Ubuntu\home\me\x:/data` therefore mounts Debian's
`/home/me/x` without warning, which can expose different data than intended.

**Fix.** Translate only when `<any>` equals the target distro (case-insensitive). Otherwise warn,
as is already done for network paths.

### F-18: `ps_quote` and PowerShell for environment writes (Info)

**Description.** `ps_quote` doubles only ASCII `'` (`src/shim_install.rs:95-97`). PowerShell also
treats U+2018, U+2019, U+201A and U+201B as single quotes. The quoted values are
`%LOCALAPPDATA%`/`%APPDATA%`-derived paths, so only an unusual profile name could break the
script. `[Environment]::SetEnvironmentVariable('Path',…,'User')` also rewrites a
`REG_EXPAND_SZ` Path as `REG_SZ`.

**Fix.**
- Write `HKCU\Environment` with `RegSetKeyValueW`, as `autostart.rs` already does, keeping `REG_EXPAND_SZ`.
- Broadcast `WM_SETTINGCHANGE`.
- Alternatively, pass values to PowerShell through `$env:` variables instead of interpolating them into the script.

### F-19: Keep-alive lock in `/tmp` (Info)

**Description.** `flock -n /tmp/.wsl-container-desktop-keepalive.lock` (`src/wsl.rs:270`) runs as the
default user. Any uid can pre-create the file and hold the lock, so keep-alive exits at once and
WSL idles the engine out. This is denial of service only.

**Fix.** Use `/run/user/$UID/wls-desktop-keepalive.lock` or `$HOME/.cache/…`.

## 5. What is done well

- **Argv everywhere.** `wsl.exe -d … [-u root] -e prog args…` (`src/wsl.rs:95-106`).
  `sh -c SCRIPT sh "$1"` keeps paths out of the script text (`src/fs.rs:68,155`). Setup scripts
  take values through `env PORT=… TLS=… U=…` instead of string formatting
  (`src/worker/engine_ops.rs:113-126`, `src/worker/distro.rs:35`).
- **`--` before user paths** in `mkdir`, `touch`, `mv -n`, `rm -rf`, `cp -r`, `chmod`, `head` and `cd`.
  `rm` of `/` or an empty path is refused (`src/fs.rs:95-127`).
- **Password on stdin**, not argv (`src/worker/distro.rs:43`). The username is validated against a
  strict charset before it reaches `useradd` and `sudoers.d` (`src/ui/settings.rs:26-30`).
- **TLS design.**
  - EC P-256 keys.
  - The CA has `basicConstraints=critical,CA:TRUE` and `keyUsage=keyCertSign,cRLSign`.
  - Leaf certificates have `CA:FALSE`, `digitalSignature` and the correct EKU: `serverAuth` with SAN `IP:127.0.0.1,DNS:localhost`, and `clientAuth`.
  - Keys are written under `umask 077` in a `chmod 700` directory (`src/engine.rs:236-251`).
  - socat binds `127.0.0.1` with `verify=1` and `cafile`, and is `BindsTo` the engine unit.
  - In TLS mode nothing listens on TCP unauthenticated: Docker runs with only `-H fd://`, and Podman listens on a unix socket in a `750 root:wslcd-api` directory.
  - The rustls client trusts only the generated CA, not webpki roots, uses `with_safe_default_protocol_versions` and presents the client certificate (`src/docker.rs:415-434`).
- **The `daemon.json` editor** rejects `hosts`, which keeps the user from reopening TCP by accident (`src/worker/engine_ops.rs:339-348`).
- **Windows integration.**
  - The Run value is a correctly quoted `"<exe>" --autostart` written through `RegSetKeyValueW` under HKCU only (`src/autostart.rs:12-15`).
  - The AUMID is registered through `reg.exe` with an argv vector (`src/notify.rs:27-30`).
  - Toast text is set with `SetInnerText`, so there is no XML injection.
  - The shim never forwards `DOCKER_HOST`, `PATH` and similar variables (`src/shim/envfwd.rs:14-18`) and only forwards names that match `[A-Za-z_][A-Za-z0-9_]*`.
- **Docker Hub data** is deserialised into typed fields with defaults, and image names and search terms are percent-encoded (`src/docker.rs:329-336,616-618,636`).
- **Bounded buffers** for the log view (4 MB, `src/app.rs:357-362`), build output (2 MB, `src/ui/build.rs:118-122`), the editor (2 MB through `head -c`, `src/fs.rs:132-151`) and binary or non-UTF-8 detection in the editor.

## 6. Recommended order of fixes

1. **F-03 and F-04.** Escape `;` for `wt.exe`, pass the directory through the environment, and
   remove the `cmd.exe /c start` fallback. It is a small patch, and it closes a one-click path from
   container-controlled names to Windows code execution.
2. **F-01 and F-02.** Make TLS the default, and plan the named-pipe transport. This removes the
   unauthenticated root API from loopback, other users, other distros and browsers.
3. **F-06.** Protect the client key with DPAPI and recommend `metadata,umask=077` for drvfs.
   Without this, the TLS mode does not protect against the in-WSL attackers it exists for.
4. **F-05.** Validate label-derived directories and never hand Explorer a file.
5. **F-07.** Recreate through the API with full `HostConfig`, or warn about dropped settings.
6. **F-09 and F-08.** Fail closed when TLS cannot load, add expiry checks, rotation and `max-children`.
7. **F-10, F-11 and F-12.** Make root-mode operations symlink-safe, keep secrets off argv, and validate the password.
8. **F-13 to F-19.** Install the engine from a signed repository, add `cargo audit` to CI, fix the
   single-instance check, add robustness caps and the remaining Info items.

## Verification notes

- Live checks used only read-only probes:
  - `ss -ltnp` showed `127.0.0.1:2375` listening.
  - `systemctl cat wslcd-podman-api` showed a plain TCP unit.
  - `wslcd-tls-proxy` is inactive and `/etc/wsl-container-desktop` is absent, so TLS mode is not enabled here.
  - `/_ping` succeeded from Windows and as `nobody`, and `/version` succeeded with a foreign `Host` header.
  - `OPTIONS` returned 405.
  - `nobody` could read `%APPDATA%\WLS_Desktop` through `/mnt/c`.
  - `nobody` could run `whoami.exe` through interop.
- **Not verified:**
  - `cargo audit`, which is not installed.
  - wt.exe `;` splitting (F-03), which was not executed, to avoid launching programs.
  - Whether Podman accepts `text/plain` JSON on `/containers/create` (F-02); the probe was blocked by the reviewer's permission policy.
  - socat's actual client-certificate enforcement and the key file ACLs, because TLS mode is not set up on this machine.
  - Reachability from a second Windows account, which is inferred from loopback semantics.
  - Browser LNA/PNA behaviour.

## Fixed after the review

| ID | Fix |
|---|---|
| F-03 | `wsl::open_terminal` escapes `;` (`\;`) in the title and every argument passed to `wt.exe`. |
| F-04 | The fallback no longer uses `cmd.exe /c start`; `wsl.exe` is started directly with `CREATE_NEW_CONSOLE`. |
| F-09 | If the TLS certificates cannot be loaded, the client keeps HTTPS with an empty CA store (every connection fails) instead of falling back to plain HTTP. |
| F-12 | A password containing a line break is rejected (not passed to `chpasswd`). |
| F-15 (partly) | Log frames larger than 16 MiB abort the stream; `short_id` slices by characters. Event lines are still unbounded. |
| F-05 (partly) | *Open in Explorer* runs only for an existing directory (`test -d`), never a file. Compose working dirs from labels are still trusted. |
| F-06 | The client key is stored DPAPI-encrypted (`key.pem.dpapi`, user scope) and decrypted in memory; a plain `key.pem` is written only when the user sets DOCKER_HOST with TLS for external tools (with a warning) and removed when DOCKER_HOST is cleared. |
| F-07 | Recreate creates the container through the API from the original `Config` + `HostConfig`, changing only the edited fields – user, capabilities, security options, read-only rootfs, privileged, devices, limits, tmpfs, healthcheck, labels, entrypoint and log options are kept. |
| F-08 | Certificates expiring within 30 days are regenerated on API setup; *Regenerate certificates* action; `ca-key.pem` is mode 0400; socat runs with `max-children=32`. |
| F-10 | Root-mode editor saves via a temp file + `mv -fT` (replaces a symlink instead of writing through it) and refuses symlinks; root `chmod` refuses symlinks; uploads use `cp -rP --remove-destination`. |
| F-11 (partly) | Wider secret-name heuristic (AUTH, CREDENTIAL, PRIVATE, DSN, DATABASE_URL, …); secret build-arg values are masked in the echoed build command. Env values are still passed on the command line. |
| F-14 | Single instance via a named mutex (`Local\WLS-Desktop-single-instance`) instead of only the window title. |

| F-01 / F-02 (mitigated) | New installations use TLS by default; existing plain setups get a one-time hint with *Turn TLS on*. A named-pipe relay (user + SYSTEM DACL, remote clients rejected) lets tools reach the API without the plain key. Plain mode remains possible for existing setups. |
| F-05 | Before `up` / `build` / `pull` / `start` of a label-detected stack the worker refuses stacks whose compose labels come from the image (`Config.Labels`) and requires an existing folder with a compose file. |
| F-11 | Container env values are passed through a temporary 0600 `--env-file` (deleted right after `run`), not on the command line; recreate goes through the API. |
| F-13 | Docker is installed from distribution packages (`docker.io` / `moby-engine` / openSUSE / Arch) or, on RHEL-like systems, from Docker's GPG-verified dnf repository – no `curl \| sh`. |
| TLS client | Session resumption is disabled – the socat/OpenSSL proxy rejected resumed sessions, so only the first new connection per client config worked. |

Still open: plain mode for existing setups (F-01 / F-02 when TLS is not turned on), F-15 (event lines), F-16 – F-19.

---

# Re-audit (2026-11-20, commit `cc91d5b`)

Read-only review of the current source, including the code added since the first review: the named-pipe relay (`npipe.rs`),
the IPv6 relay (`portrelay.rs`), the shim fast path and contexts (`shim/*.rs`), files inside containers (`container_fs.rs`),
DPAPI, compose templates and the CI workflows. Live checks were read-only (`/_ping`, `icacls`, `ss`). Dependencies were
scanned with `cargo audit` 0.23 (RustSec database of 2026-11-20).

## Status of the earlier findings

| ID | Status | Evidence |
|---|---|---|
| F-01 / F-02 | Mitigated | New setups default to TLS, a *Turn TLS on* hint for plain setups. `Config::default` still has `tls: false` and plain mode still accepts any `Host` header. |
| F-03, F-04 | Fixed | `wsl.rs` escapes `;` for `wt.exe`; fallback uses `CREATE_NEW_CONSOLE`, no `cmd.exe`. |
| F-05 | Incomplete | up / build / pull are gated; *Open in Explorer* can still be bypassed (N-02), Terminal / Notepad use label folders. |
| F-06, F-07, F-09, F-10, F-12, F-13, F-14 | Fixed | Verified in code. |
| F-08 | Fixed | Expiry check, regeneration, `ca-key.pem` 0400. socat runs with `max-children=256`, not 32. |
| F-11 | Incomplete | The interactive *Run* (Windows Terminal) still puts env values on the command line (N-06). |
| F-15 | Partial | Frame cap and `short_id` fixed; event and header lines still unbounded. |
| F-16 – F-19 | Open | Unchanged (Info). |

## New findings

| ID | Severity | Title | Location | Status |
|---|---|---|---|---|
| N-01 | Medium (fixed) | Downloading a folder from a container can write outside the target folder (symlink chains + case-insensitive NTFS names) | `src/container_fs.rs` (tar extraction) | Confirmed (reproduced by a test) |
| N-02 | Medium (fixed) | *Open in Explorer* check bypassable with trailing dots / spaces or `\` in a Linux directory name – Explorer opens a file | `src/worker/mod.rs`, `src/wsl.rs` `unc()` | Confirmed (live test) |
| N-03 | Medium, High on shared PCs (fixed) | Portable folder `C:\WSLContainerDesktop` inherits `Authenticated Users:(M)` – another local user can replace the EXEs (autostart, `docker.exe` on PATH) | `src/config.rs` portable mode | Confirmed (`icacls`) |
| N-04 | Medium | `release.yml` runs on `pull_request` with `contents: write`; a self-hosted WSL runner exists for `e2e.yml` – a fork PR could target it if the repo becomes public | `.github/workflows/*.yml` | Suspected (depends on repo visibility) |
| N-05 | Low | `\.\pipe\docker_engine` can be pre-created by another user; the app then reports it as Docker Desktop | `src/npipe.rs` | Confirmed (design) |
| N-06 | Low | Interactive *Run* passes env values in argv | `src/worker/containers.rs`, `src/worker/spec.rs` | Confirmed |
| N-07 | Low | Shim fast path: `dechunk` size overflow can panic; `read_all` unbounded | `src/shim/fast.rs` | Confirmed |
| N-08 | Low | CI: secrets exposed as job env to every step, actions pinned by tag not SHA, `cargo audit` not in CI | `.github/workflows/release.yml` | Confirmed |
| N-09 | Info | `ps_quote` now quotes the whole user PATH (U+2019 breaks it); Path rewritten as `REG_SZ` | `src/shim_install.rs` | Confirmed |
| N-10 | Info | Doc said `max-children=32`, code uses 256 | `src/engine.rs` | Confirmed |
| N-11 | Info | With WSL interop on, TLS / named pipe are not a boundary against non-root WSL users (they can start Windows programs as the user) | design | Known since F-06 |

**N-01.** Extraction checks are lexical: `..` and absolute paths are rejected, but a symlink in an intermediate component is
followed by `create_dir_all` / `File::create`. On case-insensitive NTFS a container can ship `S -> ..` and then `s/T/…`, so later
entries land outside the destination (up to the Startup folder). Needs symlink creation, which Developer Mode allows.
*Fix:* skip symlinks on Windows, or canonicalise every parent and require it inside the destination; detect case collisions.

**N-02.** `test -d` runs on the Linux path, but Win32 normalisation strips trailing dots / spaces and treats `\` as a separator,
so `/srv/app/x.hta.` passes the check while `\wsl.localhost\D\srv\app\x.hta.` opens `x.hta`. *Fix:* check `Path::is_dir()`
on the final UNC path right before spawning Explorer, reject such names, and apply the image-label trust check to it.

**N-03.** *Fix:* on first portable start set a protected DACL (user + SYSTEM + Administrators) on the folder, or warn when it is
writable by others; recommend a location inside the user profile.

**N-04.** *Fix:* keep self-hosted runners off public repositories (or restrict them with a runner group to `e2e.yml`), require
approval for outside contributors, use `contents: read` for PR runs.

## Dependencies (`cargo audit`)

| Advisory | Crate | Kind | In the Windows build? |
|---|---|---|---|
| RUSTSEC-2026-0194, RUSTSEC-2026-0195 | `quick-xml` 0.30.0 | vulnerability (DoS) | **No** – only via `accesskit_unix` → `zbus_xml` (Linux) |
| RUSTSEC-2024-0429 | `glib` 0.18.5 | unsound | No – Linux GTK backend of `rfd` / `tray-icon` |
| RUSTSEC-2026-0370 / 0371 / 0372 | `zvariant`, `zbus_names`, `zvariant_derive` | unmaintained | No – Linux D-Bus |
| RUSTSEC-2024-0370 | `proc-macro-error` | unmaintained | No |
| RUSTSEC-2024-0436 | `paste` 1.0.15 | unmaintained | Yes, compile time only (proc-macro in `accesskit_windows`) |
| RUSTSEC-2026-0192 | `ttf-parser` 0.25.1 | unmaintained | Yes, parses only the fonts embedded in the EXE (no untrusted input) |

No known vulnerability is compiled into the Windows binaries (checked with `cargo tree -i … --target x86_64-pc-windows-msvc`).
The advisories come from `Cargo.lock` covering all platforms; they disappear with the next `eframe` / `egui` upgrade.

## Verdict

**Safe to use on a single-user Windows PC** in the default configuration of new installs: TLS API with client certificate,
named-pipe relay, app installed per user (winget / `%LOCALAPPDATA%`) or as a portable package (whose folder the app now
locks to the user, N-03). The live benchmark confirms that plain HTTP without the certificate is rejected by both engines.

**Not recommended on shared / RDP machines** while plain API mode is on (any local user gets root on the engine), or while
another user can hold `\\.\pipe\docker_engine` (N-05).

Recommendations for users: keep TLS on; use the named pipe instead of exporting `key.pem`; if the app warns that it could not
restrict its portable folder, move it into your profile; if other WSL users or distributions are untrusted, consider
`[interop] enabled=false` and drvfs `umask=077`.

Fix order (original): N-01, N-03, N-02, N-04, then the Low items; add `cargo audit` to CI. N-01 – N-03 are fixed, see below.

## Fixed after the re-audit

| ID | Fix | Verified by |
|---|---|---|
| N-01 | `extract_tar` skips every entry whose path goes through an existing symlink or reparse point (this also catches case-insensitive NTFS collisions such as `S` vs `s`), symlink targets may use `..` only as a prefix, and the parent folder of every entry must resolve (`canonicalize`) inside the destination before anything is written. Hard-link sources must be regular files reached without symlinks. | `extraction_blocks_symlink_chains` – the attack from the audit, run with Developer Mode on; it fails on the previous code (`evil.txt` written outside the target) and passes now |
| N-02 | *Open in Explorer* rejects Linux paths that Win32 would normalise differently (trailing dot or space, `\`, `:`, reserved characters, `.` / `..`) and checks on the Windows side that the final `\wsl.localhost\…` path is a directory right before starting `explorer.exe` (`wsl::explorer_dir`). | `unc_safe_rejects_win32_normalization`; `live_explorer_dir` – a Linux folder `x.hta.` next to a file `x.hta` passes `test -d` but is refused |
| N-03 | In portable mode the app sets a protected DACL on its folder at start (current user, SYSTEM, Administrators – full control, inherited by subfolders and files) whenever the folder grants access to anyone else, and tells the user; if that fails it shows a warning to move the app into the user profile (`src/portable_acl.rs`). | `detects_inherited_and_shared_acl`, `protects_real_folder`; `live_portable_acl_c_root` – a folder under `C:\` loses `Authenticated Users:(M)` and `Users:(RX)` |

Still open from the re-audit: N-04 (CI / self-hosted runner), N-05 – N-11.
