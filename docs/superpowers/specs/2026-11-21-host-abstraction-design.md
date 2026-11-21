# Abstrakce hostitele, Linux a agent – návrh

Stav: schváleno uživatelem (s doplněním vlastního rootfs Lite a převzetí macOS z Kiri) · 2026-11-21

## Proč

Aplikace dnes umí jen Windows a WSL. Všechno, co se děje „v Linuxu“, jde přímo přes `wsl.exe`
(211 volání `wsl::` ve 28 souborech) a klient API enginu je natvrdo na `tcp://127.0.0.1:<port>`.
Uživatel chce dvě věci:

1. **Lepší architekturu**: oddělit „kde běží engine a jak se k němu dostanu“ od zbytku aplikace.
2. **Cestu k macOS** pro kolegy v práci, kteří používají Mac. macOS je skutečný cíl, ale přijde
   až po tomto návrhu. Tady se pro něj připravuje půda.

Linux je příjemný bonus: uživatel si ho umí představit na jiném vlastním stroji.

Inspirace: [libracoder/kiri](https://github.com/libracoder/kiri). Kiri má pod démonem jednu
vrstvu (`kiri-vmm`), která na macOS spouští VM (Virtualization.framework) a na Linuxu tentýž
agent jako obyčejný proces. Na Windows vede všechna spojení na API přes **jeden dlouho běžící
`wsl.exe -u root -- kiri bridge`** s multiplexem, protože TCP přes localhost relay WSL ztrácí
half-close (na tom stojí `docker run` a `attach`). Vlastní runtime Kiri (clone3 bez runc a
dockerd, vlastní jádro) se nepřebírá: WSL Container Desktop zůstává ovládacím panelem nad
Dockerem a Podmanem.

## Cíle

Tři kroky, každý jde vydat samostatně a nic nerozbije:

1. **Krok 1 – Hostitel a koncový bod API.**
   - Trait `Host` nahradí přímá volání `wsl::` všude, kde aplikace spouští něco v Linuxu.
   - Klient API enginu umí kromě TCP i unix socket a obecný proud bajtů (pro agenta).
   - Na Windows se chování nezmění.
2. **Krok 2 – Linux (`LocalHost`).**
   - Aplikace se sestaví a běží na Linuxu (x86-64) a připojí se k **už existujícímu** Dockeru nebo
     Podmanu přes jeho socket.
   - Funkce vázané na WSL a Windows jsou na Linuxu skryté.
3. **Krok 3 – Agent a bridge na Windows.**
   - Malý statický linuxový binár `wslcd-agent` běží v distribuci jako jeden dlouho běžící proces
     za `wsl.exe`.
   - Přes jeden kanál spouští příkazy a vede spojení na API enginu.
   - Odpadne spouštění nového `wsl.exe` pro každý příkaz a spojení na API přestanou záviset na
     TCP relayi WSL.
   - Stejný agent a protokol později poslouží macOS přes vsock.

## Mimo rozsah (non-goals)

- **macOS**: VM, vsock, balíček `.app`, notarizace. Návrh jen zajišťuje, že mu nic nebrání
  (viz *Připravenost na macOS*).
- **Instalace a konfigurace enginu na Linuxu.** Skripty z `engine.rs` přepisují `docker.service`
  a systemd. Na Linuxu by sahaly do systémového Dockeru uživatele, ne do izolované distribuce.
- **Balíčky `.deb` / `.rpm` / Flatpak / AppImage.** Na Linuxu stačí binárka a `.desktop` soubor.
- **Shim `docker` na Linuxu a macOS.** Tam funguje nativní docker CLI a kontexty.
- **Změna rychlé cesty shimu na Windows.** Shim dál mluví s API přes TCP.
- **Přepis UI.** egui zůstává.
- **Linux na ARM.** Agent se ale připravuje i pro aarch64 (macOS a Windows na ARM).

## Čemu se vyhnout (anti-goals)

- **Pomalejší Windows.** Každý krok musí projít výkonovými mantinely (níže). Jinak se nová cesta
  nezapne.
- **Únik pojmů WSL do společného rozhraní.** `Host` nesmí obsahovat UNC cesty, názvy distribucí,
  VHDX ani `.wslconfig`. Ty patří do schopností specifických pro WSL. Jinak macOS nepůjde
  doplnit bez dalšího refaktoru.
- **Agent jako síťová služba.** Agent běží jako root. Nikdy neposlouchá na TCP. Je dostupný jen
  přes stdio procesu, který spustila aplikace (Windows), přes unix socket s právy 0600 (Linux,
  pokud se použije) nebo přes vsock (macOS).
- **Regrese funkcí.** Žádná dnešní funkce na Windows nesmí zmizet ani se změnit.

## Omezení

- **Kód a závislosti:**
  - Rust, edition 2021, eframe/egui 0.31.
  - Bez async: vlákna podle stylu `npipe.rs` a `portrelay.rs`.
  - Minimum nových závislostí.
- **Windows zůstává hlavní platformou:**
  - portable režim (data v `C:\WSLContainerDesktop`) a vzájemné TLS na API zůstávají;
  - bezpečnostní nálezy v `docs/SECURITY.md` zůstávají vyřešené.
- **Testy:**
  - Živé testy (`live_tests.rs`, `live_docker.rs`, `lite/live_lite.rs`) pracují se skutečnými
    distribucemi. Před `live_switch_primary` zkontrolovat primární distribuci v
    `C:\WSLContainerDesktop\data\config.json` a vrátit sdílené iptables.
  - Živé testy smí dál volat `wsl::` napřímo. Jde o testy WSL, ne o kód aplikace.
- **Benchmark:**
  - `scripts/bench.ps1` / `src/bench.rs` je referenční měření.
  - Výchozí hodnoty jsou v `docs/bench/2026-11-21-wslcd-lite-podman.md`: shim `docker ps`
    169 ms p50, `wsl.exe -e podman ps` 408 ms p50, API až 705 req/s, aplikace v klidu 0,07 % CPU.

## Přístupy

| | Přístup | Pro | Proti |
|---|---|---|---|
| A | **Tenký trait nad dneškem**: `Host` obalí `wsl.exe`. Linux spouští příkazy lokálně. Bridge je samostatné vylepšení. | Nejlevnější, nejmenší riziko. | macOS potřebuje agenta uvnitř VM (žádný `wsl.exe`). Vznikl by až na konci, neověřený. |
| B | **Agent jako jádro** (cesta Kiri): vše přes agenta a protokol od začátku, na všech platformách. | Protokol se odladí na Windows každodenním používáním. macOS je pak „jen VM + vsock“. | Nejvíc práce najednou. Mění chování Windows hned v prvním kroku. |
| C | **Hybrid**: trait z A, navržený pod agenta z B. Nejdřív čistý refaktor, pak Linux bez agenta, pak agent na Windows se zálohou na `wsl.exe`. | Architektura hned. Linux brzy. Agent ověřený na Windows dřív, než na něm bude stát macOS. Riziko po malých krocích. | Dočasně dvě cesty na Windows (agent a `wsl.exe`). |

**Zvoleno: C** (uživatel). Výkonově: trait nestojí nic, Linux běží nativně a agent na Windows
zrychlí periodické sběry. Riziko je jen propustnost bridge u velkých přenosů a hlídají ji
mantinely níže.

## Řešení

### Krok 1a – trait `Host`

Nový modul `src/host/` (`mod.rs`, `wsl.rs`, později `local.rs` a `agent.rs`). Nástin rozhraní
(názvy a přesné typy jsou na návrháři implementace):

```rust
pub struct ExecReq<'a> {
    pub args: &'a [&'a str],        // program a argumenty, bez shellu
    pub privilege: Privilege,       // User / Engine / Root – viz níže
    pub cwd: Option<&'a str>,       // cesta v hostiteli (linuxová)
    pub stdin: Option<&'a [u8]>,
    pub env: &'a [(&'a str, &'a str)],
}

pub struct ExecOut { pub code: i32, pub stdout: Vec<u8>, pub stderr: Vec<u8> }

pub trait Host: Send + Sync {
    fn kind(&self) -> HostKind;                       // Wsl, Local (později AppleVm)
    fn label(&self) -> String;                        // pro UI a přehled „Na pozadí“
    fn caps(&self) -> &Caps;                          // co hostitel umí (řídí UI)

    fn exec(&self, req: &ExecReq) -> Result<ExecOut, HostError>;
    fn exec_stream(&self, req: &ExecReq, cancel: Option<&Cancel>,
                   on_line: &mut dyn FnMut(&str)) -> Result<i32, HostError>;
    fn open_terminal(&self, title: &str, req: &ExecReq) -> Result<(), HostError>;

    fn to_guest_path(&self, host_path: &str) -> Result<String, HostError>;   // C:\x -> /mnt/c/x
    fn host_path(&self, guest_path: &str) -> Option<std::path::PathBuf>;    // UNC / stejná cesta

    fn engine_endpoint(&self, cfg: &Config) -> Endpoint;
    fn lifecycle(&self) -> Option<&dyn Lifecycle>;    // start / sleep / stav; WSL teď, VM později
    fn wsl(&self) -> Option<&WslHost> { None }        // jediná cesta ke schopnostem jen pro WSL
}
```

**Volací místa:**
- Pomocné funkce `run`, `run_owned` a `run_stdin` nad `Host` zachovají přesnou sémantiku
  dnešních `wsl::run*`. Úspěch: stdout a za ním stderr. Chyba: `Failed { code, output }`, kde
  output je stderr, nebo stdout, když je stderr prázdný. Přepis volání je tak mechanický:
  `wsl::run(&distro, Some("root"), None, &[..])` → `host.run(Privilege::Engine | Root, None, &[..])`.
- Volání, která si dnes skládají `wsl::base()` sama (`fs.rs`, `worker/compose.rs`), přejdou na
  `exec` s oddělenými `stdout` a `stderr`.
- Záznam do přehledu „Na pozadí“ (`activity::start` v `wsl::track`) se přesune do společného
  obalu nad `Host`. Platí pak pro každou implementaci.

**Schopnosti jen pro WSL** zůstávají v `wsl.rs` a jsou dostupné přes `host.wsl()`:
- seznam a správa distribucí, `list_online`, `install_distro`, `set_default`;
- export, import, `unregister`;
- VHDX: `vhd_path`, sparse, compact;
- `keepalive`, `shutdown`;
- UNC a `explorer_dir`, `.wslconfig`.

**`Caps`** je struktura příznaků, podle kterých UI skrývá stránky a akce:

| Příznak | Co řídí |
|---|---|
| `distros` | správa distribucí |
| `engine_install` | instalace a nastavení enginu |
| `api_tls` | zabezpečení API přes TLS |
| `daemon_json` | editor `/etc/docker/daemon.json` |
| `vm_memory` | VmmemWSL, uvolňování paměti |
| `vm_disk` | VHDX a kompaktace |
| `wslconfig` | globální `.wslconfig` |
| `shim` | `docker` v PATH |
| `npipe` | pojmenovaná roura |
| `ipv6_relay` | relay pro `::1` |
| `root_sampling` | periodické sběry jako root (nsenter, ss, conntrack) |
| `lite` | WSLCD Lite |

Hostitel se odvozuje z konfigurace a worker drží `Arc<dyn Host>`:
- Windows: `WslHost` pro `cfg.distro`. Přepnutí primární distribuce vymění instanci.
- Linux: `LocalHost`.

**Jiné než primární distribuce** *(rozhodnutí návrháře)*: řada volání dnes míří do jiných
distribucí. Například síťový přehled spouští skript ve všech běžících distribucích, přepnutí
primární uvolňuje API v ostatních, průvodce připravuje novou distribuci, Lite má vlastní a k3s
dostává distribuci parametrem.
- Na tato místa slouží `host.wsl()?.for_distro(name) -> WslHost`, levná instance bez agenta (jen
  `wsl.exe`).
- Volání tím projdou stejnými pomocnými funkcemi a přehledem „Na pozadí“, ale agent se kvůli nim
  nespouští.
- Agent (krok 3) běží **jen v primární distribuci**. Ostatní distribuce tedy nedrží naživu a
  jejich chování se nemění.

**Oprávnění místo jména uživatele** *(rozhodnutí návrháře)*: `ExecReq` nedostane `user:
Option<&str>`, ale `privilege: Privilege`.
- Varianty:
  - `User`: výchozí uživatel;
  - `Engine`: potřebuje přístup k enginu nebo jeho CLI;
  - `Root`: opravdu potřebuje root (soubory v režimu root, nsenter, conntrack, zápis do `/etc`).
- Na WSL se `Engine` i `Root` mapují na `-u root`, takže chování se nemění.
- Na Linuxu `Engine` znamená aktuálního uživatele (má přístup k socketu) a `Root` znamená
  `pkexec`, nebo nedostupné podle `Caps`.
- Krok 1 proto u 39 dnešních volání s `Some("root")` rozhodne, jestli jde o `Engine` nebo `Root`.
  Je to jediná nemechanická část přepisu.

**Pořadí přepisu:** `worker/*`, `fs.rs`, `k8s/cluster.rs`, `memory.rs`, `lite/mod.rs`,
`portrelay.rs`, `app.rs`, `tray.rs`, `ui/*`, `shim_install.rs`.

### Krok 1b – koncový bod API

```rust
pub enum Endpoint {
    Tcp { port: u16 },            // dnešní stav; TLS podle cfg.tls jako dnes
    Unix(std::path::PathBuf),     // Linux (a macOS později)
    Agent(AgentLink),             // krok 3: proud přes agenta
}
```

**Klient (`docker.rs`):**
- `Docker` dostane místo `base: String` a `port: u16` koncový bod a obecný „konektor“, který
  vrací `Read + Write + Send` proud.
- Na tom se staví HTTP/1.1: běžné požadavky s keep-alive poolem i vlastní streamy (`open_stream`,
  události, logy, statistiky, soubory v kontejneru).
- Všechna místa s `Docker::new(port)` (asi 15) přejdou na `Docker::new(&endpoint)`.

**Kontexty a UI:**
- `contexts.rs` začne přijímat `unix://` (dnes ho výslovně odmítá).
- Chybové hlášky typu „API není dostupné na 127.0.0.1:{port}“ budou vypisovat koncový bod.

ureq 2.12 vlastní transport (unix socket, proud) nepodporuje. Způsob řešení je první spike
(riziko R1).

### Krok 2 – `LocalHost` (Linux)

**Sestavení:**
- `windows-sys`, `winresource` a moduly jen pro Windows přejdou pod `cfg(windows)`. Týká se to
  `npipe`, `portrelay`, `dpapi`, `portable_acl`, `eventlog`, `wslconfig`, `wsl`, `lite`,
  `shim_install` a binárky shimu.
- Platformové služby budou za malým modulem `platform::{autostart, notify, terminal, dirs}` s
  implementací pro Windows (dnešní kód) a pro Linux.

**Engine:**
- Hledání socketu v tomto pořadí:
  1. `DOCKER_HOST` (`unix://` nebo `tcp://127.0.0.1:…`);
  2. `/var/run/docker.sock`;
  3. `$XDG_RUNTIME_DIR/podman/podman.sock`;
  4. `/run/podman/podman.sock`.
  Pokud je aktivní Docker context (`~/.docker/config.json`, `DOCKER_CONTEXT`) s lokálním
  koncovým bodem `unix://` nebo `tcp://127.0.0.1`, má přednost před bodem 2 a dalšími.
- Vzdálené enginy (`tcp://` mimo localhost, `ssh://`) aplikace na Linuxu nepodporuje. Ukáže to a
  nabídne lokální socket. *(rozhodnutí návrháře)*
- `contexts.rs` na Linuxu kontexty jen čte, nevytváří ani nespravuje. Stránka kontextů je na
  Linuxu skrytá. *(rozhodnutí návrháře)*
- Typ enginu se pozná z `/version` (komponenta Podman).
- Když chybí oprávnění, aplikace vysvětlí, co udělat: skupina `docker`, nebo
  `systemctl --user enable --now podman.socket`.
- Start a stop enginu:
  - rootless Podman: `systemctl --user`;
  - systémový Docker: `pkexec systemctl start|stop docker`, jen na výslovné kliknutí.

**Spouštění příkazů:** lokální `std::process::Command`, cesty se nepřekládají (`to_guest_path`
je identita).
- `Privilege::Engine` běží jako aktuální uživatel bez dotazu na heslo.
- `Privilege::Root`:
  - **Výslovná jednorázová akce uživatele** (start a stop systémového Dockeru, zápis souboru
    jako root) jde přes `pkexec`. Systém se zeptá na heslo jednou za akci.
  - **Režim „Pracovat jako root“ na stránce Soubory** je na Linuxu v kroku 2 vypnutý. Jedno
    procházení by vyvolalo desítky dotazů na heslo. *(rozhodnutí návrháře)*
  - **Periodické sběry jako root** (nsenter a ss v síťovém přehledu, conntrack v mapě toků k8s)
    jsou na Linuxu vypnuté (`caps.root_sampling = false`). UI ukáže, že údaj není dostupný.
- Pozdější rozšíření (mimo rozsah): agent z kroku 3 jako rootová služba s polkit akcí (vzor
  `kiri host-access`) by tyto sběry povolil.

**Stránky na Linuxu:**

| Stránka | Na Linuxu |
|---|---|
| Kontejnery, Image, Volumy, Sítě, Compose | beze změny |
| Kubernetes | funguje (k3s je kontejner). Služby jsou rovnou na `localhost`. Bez mapy toků, pokud není root. |
| Soubory | lokální souborový systém |
| Paměť | `/proc/meminfo` hostitele a kontejnery. Bez VmmemWSL a uvolňování paměti WSL. |
| Disk | úložiště enginu proti souborovému systému. Bez VHDX. |
| Síťový přehled | naslouchající porty hostitele. Bez konfliktů s jinými distribucemi WSL. |
| WSL engine | jen stav, start a stop enginu. Bez distribucí, instalace, TLS a `daemon.json`. |
| Nastavení | bez `.wslconfig`, shimu, roury, portable režimu a WSLCD Lite |

**Systém:**
- Konzole: `$TERMINAL`, `x-terminal-emulator`, `gnome-terminal`, `konsole`, `xterm`, v tomto
  pořadí.
- Oznámení: `notify-send` (bez nové závislosti). Když chybí, oznámení se jen zapíšou do historie.
- Start s přihlášením: `~/.config/autostart/wsl-container-desktop.desktop`.
- Motiv: „podle systému“ přes eframe.
- Data: `$XDG_CONFIG_HOME/wsl-container-desktop`, historie a logy v `$XDG_DATA_HOME`.
- TLS a DPAPI se na Linuxu nepoužívají. Socket chrání práva souborového systému.
- Tray: `tray-icon` potřebuje na Linuxu GTK smyčku (riziko R2). Když nebude spolehlivý, Linux v
  tomto kroku tray nemá a zavření okna aplikaci ukončí.

**CI:** nová úloha na Ubuntu:
- `cargo build`, `cargo test` a kouřový test `LocalHost` proti Dockeru runneru (kontejner
  spustit, vypsat, zastavit, smazat);
- testy klienta přes unix socket s falešným HTTP serverem.

**Výstup:** binárka pro `x86_64-unknown-linux-gnu`, `.desktop` soubor a ikona v ZIPu releasu.

### Krok 3 – agent a bridge (Windows)

**Agent `wslcd-agent`:**
- Statický binár (`x86_64-unknown-linux-musl`, připravený i pro `aarch64-unknown-linux-musl`),
  cíl pod 1,5 MB. Jen std a libc.
- Jedno hlavní vlákno s `poll()`. Každý spuštěný proces a každé spojení má vlastní stream v
  multiplexu.

**Nasazení:**
- Release skript nejdřív sestaví agenta (`cargo zigbuild` nebo build ve WSL; volí návrhář
  implementace). Hlavní EXE ho vloží přes `include_bytes!`.
- Vývojový build bez agenta funguje dál, jen se zálohou na `wsl.exe`.
- V distribuci leží agent v `/usr/local/lib/wslcd/wslcd-agent-<sha256[..12]>`. Aplikace ho tam
  při prvním použití zapíše přes `wsl.exe -u root` ze stdin a ověří SHA-256.
- WSLCD Lite má agenta přímo ve vlastním rootfs (viz další oddíl).
- Nesoulad verze protokolu: agent se znovu nasadí.

### Krok 3 (fáze 3·0) – vlastní rootfs WSLCD Lite

Dnes aplikace stahuje oficiální Alpine minirootfs (`lite/mod.rs`) a za běhu ho připravuje skriptem
`PREPARE`: `apk add` nástrojů, `systemctl.sh`, `/etc/wsl.conf` a nastavení Podmanu. Nově bude
mít Lite **vlastní rootfs**, sestavený v repu a vydávaný s aplikací. Uživatel to chce. Tento
rootfs bude zároveň základem hosta VM na macOS.

**Sestavení:**
- Skript `lite/build-rootfs.sh` běží v kontejneru nebo ve WSL a výsledek je reprodukovatelný.
- Výchozí bod: Alpine minirootfs připnutý verzí a SHA-256, jako dnes.
- Dnešní obsah `PREPARE` se přesune do sestavení:
  - balíčky z `apk add`, připnuté na verze;
  - `systemctl.sh`, `/etc/wsl.conf` a `containers.conf.d`.
- Navíc obsahuje `wslcd-agent` a soubor `/etc/wslcd-lite-release` (verze rootfs, verze Alpine,
  SHA-256 agenta).
- Sestavuje se pro x86-64 a aarch64. aarch64 slouží pro Windows na ARM a pro macOS.

**Engine** *(rozhodnutí návrháře)*: rootfs engine **neobsahuje**. Docker nebo Podman se
instaluje jako dnes až po volbě uživatele. Rootfs tak zůstane malý (cíl pod 25 MB komprimovaný) a
volba enginu se nemění. Předpřipravené varianty s enginem jsou možné později.

**Distribuce** *(rozhodnutí návrháře)*:
- `wslcd-lite-rootfs-<verze>-<arch>.tar.gz` se přibalí do portable ZIPu vedle EXE. Lite tak
  funguje offline a nezávisí na tom, jestli jsou releasy veřejné.
- Aplikace má SHA-256 rootfs zakompilovaný a soubor hledá v tomto pořadí:
  1. vedle EXE;
  2. v `<data>\cache`.
- Stahování z Alpine CDN odpadá.

**Starší instalace:**
- Existující Lite distribuce (z oficiálního minirootfs) fungují dál. `refresh_shim` a nasazení
  agenta (výše) je dorovnají.
- Nová verze rootfs se do existující distribuce nepřenáší. Distribuce obsahuje data uživatele a
  rootfs slouží jen pro nové instalace.

**Testy:**
- CI rootfs sestaví a ověří obsah: balíčky, agent, `systemctl.sh`, velikost.
- Živý test `live_lite` naimportuje rootfs ze sestavení místo stahování.

### Krok 3 – běh agenta, protokol a bridge

**Spuštění:** jeden `wsl.exe -d <distro> -u root -e <agent> --stdio` na hostitele.
- Agent zároveň drží distribuci naživu a nahrazuje dnešní `keepalive` (`sleep infinity`).
- Když je `keep_alive` vypnuté *(rozhodnutí návrháře)*:
  - API aplikace zůstává na TCP. Fáze 3b a 3c se nepoužijí, protože průběžné dotazy na API by
    distribuci držely naživu.
  - Agent slouží jen pro `Exec` a skončí 30 s po posledním dokončeném `Exec`. Při dalším se znovu
    spustí, což stojí jedno spuštění `wsl.exe`.
  - Distribuce tak usíná stejně jako dnes.
- Uspání distribuce (*Put to sleep*) agenta nejdřív ukončí.

**Protokol** (přesný formát je na návrháři implementace, tohle musí splnit):
- Rámce nad libovolným proudem bajtů (stdio teď, unix socket na Linuxu, vsock na macOS).
  Například `délka | id streamu | typ | data`.
- Začíná `Hello` s verzí a schopnostmi. Udržování spojení přes `Ping`.
- **Spuštění procesu:** `Exec` (argumenty, uživatel, cwd, prostředí, stdin ano/ne). Pak `Stdin`,
  `StdinClose`, `Stdout`, `Stderr`, `Exit{code}` a `Signal{sig}` na skupinu procesů. Signál
  nahradí dnešní rušení přes `pkill -f` v `worker/build.rs`.
- **Spojení:** `Open{unix:<cesta> | tcp:127.0.0.1:<port>}`, `Data`, `HalfClose` a `Close`.
  Half-close se zachovává.
- **Řízení toku pro každý stream** (kredity, okno například 256 KiB). Velký přenos nesmí
  zablokovat ostatní streamy.
- Chyby jsou typované a mapují se na `HostError` a `ApiError`.

**Uživatel a prostředí:**
- Agent běží jako root. `Exec` s jiným uživatelem udělá `initgroups`, `setgid` a `setuid`.
- Výchozí uživatel je ten, kterého by použil `wsl.exe` (`[user] default` ve `/etc/wsl.conf`,
  jinak uid 1000).
- Prostředí (`HOME`, `USER`, `LOGNAME`, `SHELL`, `PATH`, `WSL_DISTRO_NAME`, interop proměnné)
  musí odpovídat `wsl.exe -e` pro stejného uživatele. Ověřuje se testem (R3).

**Co přes agenta vede:**

| Fáze | Co | Záloha |
|---|---|---|
| 3a | `WslHost::exec` a `exec_stream` | `wsl.exe` jako dnes: agent nenasazený, spadlý nebo vypnutý |
| 3b | Klient API aplikace přes `Endpoint::Agent` přímo na socket enginu v distribuci (bez TCP a TLS, kanál je soukromý). Relay pojmenované roury (`npipe.rs`) přes agenta (half-close jako v Kiri). | TCP + TLS jako dnes |
| 3c | Velké přenosy: `image load/save`, záloha a obnova volume, soubory v kontejneru, kontext buildu | přes agenta, jen když splní mantinel propustnosti. Jinak zůstanou na TCP. |

- TCP port enginu zůstává pro shim, `DOCKER_HOST` a nástroje.
- Nastavení: `agent: auto | off` (výchozí `auto`). `off` vrátí všechno na dnešní cesty.

**Odolnost:**
- Pád agenta: rozpracované požadavky dostanou chybu a agent se restartuje s rostoucí prodlevou
  (100 ms až 30 s).
- Do té doby jde `exec` přes `wsl.exe` a API přes TCP.
- Přehled „Na pozadí“ ukazuje příkazy stejně jako dnes.

**Bezpečnost:**
- Agent neposlouchá na žádném portu. Kanál je jen stdio procesu, který spustila aplikace.
- SHA-256 binárky se ověří před spuštěním.
- `docs/SECURITY.md` dostane oddíl o agentovi.

### Výkonové mantinely (akceptační kritéria)

`src/bench.rs` se rozšíří o měření níže. Každé se pustí před a po příslušném kroku na stejném
stroji:

| Metrika | Dnes | Požadavek |
|---|---|---|
| `Host::exec(["true"])` p50 | ~100–300 ms (`wsl.exe`) | krok 1: beze změny ±10 %; krok 3: ≤ 20 ms |
| shim `docker ps` p50 | 169 ms | beze změny ±10 % |
| API `GET /containers/json`, 4 klienti | 705 req/s (TCP+TLS) | přes agenta ≥ 90 % |
| propustnost `docker save` / záloha volume (1 GB) | změří se | přes agenta ≥ 80 % TCP, jinak 3c zůstane na TCP |
| `exec` p95 během běžícího 1GB přenosu | – | ≤ 100 ms |
| CPU aplikace v klidu (okno otevřené) | 0,07 % | nehorší |
| RSS agenta | – | ≤ 5 MB |

### Připravenost na macOS

Po kroku 3 musí platit:
- **Rozhraní `Host` a `Lifecycle`** nepoužívá pojmy WSL. Pro macOS půjde doplnit `AppleVmHost`:
  start a uspání VM, `to_guest_path` přes sdílené složky virtiofs.
- **Protokol agenta** je nezávislý na transportu, stačí proud bajtů. Agent se sestaví pro
  aarch64 musl.
- **Klient API** umí `Endpoint::Agent` a `Endpoint::Unix`.
- **Platformové služby** (`platform::*`) mají jasné místo pro implementaci pro macOS.
- **Rootfs WSLCD Lite** existuje i pro aarch64 a obsahuje agenta.

### macOS (krok 4, mimo tento návrh) – co převzít z Kiri

Uživatel nemá Mac, na kterém by mohl testovat. Proto má **virtualizace na macOS co nejvíc
vycházet z Kiri**. Kiri tuto vrstvu provozuje na Macu u skutečných uživatelů, takže převzatý kód
je ověřený, i když ho tu nikdo nespustí. Krok 4 dostane vlastní návrh. Tento oddíl stanoví
zásady, ze kterých vyjde.

**Co převzít:**

| Část | Z Kiri | Jak |
|---|---|---|
| Ovládání VM | crate `kiri-vmm` (`apple.rs`, `stop.rs`, `lib.rs`): Virtualization.framework přes `objc2-virtualization`, vsock, balloon, virtiofs, NAT, Rosetta, řízené zastavení | **Vendorovat beze změn** do `vendor/kiri-vmm`. Úpravy jen tam, kde to jinak nejde, a každá s komentářem. |
| Jádro hosta | `guest/kernel/build.sh`, `kiri.config`, `kiri-x86_64.config`, `patches/` | Převzít a přidat **vlastní fragment** s volbami, které potřebují dockerd a Podman (iptables/nftables backend, bridge, veth, overlayfs, cgroup v2, namespaces, seccomp). Kiri má vlastní runtime a některé z nich vypíná. |
| Zařazení do macOS | `kiri.entitlements` (`com.apple.security.virtualization`), sestavení `.app`, podpis a notarizace v `xtask release`, Homebrew cask | Převzít postup a přizpůsobit názvům aplikace |
| Chování VM | uspání po nečinnosti, balloon pro vracení paměti, obnova po pádu VM s rostoucí prodlevou, log konzole hosta s redakcí | Převzít logiku. Rozhodnutí o rozsahu přijdou v návrhu kroku 4. |

**Co vlastní:**
- `AppleVmHost` nad `kiri-vmm`;
- rootfs WSLCD Lite (aarch64) jako systém hosta, na ext4 disku pro data enginu;
- `wslcd-agent` jako služba hosta, spouštěná přes `systemctl.sh --boot`, s protokolem přes vsock
  místo stdio;
- Docker nebo Podman uvnitř místo runtime Kiri.

**Udržování shody s Kiri:**
- Ve vendorované složce bude `UPSTREAM.md` s commitem Kiri, ze kterého kód pochází, a seznamem
  vlastních úprav.
- Skript `scripts/sync-kiri.sh` stáhne novou verzi a ukáže rozdíl.
- Licence Apache 2.0: zachovat hlavičky, přidat `NOTICE` s uvedením Kiri a ověřit, jestli jejich
  `CLA.md` něco nemění pro převzetí, tedy ne pro přispívání.

**Testování bez Macu:**
- **CI na `macos-15`:** jen sestavení, jednotkové testy a podpis. Spustit VM tam nejde (Kiri to
  ve svém CI uvádí taky).
- **Host bez Macu:**
  - jádro s naším fragmentem a rootfs se spustí v QEMU na Linuxu (x86-64 s KVM, aarch64
    emulací);
  - v QEMU se otestuje: start, agent přes virtio-vsock, Docker/Podman, `docker run` a overlay;
  - stejná konfigurace jádra a stejná zařízení virtio pokrývají většinu toho, co uvidí VM na Macu.
- **Kolegové s Macem jako testeři:**
  - testovací build s diagnostickým balíčkem: log konzole hosta, verze, `kiri-vmm` události;
  - krátký kontrolní seznam: start, `docker run`, bind mount, port na `localhost`, uspání a
    probuzení, amd64 image přes Rosettu.
- Čím méně se vendorovaný kód liší od Kiri, tím víc z jejich provozu platí i pro nás. Je to
  hlavní důvod, proč ho neupravovat.

### Testy

- **Jednotkové (na každém OS):**
  - kódování a dekódování rámců;
  - řízení toku a half-close nad pamětovou rourou;
  - sémantika pomocných `run*` (spojení stdout a stderr, chybové kódy);
  - `Endpoint` a parsování `unix://` v kontextech.
- **Kontraktní testy `Host`:** jedna sada testů (stdout a stderr, kód, cwd, uživatel, stdin,
  streamování řádků s `\r`, zrušení) běží proti `LocalHost` v Linux CI a proti `WslHost` jako
  živý test (`--ignored`). V kroku 3 i proti `WslHost` přes agenta.
- **Agent v CI na Linuxu:** spustit lokálně přes stdio, vyzkoušet `Exec`, `Signal` a `Open` na
  testovací unix socket.
- **Shoda prostředí (živý):** výstup `env` a `id` přes `wsl.exe -e` a přes agenta pro výchozího
  uživatele i pro root se musí shodovat (kromě dočasných proměnných).
- **Regrese na Windows:** stávající jednotkové i živé testy procházejí po každém kroku.

## Rizika (ověřit spikem jako první krok příslušné fáze)

- **R1 – Transport klienta API.** ureq 2.12 neumí vlastní spojení (unix socket, proud agenta).
  Možnosti:
  - přechod na ureq 3 s vlastním transportem;
  - vlastní minimální HTTP/1.1 klient nad `Read + Write`. Základ už existuje v `open_stream` a
    `http_response`.

  Spike porovná rozsah změn a výkon (keep-alive, chunked, JSON) a vybere.
- **R2 – Tray na Linuxu.** `tray-icon` potřebuje GTK smyčku vedle eframe (winit). Pokud spike
  neukáže spolehlivé řešení na GNOME (Wayland) i KDE, Linux v kroku 2 tray nemá.
- **R3 – Shoda uživatele a prostředí agenta s `wsl.exe`** (PATH, skupiny jako `docker`, interop,
  `WSL_*` proměnné). Neshoda by rozbila compose, build nebo soubory. Proto test shody a záloha na
  `wsl.exe`.
- **R4 – Propustnost stdio `wsl.exe`.** Měřit hned na začátku kroku 3. Podle výsledku se rozhodne
  fáze 3c.
- **R5 – Nasazení agenta do cizí distribuce.** `/usr/local` může být jen pro čtení nebo
  `noexec`. Záložní umístění je `/var/lib/wslcd`. Když nic nejde, zůstane `wsl.exe`.
- **R6 – Agent drží distribuci naživu.** Ověřit, že `vmIdleTimeout`, uspání a
  `shutdown_wsl_on_quit` se chovají jako dnes.
- **R8 – Jádro Kiri a dockerd.** Kiri jádro skládá z `allnoconfig` pro vlastní runtime. Fragment
  pro dockerd a Podman nemusí stačit, nebo se může bít s jejich patchi. Ověří se v QEMU v CI na
  začátku kroku 4. Záloha je jádro Alpine `linux-virt`.
- **R7 – Šíře přepisu v kroku 1** (211 volání). Riziko snižuje mechanický přepis přes pomocné
  funkce se stejnou sémantikou, po souborech, s testy po každém souboru.

## Rozhodnutí návrháře (k odsouhlasení)

- **Linux:**
  - jen připojení k existujícímu lokálnímu enginu, bez instalace;
  - aktivní lokální Docker context se respektuje, vzdálené enginy ne;
  - periodické sběry jako root a režim root na stránce Soubory jsou vypnuté;
  - výslovné root akce jdou přes `pkexec`.
- **`Privilege` (User / Engine / Root)** místo jména uživatele. Krok 1 přeznačí 39 rootových volání.
- **Jiné distribuce** přes `for_distro()` bez agenta. Agent běží jen v primární.
- **Oznámení na Linuxu** přes `notify-send`, bez nové závislosti.
- **Agent:**
  - nahrazuje `keepalive`;
  - bez `keep_alive` slouží jen pro `Exec` (API zůstává na TCP) a končí 30 s po posledním;
  - je vložený do EXE (`include_bytes!`), v distribuci leží pod názvem s hashem;
  - nastavení `agent: auto | off`;
  - klient aplikace jde přes agenta přímo na socket enginu (bez TLS). TCP + TLS zůstává pro
    nástroje a shim.
- **Velké přenosy** přes agenta jen po splnění mantinelu propustnosti.
- **Rootfs WSLCD Lite:**
  - bez enginu (instaluje se jako dnes);
  - přibalený v portable ZIPu, bez stahování z Alpine CDN;
  - starší instalace se nemigrují.

## Na rozhodnutí implementace

- Uspořádání crate:
  - workspace s `crates/agent`, nebo další `[[bin]]`;
  - zda se agent a protokol dělí do vlastního crate, který použije i aplikace.
- Přesný formát rámců, velikost okna, názvy modulů a typů.
- Nástroj pro křížové sestavení agenta (`cargo zigbuild`, nebo build ve WSL v release skriptu).
- Podoba `Lifecycle` a `Caps` v detailu.
