# A/B benchmark – main (krok 1, 7cf0ec6) vs feature/host-linux (5c21e51)

`scripts/bench.ps1 -Quick -Podman WSLCD-Lite`, střídavě A, B, A, B (10:27–10:40, 2026-11-22). Stejné
podmínky: běžel `wslcd-k3s` a nainstalovaná aplikace (PID 34296).

| Metrika (p50, průměr 2 běhů) | A (main) | B (větev) | Rozdíl |
|---|---|---|---|
| `wsl.exe -e true` | 433,6 ms | 441,2 ms | +2 % |
| shim `docker version` | 174,5 ms | 172,8 ms | −1 % |
| shim `docker ps` | 224,6 ms | 295,5 ms (364,8 / 226,1) | jeden odlehlý běh B; druhý běh = A |
| API `ps` (TLS) | 2,1 ms | 2,4 ms | šum |
| `run --rm` přes shim | 1146 ms | 1055 ms | −8 % |
| API 1 klient | 357 req/s | 406 req/s | +14 % |
| API 16 klientů | 702 req/s | 688 req/s | −2 % |
| CPU aplikace v klidu | 0,23 % | 0,17 % | −27 % |
| Working set aplikace | 79 MB | 84 MB | +6 % (rozptyl 78–90) |

**Závěr:**
- Žádná regrese větve, rozdíly jsou v šumu běhu.
- Horší čísla v Task 8 proti starší referenci způsobilo zatížené prostředí: k3s a běžící aplikace. Dokládá to i `ps` uvnitř WSL, který na kódu aplikace nezávisí.
- Kontroly po běhu:
  - klíč Run ukazuje na `C:\WSLContainerDesktop\…\--autostart`;
  - běží jen nainstalovaná aplikace;
  - hash `config.json` je beze změny;
  - iptables FORWARD je ACCEPT.
