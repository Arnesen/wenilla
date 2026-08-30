//! The console over SOAP: `POST` an `executeCommand` envelope to mangosd's SOAP port with HTTP
//! Basic auth for a gmlevel-3 account. The command runs at console level (no target), so only
//! commands that name their subject are useful here — `account …`, `ban …`, `kick <char>`,
//! `pinfo <char>`, `announce`, `server …`, `reload config`. A fault comes back as HTTP 500 with a
//! `<faultstring>`; the reply text is free-form console output.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::RwLock;

#[derive(Debug)]
pub enum SoapError {
    /// The console ran the command and said no (`<faultstring>`).
    Fault(String),
    Unauthorized,
    Transport(String),
    BadArgument(String),
}

impl std::fmt::Display for SoapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SoapError::Fault(m) => write!(f, "the world server refused the command: {m}"),
            SoapError::Unauthorized => write!(f, "SOAP credentials rejected (HTTP 401)"),
            SoapError::Transport(m) => write!(f, "world server unreachable: {m}"),
            SoapError::BadArgument(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for SoapError {}

impl From<reqwest::Error> for SoapError {
    fn from(e: reqwest::Error) -> Self {
        SoapError::Transport(e.to_string())
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    url: String,
    creds: Arc<RwLock<(String, String)>>,
}

impl Client {
    pub fn new(url: &str, user: &str, pass: &str) -> Self {
        Self {
            // No keep-alive pooling: mangosd's gSOAP server answers one request per connection
            // and closes it, so there is nothing to reuse — a fresh socket per command is the
            // honest shape (and what curl does).
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .pool_max_idle_per_host(0)
                .build()
                .expect("reqwest client"),
            url: url.to_string(),
            creds: Arc::new(RwLock::new((user.to_string(), pass.to_string()))),
        }
    }

    /// Swap the login used from now on (after the bootstrap creates the service's own account).
    pub async fn set_credentials(&self, user: &str, pass: &str) {
        *self.creds.write().await = (user.to_string(), pass.to_string());
    }

    pub async fn exec(&self, command: &str) -> Result<String, SoapError> {
        let (user, pass) = self.creds.read().await.clone();
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"utf-8\"?>\
             <SOAP-ENV:Envelope xmlns:SOAP-ENV=\"http://schemas.xmlsoap.org/soap/envelope/\" \
             xmlns:SOAP-ENC=\"http://schemas.xmlsoap.org/soap/encoding/\" \
             xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\" \
             xmlns:xsd=\"http://www.w3.org/2001/XMLSchema\" xmlns:ns1=\"urn:MaNGOS\">\
             <SOAP-ENV:Body><ns1:executeCommand><command>{}</command></ns1:executeCommand></SOAP-ENV:Body>\
             </SOAP-ENV:Envelope>",
            xml_escape(command)
        );
        let sent = self
            .http
            .post(&self.url)
            .basic_auth(&user, Some(&pass))
            .header("Content-Type", "text/xml; charset=utf-8")
            .header("SOAPAction", "urn:MaNGOS#executeCommand")
            .body(body)
            .send()
            .await;
        // `account set password` is the one command whose handler leaves gSOAP with nothing to
        // send: mangosd closes the connection without an HTTP reply (curl: "52 Empty reply")
        // even though the verifier in `account.v` has changed. Verified live 2026-08-30. Treat
        // that shape as success for that command only; everything else keeps failing loudly.
        let silent_ok = command.starts_with("account set password ");
        let resp = match sent {
            Ok(r) => r,
            Err(e) if silent_ok && !e.is_timeout() => return Ok(String::new()),
            Err(e) => return Err(e.into()),
        };
        if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
            return Err(SoapError::Unauthorized);
        }
        let text = resp.text().await?;
        match parse_reply(&text) {
            Err(SoapError::Transport(m)) if silent_ok && m == "empty reply" => Ok(String::new()),
            other => other,
        }
    }
}

pub fn parse_reply(text: &str) -> Result<String, SoapError> {
    if let Some(f) = between(text, "<faultstring>", "</faultstring>") {
        return Err(SoapError::Fault(xml_unescape(f.trim())));
    }
    if let Some(r) = between(text, "<result>", "</result>") {
        return Ok(xml_unescape(r).trim_end().to_string());
    }
    if text.trim().is_empty() {
        return Err(SoapError::Transport("empty reply".into()));
    }
    Err(SoapError::Transport(format!(
        "unrecognised reply: {}",
        text.chars().take(200).collect::<String>()
    )))
}

fn between<'a>(s: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = s.find(open)? + open.len();
    let end = s[start..].find(close)? + start;
    Some(&s[start..end])
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#xD;", "")
        .replace("&amp;", "&")
}

/// One console argument: the console splits on whitespace and has no quoting, and XML/shell
/// metacharacters have no business in an account name, so refuse them outright.
pub fn arg(value: &str, max_len: usize) -> Result<&str, SoapError> {
    if value.is_empty() || value.len() > max_len {
        return Err(SoapError::BadArgument(format!(
            "argument must be 1–{max_len} characters"
        )));
    }
    if value
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "<>&\"'|".contains(c))
    {
        return Err(SoapError::BadArgument(
            "argument contains whitespace or a forbidden character".into(),
        ));
    }
    Ok(value)
}

/// Free text for `announce`/`notify`/`motd`: no control characters, capped length.
pub fn text(value: &str, max_len: usize) -> Result<String, SoapError> {
    let v: String = value
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .trim()
        .to_string();
    if v.is_empty() || v.len() > max_len {
        return Err(SoapError::BadArgument(format!(
            "text must be 1–{max_len} characters"
        )));
    }
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_result_and_fault() {
        let ok = "<SOAP-ENV:Envelope><SOAP-ENV:Body><ns1:executeCommandResponse><result>Account created: bob&#xD;\n</result></ns1:executeCommandResponse></SOAP-ENV:Body></SOAP-ENV:Envelope>";
        assert_eq!(parse_reply(ok).unwrap(), "Account created: bob");
        let bad = "<SOAP-ENV:Fault><faultcode>SOAP-ENV:Client</faultcode><faultstring>Account with this name already exist!&#xD;\n</faultstring></SOAP-ENV:Fault>";
        assert!(
            matches!(parse_reply(bad), Err(SoapError::Fault(m)) if m.starts_with("Account with this name"))
        );
    }

    #[test]
    fn arguments_are_strict() {
        assert!(arg("bob", 16).is_ok());
        assert!(arg("bob smith", 16).is_err());
        assert!(arg("a<b", 16).is_err());
        assert!(arg("", 16).is_err());
        assert!(arg("abcdefghijklmnopq", 16).is_err());
    }
}
