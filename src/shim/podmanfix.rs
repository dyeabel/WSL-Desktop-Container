//! Úpravy argumentů pro Podman tam, kde se jeho CLI liší od Docker CLI tak, že by skripty
//! psané pro Docker spadly.
//!
//! `docker events --format`: Podman šablonu vyhodnocuje nad vlastní strukturou události
//! (`.Status`, `.ID`, `.Name`, `.Image`, `.Attributes`, `.Time`, `.TimeNano`, `.Type`), Docker
//! nad `events.Message` (`.Action`, `.Actor.ID`, `.Actor.Attributes.*`, `.From`, `.Scope`).
//! Pole Dockeru se v šabloně přeloží na odpovídající pole Podmanu.
//!
//! `docker buildx`: `podman buildx` umí jen `build` (Buildah) – buildery (`ls`, `create`,
//! `use`, `inspect`, `bake`, `imagetools`…) nemá. Když je v distribuci Docker CLI s pluginem
//! buildx, pošlou se tyhle příkazy jemu s `DOCKER_HOST` na API Podmanu: driver
//! `docker-container` spustí BuildKit v kontejneru a `--load` nahraje image do Podmanu.
//! `buildx build` zůstává na Buildahu, dokud není vybraný vlastní builder (`--builder`,
//! `BUILDX_BUILDER`, `docker buildx use`) – výchozí builder `docker` nad Podmanem nefunguje.

/// Docker pole -> Podman výraz (delší dřív, aby `.Actor.Attributes.name` nepřebil `.Actor.Attributes`).
const EVENT_FIELDS: &[(&str, &str)] = &[
    (".Actor.Attributes.name", ".Name"),
    (".Actor.Attributes.image", ".Image"),
    (".Actor.Attributes.exitCode", ".ContainerExitCode"),
    (".Actor.Attributes", ".Attributes"),
    (".Actor.ID", ".ID"),
    (".Action", ".Status"),
    (".From", ".Image"),
    (".Scope", "\"local\""),
];

fn ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Nahradí `from` za `to`, jen když jde o celé pole (ne `.ActionX` ani `x.Action`).
fn replace_field(s: &str, from: &str, to: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(from) {
        let before = rest[..i].chars().next_back().or_else(|| out.chars().next_back());
        let after = rest[i + from.len()..].chars().next();
        let whole = !before.is_some_and(|c| ident_char(c) || c == '.' || c == ')') && !after.is_some_and(ident_char);
        out.push_str(&rest[..i]);
        out.push_str(if whole { to } else { from });
        rest = &rest[i + from.len()..];
    }
    out.push_str(rest);
    out
}

/// Přeloží pole Dockeru uvnitř `{{ … }}`; text mimo akce šablony zůstane.
pub fn events_template(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut rest = t;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after.find("}}").map(|e| e + 2).unwrap_or(after.len());
        let mut action = after[..end].to_string();
        for (from, to) in EVENT_FIELDS {
            action = replace_field(&action, from, to);
        }
        out.push_str(&action);
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Pozice příkazu (první nepřepínač) – globální přepínače s hodnotou jsou u shimu vzácné,
/// `--context` / `-c` už shim odebral.
fn command(args: &[String]) -> Vec<&str> {
    args.iter().map(|s| s.as_str()).filter(|a| !a.starts_with('-')).take(2).collect()
}

/// Upraví argumenty pro Podman.
pub fn rewrite(args: &[String]) -> Vec<String> {
    let cmd = command(args);
    let is_events = matches!(cmd.as_slice(), ["events", ..] | ["system", "events"]);
    if !is_events {
        return args.to_vec();
    }
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--format" {
            out.push(a.clone());
            if let Some(v) = it.next() {
                out.push(events_template(v));
            }
        } else if let Some(v) = a.strip_prefix("--format=") {
            out.push(format!("--format={}", events_template(v)));
        } else {
            out.push(a.clone());
        }
    }
    out
}

/// Rozhoduje v distribuci (skript pro `sh -c`): `$1` = URL API Podmanu, dál argumenty
/// od `buildx`. Skutečný buildx, pokud je k dispozici a příkaz ho potřebuje, jinak `podman`.
const BUILDX_SCRIPT: &str = r#"url="$1"; shift
real=0
if command -v docker >/dev/null 2>&1 && DOCKER_HOST="$url" docker buildx version >/dev/null 2>&1; then
  case "$2" in
    build|b)
      for a in "$@"; do case "$a" in --builder|--builder=*) real=1;; esac; done
      [ -n "$BUILDX_BUILDER" ] && [ "$BUILDX_BUILDER" != default ] && real=1
      cur="${DOCKER_CONFIG:-$HOME/.docker}/buildx/current"
      if [ -f "$cur" ] && ! grep -q '"Name":"default"' "$cur" && grep -q '"Name":"[^"]' "$cur"; then real=1; fi ;;
    *) real=1 ;;
  esac
fi
if [ "$real" = 1 ]; then DOCKER_HOST="$url" exec docker "$@"; fi
exec podman --url "$url" "$@""#;

/// `docker buildx …` u Podmanu: příkaz pro `wsl.exe -e` (bez argumentů), nebo None.
/// `cli` = `podman --url <url>` z `Engine::cli`.
pub fn buildx_cli(args: &[String], cli: &[String]) -> Option<Vec<String>> {
    if args.first().map(|a| a.as_str()) != Some("buildx") {
        return None;
    }
    let url = match cli {
        [p, flag, url] if p == "podman" && flag == "--url" => url.clone(),
        _ => return None,
    };
    Some(vec!["sh".into(), "-c".into(), BUILDX_SCRIPT.into(), "wslcd-buildx".into(), url])
}

/// Přepínače `docker compose` (před podpříkazem), které berou hodnotu.
const COMPOSE_VALUE_FLAGS: &[&str] =
    &["-f", "--file", "-p", "--project-name", "--project-directory", "--env-file", "--profile", "--ansi", "--progress", "--parallel"];

/// Jak vypsat `compose ps` spuštěné s `--format json`.
#[derive(Debug, Clone, PartialEq)]
pub enum PsOut {
    /// výchozí tabulka
    Table,
    /// `--format json`
    Json,
    /// vlastní šablona (`{{.Service}}={{.Status}}`)
    Tmpl(Vec<crate::fast::Seg>),
}

/// `docker compose ps` s výchozí tabulkou, `--format json` nebo jednoduchou šablonou: compose bere
/// `Status` z kompatibilního API Podmanu, kde chybí stav healthchecku (ten má jen v poli `Health`).
/// Vrací (argumenty s `--format json`, jak vypsat); None = spustit beze změny.
pub fn compose_ps(args: &[String]) -> Option<(Vec<String>, PsOut)> {
    let mut i = match args.first() {
        Some(a) if a == "compose" => 1,
        _ => return None,
    };
    while i < args.len() {
        let a = args[i].as_str();
        if COMPOSE_VALUE_FLAGS.contains(&a) {
            i += 2;
        } else if a.starts_with('-') {
            i += 1;
        } else {
            break;
        }
    }
    if args.get(i).map(|s| s.as_str()) != Some("ps") {
        return None;
    }
    let mut out: Vec<String> = args[..=i].to_vec();
    let mut mode = PsOut::Table;
    let mut it = args[i + 1..].iter();
    while let Some(a) = it.next() {
        let fmt = if a == "--format" {
            Some(it.next()?.clone())
        } else {
            a.strip_prefix("--format=").map(String::from)
        };
        match fmt.as_deref() {
            Some("json") => mode = PsOut::Json,
            Some("table") => mode = PsOut::Table,
            // `table …` s vlastními sloupci nechá na compose
            Some(t) if t.starts_with("table") => return None,
            Some(t) => {
                let segs = crate::fast::parse_template(&t.replace(r"\t", "\t").replace(r"\n", "\n")).ok()?;
                if segs.iter().any(|s| matches!(s, crate::fast::Seg::Field(p) if p.len() != 1)) {
                    return None;
                }
                mode = PsOut::Tmpl(segs);
            }
            None if matches!(a.as_str(), "-q" | "--quiet" | "--services") => return None,
            None => out.push(a.clone()),
        }
    }
    out.extend(["--format".to_string(), "json".to_string()]);
    Some((out, mode))
}

/// Doplní do `Status` stav healthchecku z pole `Health` (jako Docker: `Up 5 seconds (healthy)`).
fn with_health(c: &mut serde_json::Value) {
    let health = c.get("Health").and_then(|h| h.as_str()).unwrap_or("").to_string();
    let suffix = match health.as_str() {
        "healthy" => " (healthy)",
        "unhealthy" => " (unhealthy)",
        "starting" => " (health: starting)",
        _ => return,
    };
    let st = c.get("Status").and_then(|s| s.as_str()).unwrap_or("");
    if st.starts_with("Up") && !st.contains('(') {
        let s = format!("{st}{suffix}");
        c["Status"] = serde_json::Value::String(s);
    }
}

/// Image a porty jako u Dockeru: bez `docker.io/library/` a `localhost/`, publikované porty
/// z `Publishers` (Podman neposílá adresu, compose pak ukáže jen `80/tcp`).
fn docker_like(c: &mut serde_json::Value) {
    if let Some(img) = c.get("Image").and_then(|i| i.as_str()) {
        let short = crate::engine::short_image(img.strip_prefix("localhost/").unwrap_or(img));
        c["Image"] = serde_json::Value::String(short);
    }
    if let Some(pubs) = c.get("Publishers").and_then(|p| p.as_array()) {
        let ports: Vec<serde_json::Value> = pubs
            .iter()
            .map(|p| {
                serde_json::json!({
                    "IP": p.get("URL").and_then(|u| u.as_str()).unwrap_or(""),
                    "PrivatePort": p.get("TargetPort").and_then(|x| x.as_u64()).unwrap_or(0),
                    "PublicPort": p.get("PublishedPort").and_then(|x| x.as_u64()).unwrap_or(0),
                    "Type": p.get("Protocol").and_then(|x| x.as_str()).unwrap_or("tcp"),
                })
            })
            .collect();
        if !ports.is_empty() {
            c["Ports"] = serde_json::Value::String(crate::fast::displayable_ports(&ports));
        }
    }
}

/// Výstup `compose ps --format json` (pole, nebo objekt na řádek) -> tabulka jako Docker
/// Compose, nebo JSON s doplněným stavem. None = výstup nerozumíme (vypíše se beze změny).
pub fn render_compose_ps(stdout: &str, mode: &PsOut) -> Option<String> {
    let t = stdout.trim();
    let mut rows: Vec<serde_json::Value> = if t.is_empty() {
        vec![]
    } else if t.starts_with('[') {
        serde_json::from_str::<Vec<serde_json::Value>>(t).ok()?
    } else {
        t.lines().filter(|l| !l.trim().is_empty()).map(serde_json::from_str).collect::<Result<_, _>>().ok()?
    };
    rows.iter_mut().for_each(with_health);
    rows.iter_mut().for_each(docker_like);
    if let PsOut::Tmpl(segs) = mode {
        let mut s = String::new();
        for r in &rows {
            for seg in segs {
                match seg {
                    crate::fast::Seg::Lit(l) => s.push_str(l),
                    crate::fast::Seg::Field(p) => match r.get(p[0].as_str()) {
                        None | Some(serde_json::Value::Null) => {}
                        Some(serde_json::Value::String(v)) => s.push_str(v),
                        Some(v) => s.push_str(&v.to_string()),
                    },
                    crate::fast::Seg::JsonDot => s.push_str(&serde_json::to_string(r).ok()?),
                }
            }
            s.push('\n');
        }
        return Some(s);
    }
    if *mode == PsOut::Json {
        let mut s = String::new();
        for r in &rows {
            s.push_str(&serde_json::to_string(r).ok()?);
            s.push('\n');
        }
        return Some(s);
    }
    let f = |r: &serde_json::Value, k: &str| r.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let mut text = String::from("NAME\tIMAGE\tCOMMAND\tSERVICE\tCREATED\tSTATUS\tPORTS\n");
    for r in &rows {
        let cols = ["Name", "Image", "Command", "Service", "RunningFor", "Status", "Ports"].map(|k| f(r, k));
        text.push_str(&cols.join("\t"));
        text.push('\n');
    }
    Some(crate::fast::tabwrite(&text, 10, 3))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn events_fields_translated() {
        assert_eq!(events_template("{{.Action}}"), "{{.Status}}");
        assert_eq!(
            events_template("{{.Time}} {{.Type}} {{.Action}} {{.Actor.ID}} {{.Actor.Attributes.name}} {{.From}}"),
            "{{.Time}} {{.Type}} {{.Status}} {{.ID}} {{.Name}} {{.Image}}"
        );
        assert_eq!(events_template("{{index .Actor.Attributes \"com.docker.compose.service\"}}"), "{{index .Attributes \"com.docker.compose.service\"}}");
        assert_eq!(events_template("{{.Actor.Attributes.exitCode}}|{{.Scope}}"), "{{.ContainerExitCode}}|{{\"local\"}}");
        // text mimo akce, celá jména polí, json
        assert_eq!(events_template(".Action: {{.ActionX}} {{json .}}"), ".Action: {{.ActionX}} {{json .}}");
        assert_eq!(events_template("{{if eq .Action \"die\"}}x{{end}}"), "{{if eq .Status \"die\"}}x{{end}}");
    }

    #[test]
    fn buildx_routing() {
        let cli = a(&["podman", "--url", "unix:///run/wslcd-podman/podman.sock"]);
        let c = buildx_cli(&a(&["buildx", "ls"]), &cli).unwrap();
        assert_eq!(&c[..2], &a(&["sh", "-c"])[..]);
        assert_eq!(&c[3..], &a(&["wslcd-buildx", "unix:///run/wslcd-podman/podman.sock"])[..]);
        assert!(buildx_cli(&a(&["build", "."]), &cli).is_none());
        assert!(buildx_cli(&a(&["buildx", "ls"]), &a(&["docker"])).is_none());
    }

    #[test]
    fn compose_ps_custom_template_has_health() {
        let out = r#"{"Name":"r-cache-1","Service":"cache","Status":"Up 2 seconds","Health":"healthy","Publishers":[]}"#;
        let (_, mode) = compose_ps(&a(&["compose", "-p", "r", "ps", "--format", r"{{.Service}}={{.Status}}	{{.Health}}"])).unwrap();
        assert_eq!(render_compose_ps(out, &mode).unwrap(), "cache=Up 2 seconds (healthy)	healthy
");
    }

    #[test]
    fn compose_ps_image_and_ports_like_docker() {
        let out = r#"{"Name":"p-web-1","Image":"docker.io/library/nginx:alpine","Command":"x","Service":"web","RunningFor":"1 second ago","Status":"Up 1 second","Health":"","Ports":"80/tcp","Publishers":[{"Protocol":"tcp","PublishedPort":18080,"TargetPort":80,"URL":""}]}
{"Name":"p-app-1","Image":"localhost/compat-app:1.0","Command":"y","Service":"app","RunningFor":"1 second ago","Status":"Up 1 second","Health":"","Ports":"","Publishers":[]}"#;
        let t = render_compose_ps(out, &PsOut::Table).unwrap();
        assert!(t.contains("nginx:alpine ") && !t.contains("docker.io/"), "{t}");
        assert!(t.contains("0.0.0.0:18080->80/tcp"), "{t}");
        assert!(t.contains("compat-app:1.0") && !t.contains("localhost/"), "{t}");
        let j = render_compose_ps(out, &PsOut::Json).unwrap();
        assert!(j.contains(r#""Ports":"0.0.0.0:18080->80/tcp""#) && j.contains(r#""Image":"nginx:alpine""#), "{j}");
    }

    #[test]
    fn compose_ps_with_health() {
        let (args, mode) = compose_ps(&a(&["compose", "-f", "x.yml", "-p", "ps", "ps", "-a"])).unwrap();
        assert_eq!((args, mode), (a(&["compose", "-f", "x.yml", "-p", "ps", "ps", "-a", "--format", "json"]), PsOut::Table));
        assert_eq!(compose_ps(&a(&["compose", "ps", "--format", "json"])).unwrap().1, PsOut::Json);
        assert!(compose_ps(&a(&["compose", "ps", "-q"])).is_none());
        assert!(matches!(compose_ps(&a(&["compose", "ps", "--format", "{{.Name}}"])).unwrap().1, PsOut::Tmpl(_)));
        assert!(compose_ps(&a(&["compose", "ps", "--format", "table {{.Name}}"])).is_none());
        assert!(compose_ps(&a(&["compose", "ps", "--format", "{{.State.Health}}"])).is_none());
        assert!(compose_ps(&a(&["compose", "up", "-d"])).is_none());
        assert!(compose_ps(&a(&["ps"])).is_none());

        let out = r#"{"Command":"\"sleep 600\"","Health":"healthy","Image":"alpine:latest","Name":"p-web-1","Ports":"","RunningFor":"8 seconds ago","Service":"web","State":"running","Status":"Up 7 seconds"}
{"Command":"\"sleep 1\"","Health":"","Image":"alpine:latest","Name":"p-db-1","Ports":"0.0.0.0:5432->5432/tcp","RunningFor":"9 seconds ago","Service":"db","State":"running","Status":"Up 8 seconds"}"#;
        let t = render_compose_ps(out, &PsOut::Table).unwrap();
        let lines: Vec<&str> = t.lines().collect();
        assert!(lines[0].starts_with("NAME      IMAGE           COMMAND       SERVICE   CREATED         STATUS"), "{t}");
        assert!(lines[1].contains("Up 7 seconds (healthy)") && lines[2].contains("Up 8 seconds") && !lines[2].contains('('), "{t}");
        assert!(render_compose_ps(out, &PsOut::Json).unwrap().lines().next().unwrap().contains(r#""Status":"Up 7 seconds (healthy)""#));
        assert_eq!(render_compose_ps("", &PsOut::Table).unwrap().trim_end(), "NAME      IMAGE     COMMAND   SERVICE   CREATED   STATUS    PORTS");
        assert!(render_compose_ps("nesmysl", &PsOut::Table).is_none());
    }

    #[test]
    fn only_events_command() {
        assert_eq!(rewrite(&a(&["events", "--format", "{{.Action}}"])), a(&["events", "--format", "{{.Status}}"]));
        assert_eq!(rewrite(&a(&["system", "events", "--since", "1m", "--format={{.Actor.ID}}"])), a(&["system", "events", "--since", "1m", "--format={{.ID}}"]));
        assert_eq!(rewrite(&a(&["ps", "--format", "{{.Action}}"])), a(&["ps", "--format", "{{.Action}}"]));
        assert_eq!(rewrite(&a(&["events", "--format", "json"])), a(&["events", "--format", "json"]));
    }
}
