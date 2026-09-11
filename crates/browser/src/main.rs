//! Entry point: environment setup and the winit event loop.
//!
//! The TLS provider must be installed before the event loop starts so
//! that every network stack thread (Servo's included) shares the same
//! crypto provider.

#![forbid(unsafe_code)]

mod app;
mod clipboard;
mod gui;
mod ime;
mod proxy;
mod reader;
mod subscriptions;
mod waker;

use std::error::Error;

use app::{App, AppEvent};
use proxy::StartupProxy;
use winit::event_loop::EventLoop;

#[derive(Debug, Default, Eq, PartialEq)]
struct StartupOptions {
    initial_url: Option<String>,
    allow_trackers_for: Vec<String>,
    proxy: Option<StartupProxy>,
}

fn parse_startup_options(args: impl IntoIterator<Item = String>) -> Result<StartupOptions, String> {
    let mut options = StartupOptions::default();
    let mut args = args.into_iter();
    let mut proxy_uri = None;
    let mut proxy_bypass = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--allow-trackers-for" => {
                let host = args
                    .next()
                    .ok_or_else(|| "--allow-trackers-for requires a host".to_owned())?;
                options.allow_trackers_for.push(host);
            }
            "--proxy" => {
                let uri = args
                    .next()
                    .ok_or_else(|| "--proxy requires an http:// URL".to_owned())?;
                if proxy_uri.replace(uri).is_some() {
                    return Err("--proxy may be supplied only once".to_owned());
                }
            }
            "--proxy-bypass" => {
                let bypass = args
                    .next()
                    .ok_or_else(|| "--proxy-bypass requires a comma-separated list".to_owned())?;
                if proxy_bypass.replace(bypass).is_some() {
                    return Err("--proxy-bypass may be supplied only once".to_owned());
                }
            }
            _ if argument.starts_with('-') => {
                return Err(format!("unknown option: {argument}"));
            }
            _ if options.initial_url.is_none() => options.initial_url = Some(argument),
            _ => return Err("only one initial URL may be supplied".to_owned()),
        }
    }
    options.proxy = match (proxy_uri, proxy_bypass) {
        (Some(uri), bypass) => Some(StartupProxy::parse(&uri, bypass.as_deref())?),
        (None, Some(_)) => return Err("--proxy-bypass requires --proxy".to_owned()),
        (None, None) => None,
    };
    Ok(options)
}

fn main() -> Result<(), Box<dyn Error>> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let options = parse_startup_options(std::env::args().skip(1))
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
    let event_loop = EventLoop::<AppEvent>::with_user_event().build()?;
    let mut app = App::new(
        &event_loop,
        options.initial_url,
        options.allow_trackers_for,
        options.proxy,
    );
    event_loop.run_app(&mut app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn startup_options_accept_one_url_and_repeatable_tracker_hosts() {
        assert_eq!(
            parse_startup_options(strings(&[
                "--allow-trackers-for",
                "Example.COM.",
                "http://site.test/page",
                "--allow-trackers-for",
                "other.test",
            ]))
            .unwrap(),
            StartupOptions {
                initial_url: Some("http://site.test/page".to_owned()),
                allow_trackers_for: strings(&["Example.COM.", "other.test"]),
                proxy: None,
            }
        );
    }

    #[test]
    fn startup_options_reject_missing_values_unknown_flags_and_extra_urls() {
        assert!(parse_startup_options(strings(&["--allow-trackers-for"])).is_err());
        assert!(parse_startup_options(strings(&["--unknown"])).is_err());
        assert!(parse_startup_options(strings(&["https://one.test", "https://two.test"])).is_err());
    }

    #[test]
    fn startup_options_validate_proxy_before_creating_the_window() {
        let options = parse_startup_options(strings(&[
            "--proxy-bypass",
            "localhost,127.0.0.1",
            "--proxy",
            "http://Proxy.Test:8765",
        ]))
        .unwrap();
        assert_eq!(
            options.proxy,
            Some(
                StartupProxy::parse("http://proxy.test:8765", Some("localhost,127.0.0.1")).unwrap()
            )
        );
        assert!(parse_startup_options(strings(&["--proxy-bypass", "localhost"])).is_err());
        assert!(parse_startup_options(strings(&["--proxy", "socks5://proxy.test"])).is_err());
        assert!(parse_startup_options(strings(&[
            "--proxy",
            "http://one.test",
            "--proxy",
            "http://two.test",
        ]))
        .is_err());
    }
}
