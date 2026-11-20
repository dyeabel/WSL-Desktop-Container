//! Kubeconfig pro Windows. Portable řešení: aplikace nepíše do `%USERPROFILE%\.kube`, ale do
//! `<data>\kube\config` vedle své konfigurace. Nástroje ho použijí přes
//! `$env:KUBECONFIG` nebo `kubectl --kubeconfig`. Soubor je JSON (kubectl ho čte stejně jako YAML).

use serde_json::Value;
use std::path::PathBuf;

pub const CONTEXT: &str = "wslcd";

/// Upraví výstup `kubectl config view --raw -o json` z k3s: server na `127.0.0.1:<port>`,
/// cluster, uživatel i kontext pojmenované `wslcd`, aktuální kontext `wslcd`.
pub fn rewrite(k3s_json: &str, api_port: u16) -> Result<Value, String> {
    let mut v: Value = serde_json::from_str(k3s_json).map_err(|e| format!("kubeconfig: {e}"))?;
    let one = |v: &mut Value, key: &str| -> Result<(), String> {
        let arr = v.get_mut(key).and_then(|a| a.as_array_mut()).ok_or_else(|| format!("kubeconfig: chybí {key}"))?;
        arr.truncate(1);
        let first = arr.first_mut().ok_or_else(|| format!("kubeconfig: prázdné {key}"))?;
        first["name"] = Value::from(CONTEXT);
        Ok(())
    };
    one(&mut v, "clusters")?;
    one(&mut v, "users")?;
    one(&mut v, "contexts")?;
    v["clusters"][0]["cluster"]["server"] = Value::from(format!("https://127.0.0.1:{api_port}"));
    v["contexts"][0]["context"]["cluster"] = Value::from(CONTEXT);
    v["contexts"][0]["context"]["user"] = Value::from(CONTEXT);
    v["current-context"] = Value::from(CONTEXT);
    Ok(v)
}

/// Cesta ke kubeconfigu aplikace.
pub fn path() -> Option<PathBuf> {
    crate::config::data_dir().map(|d| d.join("kube").join("config"))
}

/// Zapíše kubeconfig (přes dočasný soubor, ať nezůstane poloviční). Vrací cestu.
pub fn write(cfg: &Value) -> Result<PathBuf, String> {
    let p = path().ok_or("data_dir")?;
    let dir = p.parent().ok_or("kube dir")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(cfg).unwrap_or_default()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    const K3S: &str = r#"{"kind":"Config","apiVersion":"v1","clusters":[{"name":"default","cluster":{"server":"https://127.0.0.1:6443","certificate-authority-data":"QQ=="}}],
      "users":[{"name":"default","user":{"client-certificate-data":"Qg==","client-key-data":"Qw=="}}],
      "contexts":[{"name":"default","context":{"cluster":"default","user":"default"}}],"current-context":"default"}"#;

    #[test]
    fn rewrite_sets_server_and_names() {
        let v = rewrite(K3S, 16443).unwrap();
        assert_eq!(v["clusters"][0]["cluster"]["server"], "https://127.0.0.1:16443");
        assert_eq!(v["clusters"][0]["cluster"]["certificate-authority-data"], "QQ==");
        assert_eq!(v["clusters"][0]["name"], "wslcd");
        assert_eq!(v["users"][0]["name"], "wslcd");
        assert_eq!(v["users"][0]["user"]["client-key-data"], "Qw==");
        assert_eq!(v["contexts"][0]["name"], "wslcd");
        assert_eq!(v["contexts"][0]["context"]["cluster"], "wslcd");
        assert_eq!(v["current-context"], "wslcd");
    }

    #[test]
    fn rewrite_rejects_garbage() {
        assert!(rewrite("nope", 6443).is_err());
        assert!(rewrite(r#"{"clusters":[]}"#, 6443).is_err());
    }
}
