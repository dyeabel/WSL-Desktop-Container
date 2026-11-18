//! Překlad Windows cest na cesty ve WSL distribuci (pro docker shim).
//!
//! - `C:\data\x` / `C:/data/x`          -> `/mnt/c/data/x`
//! - `\\wsl.localhost\<distro>\home\x`  -> `/home/x`   (i `\\wsl$\<distro>\…`)
//! - `.\data`, `..\x`                   -> `./data`, `../x`
//! - `~\x`, `~/x` (jen v hodnotách s cestou) -> `%USERPROFILE%\x` -> `/mnt/c/Users/…/x`
//! - `-v D:\data:/data:ro`              -> `-v /mnt/d/data:/data:ro`
//! - `--mount type=bind,source=C:\x,…`  -> `source=/mnt/c/x`
//! - `--file=C:\x\compose.yml`          -> `--file=/mnt/c/x/compose.yml`
//!
//! Přepínače jsou popsané tabulkou `RULES` (přepínač, kde platí, druh hodnoty).
//! U přepínačů s cestou (`Kind::Path`) se překládají i relativní cesty se zpětnými
//! lomítky (`conf\.env` -> `conf/.env`). Samostatné argumenty se překládají jen tehdy,
//! když jednoznačně vypadají jako Windows cesta (`C:\…`, `.\…`, `\\wsl$\…`).
//!
//! Záměrně se NEpřekládá:
//!
//! | přepínač / argument                  | proč                                                   |
//! |--------------------------------------|--------------------------------------------------------|
//! | `-e`, `--env`, `--build-arg`         | hodnoty proměnných – program v kontejneru je čte doslova |
//! | `-l`, `--label`, `--annotation`      | metadata, ne cesty                                     |
//! | `--filter`                           | filtry API                                             |
//! | `--volumes-from`                     | název kontejneru (`ctr:ro`)                            |
//! | `-w`, `--workdir`, `--entrypoint`    | cesta uvnitř kontejneru                                |
//! | `-p`, `--publish`, `--project-name`  | porty / název projektu (`compose -p`)                  |
//! | `--name`, `--format`, `--host`, `-H` | názvy, šablony, adresy                                 |
//! | `--security-opt apparmor=…`, `label=…` | profily / SELinux, ne soubory (překládá se jen `seccomp=`) |
//! | `--device /dev/x`                    | linuxová zařízení (překládá se jen Windows cesta)      |
//! | `--log-opt` mimo `path=`             | volby log driveru                                      |
//! | `context create --docker host=…`     | adresa API (kontexty řeší shim)                        |
//! | `cp ctr:/cesta`                      | strana kontejneru                                      |
//! | `~` v samostatných argumentech       | v kontejneru znamená domovskou složku kontejneru       |
//! | síťové cesty `\\server\share`        | WSL je nevidí – shim jen varuje                        |

/// Začíná řetězec Windows cestou s písmenem disku (`C:\` nebo `C:/`)?
fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/')
}

fn is_unc_wsl(s: &str) -> bool {
    let l = s.to_ascii_lowercase();
    l.starts_with(r"\\wsl.localhost\") || l.starts_with(r"\\wsl$\")
}

/// Přeloží jednu samostatnou cestu (bez dalších částí argumentu).
pub fn to_wsl_path(p: &str) -> String {
    if is_drive_path(p) {
        let drive = p[..1].to_ascii_lowercase();
        let rest = p[2..].replace('\\', "/");
        let rest = rest.trim_end_matches('/');
        return if rest.is_empty() { format!("/mnt/{drive}") } else { format!("/mnt/{drive}{rest}") };
    }
    if is_unc_wsl(p) {
        // \\wsl.localhost\Distro\home\x -> /home/x
        let parts: Vec<&str> = p.trim_start_matches('\\').splitn(3, '\\').collect();
        let rest = parts.get(2).copied().unwrap_or("");
        return format!("/{}", rest.replace('\\', "/"));
    }
    if p.starts_with(r".\") || p.starts_with(r"..\") || p == "." || p == ".." {
        return p.replace('\\', "/");
    }
    p.to_string()
}

fn looks_like_path(s: &str) -> bool {
    is_drive_path(s) || is_unc_wsl(s) || s.starts_with(r".\") || s.starts_with(r"..\")
}

/// `~`, `~\x`, `~/x` -> domovská složka Windows (jen když je známá).
fn expand_home(p: &str, home: Option<&str>) -> Option<String> {
    let home = home.filter(|h| !h.is_empty())?;
    let rest = if p == "~" {
        ""
    } else {
        p.strip_prefix(r"~\").or_else(|| p.strip_prefix("~/"))?
    };
    let h = home.trim_end_matches(['\\', '/']);
    Some(if rest.is_empty() { h.to_string() } else { format!("{h}\\{rest}") })
}

/// Hodnota přepínače, která je cestou na hostiteli: Windows cesta, `~`, nebo relativní cesta
/// se zpětnými lomítky. `-` (stdin/stdout) a URL zůstávají.
fn translate_path(v: &str, home: Option<&str>) -> String {
    if let Some(h) = expand_home(v, home) {
        return to_wsl_path(&h);
    }
    if looks_like_path(v) {
        return to_wsl_path(v);
    }
    if v.contains('\\') && !v.starts_with(r"\\") && !v.contains("://") {
        return v.replace('\\', "/");
    }
    v.to_string()
}

/// Hodnota typu `HOST:KONTEJNER[:volby]` (volume, device). Host může obsahovat dvojtečku disku.
fn translate_volume(v: &str, home: Option<&str>) -> String {
    if is_drive_path(v) {
        // hledáme dvojtečku za písmenem disku
        if let Some(i) = v[2..].find(':') {
            let (host, rest) = v.split_at(2 + i);
            return format!("{}{}", to_wsl_path(host), rest);
        }
        return to_wsl_path(v);
    }
    let (host, rest) = match v.find(':') {
        Some(i) => v.split_at(i),
        None => (v, ""),
    };
    if let Some(h) = expand_home(host, home) {
        return format!("{}{rest}", to_wsl_path(&h));
    }
    if looks_like_path(host) {
        return format!("{}{rest}", to_wsl_path(host));
    }
    v.to_string()
}

/// Klíče v `k=v` seznamech, jejichž hodnota je vždy cesta na hostiteli
/// (u nich se překládají i relativní cesty se zpětnými lomítky).
const HOST_PATH_KEYS: &[&str] = &["src", "source", "dest", "destination", "path", "from"];

/// `type=bind,source=C:\x,target=/y` – přeloží hodnoty, které jsou cestami.
fn translate_kv_list(v: &str, home: Option<&str>) -> String {
    v.split(',')
        .map(|kv| match kv.split_once('=') {
            Some((k, val)) if looks_like_path(val) => format!("{k}={}", to_wsl_path(val)),
            Some((k, val)) if HOST_PATH_KEYS.contains(&k.trim().to_ascii_lowercase().as_str()) => format!("{k}={}", translate_path(val, home)),
            _ => kv.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Hodnota „KV“ přepínače: `type=local,dest=C:\x` nebo jen cesta (`docker save -o C:\x.tar`).
fn translate_kv_value(v: &str, home: Option<&str>) -> String {
    if v.contains('=') { translate_kv_list(v, home) } else { translate_path(v, home) }
}

/// `--security-opt seccomp=C:\p.json` (i `seccomp:`); ostatní volby beze změny.
fn translate_security_opt(v: &str, home: Option<&str>) -> String {
    for sep in ['=', ':'] {
        if let Some(rest) = v.strip_prefix("seccomp").and_then(|r| r.strip_prefix(sep)) {
            if rest == "unconfined" {
                return v.to_string();
            }
            return format!("seccomp{sep}{}", translate_path(rest, home));
        }
    }
    v.to_string()
}

/// Druh hodnoty přepínače.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// cesta na hostiteli (i relativní se `\`)
    Path,
    /// jako Path, ale jen když hodnota není šablona ani `k=v` (`-f` je i `--filter` / `--format`)
    PathLoose,
    /// `HOST:KONTEJNER[:volby]`
    Volume,
    /// `k=v,k=v` nebo jen cesta
    Kv,
    /// `seccomp=<soubor>`
    SecurityOpt,
    /// nikdy nepřekládat
    Raw,
}

/// Kde přepínač platí.
#[derive(Debug, Clone, Copy)]
enum Scope {
    /// kdekoli
    Any,
    /// jen globální přepínač před podpříkazem
    Global,
    /// jen v daných podpříkazech (`load`, `image load`, …)
    Cmds(&'static [&'static str]),
}

struct Rule {
    flags: &'static [&'static str],
    scope: Scope,
    kind: Kind,
}

const RULES: &[Rule] = &[
    // globální přepínače Docker CLI
    Rule { flags: &["--config", "--tlscacert", "--tlscert", "--tlskey"], scope: Scope::Global, kind: Kind::Path },
    Rule { flags: &["-c", "--context", "-H", "--host", "-l", "--log-level"], scope: Scope::Global, kind: Kind::Raw },
    // svazky a zařízení
    Rule { flags: &["-v", "--volume", "--device"], scope: Scope::Any, kind: Kind::Volume },
    Rule { flags: &["--mount", "--secret", "--ssh", "--output", "-o", "--build-context", "--cache-from", "--cache-to", "--log-opt"], scope: Scope::Any, kind: Kind::Kv },
    Rule { flags: &["--security-opt"], scope: Scope::Any, kind: Kind::SecurityOpt },
    // soubory
    Rule { flags: &["--env-file", "--label-file", "--cidfile", "--iidfile", "--metadata-file", "--file", "--project-directory", "--input"], scope: Scope::Any, kind: Kind::Path },
    Rule { flags: &["-f"], scope: Scope::Any, kind: Kind::PathLoose },
    Rule { flags: &["-i"], scope: Scope::Cmds(&["load", "image load"]), kind: Kind::Path },
    // nikdy
    Rule {
        flags: &[
            "-e", "--env", "-l", "--label", "--build-arg", "--annotation", "--filter", "--volumes-from", "-w", "--workdir",
            "--entrypoint", "-p", "--publish", "--project-name", "--name", "--format", "--docker", "-H", "--host",
        ],
        scope: Scope::Any,
        kind: Kind::Raw,
    },
];

/// Druh hodnoty přepínače `flag` (None = přepínač bez pravidla).
fn rule_for(flag: &str, global: bool, cmd: &str) -> Option<Kind> {
    RULES
        .iter()
        .filter(|r| match r.scope {
            Scope::Any => !global,
            Scope::Global => global,
            Scope::Cmds(cs) => !global && cs.contains(&cmd),
        })
        .find(|r| r.flags.contains(&flag))
        .map(|r| r.kind)
}

/// Globální přepínače s hodnotou (kvůli nalezení podpříkazu).
const GLOBAL_VALUE_FLAGS: &[&str] = &["--config", "-c", "--context", "-H", "--host", "-l", "--log-level", "--tlscacert", "--tlscert", "--tlskey"];
/// Skupiny, u kterých je podpříkaz dvouslovný (`image load`, `container cp`).
const GROUPS: &[&str] = &["image", "container", "buildx", "builder", "volume", "network", "system", "context"];

/// Index prvního argumentu za globálními přepínači a klíč podpříkazu (`run`, `image load`).
fn command_key(args: &[String]) -> (usize, String) {
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') && args[i] != "-" {
        i += if GLOBAL_VALUE_FLAGS.contains(&args[i].as_str()) { 2 } else { 1 };
    }
    let Some(first) = args.get(i) else {
        return (i, String::new());
    };
    if GROUPS.contains(&first.as_str()) {
        if let Some(second) = args.get(i + 1).filter(|s| !s.starts_with('-')) {
            return (i, format!("{first} {second}"));
        }
    }
    (i, first.clone())
}

fn apply(kind: Kind, v: &str, home: Option<&str>) -> String {
    match kind {
        Kind::Path => translate_path(v, home),
        Kind::PathLoose if v.contains("{{") || v.contains('=') => v.to_string(),
        Kind::PathLoose => translate_path(v, home),
        Kind::Volume => translate_volume(v, home),
        Kind::Kv => translate_kv_value(v, home),
        Kind::SecurityOpt => translate_security_opt(v, home),
        Kind::Raw => v.to_string(),
    }
}

/// `docker cp` – strana `kontejner:cesta` zůstává, místní cesta se přeloží.
fn translate_cp_arg(a: &str, home: Option<&str>) -> String {
    if !is_drive_path(a) {
        if let Some(i) = a.find(':') {
            // jednopísmenný „kontejner“ by kolidoval s diskem; `C:` bere jako disk i Docker
            if i > 1 || !a.as_bytes()[0].is_ascii_alphabetic() {
                return a.to_string();
            }
        }
    }
    translate_path(a, home)
}

/// Síťové cesty (\\server\share), které WSL nevidí jako /mnt/... – vrátí je pro varování.
pub fn unreachable_paths(args: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for a in args {
        for part in std::iter::once(a.as_str()).chain(a.split(['=', ','])) {
            if part.starts_with(r"\\") && !is_unc_wsl(part) && !out.iter().any(|x: &String| x == part) {
                out.push(part.to_string());
            }
        }
    }
    out
}

/// Přeloží všechny argumenty příkazu docker (domovská složka z `USERPROFILE`).
pub fn translate_args(args: &[String]) -> Vec<String> {
    let home = std::env::var("USERPROFILE").ok();
    translate_args_with(args, home.as_deref())
}

/// Přeloží všechny argumenty příkazu docker.
pub fn translate_args_with(args: &[String], home: Option<&str>) -> Vec<String> {
    let (cmd_start, cmd) = command_key(args);
    let positional_path = matches!(cmd.as_str(), "import" | "image import");
    let is_cp = matches!(cmd.as_str(), "cp" | "container cp");
    let mut out = Vec::with_capacity(args.len());
    // druh hodnoty, kterou čeká předchozí přepínač
    let mut pending: Option<Kind> = None;
    let mut after_dashdash = false;
    for (idx, a) in args.iter().enumerate() {
        let global = idx < cmd_start;
        if let Some(kind) = pending.take() {
            out.push(apply(kind, a, home));
            continue;
        }
        if a == "--" {
            after_dashdash = true;
            out.push(a.clone());
            continue;
        }
        let t = if !after_dashdash && a.starts_with("--") && a.contains('=') {
            // --volume=…, --mount=…, --file=C:\x, --output=type=local,dest=C:\x, …
            let (k, v) = a.split_once('=').unwrap();
            match rule_for(k, global, &cmd) {
                Some(kind) => format!("{k}={}", apply(kind, v, home)),
                None if looks_like_path(v) => format!("{k}={}", to_wsl_path(v)),
                None => a.clone(),
            }
        } else if !after_dashdash && a.starts_with('-') && a.len() > 1 {
            if let Some(kind) = rule_for(a, global, &cmd) {
                pending = Some(kind);
                a.clone()
            } else if a.starts_with("-v") && !a.starts_with("--") && a.len() > 2 && looks_like_path(&a[2..]) {
                // -vC:\x:/y
                format!("-v{}", translate_volume(&a[2..], home))
            } else {
                a.clone()
            }
        } else if idx > cmd_start && is_cp {
            translate_cp_arg(a, home)
        } else if idx > cmd_start && positional_path && !a.starts_with('-') {
            translate_path(a, home)
        } else if looks_like_path(a) {
            // samostatná cesta (např. build kontext)
            translate_volume(a, home)
        } else {
            a.clone()
        };
        out.push(t);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    const HOME: Option<&str> = Some(r"C:\Users\jan");

    fn t(v: &[&str]) -> Vec<String> {
        translate_args_with(&s(v), HOME)
    }

    #[test]
    fn paths() {
        assert_eq!(to_wsl_path(r"C:\data\x"), "/mnt/c/data/x");
        assert_eq!(to_wsl_path("D:/proj/app/"), "/mnt/d/proj/app");
        assert_eq!(to_wsl_path(r"E:\"), "/mnt/e");
        assert_eq!(to_wsl_path(r"\\wsl.localhost\Debian\home\user\app"), "/home/user/app");
        assert_eq!(to_wsl_path(r"\\wsl$\Ubuntu\etc"), "/etc");
        assert_eq!(to_wsl_path(r".\data"), "./data");
        assert_eq!(to_wsl_path("nginx:alpine"), "nginx:alpine");
    }

    #[test]
    fn volumes() {
        let a = t(&["run", "-v", r"D:\data:/data:ro", "-v", "named:/x", "nginx:1.27"]);
        assert_eq!(a, s(&["run", "-v", "/mnt/d/data:/data:ro", "-v", "named:/x", "nginx:1.27"]));
        let a = t(&["run", r"--volume=C:\a b\c:/c", r".\ctx"]);
        assert_eq!(a, s(&["run", "--volume=/mnt/c/a b/c:/c", "./ctx"]));
        let a = t(&["run", "-v", r".\data:/data"]);
        assert_eq!(a, s(&["run", "-v", "./data:/data"]));
    }

    #[test]
    fn volumes_relative_and_home() {
        assert_eq!(t(&["run", "-v", r"..\shared:/s", "x"])[2], "../shared:/s");
        assert_eq!(t(&["run", "-v", r"~\data:/data:ro", "x"])[2], "/mnt/c/Users/jan/data:/data:ro");
        assert_eq!(t(&["run", "-v", "~/data:/data", "x"])[2], "/mnt/c/Users/jan/data:/data");
        assert_eq!(t(&["run", "-v", "~:/home", "x"])[2], "/mnt/c/Users/jan:/home");
        assert_eq!(t(&["run", r"-v~\x:/x", "x"])[1], r"-v~\x:/x"); // nespojitelná forma zůstává
        assert_eq!(t(&["run", r"-v.\x:/x", "x"])[1], "-v./x:/x");
        // bez známé domovské složky se ~ nechává
        assert_eq!(translate_args_with(&s(&["run", "-v", "~/d:/d"]), None)[2], "~/d:/d");
        // ~ v samostatném argumentu (příkaz v kontejneru) se nepřekládá
        assert_eq!(t(&["exec", "web", "ls", "~/x"])[3], "~/x");
    }

    #[test]
    fn mount_and_files() {
        let a = t(&["run", "--mount", r"type=bind,source=C:\x,target=/y,readonly"]);
        assert_eq!(a[2], "type=bind,source=/mnt/c/x,target=/y,readonly");
        let a = t(&["run", "--mount", r"type=bind,src=data\x,dst=/y"]);
        assert_eq!(a[2], "type=bind,src=data/x,dst=/y");
        let a = t(&["run", r"--mount=type=bind,src=~\cfg,dst=/cfg"]);
        assert_eq!(a[1], "--mount=type=bind,src=/mnt/c/Users/jan/cfg,dst=/cfg");
        let a = t(&["run", "--mount", "type=volume,source=myvol,target=/v"]);
        assert_eq!(a[2], "type=volume,source=myvol,target=/v");
        let a = t(&["compose", "-f", r"D:\p\compose.yml", "up", "-d"]);
        assert_eq!(a, s(&["compose", "-f", "/mnt/d/p/compose.yml", "up", "-d"]));
        let a = t(&["compose", r"--env-file=D:\p\.env", "up"]);
        assert_eq!(a[1], "--env-file=/mnt/d/p/.env");
    }

    #[test]
    fn file_flags() {
        for f in ["--env-file", "--label-file", "--cidfile", "--iidfile", "--metadata-file"] {
            let a = t(&["run", f, r"C:\x\f.txt", "alpine"]);
            assert_eq!(a[2], "/mnt/c/x/f.txt", "{f}");
            let a = t(&["run", f, r"conf\f.txt", "alpine"]);
            assert_eq!(a[2], "conf/f.txt", "{f}");
            let a = t(&["run", &format!("{f}=conf\\f.txt"), "alpine"]);
            assert_eq!(a[1], format!("{f}=conf/f.txt"), "{f}");
            assert_eq!(t(&["run", f, ".env"])[2], ".env");
        }
        assert_eq!(t(&["build", "-f", r"docker\Dockerfile.dev", "."])[2], "docker/Dockerfile.dev");
        assert_eq!(t(&["build", "--file", r"C:\p\Dockerfile", r"C:\p"]), s(&["build", "--file", "/mnt/c/p/Dockerfile", "/mnt/c/p"]));
        assert_eq!(t(&["buildx", "build", "--iidfile", r"out\id.txt", "--metadata-file", r"D:\m.json", "."]), s(&["buildx", "build", "--iidfile", "out/id.txt", "--metadata-file", "/mnt/d/m.json", "."]));
        // -f jako filtr / formát / force zůstává
        assert_eq!(t(&["ps", "-f", r"label=p=a\b"])[2], r"label=p=a\b");
        assert_eq!(t(&["inspect", "-f", r"{{.Config.Env}}\t", "x"])[2], r"{{.Config.Env}}\t");
        assert_eq!(t(&["rm", "-f", "web"]), s(&["rm", "-f", "web"]));
    }

    #[test]
    fn compose_flags() {
        let a = t(&["compose", "-f", r"a\compose.yml", "-f", r"D:\b\override.yml", "--env-file", r"cfg\.env", "--project-directory", r"..\proj", "-p", r"my\proj", "up"]);
        assert_eq!(a, s(&["compose", "-f", "a/compose.yml", "-f", "/mnt/d/b/override.yml", "--env-file", "cfg/.env", "--project-directory", "../proj", "-p", r"my\proj", "up"]));
        assert_eq!(t(&["compose", r"--project-directory=C:\p", "ps"])[1], "--project-directory=/mnt/c/p");
        assert_eq!(t(&["compose", "--project-name", r"a\b", "ps"])[2], r"a\b");
    }

    #[test]
    fn device_and_security() {
        assert_eq!(t(&["run", "--device", "/dev/sda:/dev/xvda:rwm", "x"])[2], "/dev/sda:/dev/xvda:rwm");
        assert_eq!(t(&["run", "--device", r"C:\dev\x:/dev/x", "x"])[2], "/mnt/c/dev/x:/dev/x");
        assert_eq!(t(&["run", "--security-opt", r"seccomp=C:\p\sec.json", "x"])[2], "seccomp=/mnt/c/p/sec.json");
        assert_eq!(t(&["run", r"--security-opt=seccomp=prof\sec.json", "x"])[1], "--security-opt=seccomp=prof/sec.json");
        assert_eq!(t(&["run", "--security-opt", "seccomp=unconfined", "x"])[2], "seccomp=unconfined");
        assert_eq!(t(&["run", "--security-opt", "apparmor=docker-default", "x"])[2], "apparmor=docker-default");
        assert_eq!(t(&["run", "--security-opt", r"label=type:a\b", "x"])[2], r"label=type:a\b");
        assert_eq!(t(&["run", "--log-opt", r"path=C:\logs\c.json", "x"])[2], "path=/mnt/c/logs/c.json");
        assert_eq!(t(&["run", "--log-opt", "max-size=10m", "x"])[2], "max-size=10m");
        assert_eq!(t(&["run", "--volumes-from", "data:ro", "x"])[2], "data:ro");
    }

    #[test]
    fn cp() {
        assert_eq!(t(&["cp", r"C:\x\file", "ctr:/path"]), s(&["cp", "/mnt/c/x/file", "ctr:/path"]));
        assert_eq!(t(&["cp", "ctr:/path", r"D:\out"]), s(&["cp", "ctr:/path", "/mnt/d/out"]));
        assert_eq!(t(&["cp", r"dir\file.txt", r"ctr:/a\b"]), s(&["cp", "dir/file.txt", r"ctr:/a\b"]));
        assert_eq!(t(&["container", "cp", "-a", "ctr:/x", r".\x"]), s(&["container", "cp", "-a", "ctr:/x", "./x"]));
        assert_eq!(t(&["cp", "-", "ctr:/x"]), s(&["cp", "-", "ctr:/x"]));
    }

    #[test]
    fn save_load_import_export() {
        assert_eq!(t(&["save", "-o", r"D:\img.tar", "alpine"])[2], "/mnt/d/img.tar");
        assert_eq!(t(&["save", "--output", r"out\img.tar", "alpine"])[2], "out/img.tar");
        assert_eq!(t(&["image", "save", r"--output=out\img.tar", "alpine"])[2], "--output=out/img.tar");
        assert_eq!(t(&["load", "-i", r"img\a.tar"])[2], "img/a.tar");
        assert_eq!(t(&["image", "load", "--input", r"C:\a.tar"])[3], "/mnt/c/a.tar");
        assert_eq!(t(&["export", "-o", r"x\c.tar", "web"])[2], "x/c.tar");
        assert_eq!(t(&["import", r"x\c.tar", "img:1"]), s(&["import", "x/c.tar", "img:1"]));
        assert_eq!(t(&["import", "https://example.com/c.tar"])[1], "https://example.com/c.tar");
        // -i mimo load = interaktivní režim
        assert_eq!(t(&["run", "-i", r"img\x"])[2], r"img\x");
    }

    #[test]
    fn buildkit_flags() {
        let a = t(&[
            "buildx", "build",
            "--secret", r"id=npm,src=C:\Users\j\.npmrc",
            "--output", r"type=local,dest=D:\out",
            "--build-context", r"shared=D:\libs\shared",
            "--ssh", r"default=C:\Users\j\.ssh\id_ed25519",
            "--cache-to", r"type=local,dest=D:\cache",
            ".",
        ]);
        assert_eq!(a[3], "id=npm,src=/mnt/c/Users/j/.npmrc");
        assert_eq!(a[5], "type=local,dest=/mnt/d/out");
        assert_eq!(a[7], "shared=/mnt/d/libs/shared");
        assert_eq!(a[9], "default=/mnt/c/Users/j/.ssh/id_ed25519");
        assert_eq!(a[11], "type=local,dest=/mnt/d/cache");
        let a = t(&["build", r"--output=type=tar,dest=D:\o.tar", r"--secret=id=x,src=C:\s"]);
        assert_eq!(a[1], "--output=type=tar,dest=/mnt/d/o.tar");
        assert_eq!(a[2], "--secret=id=x,src=/mnt/c/s");
        // relativní cesty u klíčů s cestou na hostiteli
        let a = t(&["buildx", "build", "--output", r"type=local,dest=out\dir", "--secret", r"id=x,source=sec\t.txt", "--cache-from", r"type=local,src=cache\in", "."]);
        assert_eq!((a[3].as_str(), a[5].as_str(), a[7].as_str()), ("type=local,dest=out/dir", "id=x,source=sec/t.txt", "type=local,src=cache/in"));
        let a = t(&["save", "-o", r"D:\img.tar", "alpine"]);
        assert_eq!(a[2], "/mnt/d/img.tar");
    }

    #[test]
    fn global_flags() {
        assert_eq!(t(&["--config", r"C:\Users\jan\.docker", "ps"]), s(&["--config", "/mnt/c/Users/jan/.docker", "ps"]));
        assert_eq!(t(&[r"--config=~\.docker", "ps"])[0], "--config=/mnt/c/Users/jan/.docker");
        assert_eq!(t(&["-l", "debug", "--tlscacert", r"certs\ca.pem", "ps"])[3], "certs/ca.pem");
        // `service create --config` je název swarm configu, ne cesta
        assert_eq!(t(&["service", "create", "--config", r"a\b", "nginx"])[3], r"a\b");
        // context create --docker host=… zůstává
        assert_eq!(t(&["context", "create", "x", "--docker", "host=tcp://127.0.0.1:2375"])[4], "host=tcp://127.0.0.1:2375");
    }

    #[test]
    fn command_keys() {
        assert_eq!(command_key(&s(&["-l", "debug", "image", "load", "-i", "x"])), (2, "image load".into()));
        assert_eq!(command_key(&s(&["--context", "x", "run", "alpine"])), (2, "run".into()));
        assert_eq!(command_key(&s(&["image", "--help"])), (0, "image".into()));
        assert_eq!(command_key(&s(&[])), (0, String::new()));
    }

    #[test]
    fn raw_values_not_translated() {
        let a = t(&["run", "-e", r"WIN=C:\x", "--label", r"p=C:\y", r"--env=Z=D:\z", "--build-arg", r"P=C:\q", "-w", r"C:\app", "alpine"]);
        assert_eq!(a, s(&["run", "-e", r"WIN=C:\x", "--label", r"p=C:\y", r"--env=Z=D:\z", "--build-arg", r"P=C:\q", "-w", r"C:\app", "alpine"]));
        // za `--` jde příkaz kontejneru – jen jednoznačné Windows cesty
        assert_eq!(t(&["run", "alpine", "--", "--file", r"a\b"]), s(&["run", "alpine", "--", "--file", r"a\b"]));
    }

    #[test]
    fn network_share_detected() {
        let a = s(&["run", "-v", r"\\nas\data:/data", "-v", r"\\wsl.localhost\Debian\x:/x", r"--mount=type=bind,source=\\srv\s,target=/s"]);
        assert_eq!(unreachable_paths(&a), s(&[r"\\nas\data:/data", r"\\srv\s"]));
        assert_eq!(t(&["run", "--env-file", r"\\nas\e.env"])[2], r"\\nas\e.env");
    }

    #[test]
    fn untouched() {
        let v = s(&["run", "-e", "URL=http://x:80", "-p", "8080:80", "--name", "web", "alpine", "sh", "-c", "echo hi"]);
        assert_eq!(translate_args_with(&v, HOME), v);
    }
}
