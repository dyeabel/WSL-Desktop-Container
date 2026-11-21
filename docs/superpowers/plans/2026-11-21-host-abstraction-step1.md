# Abstrakce hostitele, krok 1 – plán implementace

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Všechna volání „spusť něco v Linuxu“ půjdou přes trait `Host` (zatím jediná
implementace `WslHost`). Klient API enginu přestane záviset na ureq a TCP a bude stát na vlastním
minimálním HTTP/1.1 klientu nad obecným spojením. Na Windows se chování nezmění.

**Architecture:**
- Nový modul `src/host/`: typy `Privilege`, `ExecReq`, `ExecOut`, trait `Host` a pomocné `run*`
  se stejnou sémantikou jako dnešní `wsl::run*`. `WslHost` deleguje na `wsl.rs`.
- Nový modul `src/http/`: trait `Connect` (vrací proud), `TcpConnect` (s volitelným rustls),
  `Client` s keep-alive poolem a `Endpoint`.
- `docker.rs` přejde z ureq na `http::Client`.
- Kroky 2 (Linux) a 3 (agent) dostanou vlastní plány. Doplní `Endpoint::Unix` a `Agent`,
  `LocalHost`, `Caps` a `host_path`.

**Tech Stack:** Rust 2021, std vlákna (bez async), rustls 0.23 (už je), serde_json. Žádné nové
závislosti.

**Spec:** `docs/superpowers/specs/2026-11-21-host-abstraction-design.md` (oddíly *Krok 1a*,
*Krok 1b*, *Výkonové mantinely*, riziko R1, R7).

## Odchylky od specu (rozhodnuto v plánu)

- **R1 je rozhodnuté: vlastní klient, ne ureq 3.**
  - Vlastní transport je v ureq 3 jen v modulu `unversioned`. Přechod by stejně přepsal celý
    klient i deset míst s `request()`.
  - Základ vlastního klienta už existuje: `http_response` a `Chunked` v `docker.rs` a rustls
    `StreamOwned` v `open_stream`.
  - ureq zůstává v `Cargo.toml` pro stahování v `lite/mod.rs` a pro `k8s/live_k8s.rs`.
- **`Endpoint::Unix` přichází v plánu kroku 2.** `std::os::unix::net` se na Windows nesestaví a
  bez Linux CI by nešel otestovat. Krok 1 dodá obecný `Connect`, na který se Unix jen připojí.
- **Jen to, co krok 1 potřebuje.** `ExecReq` zatím nemá `env`. `Host` zatím nemá `caps`,
  `lifecycle`, `kind` ani `host_path`. Přidají je plány kroků 2 a 3.

## Global Constraints

- Na Windows se nesmí změnit žádné viditelné chování: texty chyb, přehled „Na pozadí“,
  argumenty `wsl.exe`, rušení buildu.
- `Privilege::User` = bez `-u`. `Privilege::Engine` a `Privilege::Root` = `-u root`.
- Komentáře a hlášky v kódu česky, jako zbytek kódu. Uživatelské texty přes `t!` / `tf!` (cs + en).
- Bez async a bez nových crate. Vlákna ve stylu `npipe.rs`.
- Časové limity klienta: připojení 800 ms, čtení 600 s, `ping` 1500 ms, otevření streamu 2 s.
- Výkon (spec, *Výkonové mantinely*):
  - `exec(["true"])` p50 a shim `docker ps` p50 beze změny ±10 %;
  - API `GET /containers/json` se 4 klienty přes TLS ≥ 635 req/s (90 % z 705);
  - CPU aplikace v klidu nehorší než 0,07 %.
- Živé testy a `scripts/bench.ps1` pracují se skutečnými distribucemi a bench přepíná primární
  distribuci. Před spuštěním zkontrolovat primární distribuci v
  `C:\WSLContainerDesktop\data\config.json` a po skončení vrátit sdílené iptables.
- Živé testy (`live_tests.rs`, `live_docker.rs`, `lite/live_lite.rs`) a `bench.rs` smí dál volat
  `wsl::` napřímo.

## Review Focus

1. **Chybová hláška samotného `wsl.exe` v UTF-16LE** (např. neexistující distribuce) musí v
   `HostError` zůstat čitelná, jako dnes přes `wsl::decode`. Test v Task 1.
2. **Přepnutí primární distribuce:** další příkaz musí jít do nové distribuce, ne do
   zapamatovaného `WslHost`. Test v Task 2.
3. **Restart enginu, zatímco v poolu leží nečinné keep-alive spojení:** další `GET` musí projít.
   Jedno opakování na novém spojení, jen u GET a HEAD. `POST` se nesmí poslat dvakrát. Test v Task 5.
4. **Nekonečné a velké odpovědi** (`logs -f`, `events`, `image save`, soubory v kontejneru) se
   nesmí načíst celé do paměti ani vrátit do poolu uprostřed. Zrušení přes `StreamCloser` musí
   odblokovat čtení. Testy v Task 5 a 6.
5. **Zrušený build** musí ukončit proces a `exec_stream` se musí vrátit, nesmí viset. Živý test v
   Task 2.

---

### Task 0: Výchozí měření

**Files:**
- Modify: `src/bench.rs` (nová metrika „exec true“)

**Interfaces:**
- Produces: metrika `exec_true` (p50 a p95 ms) ve výstupu `live_bench` a v
  `docs/bench/results.json`

- [ ] **Step 1:** Do `live_bench` přidat měření: 30× `wsl::run(distro, None, None, &["true"])`,
  zaznamenat p50 a p95 (stejná struktura `Stats` jako ostatní metriky). Vypsat a zapsat jako
  `exec_true`.
- [ ] **Step 2:** Zkontrolovat primární distribuci (Global Constraints) a spustit
  `scripts/bench.ps1`. Očekávání: report v `docs/bench/` s `exec_true`, shim `docker ps` a req/s
  pro 4 klienty.
- [ ] **Step 3:** Commit `bench: měření exec true jako výchozí bod pro abstrakci hostitele`
  (bench.rs i vygenerovaný report).

### Task 1: Typy hostitele a pomocné funkce

**Files:**
- Create: `src/host/mod.rs`
- Modify: `src/error.rs` (přejmenování `WslError` → `HostError`, alias)
- Modify: `src/wsl.rs` (`finish` a `decode` se přesunou, `wsl.rs` je re-exportuje)
- Create: `src/http/mod.rs` (zatím jen `Endpoint`)
- Modify: `src/main.rs` (`mod host;`, `mod http;`)

**Interfaces:**
- Produces (vše v `crate::host`):
  - `#[derive(Clone, Copy, Debug, PartialEq, Eq)] pub enum Privilege { User, Engine, Root }`
  - `pub struct ExecReq<'a> { pub args: &'a [&'a str], pub privilege: Privilege, pub cwd: Option<&'a str>, pub stdin: Option<&'a [u8]> }`
    se stavitelem `ExecReq::new(args)`, `.privilege(p)`, `.cwd(Option<&str>)` a `.stdin(&[u8])`.
    Výchozí hodnoty: `User`, bez cwd, bez stdin (= `Stdio::null()`).
  - `pub struct ExecOut { pub code: i32, pub stdout: Vec<u8>, pub stderr: Vec<u8> }`
  - `pub trait Host: Send + Sync`:
    - `fn label(&self) -> String;`
    - `fn exec(&self, req: &ExecReq) -> Result<ExecOut, HostError>;` Vrací `Err` jen když se
      proces nespustil. Nenulový kód je `Ok`.
    - `fn exec_stream(&self, req: &ExecReq, cancel: Option<&AtomicBool>, on_line: &mut dyn FnMut(&str)) -> Result<i32, HostError>;`
    - `fn open_terminal(&self, title: &str, req: &ExecReq) -> Result<(), HostError>;`
    - `fn to_guest_path(&self, path: &str) -> Result<String, HostError>;`
    - `fn engine_endpoint(&self, cfg: &Config) -> crate::http::Endpoint;`
    - `fn wsl(&self) -> Option<&crate::host::wsl::WslHost> { None }` přidá až Task 2, spolu s
      `WslHost`.
  - `pub trait HostExt: Host` s implementací pro všechna `T: Host + ?Sized`:
    - `fn run(&self, p: Privilege, cwd: Option<&str>, args: &[&str]) -> Result<String, HostError>;`
    - `fn run_owned(&self, p: Privilege, cwd: Option<&str>, args: &[String]) -> Result<String, HostError>;`
    - `fn run_stdin(&self, p: Privilege, args: &[&str], input: &str) -> Result<String, HostError>;`
  - `pub fn finish(out: ExecOut) -> Result<String, HostError>` (dnešní `wsl::finish` beze změny
    sémantiky)
  - `pub fn decode(bytes: &[u8]) -> String` (dnešní `wsl::decode`)
  - `pub(crate) fn track(label: &str, p: Privilege, args: &[&str]) -> activity::Guard`. Titulek je
    `label`, nebo `"{label} (root)"`, když `p != User`. Je to tentýž text jako dnes.
- `crate::error::HostError` = dnešní `WslError` (varianty `Spawn`, `Failed { code, output }`).
  `pub type WslError = HostError;` zůstane jako alias, aby zbytek kódu šel sestavit.
- `Endpoint` zatím jako `pub enum Endpoint { Tcp { port: u16 } }` v `src/http/mod.rs`. Zbytek
  modulu dodá Task 5.

- [ ] **Step 1: Napsat testy** v `src/host/mod.rs` (`#[cfg(test)]`, s `FakeHost`, který vrací
  připravené `ExecOut`):

```rust
#[test] fn run_ok_appends_stderr() {
    // code 0, stdout "a", stderr "w" -> Ok("a\nw")
}
#[test] fn run_err_prefers_stderr() {
    // code 2, stdout "x", stderr "boom" -> Err(Failed{code:2, output:"boom"})
}
#[test] fn run_err_falls_back_to_stdout() {
    // code 1, stdout "only out", stderr "" -> Err(Failed{code:1, output:"only out"})
}
#[test] fn utf16_stderr_is_readable() {
    // stderr = UTF-16LE "Neexistuje" s BOM, code 1 -> output == "Neexistuje"
}
#[test] fn run_stdin_passes_input() {
    // FakeHost zaznamená req.stdin -> Some(b"data")
}
```

- [ ] **Step 2:** `cargo test host::` → FAIL (modul neexistuje).
- [ ] **Step 3:** Implementovat typy, `finish`, `decode`, `track` a `HostExt`. `wsl.rs` si
  `finish` a `decode` vezme z `host` (`pub use crate::host::decode;`). Přejmenovat chybu v
  `error.rs` a přidat alias.
- [ ] **Step 4:** `cargo test` → všechny testy PASS (nové i stávající).
- [ ] **Step 5:** Commit `host: typy Privilege/ExecReq/ExecOut, trait Host a pomocné run*`.

### Task 2: `WslHost` a aktuální hostitel

**Files:**
- Create: `src/host/wsl.rs`
- Modify: `src/host/mod.rs` (`pub mod wsl;`, `pub fn current`, `pub fn endpoint`)
- Modify: `src/wsl.rs` (`run_stream` dostane `&mut dyn FnMut`, jinak beze změny)
- Modify: `src/worker/mod.rs` (`fn host(&self) -> Arc<dyn Host>`)

**Interfaces:**
- Consumes: Task 1 (`Host`, `ExecReq`, `track`, `finish`).
- Produces:
  - `pub struct WslHost { distro: String }` s metodami:
    - `WslHost::new(distro: impl Into<String>) -> Self`
    - `fn distro(&self) -> &str`
    - `fn for_distro(&self, name: &str) -> WslHost`
  - `pub(crate) fn wsl_args(distro: &str, req: &ExecReq) -> Vec<String>` →
    `["-d", distro, ("-u","root")?, ("--cd", cwd)?, "-e", args…]`
  - `impl Host for WslHost`:
    - `label()` vrací název distribuce.
    - `exec` spouští `wsl::hidden("wsl.exe").args(wsl_args(..))`. Se stdin pošle bajty a zavře
      stdin.
    - `exec_stream` = dnešní logika `wsl::run_stream`.
    - `open_terminal` = `wsl::open_terminal(title, &wsl_args(..))`.
    - `to_guest_path` = `wsl::to_linux_path(distro, path)`.
    - `engine_endpoint` = `Endpoint::Tcp { port: cfg.port }`.
    - `wsl()` = `Some(self)`.
  - `pub fn current(cfg: &Config) -> Arc<dyn Host>` vrací `Arc::new(WslHost::new(&cfg.distro))`.
    Nic se necachuje.
  - `pub fn endpoint(cfg: &Config) -> Endpoint` = `current(cfg).engine_endpoint(cfg)`.
  - `Worker::host(&self) -> Arc<dyn Host>` = `host::current(&self.cfg())`.

- [ ] **Step 1: Napsat testy** v `src/host/wsl.rs`:

```rust
#[test] fn args_user() {
    assert_eq!(wsl_args("Ubuntu", &ExecReq::new(&["ls", "-la"])),
               ["-d","Ubuntu","-e","ls","-la"]);
}
#[test] fn args_engine_and_root_use_root() {
    for p in [Privilege::Engine, Privilege::Root] {
        assert_eq!(wsl_args("D", &ExecReq::new(&["id"]).privilege(p).cwd(Some("/tmp"))),
                   ["-d","D","-u","root","--cd","/tmp","-e","id"]);
    }
}
#[test] fn current_follows_config() {
    let mut cfg = Config::default(); cfg.distro = "A".into();
    assert_eq!(current(&cfg).label(), "A");
    cfg.distro = "B".into();
    assert_eq!(current(&cfg).label(), "B");
}
#[test] fn for_distro_keeps_nothing_else() {
    assert_eq!(WslHost::new("A").for_distro("C").distro(), "C");
}
```

  Živý kontraktní test (`#[ignore]`, v `src/host/wsl.rs`), proti `cfg.distro` z
  `C:\WSLContainerDesktop\data\config.json`:
  - `run(User, None, ["sh","-c","echo out; echo err >&2; exit 3"])` →
    `Err(Failed{code:3, output:"err"})`;
  - `run(Root, None, ["id","-u"])` → `"0"`;
  - `run(User, Some("/tmp"), ["pwd"])` → `"/tmp"`;
  - `run_stdin(User, ["cat"], "x\ny")` → `"x\ny"`;
  - `exec_stream` s `printf 'a\rb\nc\n'` předá řádky `["b","c"]`;
  - `exec_stream` se `sleep 60` a `cancel` nastaveným po 300 ms se vrátí do 3 s.
- [ ] **Step 2:** `cargo test host::wsl` → FAIL.
- [ ] **Step 3:** Implementovat podle Interfaces.
- [ ] **Step 4:** `cargo test` PASS. Pak `cargo test --release host::wsl::live -- --ignored --test-threads=1`
  PASS.
- [ ] **Step 5:** Commit `host: WslHost nad wsl.exe a host::current z konfigurace`.

### Pravidlo pro Task 3 a 4: `Engine`, nebo `Root`

Každé dnešní `Some("root")` se přepíše na jednu z variant:
- **`Privilege::Engine`**: příkaz jen volá CLI enginu (`docker`, `podman`, `kubectl` přes
  `docker exec`) nebo testuje či čte jeho soubory (API marker, `/var/lib/docker` a
  `/var/lib/containers` jen pro čtení).
- **`Privilege::Root`**: cokoli jiného. Patří sem `systemctl`, zápis do `/etc` nebo `/usr/local`,
  `nsenter`, `ss -p`, `conntrack`, `iptables`, `sysctl` a `drop_caches`, Soubory v režimu root a
  instalační a nastavovací skripty z `engine.rs` a `lite/mod.rs`.
- Skript, který dělá obojí, je `Root`.
- `fs.rs::user(root)` se změní na `fn privilege(root: bool) -> Privilege` (`Root` / `User`).

Volba se u každého volání nekomentuje zvlášť. Pravidlo se zapíše jednou do dokumentačního
komentáře `Privilege`.

### Task 3: Převod volání – soubory, compose, build, terminály, správa WSL

**Files:**
- Modify: `src/fs.rs`, `src/worker/fs_ops.rs`, `src/worker/compose.rs`, `src/worker/build.rs`,
  `src/worker/containers.rs`, `src/ui/k8s.rs`
- Modify: `src/worker/engine_ops.rs`, `src/worker/distro.rs`, `src/worker/distro_manage.rs`,
  `src/lite/mod.rs`, `src/shim_install.rs`, `src/app.rs`, `src/tray.rs`, `src/ui/distros.rs`

**Interfaces:**
- Consumes: Task 1 a 2 (`HostExt::run*`, `Host::exec`, `exec_stream`, `open_terminal`,
  `to_guest_path`, `wsl()`, `WslHost::for_distro`, `Worker::host`).
- Funkce, které dnes berou `distro: &str` a jen spouští příkazy, budou brát `host: &dyn Host`.
  Místo, kde vzniká hostitel: `self.host()` ve workeru. V UI se hostitel předává z workeru, nevytváří
  se v UI.

- [ ] **Step 1:** `fs.rs` a `worker/fs_ops.rs`: `wsl::base(..).output()` → `host.exec(&ExecReq…)`.
  `stdout` a `stderr` se dekódují přes `host::decode` jako dnes.
- [ ] **Step 2:** `worker/compose.rs` a `worker/build.rs`:
  - cesty přes `host.to_guest_path`;
  - stream přes `host.exec_stream` (zrušení přes `&CANCEL` zůstává);
  - `pkill` a `KILL_PODMAN_BUILD_STEPS` jako `Root`.
- [ ] **Step 3:** `worker/containers.rs` a `ui/k8s.rs`: `wsl::open_terminal(title, wsl_args)` →
  `host.open_terminal(title, &ExecReq)`.
  - Varianty `Term::Distro` a `Term::Dir` (dnes `--cd ~` a `--cd dir` bez příkazu) zůstanou jako
    `host.wsl()`-specifické volání `wsl::open_terminal`, protože otevírají shell distribuce.
- [ ] **Step 4:** Správa WSL (`engine_ops`, `distro`, `distro_manage`, `lite`, `shim_install`,
  `app`, `tray`, `ui/distros`):
  - volání v primární distribuci → `self.host()`;
  - volání v jiné distribuci → `host.wsl().expect("WSL").for_distro(name)`;
  - funkce `wsl::` bez distribuce (`list`, `shutdown`, `export`, `import`, `compact`,
    `vhd_path`…) zůstávají `wsl::`.
- [ ] **Step 5:** `cargo build` bez varování a `cargo test` PASS.
- [ ] **Step 6:** Ručně v aplikaci (`cargo run`) na primární distribuci:
  - Soubory: výpis i režim root;
  - Compose: `up` a `logs`;
  - build z Dockerfile se zrušením;
  - konzole kontejneru;
  - spuštění a uspání distribuce.

  Přehled „Na pozadí“ musí ukazovat stejné titulky jako dřív.
- [ ] **Step 7:** Commit `host: soubory, compose, build, terminály a správa WSL přes Host`.

### Task 4: Převod volání – sběry, engine, k8s, síť; uzávěrka

**Files:**
- Modify: `src/worker/mod.rs`, `src/worker/memory.rs`, `src/memory.rs`, `src/worker/alerts.rs`,
  `src/worker/net_overview.rs`, `src/worker/net_overview_live.rs`, `src/worker/volumes.rs`,
  `src/worker/spec.rs`, `src/k8s/cluster.rs`, `src/portrelay.rs`, `src/worker/containers.rs`
  (zbytek), `src/worker/limits.rs`, `src/ui/*` (zbytek)

**Interfaces:**
- Consumes: totéž jako Task 3.
- Síťový přehled spouští `DISTRO_SCRIPT` ve **všech** běžících distribucích. To jde přes
  `host.wsl().map(|w| w.for_distro(&d.name))`. Když `wsl()` vrátí `None`, krok se přeskočí. Na
  Windows to nenastane.

- [ ] **Step 1:** Převést uvedené soubory podle pravidla `Engine` / `Root`.
- [ ] **Step 2: Uzávěrka.** Spustit:

```
rg -n "wsl::(run|run_owned|run_stdin|run_stream|base)\(" src -g '!src/wsl.rs' -g '!src/host/**' -g '!src/live_*.rs' -g '!src/lite/live_lite.rs' -g '!src/bench.rs'
```

  Očekávání: žádný výsledek.
- [ ] **Step 3:** `cargo build` bez varování a `cargo test` PASS.
- [ ] **Step 4:** Ručně v aplikaci:
  - Přehled (paměť a disk);
  - Síťový přehled s mapou;
  - Kubernetes (start clusteru, mapa toků);
  - limity kontejneru;
  - přepnutí primární distribuce a zpět.

  Před přepnutím zkontrolovat konfiguraci (Global Constraints).
- [ ] **Step 5:** Commit `host: zbylá volání přes Host, wsl:: jen pro správu WSL`.

### Task 5: Vlastní HTTP klient (`src/http/`)

**Files:**
- Modify: `src/http/mod.rs` (doplnění k `Endpoint` z Task 1)
- Create: `src/http/conn.rs` (`Connect`, `Stream`, `TcpConnect`)
- Create: `src/http/client.rs` (`Client`, `Request`, `Response`, pool)
- Modify: `src/docker.rs` (přesunout `http_response` a `Chunked` do `src/http/client.rs` a
  importovat je zpět)

**Interfaces:**
- Produces:
  - `pub trait Stream: Read + Write + Send`:
    - `fn set_read_timeout(&self, t: Option<Duration>) -> io::Result<()>;`
    - `fn shutdown_handle(&self) -> io::Result<Box<dyn Fn() + Send + Sync>>;` Volání ukončí
      spojení v obou směrech z jiného vlákna.
  - `pub trait Connect: Send + Sync`:
    - `fn connect(&self, timeout: Duration) -> io::Result<Box<dyn Stream>>;`
    - `fn describe(&self) -> String;` Pro hlášky, např. `"127.0.0.1:2376"`.
    - `fn host_header(&self) -> String;`
  - `pub struct TcpConnect { pub port: u16, pub tls: Option<Arc<rustls::ClientConfig>> }`
    - `impl Connect`: `127.0.0.1:port`, `set_nodelay(true)`.
    - S TLS obalí spojení přes `rustls::StreamOwned`, `ServerName` je `127.0.0.1`.
  - `pub struct Client` s konstruktorem `Client::new(conn: Arc<dyn Connect>, connect_timeout: Duration, read_timeout: Duration) -> Self`
    a metodou `fn request(&self, method: &str, path: &str) -> Request<'_>`.
  - `Request<'_>`:
    - `.set(name, value) -> Self` a `.timeout(Duration) -> Self` (časový limit čtení pro tento
      požadavek);
    - `.call() -> Result<Response, ApiError>`;
    - `.send_json(serde_json::Value) -> Result<Response, ApiError>`;
    - `.send_bytes(&[u8]) -> Result<Response, ApiError>`;
    - `.send(impl Read) -> Result<Response, ApiError>` (tělo chunked).
  - `Response`:
    - `.status() -> u16` a `.header(&str) -> Option<&str>`;
    - `.into_reader() -> Box<dyn Read + Send>`;
    - `.into_string() -> Result<String, ApiError>` a `.into_json::<T>() -> Result<T, ApiError>`.
  - Stav ≥ 400 → `Err(ApiError::Status { code, message })`. `message` je `message` z JSON těla,
    jinak tělo oříznuté jako dnes v `From<ureq::Error>`. Chyba spojení → `ApiError::Transport`.
    Chyba čtení nebo dekódování → `ApiError::Decode`.

**Pravidla poolu** (algoritmus, který signatury neurčují):
- Požadavek posílá `Connection: keep-alive` a `Host: <host_header>`.
- Do poolu se spojení vrátí, jen když byla odpověď dočtená do konce (`Content-Length` vyčerpaný,
  nebo konec chunked) a server neposlal `Connection: close`.
- Odpověď bez délky a bez chunked se do poolu nevrací nikdy.
- Spojení nečinné v poolu déle než 2 s se zahodí.
- Pool má nejvýš 8 spojení.
- Selže-li pooled spojení dřív, než přijde první bajt odpovědi (EOF nebo reset), požadavek se
  jednou zopakuje na novém spojení, **jen u GET a HEAD**.

- [ ] **Step 1: Napsat testy** v `src/http/client.rs`. Falešný server na `127.0.0.1:0`
  (`TcpListener` ve vlákně, počítá přijatá spojení):

```rust
#[test] fn keep_alive_reuses_connection()    // 3× GET s Content-Length -> server přijal 1 spojení
#[test] fn chunked_body_and_reuse()          // chunked odpověď "hello, world", další GET na stejném spojení
#[test] fn status_404_maps_message()         // {"message":"no such container"} -> ApiError::Status{code:404, message:"no such container"}
#[test] fn connection_close_not_pooled()     // Connection: close -> druhý GET = nové spojení
#[test] fn unknown_length_streams_and_is_not_pooled() // 1 MB po kouscích bez délky; into_reader čte průběžně; další GET = nové spojení
#[test] fn stale_pooled_get_is_retried_once() // server po 1. odpovědi zavře spojení; 2. GET uspěje, přijata 2 spojení
#[test] fn stale_pooled_post_is_not_retried() // totéž s POST -> Err(Transport), server viděl POST jen jednou
#[test] fn shutdown_handle_unblocks_reader()  // server posílá nekonečně pomalu; shutdown_handle() z jiného vlákna -> read vrátí do 1 s
#[test] fn connect_refused_is_transport()     // port bez posluchače -> ApiError::Transport
```

- [ ] **Step 2:** `cargo test http::` → FAIL.
- [ ] **Step 3:** Implementovat podle Interfaces a pravidel poolu. Parsování odpovědi je
  přesunutá `http_response`, jejíž tělo vrací spojení do poolu při dočtení.
- [ ] **Step 4:** `cargo test` PASS, včetně přesunutého `http_response_chunked_and_errors`.
- [ ] **Step 5:** Commit `http: vlastní HTTP/1.1 klient s keep-alive nad obecným spojením`.

### Task 6: `docker.rs` na `http::Client` a `Endpoint`

**Files:**
- Modify: `src/docker.rs`, `src/container_fs.rs`, `src/k8s/cluster.rs`, `src/bench.rs`,
  `src/logbuf.rs`, `src/memory.rs`, `src/portrelay.rs`, `src/npipe.rs`, `src/worker/*.rs`
  (místa s `Docker::new`)
- Modify: `src/error.rs` (`From<ureq::Error>` odstranit, pokud ho už nic nepoužívá)

**Interfaces:**
- Consumes: Task 5 (`Client`, `TcpConnect`, `Connect`, `Stream`), Task 2 (`host::endpoint`).
- Produces:
  - `Docker::new(ep: &Endpoint) -> Docker`, se sdílenou instancí podle (endpoint, TLS klíč) jako
    dnes podle portu. Pro `Endpoint::Tcp` použije `TcpConnect { port, tls: tls() }`.
  - `Docker::with_tls(port, tls)` zůstává pro `bench.rs`.
  - `Docker::request(&self, method, path) -> http::Request<'_>`. Nahrazuje vrácení
    `ureq::Request`. Volající v `container_fs.rs`, `k8s/cluster.rs` a `bench.rs` používají stejné
    názvy metod (`set`, `call`, `send`, `send_json`, `into_reader`, `into_string`, `into_json`).
  - `Docker::describe(&self) -> String`. Hlášky „API není dostupné na …“ ho použijí místo
    `127.0.0.1:{port}`. Pro TCP vychází stejný text jako dnes.
  - `StreamCloser` drží `Box<dyn Fn() + Send + Sync>` ze `Stream::shutdown_handle` místo
    `TcpStream`.
  - `open_stream` otevře spojení přes `Connect` (časový limit 2 s), pošle `Connection: close` a
    nepoužívá pool.
  - `npipe.rs` přeposílá na `Connect::connect` z `Docker` místo vlastního `TcpStream` a rustls.

- [ ] **Step 1:** Upravit `docker.rs` podle Interfaces. `get`, `post`, `post_json`, `delete` a
  `ping` mají stejné signatury a stejné chování chyb.
- [ ] **Step 2:** Převést volající na `Docker::new(&host::endpoint(&cfg))`. Kde je dnes jen
  `port`, sáhnout po `cfg`, odkud port pochází.
- [ ] **Step 3:** `cargo build` bez varování. `cargo test` PASS, včetně stávajících testů v
  `docker.rs`, `container_fs.rs` a `logbuf.rs`.
- [ ] **Step 4:** Ověřit, že `rg -n "ureq" src` ukazuje jen `lite/mod.rs` a `k8s/live_k8s.rs`.
- [ ] **Step 5:** Ručně v aplikaci s TLS zapnutým i vypnutým:
  - seznamy;
  - živé logy a zrušení;
  - události (spadlý kontejner → oznámení);
  - nahrání a stažení souboru v kontejneru;
  - záloha a obnova volume;
  - `docker ps` přes pojmenovanou rouru (`$env:DOCKER_HOST='npipe:////./pipe/docker_engine'`).
- [ ] **Step 6:** Commit `docker: klient API nad http::Client a Endpoint, bez ureq`.

### Task 7: Měření a uzavření kroku 1

**Files:**
- Modify: `src/bench.rs` (metrika `exec_true` přes `host::current(cfg).run(...)`)
- Modify: `docs/ROADMAP.md` (do *Done*: „Abstrakce hostitele, krok 1“)

- [ ] **Step 1:** Přepnout metriku `exec_true` na `host::current(&cfg).run(Privilege::User, None, &["true"])`.
- [ ] **Step 2:** Zkontrolovat primární distribuci a spustit `scripts/bench.ps1`. Porovnat s
  reportem z Task 0. Očekávání:
  - `exec_true` p50 a shim `docker ps` p50 v rozmezí ±10 %;
  - 4 klienti ≥ 635 req/s;
  - CPU v klidu ≤ 0,07 %, 11/11 funkčních kontrol.
- [ ] **Step 3:** Když něco mantinel nesplní, zastavit a nahlásit čísla. Necommitovat jako hotové.
- [ ] **Step 4:** Spustit živé testy podle `live_tests.rs` (`cargo test --release live_ -- --ignored --test-threads=1`).
  Pak vrátit sdílené iptables a ověřit primární distribuci.
- [ ] **Step 5:** Commit `bench: abstrakce hostitele krok 1 – měření po` a aktualizace
  `ROADMAP.md`.
