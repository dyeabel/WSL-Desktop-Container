# Kubernetes laboratoř – plán implementace

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stránka *Kubernetes*, která nad stávajícím enginem vytvoří lokální k3s cluster, nasadí
do něj prostředí (složka YAML nebo šablona FE + API + DB), ukáže mapu skutečných TCP toků mezi
pody a službami a umožní formulářem nastavit a ověřit NetworkPolicy.

**Architecture:** k3s běží jako kontejner `wslcd-k3s` v primárním enginu (Docker API). Aplikace
mluví s clusterem přes `kubectl … -o json` spuštěný `exec` v tomto kontejneru (bez Rust klienta
Kubernetes). Toky se berou ze `ss` v síťových namespace podů přes stávající `netinfo::NS_SCRIPT`
v distribuci. Čistá logika (parsery, hrany, politiky, kubeconfig) je v `src/k8s/` a testuje se
bez WSL. I/O dělá `src/worker/k8s.rs`, UI je v `src/ui/k8s*.rs`.

**Tech Stack:** Rust, egui 0.31, ureq (Docker API), serde_json, nově `serde_yaml_ng` (jen
kubeconfig), k3s v kontejneru.

**Spec:** `docs/superpowers/specs/2026-11-21-kubernetes-lab-design.md`

## Global Constraints

- Kontejner `wslcd-k3s`, síť `wslcd-k3s`, svazek `wslcd-k3s-data` → `/var/lib/rancher/k3s`.
- Spuštění k3s: `--privileged`, `--cgroupns=host`, `--restart=no`, tmpfs `/run` a `/var/run`,
  příkaz `server --disable=traefik --tls-san=127.0.0.1 --write-kubeconfig-mode=600`.
- Porty: `127.0.0.1:6443` (API) a `127.0.0.1:30080-30099` (NodePort, výchozí).
- Kontext, cluster a uživatel v kubeconfigu se jmenují `wslcd`. Před zápisem záloha
  `config.bak-wslcd`. `current-context` se nemění.
- Labely: namespace prostředí `wslcd.lab=1`, politiky aplikace `wslcd.managed=1`.
- Názvy politik: `wslcd-default-deny`, `wslcd-allow-dns`, `wslcd-allow-<zdroj>-<cíl>-<port>`.
- Konfigurace: `k3s_image`, `k3s_api_port` (6443), `k3s_nodeport_range` ((30080, 30099)).
- Sběr toků jen při otevřené záložce *Mapa toků*. Nic neběží, dokud cluster neexistuje.
- Žádný `--network host`. Aplikace nic nestahuje mimo image přes engine.
- Texty dvojjazyčně přes `t!` / `tf!` (tf! nebere pojmenované argumenty `x = …`, jen inline
  `{proměnná}`).
- PID a jiné vstupy se do skriptů předávají jako argumenty, nikdy se nevkládají do textu skriptu.
- Pokud se NetworkPolicy nevynucuje (spike, Task 1): funkce se dodá, pravidla se aplikují jako
  YAML a záložka *Pravidla* ukáže poznámku „cluster pravidla nevynucuje“.

## Review Focus

1. **Port 6443 nebo některý z NodePort je obsazený** (jiný cluster, kind, Docker Desktop) →
   vytvoření selže se srozumitelnou hláškou s číslem portu, ne s holou chybou API. Test: Task 6.
2. **Poškozený nebo nečitelný `~/.kube/config`** → aplikace soubor nepřepíše a ohlásí chybu,
   záloha zůstane netknutá. Test: Task 4.
3. **Pody bez labelu `app`** (vlastní manifesty uživatele) → komponenta se odvodí z
   `app.kubernetes.io/name`, jinak z názvu vlastníka (ReplicaSet bez hashe), jinak z názvu podu.
   Mapa i formulář fungují. Test: Task 2.
4. **Pod se restartuje mezi zjištěním PID a `nsenter`** → chybějící PID se tiše přeskočí, mapa
   nezmizí a hrany ostatních podů zůstanou. Test: Task 3.
5. **První vytvoření bez staženého image nebo offline** → průběh „stahuji image…“, časový limit
   připravenosti 90 s a chyba s posledními řádky logu k3s. Test: Task 6 (formátování chyby).

---

### Task 1: Spike – ověření rizik

Bez produkčního kódu. Výsledek je zápis rozhodnutí, podle kterého se řídí Task 3, 6 a 9.

**Files:**
- Create: `docs/superpowers/specs/2026-11-21-kubernetes-lab-spike.md`

- [ ] **Step 1:** Na Dockeru i na Podmanu (rootful) spusť ručně
  `docker run -d --name wslcd-k3s --privileged --cgroupns=host --tmpfs /run --tmpfs /var/run -p 127.0.0.1:6443:6443 -p 127.0.0.1:30080-30099:30080-30099 rancher/k3s:<aktuální stabilní> server --disable=traefik --tls-san=127.0.0.1`
  a ověř `kubectl get --raw /readyz` přes `exec` = `ok`. Zapiš použitou verzi image.
- [ ] **Step 2:** Nasaď 2 pody (`busybox` server `nc -lk -p 80`, klient smyčka). Porovnej
  `k3s crictl inspect <id>` → `info.pid` s PID v distribuci (`ps -eo pid,args` jako root
  v distribuci). Zapiš, zda se shodují. Pokud ne, ověř převod přes `NSpid` v
  `/proc/<pid>/status` hledáním procesu podle `NSpid` posledního sloupce.
- [ ] **Step 3:** Ověř `nsenter -t <pid> -n ss -tuinpeH state established` z distribuce
  ukáže spojení klient → server.
- [ ] **Step 4:** Aplikuj default deny + allow a ověř, že spojení bez povolení skončí timeoutem.
  Zapiš: NetworkPolicy se vynucuje ano/ne.
- [ ] **Step 5:** Služba typu NodePort 30080 → `curl http://localhost:30080` z Windows v NAT
  režimu (a v mirrored, pokud je k dispozici). Zapiš výsledek.
- [ ] **Step 6:** Zapiš do souboru z Files tabulku: riziko → výsledek → zvolená cesta (podle
  sekce *Co dělat, když spike selže* ve specifikaci). Úklid: smaž kontejner a svazky.
- [ ] **Step 7: Commit** `docs: k8s spike results`

---

### Task 2: Model clusteru a parsery

**Files:**
- Create: `src/k8s/mod.rs` (jen `pub mod …`), `src/k8s/model.rs`
- Create: `src/k8s/testdata/pods.json`, `services.json`, `endpoints.json`, `crictl_ps.json`, `crictl_inspect.json` (reálné výstupy ze spiku, zkrácené)
- Modify: `src/main.rs` (přidat `mod k8s;`)

**Interfaces:**
- Produces:
  - `pub struct Pod { name, namespace, component: String, ip: Option<String>, phase: String, ready: (u32, u32), restarts: u32, node: String, images: Vec<String>, labels: BTreeMap<String,String>, uid: String }`
  - `pub struct Service { name, namespace, component_selector: BTreeMap<String,String>, kind: String /* ClusterIP|NodePort|LoadBalancer */, cluster_ip: Option<String>, ports: Vec<SvcPort> }`, `pub struct SvcPort { name: Option<String>, port: u16, target: String, node_port: Option<u16>, proto: String }`
  - `pub struct Endpoints { service: String, ips: Vec<String> }`
  - `pub fn parse_pods(json: &str) -> Result<Vec<Pod>, String>`, `parse_services`, `parse_endpoints` (vstup = `kubectl get … -o json`, tj. `{"items":[…]}`)
  - `pub fn component_of(labels: &BTreeMap<String,String>, owner: Option<&str>, pod_name: &str) -> String`
  - `pub fn parse_crictl_pids(ps_json: &str, inspect_json: &str) -> HashMap<String /*pod uid*/, u32>` (první PID každého podu)

- [ ] **Step 1: Napiš testy** `model::tests`:
  - `parse_pods_fixture`: počet podů z fixtury, u `fe` `ready == (1,1)`, `ip == Some("10.42.0.…")`, `component == "fe"`.
  - `component_fallbacks`: `{app: "x"}` → `"x"`; `{app.kubernetes.io/name: "y"}` → `"y"`; žádné labely, owner `web-7d9f8c6b5` → `"web"`; nic → název podu.
  - `parse_services_nodeport`: služba se `nodePort: 30080` → `node_port == Some(30080)`, `kind == "NodePort"`.
  - `crictl_pid_per_pod`: z fixtur vrátí PID pro každý pod uid, kontejner `pause` se ignoruje.
- [ ] **Step 2:** `cargo test k8s::model` → FAIL (chybí modul).
- [ ] **Step 3:** Implementuj přes `serde_json::Value` (bez nových derive struktur pro celé
  objekty Kubernetes). Owner z `metadata.ownerReferences[0].name`, hash ReplicaSetu odřízni
  posledním `-<alnum>` segmentem.
- [ ] **Step 4:** `cargo test k8s::model` → PASS.
- [ ] **Step 5: Commit** `feat(k8s): cluster model parsers`

---

### Task 3: Mapa toků – skládání hran

**Files:**
- Create: `src/k8s/flows.rs`

**Interfaces:**
- Consumes: `Pod`, `Service`, `Endpoints` (Task 2); `crate::netinfo::{NsDump, Sock, parse_ns_dump}`.
- Produces:
  - `pub enum Node { Pod(String /*name*/), Service(String), Outside(String /*ip*/), Localhost }`
  - `pub struct Edge { from: Node, to: Node, port: u16, conns: u32, bytes: u64 }`
  - `pub fn build_edges(pods: &[Pod], svcs: &[Service], eps: &[Endpoints], dumps: &HashMap<String /*pod name*/, NsDump>, node_ips: &[String]) -> Vec<Edge>`

- [ ] **Step 1: Testy** `flows::tests`:
  - `pod_to_service_edge`: spojení z `fe` na ClusterIP služby `api:3000` → hrana `Pod("fe") → Service("api")`, port 3000.
  - `pod_to_pod_edge`: spojení na IP podu `db` → `Pod("api") → Pod("db")`.
  - `nodeport_from_localhost`: serverová strana v `fe` s peerem z `node_ips` → `Localhost → Pod("fe")`.
  - `unknown_peer_outside`: peer 1.2.3.4 → `Outside("1.2.3.4")`.
  - `missing_dump_skipped`: pod bez záznamu v `dumps` nezpůsobí chybu, hrany ostatních zůstanou (Review Focus 4).
  - `dedupe_both_sides`: totéž spojení viděné z klienta i serveru se nezapočítá dvakrát (klíč podle IP:port dvojice, server-side se použije jen pro zdroje mimo cluster).
- [ ] **Step 2:** `cargo test k8s::flows` → FAIL.
- [ ] **Step 3:** Implementuj. `bytes` = `bytes_acked + bytes_received` ze `Sock`, pokud je
  `Sock` nese, jinak 0. Rychlost se počítá v UI přes stávající `ConnTracker`.
- [ ] **Step 4:** `cargo test k8s::flows` → PASS.
- [ ] **Step 5: Commit** `feat(k8s): flow edges`

---

### Task 4: Kubeconfig – slučování

**Files:**
- Create: `src/k8s/kubeconfig.rs`
- Modify: `Cargo.toml` (`serde_yaml_ng = "0.10"`)

**Interfaces:**
- Produces:
  - `pub fn rewrite_k3s(k3s_yaml: &str, api_port: u16) -> Result<serde_yaml_ng::Value, String>` (server → `https://127.0.0.1:<port>`, jména → `wslcd`)
  - `pub fn merge(existing: Option<&str>, ours: &serde_yaml_ng::Value) -> Result<String, String>`
  - `pub fn write_user_config(ours: &serde_yaml_ng::Value) -> Result<std::path::PathBuf, String>` (I/O: `%USERPROFILE%\.kube\config`, záloha `config.bak-wslcd`)

- [ ] **Step 1: Testy:**
  - `rewrite_sets_server_and_names`: fixtura `k3s.yaml` → server `https://127.0.0.1:6443`, `contexts[0].name == "wslcd"`, user i cluster `wslcd`.
  - `merge_keeps_other_contexts`: existující config s kontextem `aks-prod` a `current-context: aks-prod` → výstup obsahuje `aks-prod` i `wslcd`, `current-context` zůstává `aks-prod`.
  - `merge_replaces_old_wslcd`: starý `wslcd` se jiným certifikátem se nahradí, ne zdvojí.
  - `merge_into_empty`: `None` → platný config s `wslcd` a `current-context: wslcd` (jen když dosud žádný nebyl).
  - `merge_rejects_garbage`: neplatný YAML → `Err`, nic se nevrací k zápisu (Review Focus 2).
- [ ] **Step 2:** `cargo test k8s::kubeconfig` → FAIL.
- [ ] **Step 3:** Implementuj. `write_user_config`: načti, `merge`, při `Err` nic nezapisuj;
  jinak zkopíruj původní soubor do `config.bak-wslcd`, zapiš do dočasného souboru a přejmenuj.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(k8s): kubeconfig merge`

---

### Task 5: Pravidla (NetworkPolicy) a šablona

**Files:**
- Create: `src/k8s/policy.rs`, `src/k8s/template.rs`

**Interfaces:**
- Produces:
  - `pub enum Peer { Component(String), Localhost }`
  - `pub struct Rule { from: Peer, to: String /*component*/, port: u16 }`
  - `pub struct RuleSet { deny_all: bool, rules: Vec<Rule> }`
  - `pub fn to_manifests(ns: &str, set: &RuleSet, label_key: &str, node_cidrs: &[String]) -> Vec<serde_json::Value>` (NetworkPolicy objekty; `label_key` = `app` nebo `app.kubernetes.io/name`)
  - `pub fn from_cluster(policies_json: &str) -> (RuleSet, Vec<String> /*cizí politiky*/)` (rekonstrukce formuláře z anotace `wslcd.rule` = JSON pravidla)
  - `pub fn to_yaml_preview(manifests: &[serde_json::Value]) -> String`
  - `pub fn demo_manifests(ns: &str, db_password: &str, with_rules: bool) -> String` (YAML text pro `kubectl apply -f -`)

- [ ] **Step 1: Testy** `policy::tests`:
  - `deny_all_adds_dns`: `deny_all` → manifesty obsahují `wslcd-default-deny` (policyTypes Ingress+Egress) a `wslcd-allow-dns` (egress UDP+TCP 53 na `k8s-app=kube-dns` v `kube-system`).
  - `rule_makes_two_policies`: `fe → api:3000` → `wslcd-allow-fe-api-3000-in` (ingress na `app=api` z `app=fe`, port 3000) a `…-out` (egress z `app=fe` na `app=api`).
  - `localhost_rule_uses_ipblock`: `Localhost → fe:80` → ingress `ipBlock` pro každé CIDR z `node_cidrs`, bez egress politiky.
  - `managed_label_and_annotation`: všechny mají label `wslcd.managed=1` a anotaci `wslcd.rule`.
  - `roundtrip_from_cluster`: `from_cluster` nad `{"items": to_manifests(…)}` vrátí původní `RuleSet`. Politika bez `wslcd.managed` je v seznamu cizích.
  - `template::demo_has_three_components`: text obsahuje Deploymenty `fe`, `api`, `db`, Secret s heslem a se `with_rules` politiky z této sady (`fe→api:3000`, `api→db:5432`, `localhost→fe:80`).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implementuj. Šablona: `nginx`, `postgrest/postgrest`, `postgres` s pevnými
  verzemi ze spiku (Task 1), init SQL v ConfigMap (tabulka `items`, 3 řádky), nginx proxy
  `/api/` → `http://api:3000/`, statická stránka volá `/api/items`. Labely `app` a `tier`.
  Image mají `imagePullPolicy: IfNotPresent`.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(k8s): network policy generation and demo template`

---

### Task 6: Cluster v enginu

**Files:**
- Create: `src/k8s/cluster.rs`
- Modify: `src/container_fs.rs` (přidat `exec_stdin`), `src/config.rs` (3 pole s výchozími hodnotami)

**Interfaces:**
- Consumes: `Docker`, `container_fs::exec`.
- Produces:
  - `pub fn exec_stdin(d: &Docker, id: &str, cmd: &[&str], input: impl Read) -> Result<ExecOut, ApiError>` v `container_fs.rs` (AttachStdin, hijack přes ureq; stdin se uzavře po EOF)
  - `pub enum ClusterState { Missing, Stopped, Starting, Running, Error(String) }`
  - `pub const NAME: &str = "wslcd-k3s";`
  - `pub fn create_body(image: &str, api_port: u16, range: (u16, u16)) -> serde_json::Value`
  - `pub fn state(d: &Docker) -> ClusterState`
  - `pub fn kubectl(d: &Docker, args: &[&str]) -> Result<String, String>` (exec `kubectl`, chyba = stderr)
  - `pub fn kubectl_apply(d: &Docker, ns: &str, manifest: &str) -> Result<String, String>`
  - `pub fn wait_ready(d: &Docker, timeout: Duration, progress: impl FnMut(&str)) -> Result<(), String>`
  - `pub fn port_conflict_message(api_err: &str) -> Option<String>` (z chyby enginu „port is already allocated / address already in use“ vytáhne port a vrátí `tf!("Port {p} je obsazený…")`)
  - `pub fn load_image(d: &Docker, image: &str) -> Result<(), String>` (`GET /images/{name}/get` → `exec_stdin` `ctr -n k8s.io images import -`)

- [ ] **Step 1: Testy** `cluster::tests`:
  - `create_body_shape`: `HostConfig.Privileged == true`, `CgroupnsMode == "host"`, `PortBindings["6443/tcp"][0] == {HostIp:"127.0.0.1",HostPort:"6443"}`, 20 vazeb pro 30080–30099, `Binds` obsahuje `wslcd-k3s-data:/var/lib/rancher/k3s`, `Tmpfs` má `/run` a `/var/run`, `Cmd` přesně podle Global Constraints.
  - `port_conflict_docker`: `"Bind for 127.0.0.1:6443 failed: port is already allocated"` → zpráva s `6443` (Review Focus 1).
  - `port_conflict_podman`: `"rootlessport listen tcp 127.0.0.1:30085: bind: address already in use"` → zpráva s `30085`.
  - `ready_timeout_message`: chyba po timeoutu obsahuje poslední řádky logu (funkce `timeout_error(log_tail: &str) -> String`, Review Focus 5).
  - `config_defaults`: nový `Config::default()` má `k3s_api_port == 6443`, `k3s_nodeport_range == (30080, 30099)` a starý `config.json` bez polí se načte.
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implementuj. Síť a svazek vytvoř před kontejnerem (`/networks/create`,
  `/volumes/create`), ignoruj 409. Image stáhni přes stávající `Docker::pull` s průběhem.
- [ ] **Step 4:** PASS + `cargo clippy` bez varování.
- [ ] **Step 5: Commit** `feat(k8s): k3s cluster container lifecycle`

---

### Task 7: Worker – operace a sběr toků

**Files:**
- Create: `src/worker/k8s.rs`
- Modify: `src/worker/mod.rs` (`Cmd::K8s(K8sOp)`, `Msg::K8s(K8sMsg)`, `pub mod k8s`, přepnutí primární distribuce)

**Interfaces:**
- Consumes: Task 2–6.
- Produces:
  - `pub enum K8sOp { Status, Create, Start, Stop, Delete, WriteKubeconfig, LoadImage(String), Deploy { ns: String, folder: String }, DeployDemo { ns: String, with_rules: bool }, DeleteEnv(String), Expose { ns: String, svc: String }, Snapshot(String /*ns*/), ApplyRules { ns: String, set: RuleSet }, Probe { ns: String, from: Peer, to: String, port: u16 } }`
  - `pub enum K8sMsg { State(ClusterState, Option<ClusterInfo>), Progress(String), Done(Result<String, String>), Env { ns: String, pods: Vec<Pod>, svcs: Vec<Service>, rules: RuleSet, foreign: Vec<String>, enforced: Option<bool> }, Namespaces(Vec<String>), Probe(Result<bool, String>) }`
  - `pub fn start_k8s_flows(…) -> K8sFlowFeed` (vzor `start_net_overview`: vlákno běží, dokud UI drží feed; data `Arc<Mutex<FlowData>>` s `edges: Vec<Edge>`, `tracker: ConnTracker`)
  - `pub struct ClusterInfo { version: String, mem: u64, cpu: f64 }`

- [ ] **Step 1: Testy** (přes `worker::testkit`, bez WSL):
  - `deploy_folder_reads_only_yaml`: složka s `a.yaml`, `b.yml`, `c.txt`, `sub/d.yaml` → pošle se `a.yaml` a `b.yml` spojené `---`.
  - `expose_picks_free_nodeport`: služby už drží 30080 a 30081 → `Expose` použije 30082; plný rozsah → `Err` se zprávou o rozsahu.
  - `probe_script_args`: příkaz pro sondu předá PID, IP a port jako argumenty `sh -c` (žádná interpolace do textu skriptu).
- [ ] **Step 2:** FAIL.
- [ ] **Step 3:** Implementuj. Sonda: `wsl` jako root v distribuci
  `sh -c 'nsenter -t "$1" -n timeout 3 sh -c "</dev/tcp/$2/$3" 2>/dev/null || nsenter -t "$1" -n nc -z -w 3 "$2" "$3"' _ <pid> <ip> <port>`
  (bash `/dev/tcp`, jinak `nc`; podle výsledku Task 1). Sonda *Localhost* = `TcpStream::connect_timeout` na `127.0.0.1:<nodePort>`.
  Po každém úspěšném `Create` / `Start` zapiš kubeconfig (`write_user_config`), protože
  certifikáty se mezi clustery liší; chyba zápisu = varování, ne selhání startu.
  Při přepnutí primární distribuce (`ask_primary` → worker) zavolej `Stop` na clusteru v předchozím
  enginu, pokud běží. `enforced` = výsledek ze spiku uložený jako konstanta, nebo `None`, pokud
  to nejde zjistit.
- [ ] **Step 4:** PASS.
- [ ] **Step 5: Commit** `feat(k8s): worker operations and flow feed`

---

### Task 8: Stránka Kubernetes – Cluster a Prostředí

**Files:**
- Create: `src/ui/k8s.rs`
- Modify: `src/app.rs` (`Page::Kubernetes` do `MAIN` za `Compose`, ikona `\u{E1B4}` nebo jiná z phosphor, `drain` pro `Msg::K8s`), `src/ui/mod.rs`, `src/ui/containers.rs` (štítek „uzel Kubernetes“ u `wslcd-k3s` a potvrzení navíc u Odstranit)

**Interfaces:**
- Consumes: `K8sOp`, `K8sMsg` (Task 7).
- Produces: `pub struct K8sUi { tab: K8sTab, ns: Option<String>, state: ClusterState, env: Option<…>, rules_draft: RuleSet, flows: Option<K8sFlowFeed>, probe: Option<Result<bool,String>> }`, `pub enum K8sTab { Cluster, Env, Flows, Rules }`, `pub fn show(app: &mut App, ui: &mut egui::Ui)`.

- [ ] **Step 1:** Implementuj záložky *Cluster* a *Prostředí* podle specifikace (sekce
  „Stránka Kubernetes“). Na Podmanu, pokud spike řekl „nefunguje“, místo tlačítek vysvětlení.
  Při chybějícím `kubectl.exe` v PATH nápověda `winget install Kubernetes.kubectl`. Logy podu:
  `kubectl logs -f` ve Windows Terminalu přes stávající `wsl::open_terminal` s
  `docker exec -it wslcd-k3s kubectl logs -f -n <ns> <pod>`.
- [ ] **Step 2:** `cargo build` a `cargo clippy` bez varování.
- [ ] **Step 3:** Ruční ověření v aplikaci: vytvořit cluster → *běží*, nasadit šablonu → 3 pody
  Ready, *Zpřístupnit* `fe` → `http://localhost:30080` ukáže stránku se třemi řádky z DB.
- [ ] **Step 4: Commit** `feat(k8s): Kubernetes page – cluster and environments`

---

### Task 9: Mapa toků, Pravidla a Zkusit spojení

**Files:**
- Create: `src/ui/k8s_map.rs`, `src/ui/k8s_rules.rs`
- Modify: `src/ui/k8s.rs`

**Interfaces:**
- Consumes: `K8sFlowFeed`, `Edge`, `Node`, `RuleSet`, `to_yaml_preview`, `K8sOp::{ApplyRules, Probe}`.
- Produces: `pub fn layout_columns(pods: &[Pod], edges: &[Edge]) -> Vec<Vec<String>>` v `k8s_map.rs` (čistá funkce, testovaná).

- [ ] **Step 1: Test** `layout_columns_by_tier`: pody s `tier=frontend|backend|database` → 3 sloupce v tomto pořadí. Bez `tier` → zdroje hran vlevo, cíle vpravo, `Localhost` vždy první sloupec.
- [ ] **Step 2:** FAIL → implementuj → PASS.
- [ ] **Step 3:** Kreslení mapy (egui painter): uzly v sloupcích, šipky s tloušťkou podle
  rychlosti z `ConnTracker`, popisek port a data, bublina s detailem. Povolené cesty bez provozu
  přerušovaně, zámek u podů pod default deny. Feed se spouští při otevření záložky a zahodí při
  odchodu.
- [ ] **Step 4:** Záložka *Pravidla*: přepínač default deny, seznam pravidel (výběr zdroje
  včetně *localhost (Windows)* s poznámkou o SNAT, cíle a portu z portů služby), *Zobrazit YAML*,
  *Použít*, cizí politiky jen ke čtení. Poznámka „cluster pravidla nevynucuje“, když
  `enforced == Some(false)`. U každého pravidla i obecně *Zkusit spojení* s výsledkem.
- [ ] **Step 5:** Ruční ověření: šablona se vzorovými pravidly → mapa ukazuje `localhost → fe →
  api → db`, sonda `fe → db:5432` = zablokováno, `fe → api:3000` = prošlo.
- [ ] **Step 6: Commit** `feat(k8s): flow map, rules and connection probe`

---

### Task 10: Živý test a dokumentace

**Files:**
- Create: `src/k8s/live_k8s.rs` (`#[cfg(test)]`, testy `#[ignore]`)
- Modify: `README.md` (řádek tabulky *Kubernetes*), `docs/ROADMAP.md` (Done)

- [ ] **Step 1:** Živý test `live_k8s_lab`: vytvoř cluster, nasaď demo se vzorovými pravidly do
  `wslcd-live`, počkej na Ready, ověř sondu `fe→api:3000` = true a `fe→db:5432` = false (jen když
  `enforced != Some(false)`), ověř, že `build_edges` po `curl` na NodePort obsahuje
  `fe → api`. Úklid smaže namespace a cluster i při chybě (guard s `Drop`). Hlavička souboru
  upozorní, že test běží proti enginu z konfigurace aplikace (viz `live_tests.rs`).
- [ ] **Step 2:** `cargo test live_k8s -- --ignored --nocapture --test-threads=1` → PASS
  (spouští se ručně).
- [ ] **Step 3:** README: řádek *Kubernetes* v tabulce Features a krátká sekce s `kubectl --context wslcd`. ROADMAP: přesunout do Done.
- [ ] **Step 4:** `cargo test` a `cargo clippy` → vše zelené.
- [ ] **Step 5: Commit** `docs(k8s): README, roadmap, live test`
