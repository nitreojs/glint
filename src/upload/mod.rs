use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};

use crate::config::{Config, Expiry};
use crate::history::Kind;
use crate::platform::format::human_size;
use crate::platform::log::log;

pub(crate) mod custom;
mod http;
pub(crate) mod proxy;
mod syntax;

use custom::Sxcu;
use http::Part;
use proxy::Setting;

pub(crate) const BUILT_IN: [&str; 4] = ["catbox", "litterbox", "uguu", "pomf"];

pub(crate) const WM_UPLOADED: u32 = WM_APP + 50;
pub(crate) const WM_UPLOAD_PROGRESS: u32 = WM_APP + 51;

const MB: u64 = 1024 * 1024;

static ACTIVE: AtomicUsize = AtomicUsize::new(0);

struct Limit {
    bytes: u64,
    text: &'static str,
}

pub(crate) struct Destination {
    pub name: String,
    sxcu: Sxcu,
    limit: Option<Limit>,
    proxy: Setting,
}

pub(crate) struct Payload {
    pub name: String,
    pub size: u64,
    part: Part,
}

pub(crate) struct Done {
    pub kind: Kind,
    pub path: Option<PathBuf>,
    pub size: u64,
    pub host: String,
    pub result: Result<String, String>,
}

impl Payload {
    pub(crate) fn bytes(name: String, bytes: Vec<u8>) -> Payload {
        Payload { name, size: bytes.len() as u64, part: Part::Bytes(bytes) }
    }

    pub(crate) fn file(path: &Path) -> Result<Payload, String> {
        let size = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.len();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        Ok(Payload { name, size, part: Part::File(path.to_path_buf()) })
    }

    fn mime(&self) -> &'static str {
        let ext = self.name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "mp4" => "video/mp4",
            _ => "application/octet-stream",
        }
    }
}

fn built_in(name: &str, expiry: Expiry) -> Option<Destination> {
    let multipart = |url: &str, field: &str, arguments: &[(&str, &str)], link: &str, error: &str| Sxcu {
        version: "17.0.0".into(),
        name: name.into(),
        method: "POST".into(),
        request_url: url.into(),
        body: "MultipartFormData".into(),
        arguments: arguments.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        file_form_name: field.into(),
        url: link.into(),
        error_message: error.into(),
        ..Sxcu::default()
    };
    let (sxcu, bytes, text) = match name {
        "catbox" => (
            multipart("https://catbox.moe/user/api.php", "fileToUpload", &[("reqtype", "fileupload")], "", ""),
            200 * MB,
            "200 MB",
        ),
        "litterbox" => (
            multipart(
                "https://litterbox.catbox.moe/resources/internals/api.php",
                "fileToUpload",
                &[("reqtype", "fileupload"), ("time", expiry.name())],
                "",
                "",
            ),
            1024 * MB,
            "1 GB",
        ),
        "uguu" => (
            multipart("https://uguu.se/upload.php", "files[]", &[], "{json:files[0].url}", "{json:description}"),
            128 * MB,
            "128 MB",
        ),
        "pomf" => (
            multipart("https://pomf.lain.la/upload.php", "files[]", &[], "{json:files[0].url}", "{json:description}"),
            1024 * MB,
            "1 GB",
        ),
        _ => return None,
    };
    Some(Destination { name: name.into(), sxcu, limit: Some(Limit { bytes, text }), proxy: Setting::System })
}

impl Destination {
    pub(crate) fn from_config(cfg: &Config) -> Result<Destination, String> {
        let proxy = proxy::parse(&cfg.upload_proxy).map_err(|e| format!("upload proxy: {e}"))?;
        let name = cfg.upload_destination.trim();
        if let Some(d) = built_in(name, cfg.litterbox_expiry) {
            return Ok(Destination { proxy, ..d });
        }
        Ok(Destination { name: name.into(), sxcu: custom::load(name)?, limit: None, proxy })
    }

    fn check_size(&self, file: &Payload) -> Result<(), String> {
        let gif = file.name.to_ascii_lowercase().ends_with(".gif");
        // catbox's faq: gifs over 20 mb are refused
        if self.name == "catbox" && gif && file.size > 20 * MB {
            return Err(format!("catbox takes GIFs up to 20 MB; this one is {}", human_size(file.size)));
        }
        match &self.limit {
            Some(limit) if file.size > limit.bytes => {
                Err(format!("{} takes files up to {}; this one is {}", self.name, limit.text, human_size(file.size)))
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn upload(&self, file: Payload) -> Result<custom::Uploaded, String> {
        self.upload_with(file, &mut |_, _| {})
    }

    fn upload_with(&self, file: Payload, progress: &mut dyn FnMut(u64, u64)) -> Result<custom::Uploaded, String> {
        self.check_size(&file)?;
        let mime = file.mime();
        let request = self.sxcu.request(&file.name, mime, file.part)?;
        let response = http::send(&request, &self.proxy, progress)?;
        self.sxcu.response(&self.name, &file.name, &response)
    }
}

pub(crate) fn active() -> bool {
    ACTIVE.load(Ordering::SeqCst) > 0
}

pub(crate) fn start(
    cfg: &Config,
    kind: Kind,
    path: Option<PathBuf>,
    file: Payload,
    notify: HWND,
) -> Result<(), String> {
    let dest = Destination::from_config(cfg)?;
    dest.check_size(&file)?;
    let hwnd = notify.0 as isize;
    ACTIVE.fetch_add(1, Ordering::SeqCst);
    std::thread::Builder::new()
        .name("upload".into())
        .spawn(move || {
            let size = file.size;
            let mut shown = None;
            let mut progress = |sent: u64, total: u64| {
                let percent = (sent * 100).checked_div(total).unwrap_or(100);
                if shown != Some(percent) {
                    shown = Some(percent);
                    let _ = unsafe {
                        PostMessageW(Some(HWND(hwnd as _)), WM_UPLOAD_PROGRESS, WPARAM(percent as usize), LPARAM(0))
                    };
                }
            };
            let result = dest.upload_with(file, &mut progress).map(|up| {
                if let Some(d) = &up.deletion_url {
                    log(&format!("uploaded {} to {}, delete with {d}", up.url, dest.name));
                }
                up.url
            });
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
            let done = Box::into_raw(Box::new(Done { kind, path, size, host: dest.name, result }));
            unsafe {
                if PostMessageW(Some(HWND(hwnd as _)), WM_UPLOADED, WPARAM(0), LPARAM(done as isize)).is_err() {
                    drop(Box::from_raw(done));
                }
            }
        })
        .map(|_| ())
        .map_err(|e| {
            ACTIVE.fetch_sub(1, Ordering::SeqCst);
            format!("can't start the upload: {e}")
        })
}

pub(crate) fn start_file(cfg: &Config, kind: Kind, path: &Path, notify: HWND) -> Result<(), String> {
    start(cfg, kind, Some(path.to_path_buf()), Payload::file(path)?, notify)
}

fn random_u64() -> u64 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.finish()
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out += &format!("%{b:02X}");
        }
    }
    out
}

fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | (u32::from(b) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[((n >> (18 - 6 * i)) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(name: &str) -> Destination {
        built_in(name, Expiry::Hour).unwrap()
    }

    fn sized(name: &str, size: u64) -> Payload {
        Payload { name: name.into(), size, part: Part::Bytes(Vec::new()) }
    }

    #[test]
    fn size_limits_refuse_early() {
        assert!(dest("catbox").check_size(&sized("a.mp4", 200 * MB)).is_ok());
        let err = dest("catbox").check_size(&sized("a.mp4", 250 * MB)).unwrap_err();
        assert_eq!(err, "catbox takes files up to 200 MB; this one is 250.0 MB");
        assert!(dest("catbox").check_size(&sized("a.GIF", 21 * MB)).unwrap_err().contains("GIFs up to 20 MB"));
        assert!(dest("litterbox").check_size(&sized("a.gif", 21 * MB)).is_ok());
        assert!(dest("litterbox").check_size(&sized("a.mp4", 1024 * MB + 1)).is_err());
        assert!(dest("uguu").check_size(&sized("a.png", 128 * MB)).is_ok());
        assert!(dest("uguu").check_size(&sized("a.png", 128 * MB + 1)).is_err());
        assert!(dest("pomf").check_size(&sized("a.mp4", 1024 * MB + 1)).is_err());
        assert!(built_in("imgur", Expiry::Day).is_none());
    }

    #[test]
    fn built_ins_build_their_requests() {
        let req = dest("litterbox").sxcu.request("a.png", "image/png", Part::Bytes(vec![7])).unwrap();
        assert_eq!(req.url, "https://litterbox.catbox.moe/resources/internals/api.php");
        let Part::Bytes(head) = &req.body[0] else { panic!() };
        let head = String::from_utf8_lossy(head);
        assert!(
            head.contains("name=\"reqtype\"\r\n\r\nfileupload\r\n") && head.contains("name=\"time\"\r\n\r\n1h\r\n")
        );
        assert!(head.contains("name=\"fileToUpload\"; filename=\"a.png\""));
    }

    #[test]
    fn documented_host_responses_parse() {
        let ok = |text: &str| http::Response { status: 200, text: text.into(), ..Default::default() };
        // catbox.moe.sxcu has no URL template: the response body is the link
        let catbox = dest("catbox");
        let up = catbox.sxcu.response("catbox", "a.png", &ok("https://files.catbox.moe/3k9x2a.png")).unwrap();
        assert_eq!(up.url, "https://files.catbox.moe/3k9x2a.png");
        // the pomf api as read by sharex's Pomf.cs: {"success":true,"files":[{hash,name,url,size}]}
        let pomf = dest("pomf");
        let body = r#"{"success":true,"files":[{"hash":"0a1b","name":"a.png","url":"https://pomf2.lain.la/f/x1y2z3.png","size":1234}]}"#;
        assert_eq!(pomf.sxcu.response("pomf", "a.png", &ok(body)).unwrap().url, "https://pomf2.lain.la/f/x1y2z3.png");
        let fail = http::Response {
            status: 400,
            text: r#"{"success":false,"errorcode":400,"description":"No input file(s)"}"#.into(),
            ..Default::default()
        };
        assert_eq!(pomf.sxcu.response("pomf", "a.png", &fail).unwrap_err(), "pomf answered 400: No input file(s)");
    }

    #[test]
    fn encoders() {
        assert_eq!(url_encode("a b&c/ü~"), "a%20b%26c%2F%C3%BC~");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        assert_ne!(random_u64(), random_u64());
    }
}
