use std::ffi::c_void;
use std::fmt;
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use windows::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR, PWSTR};

use crate::platform::log::log;

const SOCKS: &str =
    "socks proxies can't carry uploads (windows' http stack only speaks http proxies): use the proxy's http port";

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Setting {
    System,
    None,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Route {
    Direct,
    Proxy(String),
}

impl fmt::Display for Route {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Route::Direct => f.write_str("directly"),
            Route::Proxy(p) => write!(f, "through the proxy {p}"),
        }
    }
}

pub(crate) fn parse(value: &str) -> Result<Setting, String> {
    let v = value.trim();
    if v.is_empty() || v.eq_ignore_ascii_case("system") {
        return Ok(Setting::System);
    }
    if v.eq_ignore_ascii_case("none") {
        return Ok(Setting::None);
    }
    let bad = || format!("proxy must be \"system\", \"none\" or http://host:port, not \"{v}\"");
    let rest = match v.split_once("://") {
        Some((scheme, rest)) if scheme.eq_ignore_ascii_case("http") => rest,
        Some((scheme, _)) if scheme.to_ascii_lowercase().starts_with("socks") => return Err(SOCKS.into()),
        Some(_) => return Err(bad()),
        None => v,
    };
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    let (host, port) = rest.rsplit_once(':').ok_or_else(bad)?;
    let port: u16 = port.parse().ok().filter(|p| *p != 0).ok_or_else(bad)?;
    if host.is_empty() || host.contains(['/', '@', ';', ' ', '=']) {
        return Err(bad());
    }
    Ok(Setting::Custom(format!("{host}:{port}")))
}

pub(crate) fn route(
    setting: &Setting,
    session: *mut c_void,
    url: &str,
    host: &str,
    secure: bool,
) -> Result<Route, String> {
    match setting {
        Setting::None => Ok(Route::Direct),
        Setting::Custom(p) => Ok(Route::Proxy(p.clone())),
        Setting::System => {
            let ie = Ie::current();
            if ie.auto_detect || ie.pac.is_some() {
                match unsafe { auto_config(session, url, &ie) } {
                    Ok(route) => return Ok(route),
                    Err(code) => log(&format!(
                        "proxy auto-config for {host} failed (winhttp error {code}), using the fixed proxy"
                    )),
                }
            }
            fixed(ie.proxy.as_deref(), ie.bypass.as_deref(), host, secure)
        }
    }
}

pub(crate) fn describe_system() -> String {
    let ie = Ie::current();
    let mut parts = Vec::new();
    if let Some(pac) = &ie.pac {
        parts.push(format!("script {pac}"));
    }
    if ie.auto_detect {
        parts.push("auto-detect".to_string());
    }
    match ie.proxy.as_deref().map(|p| pick(p, true)) {
        Some(Ok(Some(p))) => parts.push(p),
        Some(Err(e)) => return e,
        Some(Ok(None)) | None => {}
    }
    if parts.is_empty() {
        "same as browsers: windows has no proxy set, so uploads go direct".into()
    } else {
        format!("same as browsers: windows' proxy {}", parts.join(", "))
    }
}

#[derive(Default)]
struct Ie {
    auto_detect: bool,
    pac: Option<String>,
    proxy: Option<String>,
    bypass: Option<String>,
}

impl Ie {
    fn current() -> Ie {
        let mut c = WINHTTP_CURRENT_USER_IE_PROXY_CONFIG::default();
        if unsafe { WinHttpGetIEProxyConfigForCurrentUser(&mut c) }.is_err() {
            return Ie::default();
        }
        Ie {
            auto_detect: c.fAutoDetect.as_bool(),
            pac: take(c.lpszAutoConfigUrl),
            proxy: take(c.lpszProxy),
            bypass: take(c.lpszProxyBypass),
        }
    }
}

fn take(p: PWSTR) -> Option<String> {
    if p.is_null() {
        return None;
    }
    let s = unsafe { p.to_string() }.ok();
    let _ = unsafe { GlobalFree(Some(HGLOBAL(p.0.cast()))) };
    s.filter(|s| !s.trim().is_empty())
}

unsafe fn auto_config(session: *mut c_void, url: &str, ie: &Ie) -> Result<Route, u32> {
    let script = ie.pac.as_deref().map(HSTRING::from);
    let mut options = WINHTTP_AUTOPROXY_OPTIONS {
        dwFlags: (if script.is_some() { WINHTTP_AUTOPROXY_CONFIG_URL } else { 0 })
            | (if ie.auto_detect { WINHTTP_AUTOPROXY_AUTO_DETECT } else { 0 }),
        dwAutoDetectFlags: if ie.auto_detect {
            WINHTTP_AUTO_DETECT_TYPE_DHCP | WINHTTP_AUTO_DETECT_TYPE_DNS_A
        } else {
            0
        },
        lpszAutoConfigUrl: script.as_ref().map_or(PCWSTR::null(), |s| PCWSTR(s.as_ptr())),
        fAutoLogonIfChallenged: true.into(),
        ..Default::default()
    };
    let mut info = WINHTTP_PROXY_INFO::default();
    unsafe { WinHttpGetProxyForUrl(session, &HSTRING::from(url), &mut options, &mut info) }
        .map_err(|e| (e.code().0 as u32) & 0xFFFF)?;
    let proxy = take(info.lpszProxy);
    let _ = take(info.lpszProxyBypass);
    Ok(match proxy {
        Some(p) if info.dwAccessType == WINHTTP_ACCESS_TYPE_NAMED_PROXY => Route::Proxy(p),
        _ => Route::Direct,
    })
}

pub(crate) fn listening(proxy: &str) -> bool {
    let first = proxy.split([';', ' ']).map(address).find(|p| !p.is_empty()).unwrap_or_default();
    let Some(addr) = first.to_socket_addrs().ok().and_then(|mut a| a.next()) else { return false };
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
}

fn fixed(proxy: Option<&str>, bypass: Option<&str>, host: &str, secure: bool) -> Result<Route, String> {
    let Some(proxy) = proxy else { return Ok(Route::Direct) };
    if bypassed(host, bypass.unwrap_or_default()) {
        return Ok(Route::Direct);
    }
    Ok(pick(proxy, secure)?.map_or(Route::Direct, Route::Proxy))
}

fn pick(list: &str, secure: bool) -> Result<Option<String>, String> {
    let want = if secure { "https" } else { "http" };
    let (mut bare, mut socks) = (None, None);
    for entry in list.split([';', ' ', '\t']).map(str::trim).filter(|e| !e.is_empty()) {
        match entry.split_once('=') {
            Some((scheme, addr)) if scheme.trim().eq_ignore_ascii_case(want) => return Ok(Some(address(addr))),
            Some((scheme, addr)) if scheme.trim().eq_ignore_ascii_case("socks") => socks = socks.or(Some(addr)),
            Some(_) => {}
            None => bare = bare.or(Some(entry)),
        }
    }
    match (bare, socks) {
        (Some(b), _) => Ok(Some(address(b))),
        (None, Some(s)) => Err(format!("windows' proxy is socks only ({}): {SOCKS}", address(s))),
        (None, None) => Ok(None),
    }
}

fn address(entry: &str) -> String {
    let entry = entry.trim();
    let entry = entry.split_once("://").map_or(entry, |(_, rest)| rest);
    entry.trim_end_matches('/').to_string()
}

fn bypassed(host: &str, list: &str) -> bool {
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host == "::1" || host.starts_with("127.") {
        return true;
    }
    list.split([';', ',', ' ', '\t']).map(str::trim).filter(|p| !p.is_empty()).any(|p| {
        if p.eq_ignore_ascii_case("<local>") {
            return !host.contains(['.', ':']);
        }
        let p = p.split_once("://").map_or(p, |(_, rest)| rest).to_ascii_lowercase();
        glob(p.as_bytes(), host.as_bytes())
    })
}

fn glob(p: &[u8], s: &[u8]) -> bool {
    let (mut pi, mut si, mut star, mut mark) = (0, 0, None, 0);
    while si < s.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            mark = si;
            pi += 1;
        } else if pi < p.len() && p[pi] == s[si] {
            pi += 1;
            si += 1;
        } else if let Some(st) = star {
            pi = st + 1;
            mark += 1;
            si = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_parse() {
        assert_eq!(parse("system"), Ok(Setting::System));
        assert_eq!(parse("  "), Ok(Setting::System));
        assert_eq!(parse("None"), Ok(Setting::None));
        assert_eq!(parse("http://127.0.0.1:10809"), Ok(Setting::Custom("127.0.0.1:10809".into())));
        assert_eq!(parse("HTTP://proxy.lan:3128/"), Ok(Setting::Custom("proxy.lan:3128".into())));
        assert_eq!(parse("127.0.0.1:8080"), Ok(Setting::Custom("127.0.0.1:8080".into())));
        assert_eq!(parse("http://[::1]:8080"), Ok(Setting::Custom("[::1]:8080".into())));
        assert!(parse("socks5://127.0.0.1:10808").unwrap_err().contains("http port"));
        assert!(parse("socks5h://127.0.0.1:10808").unwrap_err().contains("socks"));
        for bad in ["https://p:1", "http://p", "p:0", "p:99999", "http://:80", "http://u@p:80", "direct"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn fixed_proxy_lists_pick_by_scheme() {
        assert_eq!(pick("127.0.0.1:10809", true), Ok(Some("127.0.0.1:10809".into())));
        assert_eq!(pick("http://127.0.0.1:10809", false), Ok(Some("127.0.0.1:10809".into())));
        let list = "http=h:80;https=s:443;socks=k:1080";
        assert_eq!(pick(list, true), Ok(Some("s:443".into())));
        assert_eq!(pick(list, false), Ok(Some("h:80".into())));
        assert_eq!(pick("http=h:80", true), Ok(None));
        assert_eq!(pick("ftp=f:21; https=s:1", true), Ok(Some("s:1".into())));
        assert!(pick("socks=127.0.0.1:10808", true).unwrap_err().contains("socks only (127.0.0.1:10808)"));
        assert_eq!(pick("", true), Ok(None));
    }

    #[test]
    fn bypass_lists_match_hosts() {
        let list = "localhost;127.*;10.*;192.168.*;*.corp.example;<local>";
        assert!(bypassed("10.1.2.3", list));
        assert!(bypassed("printer", list));
        assert!(bypassed("Build.Corp.Example", list));
        assert!(bypassed("127.0.0.1", ""));
        assert!(!bypassed("catbox.moe", list));
        assert!(!bypassed("corp.example", list));
        assert!(!bypassed("110.1.2.3", list));
        assert!(bypassed("files.catbox.moe", "http://*.catbox.moe"));
        assert!(glob(b"a*b*c", b"axxbyyc") && !glob(b"a*b", b"axxc") && glob(b"*", b""));
    }

    #[test]
    fn dead_proxies_are_told_apart() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = server.local_addr().unwrap().port();
        assert!(listening(&format!("127.0.0.1:{port}")));
        assert!(listening(&format!("http://127.0.0.1:{port}; 10.0.0.1:1")));
        drop(server);
        assert!(!listening(&format!("127.0.0.1:{port}")));
        assert!(!listening("not a proxy"));
    }

    #[test]
    fn routes_follow_the_fixed_proxy() {
        let bypass = Some("localhost;127.*;192.168.*;<local>");
        let proxy = Some("127.0.0.1:10809");
        assert_eq!(fixed(proxy, bypass, "catbox.moe", true), Ok(Route::Proxy("127.0.0.1:10809".into())));
        assert_eq!(fixed(proxy, bypass, "192.168.1.5", true), Ok(Route::Direct));
        assert_eq!(fixed(None, None, "catbox.moe", true), Ok(Route::Direct));
        assert!(fixed(Some("socks=127.0.0.1:10808"), None, "catbox.moe", true).is_err());
        assert_eq!(route(&Setting::None, std::ptr::null_mut(), "https://x/", "x", true), Ok(Route::Direct));
        let custom = Setting::Custom("p:1".into());
        assert_eq!(route(&custom, std::ptr::null_mut(), "https://x/", "x", true), Ok(Route::Proxy("p:1".into())));
        assert_eq!(Route::Proxy("p:1".into()).to_string(), "through the proxy p:1");
    }
}
