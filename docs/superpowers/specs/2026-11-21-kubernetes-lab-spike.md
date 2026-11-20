# Kubernetes laboratoř – výsledky ověření (spike)

2026-11-21, WSL kernel 6.18.58.1, Docker 29.1.3 (Ubuntu), Podman 5.4.2 rootful (Debian),
image `rancher/k3s:v1.33.6-k3s1`.

| Riziko | Výsledek | Zvolená cesta |
|---|---|---|
| k3s v kontejneru na Dockeru | funguje, uzel Ready ~25 s | podle návrhu |
| k3s v kontejneru na Podmanu (rootful, cgroup v2) | funguje stejně | Docker i Podman |
| NetworkPolicy (kube-router v k3s) | **vynucuje se** na obou enginech (default deny → timeout, po smazání zase prochází) | podle návrhu, poznámka „nevynucuje“ jen jako pojistka |
| NodePort `127.0.0.1:30080` z Windows (NAT) | funguje na obou | podle návrhu, bez relay |
| PID podů z `crictl` vs. distribuce | **neshodují se** (kontejner uzlu má vlastní PID namespace) | síťový namespace podu se otevírá cestou `/proc/<PID uzlu v distribuci>/root/var/run/netns/cni-…` (cesta z `crictl inspectp`) |
| `ss` v namespace podů pro mapu toků | funguje, ale **nevidí krátká spojení** (HTTP se zavře dřív, než se změří) | **změna:** toky z `conntrack -L` uvnitř uzlu (v k3s image je), záznamy drží i ~2 min po skončení (TIME_WAIT) a ukazují zdroj → ClusterIP služby → odpovídající pod |
| Bajty v conntracku | `nf_conntrack_acct=1` v namespace uzlu počty bajtů v `conntrack -L -o extended` neukázal | mapa ukazuje **počet spojení** (živých a nedávných), ne objem dat |
| Nástroj pro „Zkusit spojení“ | v k3s image není `nc` ani bash; v distribuci ano | sonda běží v distribuci: `nsenter --net=<cesta> timeout 3 bash -c '</dev/tcp/IP/PORT'`, jinak `nc -z -w 3` |

## Důležitý vedlejší nález (mimo Kubernetes)

Všechny distribuce WSL 2 sdílejí **jeden síťový namespace**. Když běží Docker v jedné distribuci
(Ubuntu), nastaví ve sdílené tabulce `iptables` politiku `FORWARD DROP`. Kontejnery Podmanu v jiné
distribuci (Debian) pak **ztratí odchozí síť** (timeouty na internet i DNS přeposílání). Týká se to
i stávající funkce „nechat předchozí primární běžet“ (`keep_previous_primary`) u kombinace Docker +
Podman. Po zastavení Dockeru politika zůstává DROP, dokud se ručně nevrátí
(`iptables -P FORWARD ACCEPT`) nebo se nerestartuje WSL. Během spiku jsem politiku vrátil na ACCEPT.

Návrh opravy (samostatný úkol): při startu Podmanu přidat do `DOCKER-USER` pravidlo `ACCEPT` pro
rozhraní `podman*`, případně na stránce WSL engine upozornit na souběh Dockeru a Podmanu.
