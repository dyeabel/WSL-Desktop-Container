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
    Some(vec!["sh".into(), "-c".into(), BUILDX_SCRIPT.into(), "wls-buildx".into(), url])
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
        let cli = a(&["podman", "--url", "unix:///run/wls-podman/podman.sock"]);
        let c = buildx_cli(&a(&["buildx", "ls"]), &cli).unwrap();
        assert_eq!(&c[..2], &a(&["sh", "-c"])[..]);
        assert_eq!(&c[3..], &a(&["wls-buildx", "unix:///run/wls-podman/podman.sock"])[..]);
        assert!(buildx_cli(&a(&["build", "."]), &cli).is_none());
        assert!(buildx_cli(&a(&["buildx", "ls"]), &a(&["docker"])).is_none());
    }

    #[test]
    fn only_events_command() {
        assert_eq!(rewrite(&a(&["events", "--format", "{{.Action}}"])), a(&["events", "--format", "{{.Status}}"]));
        assert_eq!(rewrite(&a(&["system", "events", "--since", "1m", "--format={{.Actor.ID}}"])), a(&["system", "events", "--since", "1m", "--format={{.ID}}"]));
        assert_eq!(rewrite(&a(&["ps", "--format", "{{.Action}}"])), a(&["ps", "--format", "{{.Action}}"]));
        assert_eq!(rewrite(&a(&["events", "--format", "json"])), a(&["events", "--format", "json"]));
    }
}
