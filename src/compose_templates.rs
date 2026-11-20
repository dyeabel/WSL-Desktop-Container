//! Vestavěné šablony compose stacků (stránka Compose → Nový stack…).
//!
//! Šablona je čistě data: text `compose.yaml` s proměnnými `${VAR}` a seznam proměnných
//! pro `.env` (pevná hodnota, port nebo náhodně generované heslo). Image jsou plně
//! kvalifikované (Podman krátká jména bez nastaveného registru odmítá), data jsou
//! v pojmenovaných volume.

/// Hodnota proměnné v `.env`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Val {
    /// pevný text
    Text(&'static str),
    /// port na hostiteli
    Port(u16),
    /// náhodně vygenerované heslo
    Secret,
}

#[derive(Debug, Clone, Copy)]
pub struct Template {
    /// identifikátor (WSLCD_DEBUG_COMPOSE_NEW=<id>), zároveň výchozí název projektu
    pub id: &'static str,
    pub name: &'static str,
    desc_cs: &'static str,
    desc_en: &'static str,
    pub compose: &'static str,
    pub env: &'static [(&'static str, Val)],
}

impl Template {
    pub fn description(&self) -> &'static str {
        t!(self.desc_cs, self.desc_en)
    }

    /// Obsah `.env`: název projektu a proměnné šablony (hesla se vygenerují znovu při každém volání).
    pub fn render_env(&self, project: &str) -> String {
        let mut s = format!("# WSL Container Desktop – {}\nCOMPOSE_PROJECT_NAME={project}\n", self.name);
        for (k, v) in self.env {
            let val = match v {
                Val::Text(t) => t.to_string(),
                Val::Port(p) => p.to_string(),
                Val::Secret => password(24),
            };
            s.push_str(&format!("{k}={val}\n"));
        }
        s
    }
}

#[cfg(test)]
pub fn find(id: &str) -> Option<&'static Template> {
    TEMPLATES.iter().find(|t| t.id == id)
}

pub const TEMPLATES: &[Template] = &[
    Template {
        id: "postgres",
        name: "PostgreSQL",
        desc_cs: "Databáze PostgreSQL 17 s kontrolou stavu.",
        desc_en: "PostgreSQL 17 database with a health check.",
        compose: r#"services:
  postgres:
    image: docker.io/library/postgres:17
    restart: unless-stopped
    environment:
      POSTGRES_USER: ${POSTGRES_USER}
      POSTGRES_PASSWORD: ${POSTGRES_PASSWORD}
      POSTGRES_DB: ${POSTGRES_DB}
    ports:
      - "${POSTGRES_PORT}:5432"
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U $${POSTGRES_USER} -d $${POSTGRES_DB}"]
      interval: 10s
      timeout: 5s
      retries: 5

volumes:
  pgdata:
"#,
        env: &[
            ("POSTGRES_USER", Val::Text("app")),
            ("POSTGRES_PASSWORD", Val::Secret),
            ("POSTGRES_DB", Val::Text("app")),
            ("POSTGRES_PORT", Val::Port(5432)),
        ],
    },
    Template {
        id: "mariadb",
        name: "MySQL / MariaDB",
        desc_cs: "MariaDB 11 (kompatibilní s MySQL) s databází a uživatelem.",
        desc_en: "MariaDB 11 (MySQL compatible) with a database and a user.",
        compose: r#"services:
  mariadb:
    image: docker.io/library/mariadb:11
    restart: unless-stopped
    environment:
      MARIADB_ROOT_PASSWORD: ${MARIADB_ROOT_PASSWORD}
      MARIADB_DATABASE: ${MARIADB_DATABASE}
      MARIADB_USER: ${MARIADB_USER}
      MARIADB_PASSWORD: ${MARIADB_PASSWORD}
    ports:
      - "${MARIADB_PORT}:3306"
    volumes:
      - dbdata:/var/lib/mysql
    healthcheck:
      test: ["CMD", "healthcheck.sh", "--connect", "--innodb_initialized"]
      interval: 10s
      timeout: 5s
      retries: 5

volumes:
  dbdata:
"#,
        env: &[
            ("MARIADB_ROOT_PASSWORD", Val::Secret),
            ("MARIADB_DATABASE", Val::Text("app")),
            ("MARIADB_USER", Val::Text("app")),
            ("MARIADB_PASSWORD", Val::Secret),
            ("MARIADB_PORT", Val::Port(3306)),
        ],
    },
    Template {
        id: "redis",
        name: "Redis",
        desc_cs: "Redis 7 s heslem a trvalým ukládáním (AOF).",
        desc_en: "Redis 7 with a password and persistence (AOF).",
        compose: r#"services:
  redis:
    image: docker.io/library/redis:7-alpine
    restart: unless-stopped
    command: ["redis-server", "--appendonly", "yes", "--requirepass", "${REDIS_PASSWORD}"]
    ports:
      - "${REDIS_PORT}:6379"
    volumes:
      - redisdata:/data

volumes:
  redisdata:
"#,
        env: &[("REDIS_PASSWORD", Val::Secret), ("REDIS_PORT", Val::Port(6379))],
    },
    Template {
        id: "mongodb",
        name: "MongoDB",
        desc_cs: "MongoDB 8 s administrátorským účtem.",
        desc_en: "MongoDB 8 with an admin account.",
        compose: r#"services:
  mongo:
    image: docker.io/library/mongo:8
    restart: unless-stopped
    environment:
      MONGO_INITDB_ROOT_USERNAME: ${MONGO_USER}
      MONGO_INITDB_ROOT_PASSWORD: ${MONGO_PASSWORD}
    ports:
      - "${MONGO_PORT}:27017"
    volumes:
      - mongodata:/data/db

volumes:
  mongodata:
"#,
        env: &[("MONGO_USER", Val::Text("admin")), ("MONGO_PASSWORD", Val::Secret), ("MONGO_PORT", Val::Port(27017))],
    },
    Template {
        id: "wordpress",
        name: "WordPress + MariaDB",
        desc_cs: "WordPress 6 s databází MariaDB 11 (databáze není zvenku dostupná).",
        desc_en: "WordPress 6 with a MariaDB 11 database (the database is not exposed).",
        compose: r#"services:
  wordpress:
    image: docker.io/library/wordpress:6
    restart: unless-stopped
    depends_on:
      - db
    environment:
      WORDPRESS_DB_HOST: db
      WORDPRESS_DB_NAME: ${DB_NAME}
      WORDPRESS_DB_USER: ${DB_USER}
      WORDPRESS_DB_PASSWORD: ${DB_PASSWORD}
    ports:
      - "${WORDPRESS_PORT}:80"
    volumes:
      - wpdata:/var/www/html

  db:
    image: docker.io/library/mariadb:11
    restart: unless-stopped
    environment:
      MARIADB_ROOT_PASSWORD: ${DB_ROOT_PASSWORD}
      MARIADB_DATABASE: ${DB_NAME}
      MARIADB_USER: ${DB_USER}
      MARIADB_PASSWORD: ${DB_PASSWORD}
    volumes:
      - dbdata:/var/lib/mysql

volumes:
  wpdata:
  dbdata:
"#,
        env: &[
            ("WORDPRESS_PORT", Val::Port(8080)),
            ("DB_NAME", Val::Text("wordpress")),
            ("DB_USER", Val::Text("wordpress")),
            ("DB_PASSWORD", Val::Secret),
            ("DB_ROOT_PASSWORD", Val::Secret),
        ],
    },
    Template {
        id: "nginx",
        name: "nginx",
        desc_cs: "Statický web na nginx; obsah je ve volume „site“ (stránka Volume → procházet).",
        desc_en: "Static site on nginx; the content lives in the “site” volume (Volumes → browse).",
        compose: r#"services:
  web:
    image: docker.io/library/nginx:stable-alpine
    restart: unless-stopped
    ports:
      - "${NGINX_PORT}:80"
    volumes:
      - site:/usr/share/nginx/html

volumes:
  site:
"#,
        env: &[("NGINX_PORT", Val::Port(8080))],
    },
    Template {
        id: "adminer",
        name: "Adminer",
        desc_cs: "Webová správa databází (PostgreSQL, MySQL, …) – server zadej jako host.containers.internal.",
        desc_en: "Web database manager (PostgreSQL, MySQL, …) – use host.containers.internal as the server.",
        compose: r#"services:
  adminer:
    image: docker.io/library/adminer:latest
    restart: unless-stopped
    environment:
      ADMINER_DEFAULT_SERVER: ${ADMINER_DEFAULT_SERVER}
    ports:
      - "${ADMINER_PORT}:8080"
"#,
        env: &[("ADMINER_PORT", Val::Port(8081)), ("ADMINER_DEFAULT_SERVER", Val::Text("host.containers.internal"))],
    },
    Template {
        id: "rabbitmq",
        name: "RabbitMQ",
        desc_cs: "RabbitMQ 4 s webovou správou (port 15672).",
        desc_en: "RabbitMQ 4 with the management UI (port 15672).",
        compose: r#"services:
  rabbitmq:
    image: docker.io/library/rabbitmq:4-management
    restart: unless-stopped
    environment:
      RABBITMQ_DEFAULT_USER: ${RABBITMQ_USER}
      RABBITMQ_DEFAULT_PASS: ${RABBITMQ_PASSWORD}
    ports:
      - "${RABBITMQ_PORT}:5672"
      - "${RABBITMQ_UI_PORT}:15672"
    volumes:
      - rabbitdata:/var/lib/rabbitmq

volumes:
  rabbitdata:
"#,
        env: &[
            ("RABBITMQ_USER", Val::Text("admin")),
            ("RABBITMQ_PASSWORD", Val::Secret),
            ("RABBITMQ_PORT", Val::Port(5672)),
            ("RABBITMQ_UI_PORT", Val::Port(15672)),
        ],
    },
    Template {
        id: "mailpit",
        name: "Mailpit",
        desc_cs: "Testovací SMTP server s webovým rozhraním pro zachycené e-maily.",
        desc_en: "Test SMTP server with a web UI for captured e-mails.",
        compose: r#"services:
  mailpit:
    image: docker.io/axllent/mailpit:latest
    restart: unless-stopped
    environment:
      MP_DATABASE: /data/mailpit.db
      MP_MAX_MESSAGES: ${MAILPIT_MAX_MESSAGES}
    ports:
      - "${MAILPIT_SMTP_PORT}:1025"
      - "${MAILPIT_UI_PORT}:8025"
    volumes:
      - maildata:/data

volumes:
  maildata:
"#,
        env: &[
            ("MAILPIT_SMTP_PORT", Val::Port(1025)),
            ("MAILPIT_UI_PORT", Val::Port(8025)),
            ("MAILPIT_MAX_MESSAGES", Val::Text("5000")),
        ],
    },
    Template {
        id: "minio",
        name: "MinIO",
        desc_cs: "Objektové úložiště kompatibilní s S3, konzole na portu 9001.",
        desc_en: "S3 compatible object storage, console on port 9001.",
        compose: r#"services:
  minio:
    image: docker.io/minio/minio:latest
    restart: unless-stopped
    command: ["server", "/data", "--console-address", ":9001"]
    environment:
      MINIO_ROOT_USER: ${MINIO_ROOT_USER}
      MINIO_ROOT_PASSWORD: ${MINIO_ROOT_PASSWORD}
    ports:
      - "${MINIO_API_PORT}:9000"
      - "${MINIO_CONSOLE_PORT}:9001"
    volumes:
      - miniodata:/data

volumes:
  miniodata:
"#,
        env: &[
            ("MINIO_ROOT_USER", Val::Text("admin")),
            ("MINIO_ROOT_PASSWORD", Val::Secret),
            ("MINIO_API_PORT", Val::Port(9000)),
            ("MINIO_CONSOLE_PORT", Val::Port(9001)),
        ],
    },
];

/// Náhodné heslo z písmen a číslic (bez zaměnitelných znaků, bez znaků, které by vadily v .env / YAML).
/// Zdroj náhody: `RandomState` (klíče SipHash inicializované z náhody OS) + čas a čítač.
pub fn password(len: usize) -> String {
    use std::hash::{BuildHasher, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    const A: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz23456789";
    static N: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let mut out = String::with_capacity(len);
    while out.len() < len {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(N.fetch_add(1, Ordering::Relaxed));
        h.write_u128(nanos);
        let mut v = h.finish();
        // 8 znaků z jednoho 64bitového čísla (57^8 < 2^64)
        for _ in 0..8 {
            if out.len() == len {
                break;
            }
            out.push(A[(v % A.len() as u64) as usize] as char);
            v /= A.len() as u64;
        }
    }
    out
}

/// Platný název compose projektu: malá písmena, číslice, `-` a `_`, začíná písmenem nebo číslicí.
pub fn valid_project(name: &str) -> bool {
    let mut ch = name.chars();
    matches!(ch.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && ch.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Názvy proměnných použitých jako `${VAR}` / `${VAR:-x}` (`$${…}` je escapovaný dolar).
pub fn vars_used(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut v: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'$' && b[i + 1] == b'$' {
            i += 2;
            continue;
        }
        if b[i] == b'$' && b[i + 1] == b'{' {
            let start = i + 2;
            let mut j = start;
            while j < b.len() && (b[j].is_ascii_alphanumeric() || b[j] == b'_') {
                j += 1;
            }
            let name = &text[start..j];
            if !name.is_empty() && !v.iter().any(|x| x == name) {
                v.push(name.to_string());
            }
            i = j;
            continue;
        }
        i += 1;
    }
    v
}

/// Proměnné definované v textu `.env` (KLIC=hodnota, komentáře a prázdné řádky se přeskočí).
pub fn env_keys(env: &str) -> Vec<String> {
    env.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.trim().trim_start_matches("export ").trim().to_string()))
        .collect()
}

/// Nahradí `${VAR}` hodnotami z `.env` (pro kontrolu portů).
pub fn expand(text: &str, env: &str) -> String {
    let mut s = text.to_string();
    for l in env.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
        if let Some((k, v)) = l.split_once('=') {
            s = s.replace(&format!("${{{}}}", k.trim()), v.trim());
        }
    }
    s
}

/// Porty hostitele z bloků `ports:` (krátký zápis `"[ip:]host:kontejner"`).
pub fn host_ports(compose: &str) -> Vec<String> {
    let mut v = Vec::new();
    let mut ports_indent: Option<usize> = None;
    for line in compose.lines() {
        let indent = line.len() - line.trim_start().len();
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some(pi) = ports_indent {
            if indent > pi && t.starts_with('-') {
                let item = t.trim_start_matches('-').trim().trim_matches(|c| c == '"' || c == '\'');
                let parts: Vec<&str> = item.split(':').collect();
                if parts.len() >= 2 {
                    v.push(parts[parts.len() - 2].to_string());
                }
                continue;
            }
            ports_indent = None;
        }
        if t == "ports:" {
            ports_indent = Some(indent);
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_templates_with_unique_ids() {
        assert_eq!(TEMPLATES.len(), 10);
        for (i, t) in TEMPLATES.iter().enumerate() {
            assert!(TEMPLATES[i + 1..].iter().all(|o| o.id != t.id), "duplicitní id {}", t.id);
            assert!(valid_project(t.id), "{}", t.id);
            assert!(find(t.id).is_some());
        }
    }

    #[test]
    fn templates_look_like_compose() {
        for t in TEMPLATES {
            assert!(t.compose.starts_with("services:\n"), "{}", t.id);
            assert!(!t.compose.contains('\t'), "{}: tabulátor v YAML", t.id);
            for l in t.compose.lines().filter(|l| l.trim_start().starts_with("image:")) {
                let img = l.trim_start().trim_start_matches("image:").trim();
                assert!(img.starts_with("docker.io/"), "{}: image {img} není plně kvalifikovaný", t.id);
                assert!(img.contains(':'), "{}: image {img} bez tagu", t.id);
            }
            // pojmenované volume použité ve službách jsou deklarované
            for l in t.compose.lines().map(str::trim).filter(|l| l.starts_with("- ") && l.contains(":/")) {
                let name = l[2..].split(':').next().unwrap();
                assert!(t.compose.contains(&format!("\n  {name}:\n")), "{}: volume {name} není deklarované", t.id);
            }
        }
    }

    #[test]
    fn all_vars_defined_in_env() {
        for t in TEMPLATES {
            let env = t.render_env(t.id);
            let keys = env_keys(&env);
            assert!(keys.contains(&"COMPOSE_PROJECT_NAME".to_string()));
            let used = vars_used(t.compose);
            assert!(!used.is_empty() || t.env.is_empty(), "{}", t.id);
            for v in used {
                assert!(keys.contains(&v), "{}: ${{{v}}} chybí v .env", t.id);
            }
            // a naopak – v .env nic navíc
            for (k, _) in t.env {
                assert!(t.compose.contains(&format!("${{{k}}}")), "{}: {k} se v compose nepoužívá", t.id);
            }
        }
    }

    #[test]
    fn no_duplicate_host_ports() {
        for t in TEMPLATES {
            let ports = host_ports(&expand(t.compose, &t.render_env(t.id)));
            assert!(!ports.is_empty(), "{}: žádné porty", t.id);
            for (i, p) in ports.iter().enumerate() {
                assert!(p.parse::<u16>().is_ok(), "{}: port {p} se nerozbalil", t.id);
                assert!(!ports[i + 1..].contains(p), "{}: port {p} dvakrát", t.id);
            }
        }
    }

    #[test]
    fn passwords_random_and_safe() {
        let a = password(24);
        let b = password(24);
        assert_eq!(a.len(), 24);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
        let env = find("postgres").unwrap().render_env("db");
        let pw = env.lines().find_map(|l| l.strip_prefix("POSTGRES_PASSWORD=")).unwrap();
        assert_eq!(pw.len(), 24);
        assert!(env.contains("COMPOSE_PROJECT_NAME=db\n"));
    }

    #[test]
    fn helpers() {
        assert_eq!(vars_used("a ${X} $${Y} ${Z:-1} ${X}"), vec!["X", "Z"]);
        assert_eq!(env_keys("# c\nA=1\n\nexport B = 2\n"), vec!["A", "B"]);
        assert_eq!(host_ports("services:\n  a:\n    ports:\n      - \"127.0.0.1:80:8080\"\n      - 81:81\n    volumes:\n      - x:/y\n"), vec!["80", "81"]);
        assert!(valid_project("my-app_1"));
        assert!(!valid_project("My"));
        assert!(!valid_project("-a"));
        assert!(!valid_project(""));
        assert!(!valid_project("a b"));
    }
}
