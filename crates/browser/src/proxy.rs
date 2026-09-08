//! Validated, startup-only proxy configuration.
//!
//! Servo creates its connector once, so proxy settings must be complete
//! before `ServoBuilder::build`. Keeping validation here also prevents an
//! invalid URI from being silently interpreted as a direct connection.

use std::collections::HashSet;
use std::net::IpAddr;
use std::str::FromStr;

use url::{Host, Url};

const MAX_PROXY_URI_LENGTH: usize = 2_048;
const MAX_BYPASS_LENGTH: usize = 2_048;
const MAX_BYPASS_RULES: usize = 32;
const MAX_BYPASS_RULE_LENGTH: usize = 253;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartupProxy {
    uri: String,
    bypass: String,
}

impl StartupProxy {
    pub fn parse(uri: &str, bypass: Option<&str>) -> Result<Self, String> {
        let uri = normalize_proxy_uri(uri)?;
        let bypass = normalize_bypass_list(bypass.unwrap_or_default())?;
        Ok(Self { uri, bypass })
    }

    pub fn uri(&self) -> &str {
        &self.uri
    }

    pub fn bypass(&self) -> &str {
        &self.bypass
    }
}

fn normalize_proxy_uri(input: &str) -> Result<String, String> {
    if input.is_empty() || input.len() > MAX_PROXY_URI_LENGTH || input.trim() != input {
        return Err("proxy must be a non-empty URL without surrounding whitespace".to_owned());
    }
    if input.chars().any(char::is_control) {
        return Err("proxy URL contains a control character".to_owned());
    }

    let mut url = Url::parse(input).map_err(|_| "proxy must be a valid URL".to_owned())?;
    if url.scheme() != "http" {
        return Err("proxy URL must use http://".to_owned());
    }
    if url.host().is_none() {
        return Err("proxy URL must include a host".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("proxy credentials are not accepted on the command line".to_owned());
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err("proxy URL must not include a path, query or fragment".to_owned());
    }

    // Ensure the stored value has only the normalized scheme and authority.
    url.set_path("");
    Ok(url.to_string())
}

fn normalize_bypass_list(input: &str) -> Result<String, String> {
    if input.is_empty() {
        return Ok(String::new());
    }
    if input.len() > MAX_BYPASS_LENGTH || input.chars().any(char::is_control) {
        return Err("proxy bypass list is too long or contains a control character".to_owned());
    }

    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for raw_rule in input.split(',') {
        let rule = raw_rule.trim();
        if rule.is_empty() {
            return Err("proxy bypass list contains an empty entry".to_owned());
        }
        if normalized.len() == MAX_BYPASS_RULES {
            return Err(format!(
                "proxy bypass list may contain at most {MAX_BYPASS_RULES} entries"
            ));
        }
        if rule.len() > MAX_BYPASS_RULE_LENGTH {
            return Err("proxy bypass entry is too long".to_owned());
        }
        if rule == "*" {
            return Err("proxy bypass '*' is not allowed because it disables the proxy".to_owned());
        }

        let rule = normalize_bypass_rule(rule)?;
        if seen.insert(rule.clone()) {
            normalized.push(rule);
        }
    }
    Ok(normalized.join(","))
}

fn normalize_bypass_rule(input: &str) -> Result<String, String> {
    if let Some((address, prefix)) = input.split_once('/') {
        let address = IpAddr::from_str(address.trim_matches(['[', ']']))
            .map_err(|_| "proxy bypass CIDR must start with an IP address".to_owned())?;
        let prefix = prefix
            .parse::<u8>()
            .map_err(|_| "proxy bypass CIDR has an invalid prefix".to_owned())?;
        let max_prefix = if address.is_ipv4() { 32 } else { 128 };
        if prefix > max_prefix {
            return Err("proxy bypass CIDR prefix is out of range".to_owned());
        }
        return Ok(format!("{address}/{prefix}"));
    }

    let input = input.strip_prefix('.').unwrap_or(input);
    let input = input.strip_suffix('.').unwrap_or(input);
    if input.is_empty() {
        return Err("proxy bypass entry must include a host".to_owned());
    }
    let host = Host::parse(input)
        .map_err(|_| "proxy bypass entry must be a host, IP address or CIDR".to_owned())?;
    Ok(host.to_string().to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_uri_is_bounded_normalized_and_has_no_credentials() {
        assert_eq!(
            StartupProxy::parse("http://Proxy.Example:8080", None)
                .unwrap()
                .uri(),
            "http://proxy.example:8080/"
        );
        for invalid in [
            "",
            " http://proxy.example:8080",
            "https://proxy.example:8080",
            "http://user:secret@proxy.example:8080",
            "http://proxy.example:8080/path",
            "http://proxy.example:8080/?query",
            "http://proxy.example:8080/#fragment",
        ] {
            assert!(StartupProxy::parse(invalid, None).is_err(), "{invalid}");
        }
    }

    #[test]
    fn bypass_rules_are_normalized_deduplicated_and_bounded() {
        let proxy = StartupProxy::parse(
            "http://127.0.0.1:8765",
            Some(" Example.COM.,example.com,127.0.0.1,10.0.0.0/8,[::1] "),
        )
        .unwrap();
        assert_eq!(proxy.bypass(), "example.com,127.0.0.1,10.0.0.0/8,[::1]");

        for invalid in [
            "*",
            "example.com,",
            "https://example.com",
            "example.com:8080",
            "10.0.0.0/33",
            "not-an-ip/8",
        ] {
            assert!(
                StartupProxy::parse("http://proxy.example", Some(invalid)).is_err(),
                "{invalid}"
            );
        }
    }
}
