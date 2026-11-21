# Kubernetes laboratoř – návrh

Stav: schváleno uživatelem · 2026-11-21

## Proč

Uživatel chce v lokálním clusteru Kubernetes spouštět prostředí z více závislých částí
(frontend, backend s API, databáze…) a hlavně **vidět, jak spolu části komunikují**: kdo s kým
mluví, kam a kolik dat teče. Chce **omezovat sítě a komunikaci** mezi nimi a ověřit, že omezení
platí. Jde o laboratoř pro jeden scénář: postavit, prohlédnout, omezit, ověřit. Nejde o obecného
klienta Kubernetes.

Inspirace: [libracoder/kiri](https://github.com/libracoder/kiri) (`kiri k8s start` spustí k3s
s jedním uzlem jako kontejner a přidá kubectl context). Kiri ale cluster v okně jen zapne nebo
vypne. Tento návrh k tomu přidává pohled na toky a pravidla.

## Cíle

1. **Cluster jedním klikem**: vytvořit, spustit, zastavit a smazat lokální k3s s jedním uzlem
   nad stávajícím enginem (Docker i Podman). Ukázat stav, verzi a spotřebu.
2. **Prostředí**: nasadit složku s YAML manifesty (`kubectl apply`) nebo vestavěnou šablonu
   *FE + API + DB* do vlastního namespace. Ukázat pody, jejich stav, restarty a logy. Prostředí
   smazat.
3. **Mapa toků**: graf podů a služeb v namespace s hranami podle skutečných TCP spojení (směr,
   port, přenesená data a rychlost), obnovovaný po pár sekundách.
4. **Pravidla**: formulář „kdo smí na koho“ (zdroj → cíl, port), ze kterého aplikace vygeneruje
   NetworkPolicy. Volba „vše ostatní zakázat“ (default deny) a automatická výjimka pro DNS.
   Vygenerovaný YAML jde zobrazit. Povolené cesty jsou v mapě vidět.
5. **Ověření**: tlačítko „zkusit spojení“ mezi dvěma pody. Výsledek je *prošlo* nebo
   *zablokováno / nedostupné*.
6. **Přístup z Windows**: službu jde jedním klikem zpřístupnit na `http://localhost:<port>`.
   Kontext `wslcd` se zapíše do `%USERPROFILE%\.kube\config`, takže funguje `kubectl` i jiné
   nástroje ve Windows.
7. **Vlastní image**: image sestavený v enginu (stránka *Image* / build) jde jedním klikem
   načíst do clusteru, bez registry.

## Mimo rozsah (non-goals)

- Obecný prohlížeč všech objektů Kubernetes (CRD, RBAC, Ingress, StatefulSet do detailu…), Helm
  UI a editace libovolných objektů. Na to slouží `kubectl`, k9s a Lens.
- Cluster s více uzly, vzdálené clustery a AKS.
- Pohled na HTTP požadavky (L7) a záznam zahozených paketů. To umí jen Cilium + Hubble
  (varianta B), případně později jako volitelný režim.
- Převod compose → Kubernetes (kompose). Případně později.
- Vlastní distribuce Linuxu. To je samostatný projekt, navazuje po tomto.

## Čemu se vyhnout (anti-goals)

- Cluster nesmí zatěžovat systém, když neběží: žádné procesy ani sběr dat, dokud ho uživatel
  nezapne. Sběr toků běží jen při otevřené mapě (stejně jako dnes stránka *Síť*).
- Nesmí rozbít stávající engine: žádný `--network host`, žádné zásahy do iptables distribuce
  mimo kontejner uzlu.
- Zápis do `~/.kube/config` nesmí smazat nebo poškodit jiné kontexty uživatele.

## Omezení

- Aplikace je Rust + egui, synchronní, práce běží ve `worker` vláknech (`src/worker/*`). Do WSL
  chodí přes `wsl.exe` (`src/wsl.rs`) a do enginu přes Docker API (`src/docker.rs`). Žádný tokio.
- Engine je rootful Docker nebo rootful Podman se systemd v libovolné distribuci.
- Texty jsou dvojjazyčné přes `t!("cz", "en")`.
- Pody běží v containerd **uvnitř** kontejneru uzlu, takže je Docker API nevidí. Stávající
  stránky Kontejnery a Síť je neukážou a ukazovat nemají.

## Přístupy

| | Popis | Pro | Proti |
|---|---|---|---|
| **A – lehká (zvolena)** | k3s jako kontejner, toky z `ss` v síťových namespace podů, NetworkPolicy přes vestavěný kube-router v k3s | žádné další součásti, stovky MB RAM, znovu využije kód stránky *Síť* | nevidí zahozená spojení ani HTTP, proto tlačítko „zkusit spojení“ |
| B – Cilium + Hubble | k3s bez flannelu, Cilium jako CNI, Hubble pro toky | přesné toky včetně zahozených a důvodu, L7 | +300–500 MB RAM, pomalejší start, závislost na eBPF v jádře WSL, jiná síť clusteru |
| C – převod z compose | kompose převede stávající stacky | znovu využije compose | nepřesný převod, další závislost; jen jako doplněk |

Uživatel zvolil **A**. Až uživatel přestane potřebovat jen tento scénář, dá se B doplnit jako
volitelný režim.

## Řešení (A)

### Cluster

- Kontejner `wslcd-k3s` z image `rancher/k3s:<pinned>`. Verzi drží konstanta v kódu a jde
  přepsat v `config.json`.
- Parametry: `--privileged`, `--cgroupns=private` (k3s i pody zůstanou v cgroupě kontejneru, takže statistiky enginu počítají celý cluster; původní `host` je nechával v kořeni – opraveno 2026-11-21),
  `--restart=no`, vlastní síť `wslcd-k3s` (bridge), svazek `wslcd-k3s-data` →
  `/var/lib/rancher/k3s` (cluster přežije restart), tmpfs `/run` a `/var/run`.
  Příkaz: `server --disable=traefik --tls-san=127.0.0.1 --write-kubeconfig-mode=600`.
  Metrics-server **zůstane**, protože dává CPU a RAM podů.
- Publikované porty: `127.0.0.1:6443` (API) a rozsah `127.0.0.1:30080-30099` (NodePort pro
  zpřístupněné služby). WSL je přepošle na `localhost` ve Windows. Rozsah se zadává v nastavení
  a výchozích 20 portů stačí.
- **Spustit / Zastavit** = start / stop kontejneru. **Smazat cluster** = odstranit kontejner,
  síť i svazek, s potvrzením.
- Připravenost: `kubectl get --raw /readyz` uvnitř kontejneru, s časovým limitem 90 s a
  průběhem v UI.
- Na Podmanu stejné volání přes kompatibilní API. Kdyby Podman běžel rootless, aplikace ukáže
  hlášku, že k3s potřebuje rootful.

### Jak aplikace mluví s clusterem

- Přes **`kubectl` uvnitř kontejneru uzlu** (`exec` přes Docker API: `kubectl … -o json`),
  výstup se parsuje serde. Žádný Rust klient Kubernetes, žádné certifikáty ve Windows a žádný
  async. Je to stejný vzor, jakým aplikace dnes volá příkazy ve WSL.
- Manifesty ze složky ve Windows: aplikace je přečte a pošle na stdin `kubectl apply -n <ns> -f -`.
  Jde jen o soubory `*.yaml|*.yml` ve složce, bez rekurze. Namespace vytvoří aplikace.
  Prostředí se pozná podle labelu namespace `wslcd.lab=1`.

### Kubeconfig ve Windows

- `/etc/rancher/k3s/k3s.yaml` z kontejneru se upraví na `server: https://127.0.0.1:6443` a
  přejmenuje na cluster / user / context `wslcd`. Pak se **sloučí** do
  `%USERPROFILE%\.kube\config`: před zápisem záloha `config.bak-wslcd`, nahradí se jen položky
  `wslcd`, ostatní zůstanou beze změny, `current-context` se nemění.
- Pokud `kubectl.exe` není v PATH, aplikace ukáže nápovědu `winget install Kubernetes.kubectl`
  (nic sama nestahuje).

### Stránka „Kubernetes“

Nová položka `Page::Kubernetes` v hlavním menu za *Compose*. Má čtyři záložky:

1. **Cluster**: stav (neexistuje / zastaven / startuje / běží), verze, RAM a CPU kontejneru
   uzlu, tlačítka *Vytvořit a spustit / Spustit / Zastavit / Smazat*, *Zapsat kubeconfig*,
   *Načíst image do clusteru* (výběr z image enginu).
2. **Prostředí**: výběr namespace (jen `wslcd.lab=1`), *Nasadit složku…*, *Nasadit šablonu
   FE + API + DB*, *Smazat prostředí*. Tabulka podů (název, stav, připraveno, restarty, IP,
   uzel/image). Služby: typ, ClusterIP, porty a akce *Zpřístupnit na localhost* (patch na
   NodePort z rozsahu, ukáže `http://localhost:300xx`). Logy podu: `kubectl logs -f` ve
   stávajícím okně logů, pokud ho jde napojit na proud textu, jinak ve Windows Terminalu (volí
   implementace).
3. **Mapa toků**: viz níže.
4. **Pravidla**: viz níže.

### Mapa toků

- **Sběr** (jen při otevřené záložce, interval jako u *Sítě*):
  1. `kubectl get pods,svc,endpoints -n <ns> -o json` → IP podů a služeb, mapování
     ClusterIP:port → pody.
  2. PID kontejnerů podů: `crictl ps` / `crictl inspect` uvnitř uzlu (`k3s crictl`) vrátí
     `info.pid`. Tento PID je vidět i z distribuce, protože kontejner uzlu nemá vlastní PID
     namespace vůči distribuci (ověřit ve spike, viz Rizika). Pro každý pod stačí jeden PID
     (sdílí síťový namespace).
  3. Stávající `netinfo::NS_SCRIPT` v režimu `all` spuštěný jako root v distribuci s těmito
     PID → navázaná TCP spojení s čítači (`bytes_acked`, `bytes_received`). Parser a
     `ConnTracker` se použijí beze změny.
- **Skládání hran**: spojení z podu P na adresu X se přiřadí cíli:
  - X = IP podu → hrana pod → pod,
  - X = ClusterIP služby → hrana pod → služba → (endpoint pody),
  - jinak → uzel „mimo cluster“ (DNS v `kube-system` se ukáže jako služba `kube-dns`).

  U hrany je port, počet spojení, přenesená data a rychlost. Spojení, která na straně serveru
  vidí cizí IP (z Windows přes NodePort), se ukážou jako „z localhostu“.
- **Kreslení**: egui painter, sloupce podle vrstev. Pořadí sloupců dávají labely
  `app.kubernetes.io/component` nebo `tier`, pokud chybí, rozloží se podle směru hran (zdroje
  vlevo). Šipky mají tloušťku podle rychlosti. Najetí myší ukáže detail. Povolené cesty z
  pravidel (bez provozu) se kreslí přerušovaně, takže je vidět „smí, ale nemluví“ i „mluví“.
  Pod, na který se vztahuje default deny, má značku zámku.

### Pravidla (NetworkPolicy)

- Formulář: přepínač *Vše ostatní v namespace zakázat* (vytvoří `wslcd-default-deny` pro
  Ingress i Egress) a seznam pravidel *zdroj → cíl : port*. Zdroj a cíl jsou výběr z
  „komponent“, tedy množin podů podle labelu `app` (nebo `app.kubernetes.io/name`). Zdrojem může
  být i *localhost (Windows)*, tj. přístup přes NodePort. *(rozhodnutí návrháře)* Provoz z
  NodePort přichází po SNAT z adresy uzlu, proto se tento zdroj převede na `ipBlock` s IP uzlu
  (`/32`) a bránou podsítě podů na uzlu (`cni0`, typicky `10.42.0.1/32`). Obě adresy aplikace
  zjistí z clusteru. V UI je u zdroje poznámka, že jde o „provoz z uzlu (localhost i jiné uzlové
  procesy)“.
- Každé pravidlo = jedna NetworkPolicy `wslcd-allow-<zdroj>-<cíl>-<port>` (ingress na cíli a
  egress na zdroji, aby platila i s default deny pro egress). Při default deny se automaticky
  přidá `wslcd-allow-dns` (egress na `kube-dns` UDP/TCP 53).
- Aplikace spravuje jen politiky s labelem `wslcd.managed=1`. Cizí politiky v namespace ukáže
  jen ke čtení a v mapě je zahrne jako „cizí pravidlo“.
- *Zobrazit YAML* ukáže, co se aplikuje. *Použít* = `kubectl apply`, odebrání = `kubectl delete`.

### Zkusit spojení

- *(rozhodnutí návrháře po kontrole)* Test běží **v síťovém namespace skutečného zdrojového
  podu**, ne v samostatném probe podu. Probe pod se stejnými labely by si přisvojil ReplicaSet
  zdroje a smazal ho. Aplikace použije PID podu ze sběru toků a spustí v distribuci jako root
  `nsenter -t <pid> -n timeout 3 bash -c '</dev/tcp/<ip>/<port>'` (nebo `nc -z -w 3`, je-li
  v distribuci). PID se předává jako argument, nikdy se nevkládá do textu skriptu. Politika
  podu platí, protože provoz odchází z jeho rozhraní. Není potřeba žádný další image.
- Zdroj *localhost (Windows)*: aplikace zkusí TCP spojení na `127.0.0.1:<NodePort>` přímo
  z Windows (Rust `TcpStream::connect_timeout`, 3 s).
- Výsledek: *prošlo* (zelená), *zablokováno nebo nedostupné* (červená, s poznámkou, že
  NetworkPolicy zahazuje tiše, takže to vypadá jako timeout).

### Šablona FE + API + DB

Namespace `demo` (nebo zadané jméno), tři komponenty s labelem `app` a `tier`:

- `db`: `postgres:<pinned>` s malým init SQL (tabulka `items` s několika řádky), heslo
  generované do Secret.
- `api`: `postgrest/postgrest:<pinned>` nad `db` (REST nad tabulkou).
- `fe`: `nginx:<pinned>` se statickou stránkou, která volá `/api/items`, a s proxy `/api` →
  služba `api`.
- Volitelně *se vzorovými pravidly*: default deny + localhost → fe:80, fe → api:3000,
  api → db:5432.

Šablona je vložená v binárce jako text (vzor `compose_templates.rs`).

### Načtení image do clusteru

`docker save <image>` (přes API `/images/get`) → stream do `exec` v kontejneru uzlu
`ctr -n k8s.io images import -`. V manifestech pak stačí `imagePullPolicy: IfNotPresent`.
Šablona i nápověda to zmíní.

### Vztah k primární distribuci a enginu *(rozhodnutí návrháře)*

- Cluster patří **primárnímu** enginu, se kterým aplikace právě pracuje. Stránka Kubernetes
  ukazuje cluster primárního enginu.
- Při přepnutí primární distribuce aplikace cluster v předchozím enginu **zastaví** (uvolní
  6443 a rozsah NodePort) a nemaže ho. Po návratu ho jde zase spustit. Kontext `wslcd`
  v kubeconfigu se přepíše při každém *Spustit*, protože certifikáty se v každém clusteru liší.
- Kontejner `wslcd-k3s` se na stránce *Kontejnery* zobrazí, ale s označením „uzel Kubernetes“,
  a akce Odstranit se na něm ptá navíc. Upozornění na pád nebo unhealthy u něj platí normálně.

### Kubeconfig – způsob slučování *(rozhodnutí návrháře)*

Slučuje se strukturovaně přes YAML crate (implementace vybere udržovaný, např. `serde_yaml_ng`
nebo `yaml-rust2`). Data ostatních kontextů zůstanou, komentáře a pořadí klíčů se nezachovají.
Proto vždy vznikne záloha `config.bak-wslcd` a UI na to upozorní při prvním zápisu.

### Konfigurace

Do `config.json`: `k3s_image`, `k3s_nodeport_range` (výchozí 30080–30099), `k3s_api_port`
(6443). Ostatní stav (existence clusteru) se zjišťuje z enginu, neukládá se.

### Testy

- Jednotkové testy: skládání hran z (podů, služeb, spojení), generování NetworkPolicy
  z formuláře (včetně DNS výjimky a pojmenování), slučování kubeconfig (zachová cizí kontexty,
  nahradí `wslcd`), parsování výstupů `kubectl`/`crictl` z uložených ukázek.
- Živý test (`#[ignore]`, vzor `live_tests.rs`): vytvoř cluster, nasaď šablonu se vzorovými
  pravidly, ověř, že probe fe → api projde a fe → db neprojde, a že mapa obsahuje hranu
  fe → api. Úklid smaže cluster. Pozor: běží proti skutečnému enginu z konfigurace.

## Rizika (ověřit spikem jako první krok plánu)

1. **PID podů z distribuce**: že `info.pid` z `crictl` uvnitř privilegovaného kontejneru
   odpovídá PID v distribuci. U Dockeru má kontejner vlastní PID namespace, takže PID bude
   z jeho pohledu. Pokud se nebude shodovat, záložní cesta je spustit `nsenter`/`ss` přímo
   v kontejneru uzlu přes `/var/run/netns/cni-*` (k3s image obsahuje busybox, `ss` je třeba
   ověřit) nebo převést PID přes `/proc/<pid>/status` (`NSpid`).
2. **k3s v kontejneru na Podmanu** (cgroup v2, `--cgroupns=host`) a NetworkPolicy přes
   kube-router v kontejneru, ověřit na obou enginech.
3. **Rozsah NodePort přes WSL** na `localhost`: ověřit v režimech NAT i mirrored.

Co dělat, když spike selže *(rozhodnutí návrháře, kromě bodu NetworkPolicy)*:

- **Podman neutáhne k3s** → funkce bude jen pro Docker. Na Podmanu stránka ukáže vysvětlení.
- **NodePort nedojde na localhost** → rozšířit stávající `src/portrelay.rs` o přeposílání
  rozsahu NodePort.
- **PID podů nesouhlasí** → záložní cesty v bodě 1.
- **NetworkPolicy se nevynucuje** *(rozhodnutí uživatele)* → pro lokální vývoj vynucení není
  nutné. Funkce se dodá bez něj: pravidla se generují a aplikují jako YAML, mapa ukazuje
  povolené cesty, ale záložka Pravidla nese viditelnou poznámku „cluster pravidla nevynucuje“.
  „Zkusit spojení“ pak testuje jen dostupnost. Na Cilium se kvůli tomu nepřechází.

## Na rozhodnutí implementace

- Přesné verze image (aktuální stabilní k3s, busybox, postgres, postgrest, nginx).
- Rozložení mapy a vzhled, pokud zůstanou čitelné pro ~3–15 podů.
- Jestli logy podu poběží ve stávajícím okně logů, nebo ve Windows Terminalu.
- Rozdělení kódu: navrženo `src/k8s/` (čisté parsery, model, generování politik) +
  `src/worker/k8s.rs` + `src/ui/k8s.rs` (záložky), v souladu se stávající strukturou.
