use std::ffi::c_void;

use glint_core::log::log;
use glint_core::upload::proxy::{Route, Setting, fixed, pick};
use windows::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows::Win32::Networking::WinHttp::*;
use windows::core::{HSTRING, PCWSTR, PWSTR};

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

pub fn describe_system() -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

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
