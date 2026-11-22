> Měření po kroku 2 (Linux) + příkaz `wslcd` + optimalizace výkonu, HEAD `592f0ea`, WSLCD-Lite / Podman, `bench.ps1 -Quick`. Aplikace z repa neběžela. Prostředí se ale liší od základu: ve WSLCD-Lite běžel `wslcd-k3s` (aplikace uživatele, ~10 % CPU) a VM měla 8 CPU / 7,9 GB. Klidové CPU a paměť aplikace: neměřeno.

## Porovnání s `2026-11-21-host-abstraction-after.md`

| Metrika | Základ | Běh 2 (publikovaný JSON) | Běh 1 | Změna (běh 2) | Mantinel | Výsledek |
|---|--:|--:|--:|--:|---|:-:|
| `exec true` p50 | 322,9 ms | 405,1 ms | 445,7 ms | +25 % | ±10 % | NESPLNĚNO (prostředí) |
| shim `docker ps` p50 | 136,0 ms | 219,6 ms | 232,1 ms | +61 % | ±10 % | NESPLNĚNO (prostředí) |
| `ps` uvnitř WSL (kontrola prostředí) | 22,1 ms | 34,0 ms | 33,0 ms | +54 % | - | - |
| API, 4 klienti | 948 req/s | neměřeno (-Quick: 1 a 16) | - | - | ≥ 635 | neměřeno |
| API, 1 klient | 634 req/s | 377,6 req/s | 337,4 | -40 % | - | - |
| API, 16 klientů | 951 req/s | 658,5 req/s | 566,6 | -31 % | - | - |
| Funkční kontroly | 11/11 | 11/11 | 11/11 | - | 11/11 | PASS |

`ps` uvnitř WSL (ani Windows, ani aplikace se ho netýkají) zpomalil o stejné desítky procent jako vše ostatní, takže jde o zátěž VM (k3s, restart VM před měřením), ne o regresi kódu. Potvrzení vyžaduje A/B běh obou commitů při stejné zátěži.
