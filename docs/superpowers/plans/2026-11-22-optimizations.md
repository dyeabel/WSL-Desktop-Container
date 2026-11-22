# Optimalizace výkonu a paměti – plán

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Snížit práci aplikace v klidu (skryté okno), práci a alokace na snímek a počet spuštění
`wsl.exe`. Viditelné chování se nemění.

**Zdroj:** read-only analýza kódu (2026-11-22) nad větví `feature/host-linux`. Referenční čísla:
`docs/bench/2026-11-21-host-abstraction-after.md`:
- aplikace v klidu: 133 MB working set, ~100 MB privátní paměti, 0,07 % CPU;
- API: 948 req/s.

## Global Constraints

- **Žádná viditelná změna chování.** Platí pro texty, data v UI, frekvenci oznámení a historii CPU.
- **Rozhodnutí kontroléru:** položka „want_stats ve skrytém okně“ se nedělá, protože by vytvořila
  mezery v grafu CPU.
- **Bez nových crate a bez async.** Testy čisté logiky tam, kde je co testovat.
- **Bezpečnost jako v kroku 2:**
  - GUI ani shim bez `WSLCD_SHIM_AUTOSTART=0` nespouštět;
  - jen portable testovací data;
  - `CARGO_BUILD_JOBS=4`;
  - nespouštět build pro Windows a Linux současně.
- **Po všech úkolech** se pustí `scripts/bench.ps1 -Quick -Podman WSLCD-Lite` (shim má autostart
  vypnutý) a porovná se s referencí:
  - `exec true`, `docker ps` a req/s se nesmí zhoršit o víc než 10 %;
  - publikované `BENCHMARK.md` a `results.json` se nepřepisují. Výsledek se uloží jako
    `docs/bench/2026-11-22-optimizations.md/.json`.

---

### Task 1: Tray menu bez přestavby na každý snímek

`app.rs:359` volá `tray::refresh()` při každém `update()`. `tray.rs:409` klonuje
`tray_containers` a `Model::build` (`menu_model.rs:151-166`) třídí s `to_lowercase()` a počítá
hash. Výsledek se skoro vždy zahodí.

- `Shared.tray_gen: AtomicU64` se zvýší tam, kde worker zapisuje `tray_containers`
  (`worker/mod.rs`, zápis kontejnerů pro tray) a `status`.
- `tray::refresh` si v `Handles` drží `last_gen` a při shodě hned skončí.
- Test: `refresh_skips_when_gen_unchanged` nad čistou funkcí, která rozhoduje o přestavbě.

### Task 2: `/system/df` nejvýš každých 30 s

`worker/mod.rs:466` volá `d.df()` při každém obnovení přehledu (3 s). U Dockeru to může trvat
sekundy.

- `last_df` (Instant) ve `Shared`; `df()` nejvýš jednou za 30 s, jinak `None`.
- UI si ponechá poslední hodnotu. Ověřit, že `drain` (`app.rs:520`) s `None` hodnotu
  nepřemaže.
- Test: čistá funkce `due(last, now, every)`.

### Task 3: Méně spouštění `wsl.exe -l -v`

`worker/mod.rs:409` spouští `wsl::list()` při každém obnovení, i se skrytým oknem.

- Když je okno skryté a API enginu odpovídá, `wsl.exe` se nespouští: použije se poslední seznam
  a `distro_running = true`.
- `Msg::Distros` se pošle jen při změně (`Distro: PartialEq`).
- Bez změny intervalu při odpojení (kontrolér: volitelné zvýšení na 30 s se nedělá).
- Test: rozhodovací funkce `need_wsl_list(visible, connected)`.

### Task 4: Úklid map v notifikátoru (únik paměti)

Mapy `worker/notify.rs:70-84` (`intended`, `removed`, `sent`, `seen`, `testcontainers`,
`oom_killed`) rostou pro každé ID kontejneru bez omezení.

- Pravidelný `retain` starší než 1 h u map s `Instant`.
- Při `destroy` smazat ID i z `testcontainers` a `oom_killed`.
- Nesmí se změnit potlačení duplicitních oznámení v jejich okně.
- Testy: po `destroy` a po uplynutí 1 h (vložený čas) mapy neobsahují ID; oznámení v okně
  potlačení se dál nespojují špatně.

### Task 5: Detail kontejneru nevzorkuje při skrytém okně

`worker/detail.rs:47-80` volá `/stats` každé 2 s a `request_repaint` bez ohledu na viditelnost.

- Při skrytém okně čekat 500 ms bez dotazu a vynulovat `prev`, aby po zobrazení nevznikl falešný
  skok CPU.
- Test: rozhodovací funkce.

### Task 6: Seznamy bez hlubokých klonů na snímek

`ui/containers.rs:63-71` klonuje každý `Container` na každý snímek. Podobně `images.rs:354`,
`volumes.rs:101`, `volumes.rs:346`, `networks.rs:45`, `dashboard.rs:183`.

- `App.snap: Arc<Snapshot>` (přiřazení jen v `app.rs:508`) a `let snap = app.snap.clone()` na
  začátku `show()`. Iterace přes reference.
- Řazení přes `sort_by_cached_key` s malými písmeny.
- Bez změny pořadí a filtrů (testy existujícího řazení musí projít).

### Task 7: Síťový přehled bez klonu celého NetData

`ui/net_overview.rs:78` a `ui/net_overview.rs:711` klonují celé `NetData` na každý snímek.

- `Arc<NetData>`; worker zapisuje nový `Arc`, scanner distribucí použije `Arc::make_mut`.

### Task 8: Drobnosti

- `memory.rs` (`vmmem_ntquery`) alokuje 512 KB buffer při každém vzorku. Nahradit opakovaně
  použitým bufferem (`thread_local`).
- `wslconfig::read()` cachovat podle mtime.
- **Ověření:** `cargo test` na Windows i Linuxu, bench podle Global Constraints a záznam čísel
  před a po.
