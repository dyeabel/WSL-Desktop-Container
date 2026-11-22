# `wslcd` / `docker.exe` shim – reference pro testy z příkazové řádky

Tento dokument je určený hlavně pro jinou AI (nebo skript), která má WSL Container Desktop testovat
z příkazové řádky. Popisuje, co umí shim `docker.exe` a jeho dvojče `wslcd.exe`, jaké mají výstupy,
návratové kódy a co je při testování potřeba hlídat.

## 1. Co je co

| Soubor | Chování |
|---|---|
| `docker.exe` | Docker CLI pro Windows: předává příkazy enginu (Docker nebo Podman) v primární WSL distribuci. Respektuje `DOCKER_CONTEXT` / `DOCKER_HOST`. |
| `docker-compose.exe` | Totéž, ale chová se jako `docker compose …`. |
| `wslcd.exe` | Stejná binárka jako `docker.exe` (rozlišuje se podle jména souboru). Míří **vždy** na engine aplikace (ignoruje `DOCKER_CONTEXT` a `DOCKER_HOST`) a **navíc** má příkazy pro ovládání aplikace: `status`, `distro`, `engine`, `memory`, `metrics`, `help-wslcd`. |
| `wsl-container-desktop.exe` | GUI aplikace (tray). Drží řídicí rouru, přes kterou `wslcd` provádí akce. |

Portable instalace (vývojový stroj): `C:\WSLContainerDesktop\` – všechny čtyři EXE, data v `C:\WSLContainerDesktop\data\`
(`config.json` = nastavení včetně primární distribuce). Složka je v PATH, takže stačí volat `wslcd …`.

Příkazy navíc jsou **jen ve `wslcd.exe`**; `docker.exe status` skončí chybou Dockeru „unknown command“.

## 2. Docker příkazy (`docker …` i `wslcd …`)

`wslcd <args>` = `docker <args>` = `wsl.exe -d <primární distribuce> --cd <aktuální složka> -e docker <args>`
(u Podmanu `podman --url tcp://127.0.0.1:<port> …`). Návratový kód je kód příkazu ve WSL.

- **Rychlá cesta (~50 ms místo ~250 ms):** `ps`/`container ls`, `images`, `volume ls`, `network ls`, `inspect`,
  `logs` bez `-f`, `version --format`, `info --format` odpovídá shim přímo z API enginu. Cokoli jiného jde přes `wsl.exe`.
  Vypnutí: `WSLCD_SHIM_FAST=0`.
- **Překlad cest:** Windows cesty v argumentech (`-v D:\data:/data`, `-f C:\p\compose.yml`, `.\data`) → `/mnt/d/…`.
- **Proměnné prostředí:** do WSL jde jen to, co příkaz potřebuje (`-e VAR`, `${VAR}` v compose, `WSLCD_FORWARD_ENV=A,B`).
- **Kontexty:** `wslcd context ls|show|use|create|rm|inspect` – kontext = distribuce + engine + port + TLS
  (uložené v `%APPDATA%\WSL_Container_Desktop\contexts.json` resp. v datové složce). `--context <jméno>` funguje i u `wslcd`.
- **Auto-start:** když aplikace nebo engine neběží, první příkaz, který engine potřebuje, spustí aplikaci schovanou v tray
  a počká na API (výchozí max. 120 s, `WSLCD_SHIM_WAIT=<s>`). Vypnutí: `WSLCD_SHIM_AUTOSTART=0`.

### Proměnné pro ladění a skripty

| Proměnná | Význam |
|---|---|
| `WSLCD_SHIM_DEBUG=1` | na stderr vypíše kontext, zvolenou cestu (fast path / wsl.exe) a u `wslcd` příkazů i JSON požadavek |
| `WSLCD_SHIM_FAST=0` | vypne rychlou cestu |
| `WSLCD_SHIM_AUTOSTART=0` | nespouštět aplikaci / engine automaticky |
| `WSLCD_SHIM_WAIT=<s>` | jak dlouho čekat na API (docker příkazy, výchozí 120) a na řídicí rouru (`wslcd` příkazy, výchozí 60) |
| `WSLCD_DISTRO`, `WSLCD_ENGINE=docker\|podman`, `WSLCD_PORT` | přepis cíle jen pro docker příkazy (ne pro `wslcd distro …`) |

## 3. Příkazy navíc ve `wslcd.exe`

Všechny jdou přes řídicí rouru běžící aplikace `\\.\pipe\wsl-container-desktop-control-<USERNAME>` (jen pro aktuálního
uživatele). Když aplikace neběží, `wslcd` ji spustí v tray (na stderr napíše `wslcd: starting WSL Container Desktop…`)
a počká na rouru. Akce běží stejnou cestou jako tlačítka v GUI – v okně aplikace se ukáže oznámení a záznam v přehledu
„Na pozadí“.

```
wslcd status [--json]
wslcd distro ls [--json]
wslcd distro use <name> [--engine docker|podman] [--sleep-old|--keep-old] [-v] [--json]
wslcd distro start <name>
wslcd distro stop <name>            (alias: distro terminate)
wslcd engine start|stop|restart
wslcd memory free
wslcd keys clean [--min-age <hodiny>]
wslcd metrics [--interval <ms>] [--count <n>] [--no-containers] [--json]
wslcd help-wslcd                    (nebo -h / --help u kteréhokoli z příkazů výše)
```

### Návratové kódy

| Kód | Význam |
|---|---|
| 0 | OK |
| 1 | akce selhala (aplikace vrátila `ok: false`, chyba je ve `message`, u `distro use` i log kroků) |
| 2 | chybné použití (neznámý podpříkaz / přepínač, chybějící jméno distribuce, `--interval` mimo 100–60000) |
| 3 | aplikace není dostupná (nejde spustit, neotevřela rouru včas, běží starší verze bez řídicí roury) |

### `--json`

Vypíše surovou odpověď aplikace (u `metrics --count ≠ 1` jeden řádek JSON na vzorek – JSON Lines):

```json
{ "ok": true, "message": "text pro člověka", "log": "průběh (jen u distro use)", "data": { ... } }
```

`log` a `data` chybí, když jsou prázdné. Texty `message` a `log` jsou v jazyce aplikace (čeština / angličtina podle
nastavení), proto testy mají kontrolovat `ok`, návratový kód a pole v `data`, ne přesné znění textu.
Chyby samotného `wslcd` (kód 2 a 3) jsou vždy anglicky.

### `wslcd status`

Bez vedlejších efektů (distribuci neprobouzí). `data`:

| Pole | Typ | Popis |
|---|---|---|
| `app_version` | string | verze aplikace |
| `app_pid` | number | PID běžící aplikace |
| `primary_distro` | string | primární distribuce (kam míří API a docker příkazy) |
| `distro_running` | bool / null | běží primární distribuce (null = není v `wsl -l -v`) |
| `engine` | `"Docker"` / `"Podman"` | engine primární distribuce |
| `port`, `tls` | number, bool | API `tcp://127.0.0.1:<port>`, s TLS (klientský certifikát) |
| `api_connected` | bool | API enginu odpovídá na `/_ping` |
| `keep_alive` | bool | aplikace drží distribuci naživu |
| `keep_previous_primary` | bool | výchozí chování `distro use` (false = předchozí primární se uspí) |
| `busy_actions` | number | počet právě běžících akcí aplikace (po startu chvíli 1 = start enginu) |

### `wslcd distro ls`

Bez vedlejších efektů. `data` = pole `{ name, state ("Running"/"Stopped"/…), version ("1"/"2"), wsl_default (bool),
primary (bool), engine ("Docker"/"Podman"/null – zapamatovaný engine distribuce) }`. Lidský výstup:

```
NAME                   STATE     WSL   PRIMARY   ENGINE
WSLCD-Lite (default)   Stopped   2               -
WSLCD-Lite-Docker      Running   2     *         Docker
```

### `wslcd distro use <name>` – přepnutí primární distribuce

Stejné jako „Nastavit jako primární“ v GUI (`set_primary` ve workeru):

1. zastaví Kubernetes laboratoř předchozího enginu (drží porty),
2. zjistí enginy v cílové distribuci (`--engine` vybere, jinak zapamatovaný / jediný nainstalovaný),
3. v ostatních distribucích vypne TCP API (engine a kontejnery tam běží dál),
4. podle `--sleep-old` / `--keep-old` (výchozí = nastavení aplikace) uspí předchozí primární (`wsl --terminate`),
5. uloží konfiguraci (`distro`, `engine`, `distro_engines`, `keep_previous_primary`),
6. nastaví API v nové distribuci na stejném portu (TLS podle nastavení) a počká na něj.

Trvá typicky 10–60 s. Jméno distribuce se porovnává bez ohledu na velikost písmen (musí existovat v `wsl -l -v`).
Úspěch: `ok: true`, `message` např. „<distro> je primární distribuce (Docker) – API na tcp://127.0.0.1:2376“, `data` =
`{ primary_distro, engine, port, tls }` po přepnutí, `log` = jednotlivé kroky (`-v` ho vypíše i v lidském režimu).
Distribuce bez enginu: přepne se, ale `message` vyzve k instalaci enginu (API nebude).

Ověření po přepnutí: `wslcd status --json` → `primary_distro`, `api_connected: true`; `wslcd version --format '{{.Server.Version}}'`.

> **Pozor – mění skutečné prostředí.** Primární distribuce je uložená v `C:\WSLContainerDesktop\data\config.json`.
> Test si má původní hodnotu zapamatovat (`wslcd status --json` → `primary_distro`, `engine`) a na konci ji vrátit
> (`wslcd distro use <původní> --engine <původní> --keep-old`). `--sleep-old` zastaví kontejnery v předchozí distribuci.

### `wslcd distro start <name>` / `wslcd distro stop <name>`

Spustí (`wsl -d`, u primární s udržováním naživu) / ukončí (`wsl --terminate`) distribuci. Ukončení primární zastaví
i udržovací proces aplikace – engine pak neodpovídá, dokud se distribuce znovu nespustí (`wslcd distro start` nebo
`wslcd engine start`).

### `wslcd engine start|stop|restart`

`systemctl start|stop|restart` služeb enginu v primární distribuci; `start` a `restart` čekají na API (max. 20 s).
`message` např. „Docker běží“, „Docker zastaven“, „Docker restartován“ (anglicky „Docker is running“ …).

### `wslcd memory free`

Zahodí cache Linuxu a zhutní paměť VM, aby ji VmmemWSL vrátil Windows. `message`:
„Paměť WSL uvolněna: VmmemWSL 720 MB → 644 MB (cache Linuxu 287 MB → 181 MB)“.

### `wslcd keys clean` – kopie klientského klíče ve Windows

Nástroje, které čtou `DOCKER_CERT_PATH` (Testcontainers / Docker.DotNet na Windows), při každém spuštění naimportují
`key.pem` do úložiště klíčů Windows (`%APPDATA%\Microsoft\Crypto\Keys`) a po sobě ho nesmažou. Aplikace si u každé
distribuce pamatuje veřejný klíč jejího klientského certifikátu (`data\tls\client-keys.json`, zapisuje se při stažení
certifikátů) a maže jen kopie **těchto** klíčů – cizích klíčů se nedotkne:

- distribuce existuje → zůstane nejnovější kopie, starší se smažou;
- distribuce už neexistuje → smažou se všechny kopie;
- kopie mladší než `--min-age` hodin (výchozí 6) zůstanou vždy – může je mít otevřené běžící test.
  `--min-age 0` smaže všechny kopie kromě nejnovější (jen když zrovna neběží žádné testy).

Totéž dělá aplikace sama: minutu po startu, pak každých 6 hodin a po každém stažení certifikátů (přepnutí distribuce,
nastavení API). `data`: `{"deleted": 12, "kept": 1, "errors": []}`; `ok` je `false`, když některý klíč nešel smazat.

### `wslcd metrics` – čítače paměti a CPU

Dva vzorky s odstupem `--interval` ms (výchozí 1000); CPU je průměr za skutečně uplynulou dobu (`interval_ms` ve výsledku,
může být delší než požadovaná, protože první vzorek přes `wsl.exe` trvá ~100–500 ms). Distribuci **neprobouzí** –
když neběží, má `wsl.running: false` a chybí paměť / CPU distribuce i kontejnery.

`data`:

| Pole | Popis |
|---|---|
| `time` | Unix čas vzorku (s) |
| `interval_ms` | skutečná délka měření CPU |
| `app.pid`, `app.cpu_pct` | proces aplikace; CPU v % jednoho jádra (100 = jedno plné jádro) |
| `app.cpu_time_ms` | kumulovaný čas CPU aplikace |
| `app.working_set`, `app.private_bytes` | paměť aplikace (B) |
| `windows.ram_total`, `windows.ram_used` | fyzická RAM Windows (B), obsazeno všemi procesy včetně VmmemWSL |
| `windows.logical_cpus` | počet logických CPU Windows |
| `wsl.distro`, `wsl.running` | primární distribuce a zda běží |
| `wsl.vmmem` | paměť procesu VmmemWSL (B; null, když nejde zjistit) |
| `wsl.mem_total`, `wsl.mem_used`, `wsl.mem_cache` | `/proc/meminfo` VM (B) |
| `wsl.engine_rss` | RSS procesů enginu (dockerd/containerd/podman…) (B) |
| `wsl.cpu_pct` | vytížení celé VM z `/proc/stat` (100 = všechna jádra VM) |
| `engine.name`, `engine.connected` | engine a dostupnost API |
| `engine.containers_running`, `engine.containers_total`, `engine.ncpu` | z `/info` enginu |
| `containers[]` | běžící kontejnery: `id` (12 znaků), `name`, `cpu_pct` (100 = jedno jádro, jako `docker stats`), `mem`, `mem_limit`, `net_rx`, `net_tx`, `blk_read`, `blk_write` (B) |

Lidský výstup:

```
App (PID 44888)            CPU 1.1 %   working set 88.1 MiB   private 90.5 MiB
Windows                    RAM 24.3 GiB / 30.7 GiB used   16 logical CPUs
WSL (WSLCD-Lite-Docker)    CPU 12.9 %   used 451.7 MiB   cache 265.3 MiB   total 7.8 GiB   VmmemWSL 720.1 MiB   engine RSS 199.1 MiB
Engine                     Docker connected   containers 1/1 running

CONTAINER      NAME                 CPU %     MEM       LIMIT     NET RX / TX     BLOCK R / W
305826cdccd7   wslcd-test-metrics   100.1 %   1.4 MiB   7.8 GiB   806 B / 126 B   0 B / 0 B
```

Průběžné sledování: `wslcd metrics --count 0 --interval 2000 --json > metrics.jsonl` (Ctrl+C ukončí).
`--no-containers` přeskočí statistiky kontejnerů (rychlejší; u Dockeru trvá dotaz na kontejner ~1–2 s, běží souběžně).

## 4. Doporučené testovací scénáře

Konvence: testovací kontejnery, sítě a volume pojmenovávat `wslcd-test-*` a na konci smazat.

1. **Dostupnost:** `wslcd status --json` → `ok`, `api_connected: true`; `wslcd version` → kód 0.
2. **Seznam distribucí:** `wslcd distro ls --json` → přesně jedna položka má `primary: true` a odpovídá `status.primary_distro`.
3. **Chybové cesty:** `wslcd distro use NoSuchDistro` → kód 1 (`ok: false`); `wslcd distro bogus` → kód 2;
   `wslcd metrics --interval 10` → kód 2; `docker.exe status` → chyba Dockeru (příkazy navíc jen ve `wslcd`).
4. **Engine:** `wslcd engine restart` → kód 0, pak `wslcd ps` → kód 0; `wslcd engine stop` → `wslcd status --json`
   má `api_connected: false`; `wslcd engine start` → zase `true`.
5. **Čítače CPU:** `wslcd run -d --name wslcd-test-cpu alpine:3.20 sh -c 'while :; do :; done'`, pak
   `wslcd metrics --json` → kontejner `wslcd-test-cpu` má `cpu_pct` ≈ 100, `wsl.cpu_pct` ≈ 100 / počet jader VM;
   úklid `wslcd rm -f wslcd-test-cpu`.
6. **Paměť:** `wslcd metrics --json --no-containers` (zapamatovat `wsl.mem_cache`, `wsl.vmmem`) → `wslcd memory free` →
   znovu `metrics`: `mem_cache` by měla klesnout.
7. **Výkon aplikace:** `wslcd metrics --count 30 --interval 1000 --no-containers --json` při nečinnosti → `app.cpu_pct`
   by měl být nízký (jednotky %), `app.working_set` stabilní (bez trvalého růstu).
8. **Přepnutí distribuce (mění prostředí, viz varování výše):**
   uložit `status` → `wslcd distro use <jiná> --keep-old -v` → `status.primary_distro` = jiná, `api_connected: true`,
   `wslcd ps` funguje → `wslcd distro use <původní> --engine <původní> --keep-old` → stav jako na začátku.
9. **Auto-start:** při ukončené aplikaci `wslcd status` → na stderr „starting WSL Container Desktop…“, kód 0, nový `app_pid`.
   (Aplikaci nespouštět podruhé ručně – druhá instance jen aktivuje okno první.)

## 5. Implementace (pro orientaci v kódu)

- `src/shim/docker.rs` – vstup shimu, rozlišení `docker` / `docker-compose` / `wslcd`, rychlá cesta, předání do `wsl.exe`.
- `src/shim/wslcmd.rs` – parsování a výpis příkazů navíc (`wslcd`), klient řídicí roury.
- `src/worker/control.rs` – server řídicí roury v aplikaci, protokol (`Request` / `Response`), sběr čítačů.
- `src/wake.rs` – název řídicí roury (`control_pipe_name`), spuštění / probuzení aplikace.
- `src/npipe.rs` – server pojmenované roury (DACL jen pro uživatele, ochrana proti squattingu).

Protokol roury: klient pošle jeden řádek JSON, např. `{"cmd":"distro-use","distro":"Ubuntu","engine":"docker","sleep_old":false}`
(`cmd`: `status`, `distro-ls`, `distro-use`, `distro-start`, `distro-stop`, `engine` + `action`, `memory-free`,
`metrics` + `interval_ms`, `containers`), server odpoví jedním řádkem JSON (`ok`, `message`, `log`, `data`) a zavře spojení.
