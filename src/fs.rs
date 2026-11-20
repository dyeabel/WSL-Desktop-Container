//! Správa souborů uvnitř WSL distribuce – vše přes wsl.exe, takže funguje
//! pro libovolnou distribuci a volitelně i jako root.

use crate::wsl;
use std::process::Stdio;

#[derive(Debug, Clone, Default)]
pub struct Entry {
    /// typ položky (d = adresář, f = soubor, l = odkaz, …)
    pub kind: char,
    /// typ cíle (pro odkazy; jinak stejný jako kind)
    pub target_kind: char,
    pub size: u64,
    pub mtime: i64,
    pub owner: String,
    pub mode: String,
    pub name: String,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.kind == 'd' || (self.kind == 'l' && self.target_kind == 'd')
    }
    pub fn is_link(&self) -> bool {
        self.kind == 'l'
    }
}

/// Vypíše adresář. `~` se rozbalí na $HOME daného uživatele.
const LIST_SCRIPT: &str = r#"p="$1"
case "$p" in "~"|"~/"*) p="$HOME${p#\~}";; esac
cd -- "$p" || exit 1
pwd
find . -mindepth 1 -maxdepth 1 -printf '%y\t%Y\t%s\t%T@\t%u\t%m\t%f\0' 2>/dev/null
exit 0"#;

fn user(root: bool) -> Option<&'static str> {
    if root { Some("root") } else { None }
}

pub fn parse_listing(out: &str) -> (String, Vec<Entry>) {
    let (cwd, rest) = out.split_once('\n').unwrap_or((out, ""));
    let mut v: Vec<Entry> = rest
        .split('\0')
        .filter(|s| !s.is_empty())
        .filter_map(|rec| {
            let p: Vec<&str> = rec.splitn(7, '\t').collect();
            if p.len() < 7 {
                return None;
            }
            Some(Entry {
                kind: p[0].chars().next().unwrap_or('?'),
                target_kind: p[1].chars().next().unwrap_or('?'),
                size: p[2].parse().unwrap_or(0),
                mtime: p[3].split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0),
                owner: p[4].to_string(),
                mode: p[5].to_string(),
                name: p[6].to_string(),
            })
        })
        .collect();
    v.sort_by(|a, b| b.is_dir().cmp(&a.is_dir()).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    (cwd.trim().to_string(), v)
}

pub fn list(distro: &str, root: bool, path: &str) -> Result<(String, Vec<Entry>), String> {
    let out = wsl::base(distro, user(root), None)
        .args(["sh", "-c", LIST_SCRIPT, "sh", path])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let e = wsl::decode(&out.stderr);
        return Err(if e.trim().is_empty() { tf!("Adresář {path} nelze otevřít", "Cannot open directory {path}") } else { e.trim().to_string() });
    }
    Ok(parse_listing(&String::from_utf8_lossy(&out.stdout)))
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') { format!("{dir}{name}") } else { format!("{dir}/{name}") }
}

pub fn parent(path: &str) -> String {
    let t = path.trim_end_matches('/');
    match t.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => t[..i].to_string(),
    }
}

fn run(distro: &str, root: bool, args: &[&str]) -> Result<String, String> {
    Ok(wsl::run(distro, user(root), None, args)?)
}

pub fn mkdir(distro: &str, root: bool, path: &str) -> Result<String, String> {
    run(distro, root, &["mkdir", "-p", "--", path])
}

pub fn touch(distro: &str, root: bool, path: &str) -> Result<String, String> {
    run(distro, root, &["touch", "--", path])
}

pub fn rename(distro: &str, root: bool, from: &str, to: &str) -> Result<String, String> {
    run(distro, root, &["mv", "-n", "--", from, to])
}

pub fn delete(distro: &str, root: bool, paths: &[String]) -> Result<String, String> {
    let mut a: Vec<&str> = vec!["rm", "-rf", "--"];
    for p in paths {
        if p == "/" || p.is_empty() {
            return Err(t!("Mazání kořenového adresáře není povoleno", "Deleting the root directory is not allowed").into());
        }
        a.push(p);
    }
    run(distro, root, &a)
}

pub fn copy(distro: &str, root: bool, sources: &[String], dest_dir: &str) -> Result<String, String> {
    // -P: symlinky kopírovat jako symlinky, --remove-destination: nepsat skrz existující symlink v cíli
    let mut a: Vec<&str> = vec!["cp", "-rP", "--remove-destination", "--"];
    a.extend(sources.iter().map(|s| s.as_str()));
    a.push(dest_dir);
    run(distro, root, &a)
}

pub fn chmod(distro: &str, root: bool, mode: &str, path: &str) -> Result<String, String> {
    if root {
        // jako root neměnit práva cíle symlinku (třeba /etc/shadow) – F-10
        return run(distro, root, &["sh", "-c", SAFE_CHMOD, "sh", mode, path]);
    }
    run(distro, root, &["chmod", "--", mode, path])
}

/// Max. velikost souboru, který otevřeme ve vestavěném editoru.
pub const MAX_EDIT: usize = 2 * 1024 * 1024;

pub fn read_text(distro: &str, root: bool, path: &str) -> Result<String, String> {
    let out = wsl::base(distro, user(root), None)
        .args(["head", "-c", &(MAX_EDIT + 1).to_string(), "--", path])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(wsl::decode(&out.stderr).trim().to_string());
    }
    if out.stdout.len() > MAX_EDIT {
        return Err(t!(
            "Soubor je větší než 2 MB – otevři ho v Notepadu nebo terminálu",
            "The file is larger than 2 MB – open it in Notepad or a terminal"
        )
        .into());
    }
    if out.stdout.contains(&0) {
        return Err(t!("Binární soubor nelze upravit v textovém editoru", "A binary file cannot be edited in the text editor").into());
    }
    String::from_utf8(out.stdout).map_err(|_| t!("Soubor není v UTF-8", "The file is not UTF-8").to_string())
}

/// chmod, který odmítne symlink.
const SAFE_CHMOD: &str = r#"if [ -L "$2" ]; then echo "symlink – nezměněno: $2" >&2; exit 3; fi; exec chmod -- "$1" "$2""#;

/// Zápis jako root: do dočasného souboru ve stejné složce a přejmenování přes cíl, takže se
/// symlink nahradí, ne přepíše jeho cíl (F-10). Symlink rovnou odmítne; práva a vlastníka
/// převezme z původního souboru.
const SAFE_WRITE: &str = r#"f="$1"
if [ -L "$f" ]; then echo "symlink – neuloženo: $f" >&2; exit 3; fi
d=$(dirname -- "$f")
t=$(mktemp "$d/.wslcd-edit.XXXXXX") || exit 1
if ! cat > "$t"; then rm -f -- "$t"; exit 1; fi
if [ -e "$f" ]; then chmod --reference="$f" -- "$t" 2>/dev/null; chown --reference="$f" -- "$t" 2>/dev/null; else chmod 644 -- "$t"; fi
mv -fT -- "$t" "$f""#;

pub fn write_text(distro: &str, root: bool, path: &str, text: &str) -> Result<String, String> {
    let script = if root { SAFE_WRITE } else { "cat > \"$1\"" };
    Ok(wsl::run_stdin(distro, user(root), &["sh", "-c", script, "sh", path], text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_listing() {
        let out = "/home/user\nf\tf\t12\t1700000000.5\tuser\t644\tb.txt\0d\td\t4096\t1700000000.0\tuser\t755\tAdir\0l\td\t7\t1\troot\t777\tlink\0";
        let (cwd, e) = parse_listing(out);
        assert_eq!(cwd, "/home/user");
        assert_eq!(e.len(), 3);
        assert!(e[0].is_dir() && e[1].is_dir());
        assert_eq!(e[0].name, "Adir");
        assert!(e[1].is_link());
        assert_eq!(e[2].name, "b.txt");
        assert_eq!(e[2].size, 12);
        assert_eq!(e[2].mtime, 1700000000);
    }

    #[test]
    fn name_with_tab_is_kept() {
        let (_, e) = parse_listing("/\nf\tf\t1\t1\tu\t644\ta\tb\0");
        assert_eq!(e[0].name, "a\tb");
    }

    #[test]
    fn paths() {
        assert_eq!(join("/", "etc"), "/etc");
        assert_eq!(join("/etc", "docker"), "/etc/docker");
        assert_eq!(parent("/etc/docker"), "/etc");
        assert_eq!(parent("/etc"), "/");
        assert_eq!(parent("/"), "/");
    }
}
