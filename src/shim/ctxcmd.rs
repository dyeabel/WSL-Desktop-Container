//! `docker context …` – kontexty WLS (distribuce + engine + port + TLS), viz `contexts.rs`.
//!
//! Příkazy se vyřizují přímo v shimu (do WSL se nepředávají):
//! `ls` (`-q`, `--format`), `show`, `use`, `create`, `rm`, `inspect`.

use crate::contexts::{self, Context, Store, Target, DEFAULT};
use crate::fast::{go_json_str, parse_template, tabwrite, Seg};
use serde::Serialize;
use std::path::PathBuf;

/// Výsledek příkazu: stdout, stderr, návratový kód.
#[derive(Debug, Default, PartialEq)]
pub struct Res {
    pub out: String,
    pub err: String,
    pub code: u8,
}

fn fail(msg: impl Into<String>) -> Res {
    Res { out: String::new(), err: format!("{}\n", msg.into()), code: 1 }
}

/// Prostředí příkazu (pro testy nahraditelné).
pub struct CtxEnv<'a> {
    pub store_path: PathBuf,
    pub default: Target,
    /// hodnota DOCKER_CONTEXT
    pub env_ctx: Option<String>,
    /// seznam WSL distribucí (wsl.exe -l -q)
    pub distros: &'a dyn Fn() -> Result<Vec<String>, String>,
}

struct Row {
    name: String,
    description: String,
    endpoint: String,
    current: bool,
}

fn default_description(t: &Target) -> String {
    format!("WLS Desktop settings ({}, {})", t.distro, t.engine.name())
}

fn rows(store: &Store, env: &CtxEnv, current: &str) -> Vec<Row> {
    let mut out = vec![Row { name: DEFAULT.into(), description: default_description(&env.default), endpoint: env.default.endpoint(), current: current == DEFAULT }];
    let mut cs: Vec<&Context> = store.contexts.iter().collect();
    cs.sort_by(|a, b| a.name.cmp(&b.name));
    for c in cs {
        out.push(Row { name: c.name.clone(), description: c.description.clone(), endpoint: c.target().endpoint(), current: c.name == current });
    }
    out
}

const LS_FIELDS: &[&str] = &["Name", "Description", "DockerEndpoint", "Current", "Error"];

fn ls_field(r: &Row, f: &str) -> String {
    match f {
        "Name" => r.name.clone(),
        "Description" => r.description.clone(),
        "DockerEndpoint" => r.endpoint.clone(),
        "Current" => r.current.to_string(),
        _ => String::new(),
    }
}

fn ls_json(r: &Row) -> String {
    format!(
        "{{\"Current\":{},\"Description\":{},\"DockerEndpoint\":{},\"Error\":\"\",\"Name\":{}}}",
        r.current,
        go_json_str(&r.description),
        go_json_str(&r.endpoint),
        go_json_str(&r.name)
    )
}

fn render(segs: &[Seg], r: &Row) -> Result<String, String> {
    let mut s = String::new();
    for seg in segs {
        match seg {
            Seg::Lit(l) => s.push_str(l),
            Seg::Field(p) if p.len() == 1 && LS_FIELDS.contains(&p[0].as_str()) => s.push_str(&ls_field(r, &p[0])),
            Seg::Field(p) => return Err(format!("template: unsupported field .{}", p.join("."))),
            Seg::JsonDot => s.push_str(&ls_json(r)),
        }
    }
    Ok(s)
}

/// Tabulka / formát pro `context ls`.
fn ls_output(rows: &[Row], quiet: bool, format: Option<&str>) -> Result<String, String> {
    let prep = |s: &str| s.trim_matches(' ').replace(r"\t", "\t").replace(r"\n", "\n");
    let mut out = String::new();
    match format {
        _ if quiet => rows.iter().for_each(|r| {
            out.push_str(&r.name);
            out.push('\n');
        }),
        Some("json") => rows.iter().for_each(|r| {
            out.push_str(&ls_json(r));
            out.push('\n');
        }),
        Some(f) if !f.starts_with("table") => {
            let segs = parse_template(&prep(f)).map_err(|e| format!("template parsing error: {e}"))?;
            for r in rows {
                out.push_str(&render(&segs, r)?);
                out.push('\n');
            }
        }
        f => {
            let t = f.and_then(|f| f.strip_prefix("table")).map(prep).filter(|t| !t.trim().is_empty());
            let segs = match t {
                Some(t) => parse_template(&t).map_err(|e| format!("template parsing error: {e}"))?,
                None => parse_template("{{.Name}}\t{{.Description}}\t{{.DockerEndpoint}}").unwrap(),
            };
            let hdr = |f: &str| match f {
                "Name" => "NAME",
                "Description" => "DESCRIPTION",
                "DockerEndpoint" => "DOCKER ENDPOINT",
                "Current" => "CURRENT",
                "Error" => "ERROR",
                _ => "",
            };
            let mut text = String::new();
            for seg in &segs {
                match seg {
                    Seg::Lit(l) => text.push_str(l),
                    Seg::Field(p) => text.push_str(hdr(&p[0])),
                    Seg::JsonDot => {}
                }
            }
            text.push('\n');
            for r in rows {
                // jako Docker: aktuální kontext má u názvu hvězdičku
                let shown = Row { name: if r.current { format!("{} *", r.name) } else { r.name.clone() }, description: r.description.clone(), endpoint: r.endpoint.clone(), current: r.current };
                text.push_str(&render(&segs, &shown)?);
                text.push('\n');
            }
            out = tabwrite(&text, 10, 3);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------ inspect

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct WlsMeta {
    distro: String,
    engine: String,
    port: u16,
    #[serde(rename = "TLS")]
    tls: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Meta {
    description: String,
    #[serde(rename = "WLS")]
    wls: WlsMeta,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct DockerEp {
    host: String,
    #[serde(rename = "SkipTLSVerify")]
    skip_tls_verify: bool,
}

#[derive(Serialize)]
struct Endpoints {
    docker: DockerEp,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Storage {
    metadata_path: String,
    #[serde(rename = "TLSPath")]
    tls_path: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Inspect {
    name: String,
    metadata: Meta,
    endpoints: Endpoints,
    #[serde(rename = "TLSMaterial")]
    tls_material: serde_json::Map<String, serde_json::Value>,
    storage: Storage,
}

fn inspect_one(name: &str, desc: &str, t: &Target, store_path: &std::path::Path) -> Inspect {
    let tls_dir = crate::apitls::tls_dir().map(|d| d.display().to_string()).unwrap_or_default();
    Inspect {
        name: name.into(),
        metadata: Meta { description: desc.into(), wls: WlsMeta { distro: t.distro.clone(), engine: t.engine.name().to_ascii_lowercase(), port: t.port, tls: t.tls } },
        endpoints: Endpoints { docker: DockerEp { host: t.endpoint(), skip_tls_verify: false } },
        tls_material: Default::default(),
        storage: Storage {
            metadata_path: if name == DEFAULT { "<IN MEMORY>".into() } else { store_path.display().to_string() },
            tls_path: if t.tls { tls_dir } else { "<IN MEMORY>".into() },
        },
    }
}

/// JSON s odsazením 4 mezery (jako `docker context inspect`).
fn pretty4<T: Serialize>(v: &T) -> String {
    let mut buf = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, serde_json::ser::PrettyFormatter::with_indent(b"    "));
    let _ = v.serialize(&mut ser);
    String::from_utf8(buf).unwrap_or_default()
}

// ------------------------------------------------------------ přepínače

struct Parsed {
    bools: Vec<String>,
    values: Vec<(String, String)>,
    pos: Vec<String>,
}

impl Parsed {
    fn has(&self, f: &str) -> bool {
        self.bools.iter().any(|b| b == f)
    }
    fn last(&self, f: &str) -> Option<&str> {
        self.values.iter().rev().find(|(k, _)| k == f).map(|(_, v)| v.as_str())
    }
}

/// Jednoduchý rozbor: `bools`/`values` jsou (krátký, dlouhý) názvy.
fn parse(args: &[String], bools: &[(&str, &str)], values: &[(&str, &str)]) -> Result<Parsed, String> {
    let mut p = Parsed { bools: Vec::new(), values: Vec::new(), pos: Vec::new() };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        i += 1;
        if a == "--" {
            p.pos.extend(args[i..].iter().cloned());
            break;
        }
        if !a.starts_with('-') || a == "-" {
            p.pos.push(a.clone());
            continue;
        }
        let (name, inline) = match a.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (a.as_str(), None),
        };
        if let Some((_, l)) = bools.iter().find(|(s, l)| *s == name || *l == name) {
            if inline.as_deref() != Some("false") {
                p.bools.push(l.to_string());
            }
        } else if let Some((_, l)) = values.iter().find(|(s, l)| *s == name || *l == name) {
            let v = match inline {
                Some(v) => v,
                None => {
                    let v = args.get(i).ok_or_else(|| format!("flag needs an argument: {name}"))?.clone();
                    i += 1;
                    v
                }
            };
            p.values.push((l.to_string(), v));
        } else {
            return Err(format!("unknown flag: {name}"));
        }
    }
    Ok(p)
}

const USAGE: &str = "Usage:  docker context COMMAND

Manage contexts (WLS Desktop: a context is a WSL distribution + engine + API port)

Commands:
  create      Create a context
  inspect     Display detailed information on one or more contexts
  ls          List contexts
  rm          Remove one or more contexts
  show        Print the name of the current context
  use         Set the current docker context
";

/// Spustí `docker context <args>`.
pub fn run(args: &[String], env: &CtxEnv) -> Res {
    let store = match Store::load_from(&env.store_path) {
        Ok(s) => s,
        Err(e) => return fail(format!("error: cannot read contexts: {e}")),
    };
    let current = contexts::select(None, env.env_ctx.as_deref(), &store).to_string();
    let sub = args.first().map(|s| s.as_str()).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    if matches!(sub, "" | "--help" | "-h" | "help") {
        return Res { out: USAGE.into(), ..Default::default() };
    }
    let r = match sub {
        "ls" | "list" => parse(rest, &[("-q", "--quiet")], &[("", "--format")]).and_then(|p| {
            if !p.pos.is_empty() {
                return Err("\"docker context ls\" accepts no arguments.".into());
            }
            let rows = rows(&store, env, &current);
            Ok(Res { out: ls_output(&rows, p.has("--quiet"), p.last("--format"))?, ..Default::default() })
        }),
        "show" => Ok(Res { out: format!("{current}\n"), ..Default::default() }),
        "use" => (|| {
            let p = parse(rest, &[], &[])?;
            let [name] = p.pos.as_slice() else {
                return Err("\"docker context use\" requires exactly 1 argument.".into());
            };
            if name != DEFAULT && store.get(name).is_none() {
                return Err(format!("context \"{name}\" does not exist"));
            }
            let mut st = store.clone();
            st.current = if name == DEFAULT { None } else { Some(name.clone()) };
            st.save_to(&env.store_path)?;
            let mut err = format!("Current context is now \"{name}\"\n");
            if env.env_ctx.as_deref().is_some_and(|e| !e.is_empty() && e != name) {
                err.push_str(&format!(
                    "Warning: DOCKER_CONTEXT environment variable overrides the active context ({:?}).\n",
                    env.env_ctx.as_deref().unwrap_or("")
                ));
            }
            Ok(Res { out: format!("{name}\n"), err, code: 0 })
        })(),
        "create" => create(rest, &store, env),
        "rm" | "remove" => (|| {
            let p = parse(rest, &[("-f", "--force")], &[])?;
            if p.pos.is_empty() {
                return Err("\"docker context rm\" requires at least 1 argument.".into());
            }
            let mut st = store.clone();
            let mut out = String::new();
            let mut errs = Vec::new();
            for name in &p.pos {
                if name == DEFAULT {
                    errs.push("default: context \"default\" cannot be removed".to_string());
                } else if st.get(name).is_none() {
                    errs.push(format!("{name}: context \"{name}\" does not exist"));
                } else if st.current_name() == name && !p.has("--force") {
                    errs.push(format!("{name}: context \"{name}\" is in use, set -f flag to force remove"));
                } else {
                    if st.current_name() == name {
                        st.current = None;
                    }
                    st.contexts.retain(|c| &c.name != name);
                    out.push_str(name);
                    out.push('\n');
                }
            }
            st.save_to(&env.store_path)?;
            if errs.is_empty() {
                Ok(Res { out, ..Default::default() })
            } else {
                Ok(Res { out, err: format!("failed to remove one or more contexts\n{}\n", errs.join("\n")), code: 1 })
            }
        })(),
        "inspect" => (|| {
            let p = parse(rest, &[], &[("-f", "--format")])?;
            let names = if p.pos.is_empty() { vec![current.clone()] } else { p.pos.clone() };
            let mut items = Vec::new();
            for n in &names {
                let it = if n == DEFAULT {
                    inspect_one(DEFAULT, &default_description(&env.default), &env.default, &env.store_path)
                } else {
                    let c = store.get(n).ok_or_else(|| format!("context \"{n}\" does not exist"))?;
                    inspect_one(n, &c.description, &c.target(), &env.store_path)
                };
                items.push(it);
            }
            match p.last("--format") {
                None | Some("") => Ok(Res { out: format!("{}\n", pretty4(&items)), ..Default::default() }),
                Some(f) => {
                    let segs = parse_template(f).map_err(|e| format!("template parsing error: {e}"))?;
                    let mut out = String::new();
                    for it in &items {
                        let v = serde_json::to_value(it).map_err(|e| e.to_string())?;
                        for seg in &segs {
                            match seg {
                                Seg::Lit(l) => out.push_str(l),
                                Seg::JsonDot => out.push_str(&serde_json::to_string(&v).unwrap_or_default()),
                                Seg::Field(path) => {
                                    let mut cur = &v;
                                    for k in path {
                                        cur = cur.get(k).ok_or_else(|| format!("template: no field .{}", path.join(".")))?;
                                    }
                                    match cur {
                                        serde_json::Value::String(s) => out.push_str(s),
                                        serde_json::Value::Bool(_) | serde_json::Value::Number(_) => out.push_str(&cur.to_string()),
                                        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
                                    }
                                }
                            }
                        }
                        out.push('\n');
                    }
                    Ok(Res { out, ..Default::default() })
                }
            }
        })(),
        "update" | "export" | "import" => Err(format!("\"docker context {sub}\" is not supported by WLS Desktop contexts (use rm + create)")),
        other => Err(format!("docker context: unknown command \"{other}\"\nSee 'docker context --help'.")),
    };
    match r {
        Ok(r) => r,
        Err(e) => fail(e),
    }
}

fn create(rest: &[String], store: &Store, env: &CtxEnv) -> Result<Res, String> {
    let p = parse(
        rest,
        &[("", "--tls")],
        &[("", "--description"), ("", "--distro"), ("", "--engine"), ("", "--port"), ("", "--docker"), ("", "--from")],
    )?;
    let [name] = p.pos.as_slice() else {
        return Err("\"docker context create\" requires exactly 1 argument.".into());
    };
    if !contexts::valid_name(name) {
        return Err(format!("context name {name:?} is invalid, names are validated against regexp \"^[a-zA-Z0-9][a-zA-Z0-9_.+-]+$\""));
    }
    if name == DEFAULT {
        return Err("\"default\" is a reserved context name".into());
    }
    if store.get(name).is_some() {
        return Err(format!("context \"{name}\" already exists"));
    }
    // výchozí hodnoty: --from <kontext>, jinak nastavení aplikace
    let base = match p.last("--from") {
        Some(f) => store.resolve(f, &env.default)?,
        None => env.default.clone(),
    };
    let distro = match p.last("--distro") {
        Some(d) => contexts::match_distro(d, &(env.distros)()?)?,
        None => base.distro.clone(),
    };
    let engine = match p.last("--engine").map(|e| e.to_ascii_lowercase()) {
        None => base.engine,
        Some(e) if e == "docker" => crate::engine::Engine::Docker,
        Some(e) if e == "podman" => crate::engine::Engine::Podman,
        Some(e) => return Err(format!("invalid --engine {e:?}: expected docker or podman")),
    };
    let tls = p.has("--tls");
    let mut port = match p.last("--port") {
        Some(v) => Some(v.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| format!("invalid --port {v:?}"))?),
        None => None,
    };
    if let Some(d) = p.last("--docker") {
        let dp = contexts::parse_docker_endpoint(d)?;
        if port.is_some_and(|x| x != dp) {
            return Err("--port and --docker host=… disagree".into());
        }
        port = Some(dp);
    }
    let port = port.unwrap_or(if tls && !base.tls { 2376 } else { base.port });
    let ctx = Context { name: name.clone(), description: p.last("--description").unwrap_or("").to_string(), distro, engine, port, tls };
    let mut st = store.clone();
    st.contexts.push(ctx);
    st.save_to(&env.store_path)?;
    Ok(Res { out: format!("{name}\n"), err: format!("Successfully created context \"{name}\"\n"), code: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn distros() -> Result<Vec<String>, String> {
        Ok(vec!["Debian".into(), "Ubuntu".into()])
    }

    struct Tmp(PathBuf);
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn env_in(dir: &std::path::Path, env_ctx: Option<&str>) -> CtxEnv<'static> {
        CtxEnv {
            store_path: dir.join("contexts.json"),
            default: Target { distro: "Debian".into(), engine: Engine::Podman, port: 2375, tls: false },
            env_ctx: env_ctx.map(String::from),
            distros: &distros,
        }
    }

    #[test]
    fn lifecycle() {
        let dir = std::env::temp_dir().join(format!("wls-ctxcmd-{}", std::process::id()));
        let _g = Tmp(dir.clone());
        let env = env_in(&dir, None);
        let r = run(&s(&["ls"]), &env);
        assert_eq!(r.out, "NAME        DESCRIPTION                             DOCKER ENDPOINT
default *   WLS Desktop settings (Debian, Podman)   tcp://127.0.0.1:2375
");
        assert_eq!(run(&s(&["show"]), &env).out, "default\n");

        let r = run(&s(&["create", "ubu", "--description", "Ubuntu Docker", "--distro", "ubuntu", "--engine", "docker", "--port", "2380"]), &env);
        assert_eq!((r.out.as_str(), r.code), ("ubu\n", 0), "{}", r.err);
        let r = run(&s(&["create", "sec", "--docker", "host=tcp://127.0.0.1:2390", "--tls"]), &env);
        assert_eq!(r.code, 0, "{}", r.err);
        let st = Store::load_from(&env.store_path).unwrap();
        assert_eq!(st.get("ubu").unwrap().distro, "Ubuntu");
        assert_eq!(st.get("sec").unwrap().target(), Target { distro: "Debian".into(), engine: Engine::Podman, port: 2390, tls: true });

        // chyby
        assert!(run(&s(&["create", "x2", "--distro", "Fedora"]), &env).err.contains("WSL distribution \"Fedora\" does not exist"));
        assert!(run(&s(&["create", "ubu"]), &env).err.contains("already exists"));
        assert!(run(&s(&["create", "default"]), &env).err.contains("reserved"));
        assert_eq!(run(&s(&["use", "nope"]), &env).err, "context \"nope\" does not exist\n");

        let r = run(&s(&["use", "ubu"]), &env);
        assert_eq!((r.out.as_str(), r.err.as_str()), ("ubu\n", "Current context is now \"ubu\"\n"));
        assert_eq!(run(&s(&["show"]), &env).out, "ubu\n");
        let r = run(&s(&["ls"]), &env);
        assert_eq!(
            r.out,
            "NAME      DESCRIPTION                             DOCKER ENDPOINT
default   WLS Desktop settings (Debian, Podman)   tcp://127.0.0.1:2375
sec                                               tcp://127.0.0.1:2390
ubu *     Ubuntu Docker                           tcp://127.0.0.1:2380
"
        );
        assert_eq!(run(&s(&["ls", "-q"]), &env).out, "default\nsec\nubu\n");
        assert_eq!(run(&s(&["ls", "--format", "{{.Name}}"]), &env).out, "default\nsec\nubu\n");
        assert_eq!(run(&s(&["ls", "--format", "{{.Name}}:{{.Current}}"]), &env).out, "default:false\nsec:false\nubu:true\n");
        let j = run(&s(&["ls", "--format", "json"]), &env).out;
        let v: serde_json::Value = serde_json::from_str(j.lines().nth(2).unwrap()).unwrap();
        assert_eq!((v["Name"].as_str(), v["Current"].as_bool()), (Some("ubu"), Some(true)));

        // DOCKER_CONTEXT má přednost před souborem
        let env2 = env_in(&dir, Some("sec"));
        assert_eq!(run(&s(&["show"]), &env2).out, "sec\n");

        let r = run(&s(&["inspect", "ubu"]), &env);
        let v: serde_json::Value = serde_json::from_str(&r.out).unwrap();
        assert_eq!(v[0]["Endpoints"]["docker"]["Host"], "tcp://127.0.0.1:2380");
        assert_eq!(v[0]["Metadata"]["WLS"]["Engine"], "docker");
        assert!(r.out.starts_with("[\n    {\n        \"Name\": \"ubu\",\n"), "{}", r.out);
        assert_eq!(run(&s(&["inspect", "--format", "{{.Metadata.WLS.Distro}} {{.Metadata.WLS.Port}}"]), &env).out, "Ubuntu 2380\n");
        assert_eq!(run(&s(&["inspect", "-f", "{{.Name}}", "default"]), &env).out, "default\n");
        assert_eq!(run(&s(&["inspect", "nope"]), &env).code, 1);

        // rm: aktuální jen s -f, default nikdy
        let r = run(&s(&["rm", "ubu"]), &env);
        assert!(r.err.contains("is in use"), "{}", r.err);
        assert_eq!(run(&s(&["rm", "default"]), &env).code, 1);
        let r = run(&s(&["rm", "-f", "ubu", "sec"]), &env);
        assert_eq!((r.out.as_str(), r.code), ("ubu\nsec\n", 0));
        assert_eq!(run(&s(&["show"]), &env).out, "default\n");
        assert_eq!(Store::load_from(&env.store_path).unwrap(), Store::default());

        // use default = reset
        run(&s(&["create", "abc"]), &env);
        run(&s(&["use", "abc"]), &env);
        run(&s(&["use", "default"]), &env);
        assert_eq!(Store::load_from(&env.store_path).unwrap().current, None);
        assert_eq!(run(&s(&["bogus"]), &env).code, 1);
    }
}
