//! Šablona prostředí FE + API + DB: nginx se statickou stránkou a proxy `/api/` → PostgREST
//! nad PostgreSQL. Stránka se každé 2 s ptá API, takže mapa toků má co ukázat.
//! Vzorová pravidla jsou `policy::RuleSet::demo()`.

pub const NGINX: &str = "docker.io/library/nginx:1.27-alpine";
pub const POSTGREST: &str = "docker.io/postgrest/postgrest:v12.2.8";
pub const POSTGRES: &str = "docker.io/library/postgres:17-alpine";

const PAGE: &str = r#"<!doctype html><meta charset="utf-8"><title>WSLCD demo</title>
<style>body{font:15px system-ui;margin:40px;background:#1e1e1e;color:#ddd}li{margin:4px 0}small{color:#888}</style>
<h1>FE → API → DB</h1><p><small>nginx → PostgREST → PostgreSQL, obnovuje se každé 2 s</small></p><ul id="l"></ul><p id="e"></p>
<script>
async function load(){try{const r=await fetch('/api/items');const j=await r.json();
document.getElementById('l').innerHTML=j.map(i=>'<li>'+i.id+' · '+i.name+'</li>').join('');document.getElementById('e').textContent='';}
catch(x){document.getElementById('e').textContent='API nedostupné: '+x;}}
load();setInterval(load,2000);
</script>"#;

const NGINX_CONF: &str = r#"server {
  listen 80;
  location / { root /usr/share/nginx/html; index index.html; }
  location /api/ { proxy_pass http://api:3000/; }
}"#;

const INIT_SH: &str = r#"#!/bin/sh
set -e
psql -v ON_ERROR_STOP=1 --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" <<EOSQL
create schema api;
create table api.items(id serial primary key, name text not null);
insert into api.items(name) values ('první položka z databáze'), ('druhá položka'), ('třetí položka');
create role web_anon nologin;
grant usage on schema api to web_anon;
grant select on api.items to web_anon;
create role authenticator noinherit login password '$AUTH_PASSWORD';
grant web_anon to authenticator;
EOSQL
"#;

fn indent(text: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    text.lines().map(|l| if l.is_empty() { String::new() } else { format!("{pad}{l}") }).collect::<Vec<_>>().join("\n")
}

/// Manifesty šablony pro `kubectl apply -n <ns> -f -`. `password` = heslo databáze i role
/// `authenticator` (generuje volající).
pub fn demo_manifests(password: &str) -> String {
    format!(
        r#"apiVersion: v1
kind: Secret
metadata:
  name: db
type: Opaque
stringData:
  password: "{password}"
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: db-init
data:
  init.sh: |
{init}
---
apiVersion: v1
kind: ConfigMap
metadata:
  name: fe
data:
  index.html: |
{page}
  default.conf: |
{conf}
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: db
  labels: {{app: db, tier: database}}
spec:
  replicas: 1
  selector: {{matchLabels: {{app: db}}}}
  template:
    metadata:
      labels: {{app: db, tier: database}}
    spec:
      containers:
        - name: postgres
          image: {POSTGRES}
          imagePullPolicy: IfNotPresent
          ports: [{{containerPort: 5432}}]
          env:
            - {{name: POSTGRES_DB, value: app}}
            - {{name: POSTGRES_USER, value: app}}
            - name: POSTGRES_PASSWORD
              valueFrom: {{secretKeyRef: {{name: db, key: password}}}}
            - name: AUTH_PASSWORD
              valueFrom: {{secretKeyRef: {{name: db, key: password}}}}
          volumeMounts:
            - {{name: init, mountPath: /docker-entrypoint-initdb.d}}
            - {{name: data, mountPath: /var/lib/postgresql/data}}
          readinessProbe:
            exec: {{command: [pg_isready, -U, app, -d, app]}}
            periodSeconds: 3
      volumes:
        - name: init
          configMap: {{name: db-init}}
        - name: data
          emptyDir: {{}}
---
apiVersion: v1
kind: Service
metadata:
  name: db
spec:
  selector: {{app: db}}
  ports: [{{port: 5432, targetPort: 5432}}]
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: api
  labels: {{app: api, tier: backend}}
spec:
  replicas: 1
  selector: {{matchLabels: {{app: api}}}}
  template:
    metadata:
      labels: {{app: api, tier: backend}}
    spec:
      containers:
        - name: postgrest
          image: {POSTGREST}
          imagePullPolicy: IfNotPresent
          ports: [{{containerPort: 3000}}]
          env:
            - name: PGRST_DB_PASS
              valueFrom: {{secretKeyRef: {{name: db, key: password}}}}
            - {{name: PGRST_DB_URI, value: "postgres://authenticator:$(PGRST_DB_PASS)@db:5432/app"}}
            - {{name: PGRST_DB_SCHEMAS, value: api}}
            - {{name: PGRST_DB_ANON_ROLE, value: web_anon}}
---
apiVersion: v1
kind: Service
metadata:
  name: api
spec:
  selector: {{app: api}}
  ports: [{{port: 3000, targetPort: 3000}}]
---
apiVersion: apps/v1
kind: Deployment
metadata:
  name: fe
  labels: {{app: fe, tier: frontend}}
spec:
  replicas: 1
  selector: {{matchLabels: {{app: fe}}}}
  template:
    metadata:
      labels: {{app: fe, tier: frontend}}
    spec:
      containers:
        - name: nginx
          image: {NGINX}
          imagePullPolicy: IfNotPresent
          ports: [{{containerPort: 80}}]
          volumeMounts:
            - {{name: site, mountPath: /usr/share/nginx/html/index.html, subPath: index.html}}
            - {{name: site, mountPath: /etc/nginx/conf.d/default.conf, subPath: default.conf}}
      volumes:
        - name: site
          configMap: {{name: fe}}
---
apiVersion: v1
kind: Service
metadata:
  name: fe
spec:
  selector: {{app: fe}}
  ports: [{{port: 80, targetPort: 80}}]
"#,
        init = indent(INIT_SH, 4),
        page = indent(PAGE, 4),
        conf = indent(NGINX_CONF, 4),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_has_three_components() {
        let m = demo_manifests("s3cr3t");
        for c in ["name: fe", "name: api", "name: db"] {
            assert!(m.contains(&format!("kind: Deployment\nmetadata:\n  {c}")), "{c}");
        }
        assert!(m.contains("password: \"s3cr3t\""));
        assert!(m.contains("tier: frontend") && m.contains("tier: backend") && m.contains("tier: database"));
        assert!(m.contains("proxy_pass http://api:3000/;"));
        // blokové texty jsou odsazené pod klíčem (jinak by YAML nebyl platný)
        assert!(m.contains("  init.sh: |\n    #!/bin/sh\n"));
        assert!(!m.contains("NetworkPolicy"), "pravidla aplikuje formulář, ne šablona");
    }
}
