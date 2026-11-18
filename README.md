# WLS Desktop

Nativní desktopová aplikace pro Windows (Rust + egui, jeden EXE bez webového UI)
pro správu **Docker Engine běžícího ve WSL distribuci**, bez Docker Desktopu
a bez Docker CLI na Windows.

## Funkce

| Stránka | Co umí |
|---|---|
| **Přehled** | stav enginu, počty kontejnerů, image a volume, CPU a RAM ve WSL, využití disku, úklid (prune), živé statistiky CPU, paměti, sítě a disku |
| **Kontejnery** | seskupení podle compose projektu, start, stop, restart, pozastavení, odstranění, logy (živě, filtr, kopírování), konzole do kontejneru, dialog „Spustit kontejner“ |
| **Image** | pull, odstranění, prune, spuštění kontejneru z image, přehled, které kontejnery image používají |
| **Volume** | vytvoření, odstranění, prune, kdo volume používá, otevření v Průzkumníku |
| **Sítě** | vytvoření (bridge, ipvlan, macvlan), odstranění, prune, subnety, připojené kontejnery |
| **Compose** | stacky rozpoznané podle labelů: up, stop, start, restart, pull, build, ps, logs, down; úprava compose souboru v Notepadu; spuštění stacku z libovolné složky (Windows i Linux cesta) |
| **Soubory** | prohlížeč souborového systému distribuce: procházení, záložky, nová složka nebo soubor, přejmenování a přesun, mazání, chmod, vestavěný textový editor (Ctrl+S), nahrávání z Windows a stahování do Windows, režim „Jako root“ |
| **WSL engine** | start, stop a restart Dockeru, uspání distribuce, pozastavení všech kontejnerů, nastavení TCP API, udržování distribuce naživu, seznam distribucí (start, ukončení, výchozí, „Použít pro Docker“), editor `/etc/docker/daemon.json` |
| **Nastavení** | distribuce, port API, interval obnovení, autostart enginu, globální `.wslconfig` (paměť, CPU, swap, `vmIdleTimeout`, `networkingMode`) |

**Oznamovací oblast (tray):** ikona se stavovou tečkou (zelená = engine běží) a tooltipem.
Levý klik okno zobrazí nebo schová, pravý klik otevře nabídku: stav, otevřít, start, stop
a restart Dockeru, uspání distribuce, pozastavení a obnovení kontejnerů, terminál, soubory
v Průzkumníku, přepínače „zavírat / minimalizovat do oznamovací oblasti“ a Ukončit.
Křížek i minimalizace ve výchozím stavu okno jen schovají (lze změnit v Nastavení).
Aplikace běží v jedné instanci, takže další spuštění jen zobrazí běžící okno.
Windows 11 dává ikony nových aplikací do skryté části oznamovací oblasti (šipka ^).
Trvale zobrazit ji lze v *Nastavení → Přizpůsobení → Hlavní panel → Ikony v oznamovací oblasti*.

**Engine Docker / Podman:** na stránce *WSL engine* se volí engine pro distribuci.
- **Nainstalovat a nastavit:** nainstaluje engine přes apt (Debian/Ubuntu). Když neběží systemd,
  zapne ho v `/etc/wsl.conf` a distribuci restartuje. Pak nastaví TCP API a ověří, že odpovídá.
  Průběh se ukáže v okně výstupu.
- **Odinstalovat:** vypne služby, odebere nastavení API a balíčky (apt purge). Volitelně smaže
  i data (`/var/lib/docker…` nebo `/var/lib/containers`).
- **Podman** běží jako rootful služba `wls-podman-api` (`podman system service tcp://127.0.0.1:<port>`).
  Aplikace s ním mluví stejným Docker-kompatibilním API. CLI příkazy (spuštění kontejneru, compose,
  konzole, logy, shim) se překládají na `podman --url tcp://127.0.0.1:<port> …`, takže vidí tytéž
  kontejnery. Krátká jména image se doplní na `docker.io/library/…`. Nastavení jednoho enginu
  druhý vypne, aby nebojovaly o port.

**Příkaz `docker` ve Windows (shim):** projekt obsahuje i malý `docker.exe` (`src/shim/docker.rs`).
Příkazy předává Dockeru ve WSL: `docker ps` ve Windows se provede jako
`wsl.exe -d <distribuce> --cd <aktuální složka> -e docker ps`. Distribuci bere z nastavení
WLS Desktop (nebo z proměnné `WLS_DISTRO`). Windows cesty v argumentech přeloží
(`-v D:\data:/data`, `--mount source=C:\x`, `-f C:\p\compose.yml`, `.\data`,
`\\wsl.localhost\Debian\…`) a vrací návratový kód příkazu. Pod jménem `docker-compose.exe`
se chová jako `docker compose`. Instaluje se v *Nastavení → Příkaz docker ve Windows*: zkopíruje se
do `%LOCALAPPDATA%\WLS_Desktop\bin` a tato složka se přidá do uživatelského PATH, bez admin práv.
`WLS_SHIM_DEBUG=1` vypíše spouštěný příkaz, `WLS_ENGINE=podman|docker` a `WLS_PORT` přepíšou nastavení.

Aby se shim choval co nejvíc jako Docker CLI:
- **Proměnné prostředí z Windows** se předávají přes `WSLENV`. Předá se, co příkaz potřebuje:
  `-e VAR` a `--env VAR` bez hodnoty, proměnné použité v compose souborech (`${VAR}`, `$VAR`)
  a navíc to, co vyjmenuješ v `WLS_FORWARD_ENV=A,B`. `DOCKER_HOST`, `PATH` apod. se nepředávají.
- **Cesty i v BuildKit volbách:** `--secret src=…`, `--output dest=…`, `--build-context x=…`,
  `--cache-from` a `--cache-to`, `--ssh`, `save -o …`. Hodnoty `-e`, `--label`, `--build-arg`
  a `--filter` se nepřekládají. Na síťové cesty `\\server\share`, které WSL nevidí, shim upozorní.
- **Compose s Podmanem:** instalace Podmanu přidá `docker-compose` jako provider.
  `podman compose` pak mluví se stejným API, takže stacky vidí i aplikace.
- **`DOCKER_HOST` pro nástroje** (VS Code Dev Containers, Testcontainers, SDK), které nevolají CLI,
  ale mluví přímo s API: na stránce *WSL engine → Instalace a nastavení* lze nastavit uživatelskou
  proměnnou `DOCKER_HOST=tcp://127.0.0.1:<port>`.

Zbývající rozdíly oproti Docker CLI pro Windows: `docker login`, `config.json` a contexts žijí ve WSL.
Každé volání má režii spuštění `wsl.exe` (~0,1–0,3 s). U Podmanu chybí buildx, swarm a contexts.

Konzole a logy v terminálu se otevírají ve **Windows Terminalu**
(`wt.exe`). Pokud není k dispozici, použije se klasické okno konzole.

## Jak to funguje

- **Docker API přes TCP:** `dockerd` poslouchá kromě socketu i na `tcp://127.0.0.1:2375`.
  Zajišťuje to systemd override
  `/etc/systemd/system/docker.service.d/10-wls-desktop-tcp.conf`, který vytvoří tlačítko
  **Nastavit TCP API**. WSL port automaticky přeposílá na `localhost` ve Windows.
  API je bez autentizace a dostupné jen lokálně.
- **WSL operace** (start a stop distribuce, `systemctl`, `docker compose`, souborové
  operace) běží přes `wsl.exe -d <distribuce> [-u root] -e …`.
- **Výměna distribuce:** v Nastavení (nebo na stránce WSL engine tlačítkem *Použít pro Docker*)
  vyber jinou distribuci a jednou v ní klikni na **Nastavit TCP API**. Požadavky na distribuci:
  systemd (`[boot] systemd=true` v `/etc/wsl.conf`) a nainstalovaný Docker Engine.
- **Udržování naživu:** WSL po chvíli nečinnosti distribuci zastaví i s Dockerem.
  Aplikace proto spouští skrytý `wsl.exe … flock … sleep infinity`, který v distribuci
  běží vždy nejvýš jednou a po zavření aplikace zůstane běžet, takže Docker nespadne.
  **Uspat** ho ukončí a distribuci vypne.

Konfigurace aplikace se ukládá do `%APPDATA%\WLS_Desktop\config.json`.

## Sestavení

Potřeba: Rust (MSVC toolchain) a Visual Studio Build Tools s C++.

```bash
cargo build --release
```

Výsledek: `target\release\wls-desktop.exe` (cca 6 MB, bez závislostí).

## Testy

```bash
cargo test
```

Integrační testy proti skutečnému WSL a Dockeru (distribuce z `WLS_DISTRO`, výchozí `Ubuntu`):

```bash
cargo test -- --ignored --test-threads=1
```

Pro ladění lze zvolit úvodní stránku proměnnou `WLS_START_PAGE`
(`containers`, `images`, `volumes`, `networks`, `compose`, `files`, `engine`, `settings`)
a `WLS_DEBUG_DIALOG=1` zobrazí po startu ukázkový potvrzovací dialog.

## UI konvence

- Ikony jsou z fontu [Phosphor](https://phosphoricons.com/) (`egui-phosphor`) a píšou se přímo
  do textu jako `"\u{E4A6} Smazat"`.
- Tabulky: `ui::table()` / `ui::table_in()` se sloupci `W::Px` (pevná šířka), `W::Fr` (podíl
  ze zbytku) a `W::PxOpt` / `W::FrOpt` (méně důležité sloupce, které se v úzkém okně skryjí).
  Sloupec s ikonami má šířku `icons_w(n)`.
- Lišty `ui::toolbar()`, vstupy `ui::field()` / `ui::area()`, dialogy `ui::modal()` + `ui::dialog_buttons()`.

## Struktura

```
src/
  main.rs        vstupní bod, okno
  app.rs         stav aplikace, navigace, dialogy, toasty
  worker.rs      vlákno na pozadí: obnovování a provádění akcí
  docker.rs      klient Docker Engine API (ureq)
  wsl.rs         wsl.exe: distribuce, příkazy, terminál, TCP setup skript
  fs.rs          souborové operace v distribuci
  wslconfig.rs   úprava %USERPROFILE%\.wslconfig
  theme.rs       barvy, styl
  icon_art.rs    kresba ikony (sdílí ji i build.rs pro .ico v EXE)
  tray.rs        ikona v oznamovací oblasti, nabídka, skrývání okna, jedna instance
build.rs         vygeneruje .ico a vloží ho do EXE (winresource, potřebuje rc.exe z Windows SDK)
  ui/            jednotlivé stránky
```

Rozložení UI vychází z projektu *WSL Container Desktop* (GPLv3). Převzatý je jen vzhled,
kód je napsaný nově.
