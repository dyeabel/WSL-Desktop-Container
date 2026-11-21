# Abstrakce hostitele, krok 2 (Linux) – plán implementace

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Aplikace běží na Linuxu (x86-64) jako ovládací panel nad **už existujícím** lokálním
Dockerem nebo Podmanem přes jeho unix socket. Funkce vázané na WSL a Windows jsou na Linuxu
skryté. Na Windows se nic nemění.

**Architecture:**
- `LocalHost` (`src/host/local.rs`) implementuje `Host`: lokální procesy, `pkexec` pro výslovné
  root akce.
- `Endpoint::Unix` + `UnixConnect` (`src/http/conn.rs`) dávají klientovi API unix socket.
- `src/host/discover.rs` najde engine: `DOCKER_HOST`, aktivní Docker context, výchozí sockety.
- `Caps` (schopnosti hostitele) řídí, které stránky a akce UI ukáže.
- Platformové služby pro Linux: XDG cesty, `notify-send`, XDG autostart, výběr terminálu.

**Tech Stack:** Rust 2021, eframe/egui 0.31, std vlákna, `std::os::unix::net`, ring (SHA-256 pro
Docker context). Žádné nové crate.

**Spec:** `docs/superpowers/specs/2026-11-21-host-abstraction-design.md` (oddíly *Krok 2*,
*Caps*, *Oprávnění místo jména uživatele*, rizika R1, R2).

## Výchozí stav (zjištěno před plánem)

- `cargo check --bins` pro `x86_64-unknown-linux-gnu` **projde** (rust:1-bookworm + `libgtk-3-dev
  libxdo-dev libayatana-appindicator3-dev`). Hlásí jen 14 varování o nepoužitém kódu pro Windows
  (`npipe.rs`, `portable_acl.rs`, `memory.rs::is_vmmem`, `notify.rs::AUMID`,
  `wsl.rs::CREATE_NO_WINDOW`, `shim/fast.rs::zone_abbr`).
- Mnoho modulů už má `cfg(not(windows))` náhrady (`notify`, `autostart::reg`, `tray::win`, `wake`).
- Krok 1 je v `main` (`Host`, `WslHost`, `http::Client`, `Endpoint::Tcp`).

## Odchylky od specu (rozhodnuto v plánu)

- **Docker contexty na Linuxu čte `src/host/discover.rs`, ne `contexts.rs`.** `contexts.rs` sdílí
  binárka shimu a spravuje kontexty aplikace. Na Linuxu se jen čte kontext Docker CLI.
- **Jedna datová složka a zvlášť konfigurace.** Data jdou do `$XDG_DATA_HOME/wsl-container-desktop`
  (výchozí `~/.local/share/...`), `config.json` do `$XDG_CONFIG_HOME/wsl-container-desktop`
  (výchozí `~/.config/...`). Tak to říká spec.
- **Shim (`[[bin]] docker`) se na Linuxu dál sestavuje kvůli `cargo test`**, ale do linuxového
  balíčku nepatří.

## Global Constraints

- **Windows beze změny:** argumenty `wsl.exe`, texty chyb, přehled „Na pozadí“, stránky a
  nastavení. Na Windows vrací `WslHost` `Caps::WSL` se všemi příznaky `true`.
- **Na Linuxu se nic neinstaluje ani nekonfiguruje:**
  - engine se nenastavuje a skripty z `engine.rs` se na Linuxu nikdy nespouští;
  - nemění se `docker.service`, skupiny ani systémové soubory;
  - `pkexec` jen na výslovné kliknutí uživatele, nikdy v periodickém sběru.
- **Vzdálené enginy** (`tcp://` mimo 127.0.0.1/localhost, `ssh://`) nejsou podporované. Aplikace
  ukáže hlášku a použije lokální socket, pokud existuje.
- Komentáře česky, uživatelské texty přes `t!` / `tf!` (cs + en). Bez async a bez nových crate.
- **Bezpečnost při vývoji (závazné pro všechny úkoly):**
  - GUI aplikaci nikdy nespouštět (Windows ani Linux);
  - shim jen s `WSLCD_SHIM_AUTOSTART=0`;
  - testy jen s portable daty (`target/test-data/<pid>`), nikdy `%APPDATA%`;
  - psát jen do repa a `C:\WSLContainerDesktop`;
  - linuxové testy běží v kontejneru přes Podman ve WSLCD-Lite (`scripts/linux-test.ps1`).
    Vytvářet a mazat tam image a volume je dovolené;
  - před a po kontrolovat `C:\WSLContainerDesktop\data\config.json` (distro=WSLCD-Lite,
    engine=podman); když byla WSLCD-Lite předtím uspaná, na konci `wsl.exe --terminate WSLCD-Lite`.
- Na Windows `cargo build` i `cargo test` bez varování. Na Linuxu (`scripts/linux-test.ps1 test`)
  totéž.
- Commity končí řádkem `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

1. **Socket existuje, ale chybí oprávnění** (uživatel není ve skupině `docker`): hláška musí říct
   co udělat, nesmí jít jen o „Permission denied“. Test v Task 3.
2. **`DOCKER_HOST=ssh://…` nebo `tcp://10.0.0.5:2375`**: aplikace nesmí tiše použít vzdálený
   engine ani spadnout. Hláška a pád zpět na lokální socket. Test v Task 3.
3. **Žádný engine na stroji** (žádný socket): aplikace se spustí a ukáže „engine nenalezen“ s
   radou (Docker, nebo `systemctl --user enable --now podman.socket`). Žádné opakované dotazy na
   heslo. Test v Task 3.
4. **Periodický sběr nesmí na Linuxu vyvolat `pkexec`** (síťový přehled, mapa toků k8s, alerty):
   jinak dotaz na heslo každé 2 s. Test v Task 6.
5. **Na Windows se nesmí změnit viditelné stránky ani nastavení.** Test na `Caps::WSL` a viditelnost
   stránek v Task 2.

---

### Task 0: Linuxové testovací prostředí a CI

**Files:**
- Create: `scripts/linux-dev.Containerfile`, `scripts/linux-test.sh`, `scripts/linux-test.ps1`
- Create: `.github/workflows/linux.yml`

**Interfaces:**
- Produces:
  - `scripts/linux-test.ps1 <check|test|live> [-- <cargo args>]` spustí v WSLCD-Lite
    `scripts/linux-test.sh`.
  - Ten v Podmanu sestaví (jednou) image `wslcd-rust-dev` z Containerfile (`FROM
    docker.io/library/rust:1-bookworm` + `libgtk-3-dev libxdo-dev libayatana-appindicator3-dev`) a
    v něm pustí cargo:
    - zdroj připojený jen pro čtení na `/src`;
    - `CARGO_TARGET_DIR=/target` ve volume `wslcd-rust-target`;
    - registr ve volume `wslcd-cargo-registry`.
  - Režimy:
    - `check` = `cargo check --locked --bins --tests`;
    - `test` = `cargo test --locked`;
    - `live` = jako `test` s `-- --ignored` pro filtr předaný za `--`. Navíc spustí v WSLCD-Lite
      dočasný `podman system service --time=0 unix:///run/wslcd-dev/podman.sock`, připojí socket do
      kontejneru jako `/run/podman/podman.sock`, nastaví `DOCKER_HOST=unix:///run/podman/podman.sock`
      a po skončení službu ukončí.
  - Cesta k repu se v PowerShellu převede na `/mnt/<disk>/...` (interop ve WSLCD-Lite je
    vypnutý, `wslpath` nejde volat zevnitř).
  - Skripty nesmí ležet v `%TEMP%` (MSIX virtualizace), jen v repu.
  - `.github/workflows/linux.yml`: `ubuntu-24.04`, apt závislosti jako v Containerfile,
    `cargo build --locked`, `cargo test --locked`, pak
    `cargo test --locked host::local -- --ignored` proti Dockeru runneru.

- [ ] **Step 1:** Napsat Containerfile a oba skripty.
- [ ] **Step 2:** `scripts/linux-test.ps1 check` → PASS (varování smí zůstat, řeší je Task 5).
- [ ] **Step 3:** `scripts/linux-test.ps1 test` → všechny unit testy PASS na Linuxu. Testy, které
  na Linuxu neprojdou kvůli Windows předpokladům, se označí `#[cfg(windows)]` a vypíší v reportu.
- [ ] **Step 4:** Napsat `.github/workflows/linux.yml`. Ověřit syntaxi. CI se nespouští, nic se
  nepushuje.
- [ ] **Step 5:** Commit `linux: testovací kontejner (WSLCD-Lite) a CI úloha pro Ubuntu`.

### Task 1: `Endpoint::Unix` a `UnixConnect`

**Files:**
- Modify: `src/http/mod.rs`, `src/http/conn.rs`, `src/docker.rs`
- Test: `src/http/conn.rs` (`#[cfg(all(test, unix))]`), `src/http/client.rs`

**Interfaces:**
- Produces:
  - `Endpoint::Unix(std::path::PathBuf)`: varianta je **bez cfg**, aby `match` byly na všech
    platformách úplné.
  - `pub struct UnixConnect { pub path: PathBuf }`, `impl Connect`:
    - `describe()` = `"unix://<path>"`, `host_header()` = `"localhost"`;
    - na Linuxu `std::os::unix::net::UnixStream` (connect timeout přes neblokující connect není
      nutný: unix socket odpoví hned);
    - `set_read_timeout` a `shutdown_handle` přes `try_clone`;
    - na Windows vrátí `io::Error::other(t!("Unix socket není na Windows podporovaný", …))`.
  - Chyba `PermissionDenied` při připojení se přeloží na hlášku, co dělat:
    `tf!("Nemáte přístup k {path}: přidejte se do skupiny docker (pak se odhlaste a přihlaste), nebo použijte rootless Podman: systemctl --user enable --now podman.socket", …)`.
  - `Docker::new(&Endpoint::Unix(p))` použije `UnixConnect` (TLS se nepoužije). Sdílená instance
    se klíčuje stejně jako dnes (endpoint + TLS).

- [ ] **Step 1: Testy** (Linux, falešný HTTP server na `UnixListener` v dočasné složce):
  `unix_keep_alive_reuses_connection`, `unix_permission_denied_has_hint` (socket s právy `000`;
  při běhu jako root se přeskočí s výpisem), `unix_describe`.
- [ ] **Step 2:** `scripts/linux-test.ps1 test -- http::` → FAIL.
- [ ] **Step 3:** Implementovat.
- [ ] **Step 4:** Linux `test` PASS, Windows `cargo test` PASS, oba bez nových varování.
- [ ] **Step 5:** Commit `http: Endpoint::Unix a UnixConnect`.

### Task 2: `Caps`, `HostKind` a viditelnost v UI

**Files:**
- Modify: `src/host/mod.rs`, `src/host/wsl.rs`, `src/app.rs`, `src/ui/mod.rs`,
  `src/ui/settings.rs`, `src/ui/engine.rs`, `src/ui/dashboard.rs`, `src/ui/files.rs`,
  `src/ui/net_overview.rs` (jen místa, která ukazují funkce WSL)

**Interfaces:**
- Produces (v `crate::host`):
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum HostKind { Wsl, Local }`
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub struct Caps { pub distros, pub engine_install, pub api_tls, pub daemon_json, pub vm_memory, pub vm_disk, pub wslconfig, pub shim, pub npipe, pub ipv6_relay, pub root_sampling, pub files_root, pub lite, pub contexts: bool }`
    s konstantami `Caps::WSL` (vše `true`) a `Caps::LOCAL` (vše `false`).
  - `Host::kind(&self) -> HostKind` a `Host::caps(&self) -> Caps`. `WslHost` vrací `Wsl` a
    `Caps::WSL`.
  - `App::caps(&self) -> Caps` = `host::current(&cfg).caps()`. UI ho čte, nikdy nevytváří hostitele
    jinak.
  - `Page::label_for(self, kind: HostKind) -> &'static str`: pro `Page::Engine` vrací na `Local`
    `t!("\u{EB02}  Engine", …)`, jinak dnešní `label()`.
- **Pravidla viditelnosti** (podle Caps; na WSL je vše vidět jako dnes):
  - *Nastavení:* `.wslconfig` (`wslconfig`), shim v PATH (`shim`), roura (`npipe`), relay `::1`
    (`ipv6_relay`), portable a WSLCD Lite (`lite`), průvodce přidáním distribuce (`distros`);
  - *Engine:* distribuce (`distros`), instalace, API a odinstalace (`engine_install`), TLS
    (`api_tls`), `daemon.json` (`daemon_json`), export, import a kompaktace (`vm_disk`);
  - *Přehled:* VmmemWSL a uvolňování paměti (`vm_memory`), VHDX a kompaktace (`vm_disk`);
  - *Soubory:* přepínač „Pracovat jako root“ (`files_root`);
  - *Sítě:* konflikty s jinými distribucemi (`distros`), sloupce ze `ss -p` jako root
    (`root_sampling`).

- [ ] **Step 1: Testy** v `src/host/mod.rs` a `src/app.rs`:
  `wsl_caps_all_true` (WslHost::caps == Caps::WSL a každý příznak true),
  `local_caps_all_false`, `engine_label_wsl_unchanged`
  (`Page::Engine.label_for(Wsl) == Page::Engine.label()`), `engine_label_local`.
- [ ] **Step 2:** FAIL → implementovat → PASS (Windows i Linux).
- [ ] **Step 3:** Commit `host: Caps a HostKind, UI skrývá funkce WSL podle hostitele`.

### Task 3: Hledání enginu (`src/host/discover.rs`)

**Files:**
- Create: `src/host/discover.rs`
- Test: tamtéž

**Interfaces:**
- Produces:
  - `pub enum Found { Endpoint(Endpoint), Remote(String), None }`
  - `pub fn discover(env: &dyn Fn(&str) -> Option<String>, home: &Path, exists: &dyn Fn(&Path) -> bool) -> (Found, Option<String>)`.
    Druhá složka je nepovinná hláška pro UI (např. o ignorovaném vzdáleném enginu).
- **Pořadí** (spec):
  1. `DOCKER_HOST`:
     - `unix://<cesta>` → `Unix`;
     - `tcp://127.0.0.1:<p>` nebo `tcp://localhost:<p>` → `Tcp{p}`;
     - cokoli jiného → zapamatovat `Remote` a pokračovat.
  2. Aktivní Docker context: `DOCKER_CONTEXT`, jinak `currentContext` z `<home>/.docker/config.json`,
     pokud není `default`. Endpoint se vezme z
     `<home>/.docker/contexts/meta/<sha256 hex jména>/meta.json`, pole `Endpoints.docker.Host`.
     Platí stejná pravidla lokálnosti jako u bodu 1.
  3. `/var/run/docker.sock`
  4. `$XDG_RUNTIME_DIR/podman/podman.sock`
  5. `/run/podman/podman.sock`

  Body 3–5 jen když `exists(path)`. Výsledek `None`, když nic neexistuje. Když byl zapamatován
  `Remote` a nic lokálního se nenašlo, výsledek je `Remote`.
- **Hlášky** (`t!`/`tf!`):
  - vzdálený engine: „DOCKER_HOST/kontext ukazuje na vzdálený engine ({x}), který aplikace na
    Linuxu nepodporuje; používá se lokální {socket}.“;
  - nic: „Engine nenalezen. Spusťte Docker (sudo systemctl enable --now docker), nebo rootless
    Podman (systemctl --user enable --now podman.socket).“

- [ ] **Step 1: Testy** (čisté funkce, běží na Windows i Linuxu):
  - `docker_host_unix`, `docker_host_tcp_local`, `docker_host_remote_falls_back`, `ssh_is_remote`;
  - `context_from_config` (dočasný HOME s `config.json` a `meta/<sha>/meta.json`), `default_context_ignored`;
  - `default_docker_sock`, `rootless_podman`, `nothing_found`;
  - `docker_context_env_overrides_config`.
- [ ] **Step 2:** FAIL → implementovat → PASS (Windows i Linux).
- [ ] **Step 3:** Commit `host: hledání lokálního enginu (DOCKER_HOST, Docker context, sockety)`.

### Task 4: `LocalHost`

**Files:**
- Create: `src/host/local.rs` (`#[cfg(unix)]`)
- Modify: `src/host/mod.rs` (`current()` na Linuxu vrací `LocalHost`), `src/engine.rs`
  (CLI pro endpoint)

**Interfaces:**
- Consumes: Task 2 (`Caps`, `HostKind`), Task 3 (`discover`), Task 1 (`Endpoint::Unix`).
- Produces:
  - `pub struct LocalHost { found: Found, note: Option<String> }`. Konstruktor
    `LocalHost::new() -> Self` zavolá `discover` se skutečným prostředím, HOME a `Path::exists`.
  - `impl Host for LocalHost`:
    - `kind()` = `Local`, `caps()` = `Caps::LOCAL`, `label()` = `"localhost"`;
    - `exec` a `exec_stream`: `std::process::Command::new(args[0]).args(&args[1..])`,
      `current_dir(cwd)`, stdin ze samostatného vlákna jako ve `WslHost` (BrokenPipe se
      ignoruje). Záznam aktivity přes `host::track_req` stejně jako ve `WslHost`;
    - `Privilege::User` a `Engine` = aktuální uživatel. `Privilege::Root` = `pkexec <args…>`, pokud
      proces neběží jako root (`std::fs::metadata("/proc/self")?.uid() != 0`), jinak přímo;
    - `open_terminal`: první funkční kandidát z `terminal_candidates(env("TERMINAL"))`;
    - `to_guest_path`: identita;
    - `engine_endpoint(cfg)`: `Found::Endpoint(e)` → `e`, jinak
      `Endpoint::Unix("/var/run/docker.sock")`. UI pak ukáže nedostupnost s hláškou z `note()`;
    - `wsl()`: `None`.
  - `pub fn note(&self) -> Option<&str>`: hláška z discover pro UI.
  - `pub(crate) fn terminal_candidates(terminal: Option<&str>) -> Vec<(String, Vec<String>)>`:
    pořadí `$TERMINAL -e`, `x-terminal-emulator -e`, `gnome-terminal --`, `konsole -e`,
    `xterm -e`. Každá položka je (program, prefix argumentů před příkazem).
  - `Engine::cli_for(self, ep: &Endpoint, tls: bool) -> Vec<String>`:
    - pro `Tcp` vrací totéž co dnešní `cli(port, tls)`;
    - pro `Unix(p)`: Docker → `["docker", "-H", "unix://p"]`, Podman →
      `["podman", "--url", "unix://p"]`.

    Volající `cli(...)` (16 míst) přejdou na `cli_for(&host.engine_endpoint(cfg), cfg.tls)`.
  - Typ enginu na Linuxu: při startu workeru, pokud `kind()==Local`, se z `/version` zjistí
    Podman nebo Docker (komponenta `Podman Engine`) a nastaví se `cfg.engine` (v paměti i v
    konfiguraci).

- [ ] **Step 1: Testy** (`#[cfg(all(test, unix))]`, běží jen na Linuxu):
  kontraktní test jako `host::wsl::tests::live` (stdout a stderr, kód, cwd, stdin 1 MiB přes `cat`,
  streamování řádků s `\r`, zrušení `sleep 60` do 3 s, jeden záznam aktivity, `quiet` bez
  záznamu), `terminal_candidates_order`, `root_uses_pkexec_when_not_root` (jen sestavení příkazu,
  bez spuštění), `cli_for_unix_docker_and_podman`, `cli_for_tcp_matches_cli` (běží i na Windows).
  Živý test `#[ignore] local_engine_ping`: `Docker::new(&LocalHost::new().engine_endpoint(..)).ping()`.
- [ ] **Step 2:** FAIL → implementovat → `scripts/linux-test.ps1 test` PASS,
  `scripts/linux-test.ps1 live -- host::local` PASS (Podman z WSLCD-Lite), Windows `cargo test` PASS.
- [ ] **Step 3:** Commit `host: LocalHost pro Linux (lokální procesy, pkexec, terminál)`.

### Task 5: Platformové služby na Linuxu a čistý build

**Files:**
- Modify: `src/config.rs` (XDG cesty), `src/notify.rs`, `src/autostart.rs`, `src/main.rs`,
  `src/npipe.rs`, `src/portable_acl.rs`, `src/memory.rs`, `src/wsl.rs`, `src/shim/fast.rs`
  (jen gating)

**Interfaces:**
- Produces:
  - `config::data_dir()` na Linuxu (mimo testy): `WSLCD_DATA_DIR` → portable vedle EXE →
    `$XDG_DATA_HOME/wsl-container-desktop` (výchozí `~/.local/share/wsl-container-desktop`).
    Cesta ke `config.json` na Linuxu je `$XDG_CONFIG_HOME/wsl-container-desktop/config.json`
    (výchozí `~/.config/...`). Při `WSLCD_DATA_DIR` nebo portable zůstává `config.json` v datové
    složce jako dnes. Testy dál jen `target/test-data`.
  - `notify::show_with` na Linuxu: `notify-send --app-name="WSL Container Desktop" <title> <body>`.
    Když `notify-send` chybí, nic se nestane (záznam v historii zůstává). `on_click` se na Linuxu
    nevolá.
  - `autostart` na Linuxu: `is_enabled`/`set` = existence a zápis
    `$XDG_CONFIG_HOME/autostart/wsl-container-desktop.desktop` (`Exec=<exe> --autostart`).
    `fix_path` aktualizuje `Exec`, jen když soubor existuje.
  - Windows-only moduly, které se na Linuxu nepoužívají (`npipe`, `portable_acl`, části `memory`,
    `notify::AUMID`, `wsl::CREATE_NO_WINDOW`, `shim::fast::zone_abbr`), dostanou `#[cfg(windows)]`
    nebo náhradu tak, aby Linux build byl **bez varování**.
- **Tray (riziko R2):** na Linuxu se v tomto úkolu tray **nezapíná**. Ruling plánu: bez možnosti
  spustit GUI ho nejde ověřit. „Zavřít do tray“ a „minimalizovat do tray“ jsou na Linuxu skryté,
  zavření okna aplikaci ukončí. Zapnutí tray je follow-up s ručním ověřením.

- [ ] **Step 1: Testy:**
  - `xdg_paths` (čistá funkce `xdg_dirs(env) -> (data, config)`);
  - `autostart_desktop_file_content` (čistá funkce, `Exec` s cestou a `--autostart`);
  - `notify_send_args` (čistá funkce sestavení argumentů).
- [ ] **Step 2:** FAIL → implementovat → PASS na Windows i Linuxu.
- [ ] **Step 3:** `scripts/linux-test.ps1 check` bez jediného varování a `cargo build` na Windows
  bez varování.
- [ ] **Step 4:** Commit `linux: XDG cesty, notify-send, XDG autostart, build bez varování`.

### Task 6: Worker a UI na Linuxu

**Files:**
- Modify: `src/worker/mod.rs`, `src/worker/net_overview.rs`, `src/worker/net_overview_live.rs`,
  `src/worker/k8s.rs`, `src/worker/alerts.rs`, `src/worker/memory.rs`, `src/worker/engine_ops.rs`,
  `src/ui/engine.rs`, `src/ui/dashboard.rs`, `src/tray.rs` (jen skrytí), `src/app.rs`

**Interfaces:**
- Consumes: `Host::caps()`, `LocalHost::note()`.
- **Pravidla:**
  - Každý periodický sběr, který dnes běží s `Privilege::Root`, se spustí jen při
    `caps.root_sampling`:
    - síťový přehled: `OVERVIEW_SCRIPT`, `DISTRO_SCRIPT` jako root, `readlink` netns, `NS_SCRIPT`;
    - mapa toků k8s (conntrack přes nsenter);
    - alerty (pokud nějaký sběr používá root).

    Bez schopnosti se sběr přeskočí a UI ukáže „nedostupné na tomto systému“.
  - `ExecReq` dostane příznak `interactive()`. `LocalHost` spustí `pkexec` **jen** pro
    `Privilege::Root` s `interactive == true`. Root bez něj vrátí
    `HostError::Spawn(t!("Vyžaduje oprávnění roota", …))`. Pojistka proti pravidlu Review Focus 4.
  - **Engine stránka na Linuxu:**
    - stav (verze, typ, koncový bod, hláška z `note()`);
    - Start a Stop:
      - rootless Podman (endpoint pod `$XDG_RUNTIME_DIR`) → `systemctl --user start|stop podman.socket`;
      - jinak `pkexec systemctl start|stop docker` (interactive).
  - **Přehled paměti na Linuxu:** `/proc/meminfo` hostitele a kontejnery, bez VmmemWSL.
    `memory.rs` na Linuxu vrací pro VM `None`.
  - Tray na Linuxu se neinicializuje (Task 5 ruling).

- [ ] **Step 1: Testy:**
  - `root_sampling_skipped_without_cap` (FakeHost s `Caps::LOCAL` zaznamená volání; sběr síťového
    přehledu nevolá exec s `Privilege::Root`);
  - `local_root_without_interactive_is_refused`;
  - `engine_start_stop_commands` (čistá funkce: rootless Podman vs Docker).
- [ ] **Step 2:** FAIL → implementovat → PASS na Windows i Linuxu. `scripts/linux-test.ps1 live --
  host::local` PASS.
- [ ] **Step 3:** Commit `linux: worker a UI bez WSL, root jen interaktivně`.

### Task 7: Balíček pro Linux a dokumentace

**Files:**
- Create: `assets/linux/wsl-container-desktop.desktop`, `scripts/package-linux.sh`
- Modify: `.github/workflows/linux.yml` (artefakt), `README.md` (oddíl *Linux*), `docs/ROADMAP.md`

**Interfaces:**
- `scripts/package-linux.sh <verze>` (běží v `wslcd-rust-dev` přes `linux-test.ps1`, nebo v CI)
  udělá `cargo build --release --locked --bin wsl-container-desktop` a vytvoří
  `dist/WSL-Container-Desktop-<verze>-linux-x64.tar.gz` s:
  - binárkou;
  - `.desktop` souborem;
  - ikonou 256 px (`assets/icon.png`);
  - krátkým `README-linux.txt` (požadavky: GTK 3, přístup k socketu Dockeru nebo rootless Podman).

  Plus `.sha256`.
- CI úloha `linux` nahraje tar.gz jako artefakt. Při tagu `v*` ho přiloží k releasu stejně jako
  ZIPy z Windows úlohy.
- README: co na Linuxu funguje, co ne (WSL funkce, tray), jak dát uživateli přístup k socketu.

- [ ] **Step 1:** Napsat soubory. Spustit `scripts/package-linux.sh 0.9.0-dev` přes kontejner.
  Ověřit obsah archivu (`tar tzf`). Archiv necommitovat (`dist/` je ignorovaný).
- [ ] **Step 2:** Commit `linux: balíček tar.gz, .desktop a dokumentace`.

### Task 8: Ověření kroku 2

- [ ] **Step 1:** Na Windows `cargo build` a `cargo test` bez varování. `scripts/bench.ps1 -Quick
  -Podman WSLCD-Lite` (shim má `WSLCD_SHIM_AUTOSTART=0`). Výsledky uložit jako
  `docs/bench/2026-11-21-host-abstraction-step2.md/.json`. Publikované `BENCHMARK.md` a
  `results.json` vrátit, pokud je skript přepsal. Mantinely jako v kroku 1:
  - `exec true` a `docker ps` p50 ±10 % proti
    `docs/bench/2026-11-21-host-abstraction-after.md`;
  - 4 klienti ≥ 635 req/s.
- [ ] **Step 2:** Na Linuxu `scripts/linux-test.ps1 test` a `scripts/linux-test.ps1 live --
  host::local` PASS, `check` bez varování.
- [ ] **Step 3:** Bezpečná sada živých testů na Windows (`WSLCD_TLS=1 WSLCD_PORT=2376
  WSLCD_DISTRO=WSLCD-Lite`):
  - `host::wsl::tests::live`;
  - `live_podman_api`, `live_events_stream`, `live_npipe_plain`;
  - `live_volume_backup_restore`, `live_build_image`, `live_container_detail`.
- [ ] **Step 4:** `docs/ROADMAP.md`: do *Done* „Abstrakce hostitele, krok 2 (Linux)“. Commit.
