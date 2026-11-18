//! Typované chyby. API klient vrací [`ApiError`], takže volající může rozlišit
//! „neexistuje“ (404), konflikt (409), nedostupný engine atd. bez porovnávání textu.
//! Pro zbytek aplikace, kde se chyby jen zobrazují, existuje převod na `String`.

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// engine odpověděl chybovým stavem (zpráva z JSON `message`)
    Status { code: u16, message: String },
    /// spojení se nepodařilo / přerušilo (engine neběží, TLS, timeout)
    Transport(String),
    /// chyba při čtení / dekódování odpovědi
    Decode(String),
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Status { code, .. } => Some(*code),
            _ => None,
        }
    }

    /// Objekt (kontejner, image, volume…) neexistuje.
    pub fn is_not_found(&self) -> bool {
        self.status() == Some(404)
    }

    /// Konflikt – název už existuje, objekt se používá…
    pub fn is_conflict(&self) -> bool {
        self.status() == Some(409)
    }

    /// Engine není dostupný (neběží, špatný port, TLS).
    pub fn is_unreachable(&self) -> bool {
        matches!(self, ApiError::Transport(_))
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Status { code, message } => write!(f, "HTTP {code}: {message}"),
            ApiError::Transport(e) | ApiError::Decode(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<ApiError> for String {
    fn from(e: ApiError) -> String {
        e.to_string()
    }
}

impl From<ureq::Error> for ApiError {
    fn from(e: ureq::Error) -> Self {
        match e {
            ureq::Error::Status(code, r) => {
                let body = r.into_string().unwrap_or_default();
                let message = serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
                    .unwrap_or(body);
                ApiError::Status { code, message: message.trim().to_string() }
            }
            ureq::Error::Transport(t) => ApiError::Transport(t.to_string()),
        }
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Decode(e.to_string())
    }
}

/// Chyba příkazu ve WSL: `wsl.exe` nejde spustit, nebo příkaz skončil nenulovým kódem.
#[derive(Debug, Clone, PartialEq)]
pub enum WslError {
    /// wsl.exe se nepodařilo spustit (chybí, odepřeno…)
    Spawn(String),
    /// příkaz doběhl s chybou – kód a text (stderr, jinak stdout)
    Failed { code: i32, output: String },
}

impl WslError {
    /// Návratový kód příkazu (None = nespustil se).
    pub fn code(&self) -> Option<i32> {
        match self {
            WslError::Failed { code, .. } => Some(*code),
            WslError::Spawn(_) => None,
        }
    }

    /// Výstup / popis chyby bez kódu.
    pub fn output(&self) -> &str {
        match self {
            WslError::Failed { output, .. } => output,
            WslError::Spawn(e) => e,
        }
    }
}

impl fmt::Display for WslError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WslError::Spawn(e) => f.write_str(e),
            WslError::Failed { code, output } => f.write_str(&tf!("{} (kód {code})", "{} (code {code})", output.trim())),
        }
    }
}

impl std::error::Error for WslError {}

impl From<WslError> for String {
    fn from(e: WslError) -> String {
        e.to_string()
    }
}

/// Text výstupu (bez kódu) – pro logy a hledání v hlášce (`e.contains(…)`).
impl std::ops::Deref for WslError {
    type Target = str;
    fn deref(&self) -> &str {
        self.output()
    }
}

/// Chyby, které vzniknou ještě před spuštěním (např. převod cesty) – jako `Spawn`.
impl From<String> for WslError {
    fn from(e: String) -> Self {
        WslError::Spawn(e)
    }
}

impl From<std::io::Error> for WslError {
    fn from(e: std::io::Error) -> Self {
        WslError::Spawn(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_error() {
        let e = WslError::Failed { code: 2, output: "nope\n".into() };
        assert_eq!(e.code(), Some(2));
        assert_eq!(e.output(), "nope\n");
        assert!(String::from(e).starts_with("nope ("));
        assert_eq!(WslError::Spawn("x".into()).code(), None);
    }

    #[test]
    fn classifies_and_displays() {
        let e = ApiError::Status { code: 404, message: "no such container".into() };
        assert!(e.is_not_found() && !e.is_conflict() && !e.is_unreachable());
        assert_eq!(String::from(e), "HTTP 404: no such container");
        assert!(ApiError::Status { code: 409, message: String::new() }.is_conflict());
        assert!(ApiError::Transport("refused".into()).is_unreachable());
    }
}
