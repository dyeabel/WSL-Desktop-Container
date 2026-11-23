//! Živý test výpočtu stropu paměti (jen čtení, `WSLCD_GUARD_DRY=1`).
//! Spuštění: `cargo test live_guard_compute -- --ignored --nocapture`

use crate::guard::GUARD_SH;
use crate::wsl;

#[test]
#[ignore]
fn live_guard_compute() {
    let distro = std::env::var("WSLCD_DISTRO").unwrap_or_else(|_| "Ubuntu".into());
    let rows: [(u64, &str, Option<&str>); 5] = [
        (16777216, "0", Some("max=14602889216 high=13142600294 reserve=2576979968")),
        (4194304, "0", Some("max=3221225472 high=2899102924 reserve=1073741824")),
        (16777216, "2048", Some("max=15032385536 high=13529146982 reserve=2147483648")),
        (2097152, "1536", Some("max=536870912 high=483183820 reserve=1610612736")),
        (1048576, "0", None),
    ];
    for (t, mb, want) in rows {
        let out = wsl::run(
            &distro, None, None,
            &["env", "WSLCD_GUARD_DRY=1", &format!("WSLCD_GUARD_MEMTOTAL_KB={t}"), "sh", "-c", GUARD_SH, "guard", mb],
        )
        .unwrap();
        let line = out.lines().find_map(|l| l.strip_prefix("wslcd-guard: "));
        match want {
            Some(w) => assert_eq!(line, Some(w), "{t} {mb}"),
            None => assert!(line.is_none() && out.contains("VAROVANI"), "{t} {mb}: {out}"),
        }
    }
}
