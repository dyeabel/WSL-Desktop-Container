//! Překlad Windows cest na cesty ve WSL distribuci (pro docker shim).
//!
//! - `C:\data\x` / `C:/data/x`          -> `/mnt/c/data/x`
//! - `\\wsl.localhost\<distro>\home\x`  -> `/home/x`   (i `\\wsl$\<distro>\…`)
//! - `.\data`, `..\x`                   -> `./data`, `../x`
//! - `-v D:\data:/data:ro`              -> `-v /mnt/d/data:/data:ro`
//! - `--mount type=bind,source=C:\x,…`  -> `source=/mnt/c/x`
//! - `--file=C:\x\compose.yml`          -> `--file=/mnt/c/x/compose.yml`

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

/// Hodnota typu `HOST:KONTEJNER[:volby]` (volume). Host může obsahovat dvojtečku disku.
fn translate_volume(v: &str) -> String {
    if is_drive_path(v) {
        // hledáme dvojtečku za písmenem disku
        if let Some(i) = v[2..].find(':') {
            let (host, rest) = v.split_at(2 + i);
            return format!("{}{}", to_wsl_path(host), rest);
        }
        return to_wsl_path(v);
    }
    if looks_like_path(v) {
        if let Some(i) = v.find(':') {
            let (host, rest) = v.split_at(i);
            return format!("{}{}", to_wsl_path(host), rest);
        }
        return to_wsl_path(v);
    }
    v.to_string()
}

/// `type=bind,source=C:\x,target=/y` – přeloží hodnoty, které jsou cestami.
fn translate_kv_list(v: &str) -> String {
    v.split(',')
        .map(|kv| match kv.split_once('=') {
            Some((k, val)) if looks_like_path(val) => format!("{k}={}", to_wsl_path(val)),
            _ => kv.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Přepínače, jejichž hodnota je seznam `klíč=hodnota` (cesty jsou v hodnotách).
const KV_FLAGS: &[&str] = &["--mount", "--secret", "--output", "-o", "--build-context", "--cache-from", "--cache-to", "--ssh"];

/// Přepínače, jejichž hodnoty se nikdy nepřekládají (proměnné, labely, filtry…).
const RAW_FLAGS: &[&str] = &["-e", "--env", "-l", "--label", "--build-arg", "--annotation", "--filter"];

/// Hodnota „KV“ přepínače: `type=local,dest=C:\x` nebo jen cesta (`docker save -o C:\x.tar`).
fn translate_kv_value(v: &str) -> String {
    if v.contains('=') { translate_kv_list(v) } else { translate_volume(v) }
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

/// Přeloží všechny argumenty příkazu docker.
pub fn translate_args(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut prev = "";
    for a in args {
        let t = if RAW_FLAGS.contains(&prev) {
            a.clone()
        } else if matches!(prev, "-v" | "--volume") {
            translate_volume(a)
        } else if KV_FLAGS.contains(&prev) {
            translate_kv_value(a)
        } else if a.starts_with("--") && a.contains('=') {
            // --volume=…, --mount=…, --file=C:\x, --output=type=local,dest=C:\x, …
            let (k, v) = a.split_once('=').unwrap();
            if RAW_FLAGS.contains(&k) {
                a.clone()
            } else if k == "--volume" {
                format!("{k}={}", translate_volume(v))
            } else if KV_FLAGS.contains(&k) {
                format!("{k}={}", translate_kv_value(v))
            } else if looks_like_path(v) {
                format!("{k}={}", to_wsl_path(v))
            } else {
                a.clone()
            }
        } else if a.starts_with("-v") && a.len() > 2 && !a.starts_with("--") && looks_like_path(&a[2..]) {
            // -vC:\x:/y
            format!("-v{}", translate_volume(&a[2..]))
        } else if looks_like_path(a) {
            // samostatná cesta (např. build kontext, -f hodnota, docker cp)
            translate_volume(a)
        } else {
            a.clone()
        };
        out.push(t);
        prev = a.as_str();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
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
        let a = translate_args(&s(&["run", "-v", r"D:\data:/data:ro", "-v", "named:/x", "nginx:1.27"]));
        assert_eq!(a, s(&["run", "-v", "/mnt/d/data:/data:ro", "-v", "named:/x", "nginx:1.27"]));
        let a = translate_args(&s(&["run", r"--volume=C:\a b\c:/c", r".\ctx"]));
        assert_eq!(a, s(&["run", "--volume=/mnt/c/a b/c:/c", "./ctx"]));
        let a = translate_args(&s(&["run", "-v", r".\data:/data"]));
        assert_eq!(a, s(&["run", "-v", "./data:/data"]));
    }

    #[test]
    fn mount_and_files() {
        let a = translate_args(&s(&["run", "--mount", r"type=bind,source=C:\x,target=/y,readonly"]));
        assert_eq!(a[2], "type=bind,source=/mnt/c/x,target=/y,readonly");
        let a = translate_args(&s(&["compose", "-f", r"D:\p\compose.yml", "up", "-d"]));
        assert_eq!(a, s(&["compose", "-f", "/mnt/d/p/compose.yml", "up", "-d"]));
        let a = translate_args(&s(&["compose", r"--env-file=D:\p\.env", "up"]));
        assert_eq!(a[1], "--env-file=/mnt/d/p/.env");
    }

    #[test]
    fn buildkit_flags() {
        let a = translate_args(&s(&[
            "buildx", "build",
            "--secret", r"id=npm,src=C:\Users\j\.npmrc",
            "--output", r"type=local,dest=D:\out",
            "--build-context", r"shared=D:\libs\shared",
            "--ssh", r"default=C:\Users\j\.ssh\id_ed25519",
            "--cache-to", r"type=local,dest=D:\cache",
            ".",
        ]));
        assert_eq!(a[3], "id=npm,src=/mnt/c/Users/j/.npmrc");
        assert_eq!(a[5], "type=local,dest=/mnt/d/out");
        assert_eq!(a[7], "shared=/mnt/d/libs/shared");
        assert_eq!(a[9], "default=/mnt/c/Users/j/.ssh/id_ed25519");
        assert_eq!(a[11], "type=local,dest=/mnt/d/cache");
        let a = translate_args(&s(&["build", r"--output=type=tar,dest=D:\o.tar", r"--secret=id=x,src=C:\s"]));
        assert_eq!(a[1], "--output=type=tar,dest=/mnt/d/o.tar");
        assert_eq!(a[2], "--secret=id=x,src=/mnt/c/s");
        let a = translate_args(&s(&["save", "-o", r"D:\img.tar", "alpine"]));
        assert_eq!(a[2], "/mnt/d/img.tar");
    }

    #[test]
    fn raw_values_not_translated() {
        let a = translate_args(&s(&["run", "-e", r"WIN=C:\x", "--label", r"p=C:\y", r"--env=Z=D:\z", "--build-arg", r"P=C:\q", "alpine"]));
        assert_eq!(a, s(&["run", "-e", r"WIN=C:\x", "--label", r"p=C:\y", r"--env=Z=D:\z", "--build-arg", r"P=C:\q", "alpine"]));
    }

    #[test]
    fn network_share_detected() {
        let a = s(&["run", "-v", r"\\nas\data:/data", "-v", r"\\wsl.localhost\Debian\x:/x", r"--mount=type=bind,source=\\srv\s,target=/s"]);
        assert_eq!(unreachable_paths(&a), s(&[r"\\nas\data:/data", r"\\srv\s"]));
    }

    #[test]
    fn untouched() {
        let v = s(&["run", "-e", "URL=http://x:80", "-p", "8080:80", "--name", "web", "alpine", "sh", "-c", "echo hi"]);
        assert_eq!(translate_args(&v), v);
    }
}
