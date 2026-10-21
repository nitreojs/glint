use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::http::{Part, Request, Response};
use super::syntax::{Context, short};
use super::{BUILT_IN, random_u64, url_encode};
use crate::config::BAD_NAME_CHARS;
use crate::paths::app_dir;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Sxcu {
    pub version: String,
    pub name: String,
    pub destination_type: String,
    pub method: String,
    pub request_url: String,
    pub parameters: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub arguments: Vec<(String, String)>,
    pub file_form_name: String,
    pub url: String,
    pub thumbnail_url: String,
    pub deletion_url: String,
    pub error_message: String,
    pub regex_list: Vec<String>,
}

#[derive(Debug)]
pub struct Uploaded {
    pub url: String,
    pub deletion_url: Option<String>,
}

const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];

fn scalar(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn version(v: &str) -> Vec<u32> {
    v.split('.').map(|p| p.trim().parse().unwrap_or(0)).collect()
}

impl Sxcu {
    pub(crate) fn parse(text: &str) -> Result<Sxcu, String> {
        let value: Value = serde_json::from_str(text.trim_start_matches('\u{feff}'))
            .map_err(|e| format!("not a ShareX uploader: {e}"))?;
        let Value::Object(map) = value else { return Err("not a ShareX uploader".into()) };
        Sxcu::from_map(&map)
    }

    fn from_map(map: &Map<String, Value>) -> Result<Sxcu, String> {
        let get = |key: &str| map.get(key).map(scalar).unwrap_or_default();
        let pairs = |key: &str| match map.get(key) {
            Some(Value::Object(m)) => m.iter().map(|(k, v)| (k.clone(), scalar(v))).collect(),
            _ => Vec::new(),
        };
        let mut sxcu = Sxcu {
            version: get("Version"),
            name: get("Name"),
            destination_type: get("DestinationType"),
            method: get("RequestMethod").to_ascii_uppercase(),
            request_url: get("RequestURL"),
            parameters: pairs("Parameters"),
            headers: pairs("Headers"),
            body: get("Body"),
            arguments: pairs("Arguments"),
            file_form_name: get("FileFormName"),
            url: get("URL"),
            thumbnail_url: get("ThumbnailURL"),
            deletion_url: get("DeletionURL"),
            error_message: get("ErrorMessage"),
            regex_list: match map.get("RegexList") {
                Some(Value::Array(list)) => list.iter().map(scalar).collect(),
                _ => Vec::new(),
            },
        };
        if sxcu.method.is_empty() {
            sxcu.method = "POST".into();
        }
        if sxcu.body.is_empty() {
            sxcu.body = "None".into();
        }
        sxcu.check()?;
        sxcu.query_to_parameters();
        if version(&sxcu.version) <= version("13.7.1") {
            sxcu.migrate_old_syntax();
        }
        Ok(sxcu)
    }

    fn check(&self) -> Result<(), String> {
        if self.version.is_empty() || version(&self.version) <= version("12.3.1") {
            return Err(format!("{} was made for ShareX 12.3.1 or older, which isn't supported", self.title()));
        }
        if self.request_url.trim().is_empty() {
            return Err(format!("{} has no request URL", self.title()));
        }
        if !METHODS.contains(&self.method.as_str()) {
            return Err(format!("{} uses an unknown request method {}", self.title(), self.method));
        }
        let kinds = self.destination_type.to_ascii_lowercase();
        if !kinds.trim().is_empty()
            && kinds != "none"
            && !kinds.contains("imageuploader")
            && !kinds.contains("fileuploader")
        {
            return Err(format!("{} uploads text or links, not files ({})", self.title(), self.destination_type));
        }
        match self.body.to_ascii_lowercase().as_str() {
            "multipartformdata" if self.file_form_name.is_empty() => {
                Err(format!("{} has no file form name", self.title()))
            }
            "multipartformdata" | "binary" => Ok(()),
            _ => Err(format!("{} sends {} bodies; files need multipart form data or binary", self.title(), self.body)),
        }
    }

    pub(crate) fn title(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        let host = self.request_url.split_once("://").map_or(self.request_url.as_str(), |(_, rest)| rest);
        let host = host.split(['/', '?', ':']).next().unwrap_or_default();
        let host = host.strip_prefix("www.").unwrap_or(host);
        if host.is_empty() { "Name".into() } else { host.to_string() }
    }

    fn query_to_parameters(&mut self) {
        let Some((base, query)) = self.request_url.split_once('?') else { return };
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = match pair.split_once('=') {
                Some((k, v)) => (url_decode(k), url_decode(v)),
                None => (url_decode(pair), String::new()),
            };
            if !self.parameters.iter().any(|(k, _)| *k == key) {
                self.parameters.push((key, value));
            }
        }
        self.request_url = base.to_string();
    }

    fn migrate_old_syntax(&mut self) {
        self.request_url = migrate(&self.request_url);
        for (_, v) in self.parameters.iter_mut().chain(&mut self.headers).chain(&mut self.arguments) {
            *v = migrate(v);
        }
        for field in [&mut self.url, &mut self.thumbnail_url, &mut self.deletion_url, &mut self.error_message] {
            *field = migrate(field);
        }
    }

    fn context<'a>(&'a self, file_name: &'a str, url_encode: bool, response: Option<&'a Response>) -> Context<'a> {
        Context { file_name, url_encode, response, regex_list: &self.regex_list }
    }

    pub(crate) fn request(&self, file_name: &str, mime: &str, file: Part) -> Result<Request, String> {
        let plain = self.context(file_name, false, None);
        let mut url = self.context(file_name, true, None).parse(&self.request_url)?;
        if !url.contains("://") {
            url = format!("https://{url}");
        }
        let mut query = Vec::new();
        for (k, v) in &self.parameters {
            match plain.parse(v)? {
                v if v.is_empty() => query.push(k.clone()),
                v => query.push(format!("{k}={}", url_encode(&v))),
            }
        }
        if !query.is_empty() {
            url += if url.contains('?') { "&" } else { "?" };
            url += &query.join("&");
        }
        let mut headers = Vec::new();
        for (k, v) in &self.headers {
            headers.push((k.clone(), plain.parse(v)?));
        }
        let body = if self.body.eq_ignore_ascii_case("binary") {
            if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
                headers.push(("Content-Type".into(), mime.into()));
            }
            vec![file]
        } else {
            let mut fields = Vec::new();
            for (k, v) in &self.arguments {
                fields.push((k.clone(), plain.parse(v)?));
            }
            let boundary = format!("{}{:016x}{:016x}", "-".repeat(20), random_u64(), random_u64());
            headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
            headers.push(("Content-Type".into(), format!("multipart/form-data; boundary={boundary}")));
            multipart(&boundary, &fields, &self.file_form_name, file_name, mime, file)
        };
        let method = METHODS.into_iter().find(|m| *m == self.method).unwrap_or("POST");
        Ok(Request { method, url, headers, body })
    }

    pub(crate) fn response(&self, host: &str, file_name: &str, resp: &Response) -> Result<Uploaded, String> {
        let ctx = self.context(file_name, true, Some(resp));
        if !resp.ok() {
            let message = if self.error_message.is_empty() {
                String::new()
            } else {
                ctx.parse(&self.error_message).unwrap_or_default()
            };
            let message = if message.trim().is_empty() { short(&resp.text) } else { message };
            return Err(format!("{host} answered {}: {message}", resp.status).trim_end_matches([':', ' ']).to_string());
        }
        let url = if self.url.is_empty() { resp.text.clone() } else { ctx.parse(&self.url)? };
        let url = url.trim();
        let scheme = url.split_once("://").map(|(s, _)| s.to_ascii_lowercase());
        if !matches!(scheme.as_deref(), Some("http" | "https")) {
            return Err(format!("{host} didn't return a link: {}", short(&resp.text)));
        }
        let deletion_url = ctx.parse(&self.deletion_url).ok().filter(|d| !d.trim().is_empty());
        Ok(Uploaded { url: url.to_string(), deletion_url })
    }
}

// sharex's CustomUploaderItem.MigrateOldSyntax, quirks included: a backslash drops itself and the next character
fn migrate(input: &str) -> String {
    let mut out = String::new();
    let mut start = true;
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        match c {
            '$' => {
                out.push(if start { '{' } else { '}' });
                start = !start;
            }
            '\\' => {
                chars.next();
            }
            '{' | '}' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let hex = |i: usize| bytes.get(i..i + 2).and_then(|h| u8::from_str_radix(std::str::from_utf8(h).ok()?, 16).ok());
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match (bytes[i], hex(i + 1)) {
            (b'+', _) => out.push(b' '),
            (b'%', Some(v)) => {
                out.push(v);
                i += 2;
            }
            (b, _) => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn quote(s: &str) -> String {
    s.replace('"', "%22").replace(['\r', '\n'], " ")
}

pub(crate) fn multipart(
    boundary: &str,
    fields: &[(String, String)],
    file_field: &str,
    file_name: &str,
    mime: &str,
    file: Part,
) -> Vec<Part> {
    let mut head = String::new();
    for (k, v) in fields {
        head += &format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{v}\r\n", quote(k));
    }
    head += &format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {mime}\r\n\r\n",
        quote(file_field),
        quote(file_name)
    );
    let tail = format!("\r\n--{boundary}--\r\n");
    vec![Part::Bytes(head.into_bytes()), file, Part::Bytes(tail.into_bytes())]
}

pub(crate) fn dir() -> PathBuf {
    app_dir().join("uploaders")
}

pub fn names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir())
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sxcu")))
        .filter_map(|p| Some(p.file_stem()?.to_string_lossy().into_owned()))
        .collect();
    names.sort_by_key(|n| n.to_lowercase());
    names
}

pub(crate) fn load(name: &str) -> Result<Sxcu, String> {
    let path = dir().join(format!("{name}.sxcu"));
    let text =
        std::fs::read_to_string(&path).map_err(|_| format!("the uploader \"{name}\" isn't installed anymore"))?;
    Sxcu::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn remove(name: &str) -> Result<(), String> {
    let path = dir().join(format!("{name}.sxcu"));
    std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))
}

fn file_name_for(sxcu: &Sxcu) -> String {
    let name: String = sxcu.title().chars().map(|c| if BAD_NAME_CHARS.contains(&c) { '_' } else { c }).collect();
    let name = name.trim().trim_end_matches('.').to_string();
    if BUILT_IN.iter().any(|b| b.eq_ignore_ascii_case(&name)) { format!("{name} (custom)") } else { name }
}

fn install(sxcu: &Sxcu, text: &str) -> Result<String, String> {
    let name = file_name_for(sxcu);
    let dir = dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("{name}.sxcu"));
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(name)
}

pub fn import(path: &Path) -> Result<String, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let sxcu = Sxcu::parse(&text)?;
    install(&sxcu, &text)
}

pub fn sharex_config(documents: PathBuf) -> Option<PathBuf> {
    Some(documents.join("ShareX").join("UploadersConfig.json")).filter(|p| p.is_file())
}

pub fn import_sharex(config: &Path) -> Result<(Vec<String>, Vec<String>), String> {
    let text = std::fs::read_to_string(config).map_err(|e| format!("{}: {e}", config.display()))?;
    let value: Value =
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| format!("{}: {e}", config.display()))?;
    let list = match value.get("CustomUploadersList") {
        Some(Value::Array(list)) => list.clone(),
        _ => Vec::new(),
    };
    let (mut done, mut skipped) = (Vec::new(), Vec::new());
    for item in list {
        let Value::Object(map) = item else { continue };
        let text = serde_json::to_string_pretty(&map).unwrap_or_default();
        match Sxcu::from_map(&map).and_then(|s| install(&s, &text)) {
            Ok(name) => done.push(name),
            Err(e) => skipped.push(e),
        }
    }
    Ok((done, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CATBOX: &str = r#"{
  "Version": "12.4.1",
  "DestinationType": "ImageUploader, TextUploader, FileUploader",
  "RequestMethod": "POST",
  "RequestURL": "https://catbox.moe/user/api.php",
  "Body": "MultipartFormData",
  "Arguments": {
    "reqtype": "fileupload"
  },
  "FileFormName": "fileToUpload"
}"#;

    const UGUU: &str = r#"{
  "Version": "12.4.1",
  "DestinationType": "FileUploader, ImageUploader",
  "RequestMethod": "POST",
  "RequestURL": "https://uguu.se/upload.php",
  "Parameters": {
    "output": "text"
  },
  "Body": "MultipartFormData",
  "FileFormName": "files[]"
}"#;

    const EXAMPLE: &str = r#"{
  "Version": "13.3.0",
  "Name": "Example",
  "DestinationType": "ImageUploader, TextUploader, FileUploader",
  "RequestMethod": "POST",
  "RequestURL": "https://example.com/upload.php",
  "Parameters": {
    "Parameter1": "Value1",
    "Parameter2": "Value2",
    "Parameter3": "Value3"
  },
  "Headers": {
    "Header1": "Value1",
    "Header2": "Value2",
    "Header3": "Value3"
  },
  "Body": "MultipartFormData",
  "Arguments": {
    "Argument1": "Value1",
    "Argument2": "Value2",
    "Argument3": "Value3"
  },
  "FileFormName": "file",
  "URL": "$json:url$",
  "ThumbnailURL": "$json:thumbnail_url$",
  "DeletionURL": "$json:deletion_url$",
  "ErrorMessage": "$json:error$"
}"#;

    const JIRAFEAU: &str = r#"{
  "Version": "12.4.1",
  "Name": "Jirafeau",
  "DestinationType": "ImageUploader, TextUploader, FileUploader",
  "RequestMethod": "POST",
  "RequestURL": "https://jirafeau_domain_address/script.php",
  "Body": "MultipartFormData",
  "Arguments": {
    "upload_password": "put_password_here",
    "time": "month"
  },
  "FileFormName": "file",
  "RegexList": [
    "(.+)\\n(.+)"
  ],
  "URL": "http://jirafeau_domain_address/f.php?h=$regex:1|1$&p=1",
  "DeletionURL": "https://jirafeau_domain_address/f.php?h=$regex:1|1$&d=$regex:1|2$"
}"#;

    const DISCORD_SHORTENER: &str = r#"{
  "Version": "13.1.0",
  "Name": "Discord webhook (URL sharing service)",
  "DestinationType": "URLSharingService",
  "RequestMethod": "POST",
  "RequestURL": "Webhook URL",
  "Body": "JSON",
  "Data": "{\"content\":\"$input$\"}"
}"#;

    fn response(status: u32, text: &str) -> Response {
        Response { status, text: text.into(), headers: Vec::new(), url: String::new() }
    }

    fn bytes(parts: &[Part]) -> Vec<u8> {
        parts
            .iter()
            .flat_map(|p| match p {
                Part::Bytes(b) => b.clone(),
                Part::File(_) => panic!("file part"),
            })
            .collect()
    }

    #[test]
    fn sharex_examples_parse() {
        let catbox = Sxcu::parse(CATBOX).unwrap();
        assert_eq!(catbox.title(), "catbox.moe");
        assert_eq!(catbox.arguments, vec![("reqtype".to_string(), "fileupload".to_string())]);
        assert_eq!((catbox.file_form_name.as_str(), catbox.url.as_str()), ("fileToUpload", ""));

        let uguu = Sxcu::parse(UGUU).unwrap();
        assert_eq!(uguu.parameters, vec![("output".to_string(), "text".to_string())]);

        let example = Sxcu::parse(&format!("\u{feff}{EXAMPLE}")).unwrap();
        assert_eq!(example.title(), "Example");
        assert_eq!(example.headers.len(), 3);
        assert_eq!(example.url, "{json:url}");
        assert_eq!(example.error_message, "{json:error}");

        let jirafeau = Sxcu::parse(JIRAFEAU).unwrap();
        assert_eq!(jirafeau.regex_list, vec![r"(.+)\n(.+)".to_string()]);
        assert_eq!(jirafeau.url, "http://jirafeau_domain_address/f.php?h={regex:1|1}&p=1");

        assert!(Sxcu::parse(DISCORD_SHORTENER).unwrap_err().contains("not files"));
        let old = CATBOX.replace("12.4.1", "12.3.0");
        assert!(Sxcu::parse(&old).unwrap_err().contains("12.3.1"));
        assert!(Sxcu::parse("[1]").is_err() && Sxcu::parse("{nope").is_err());
        let binary =
            r#"{"Version":"15.0.0","RequestURL":"https://h/put/{filename}","RequestMethod":"put","Body":"Binary"}"#;
        assert_eq!(Sxcu::parse(binary).unwrap().method, "PUT");
    }

    #[test]
    fn old_syntax_migrates_like_sharex() {
        assert_eq!(migrate("http://i.pomf.pl/$json:files[0].url$"), "http://i.pomf.pl/{json:files[0].url}");
        assert_eq!(migrate("a{b}$response$"), r"a\{b\}{response}");
        assert_eq!(migrate(r"x\$y"), "xy");
    }

    #[test]
    fn requests_follow_the_uploader() {
        let sxcu = Sxcu::parse(EXAMPLE).unwrap();
        let req = sxcu.request("shot 1.png", "image/png", Part::Bytes(b"PNG".to_vec())).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "https://example.com/upload.php?Parameter1=Value1&Parameter2=Value2&Parameter3=Value3");
        assert!(req.headers.contains(&("Header2".to_string(), "Value2".to_string())));
        let (_, ct) = req.headers.iter().find(|(k, _)| k == "Content-Type").unwrap();
        let boundary = ct.strip_prefix("multipart/form-data; boundary=").unwrap();
        assert_eq!(boundary.len(), 52);
        let body = String::from_utf8(bytes(&req.body)).unwrap();
        assert!(body.starts_with(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"Argument1\"\r\n\r\nValue1\r\n--{boundary}"
        )));
        assert!(body.ends_with(&format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"shot 1.png\"\r\nContent-Type: image/png\r\n\r\nPNG\r\n--{boundary}--\r\n"
        )));

        let query = r#"{"Version":"15.0.0","RequestURL":"h.example/up?key=a%20b&flag","Parameters":{"name":"{filename}"},"Body":"Binary","Headers":{"Content-Type":"x/y"}}"#;
        let req = Sxcu::parse(query).unwrap().request("a&b.mp4", "video/mp4", Part::Bytes(vec![1])).unwrap();
        assert_eq!(req.url, "https://h.example/up?name=a%26b.mp4&key=a%20b&flag");
        assert_eq!(req.headers, vec![("Content-Type".to_string(), "x/y".to_string())]);
        assert_eq!(bytes(&req.body), vec![1]);
    }

    #[test]
    fn multipart_body_layout() {
        let fields = [("reqtype".to_string(), "fileupload".to_string()), ("time".to_string(), "1h".to_string())];
        let parts = multipart("XB", &fields, "fileToUpload", "a\"b.png", "image/png", Part::Bytes(b"\x89PNG".to_vec()));
        assert_eq!(
            bytes(&parts),
            b"--XB\r\nContent-Disposition: form-data; name=\"reqtype\"\r\n\r\nfileupload\r\n\
--XB\r\nContent-Disposition: form-data; name=\"time\"\r\n\r\n1h\r\n\
--XB\r\nContent-Disposition: form-data; name=\"fileToUpload\"; filename=\"a%22b.png\"\r\nContent-Type: image/png\r\n\r\n\x89PNG\r\n--XB--\r\n"
                .to_vec()
        );
    }

    #[test]
    fn responses_turn_into_links_or_errors() {
        let example = Sxcu::parse(EXAMPLE).unwrap();
        let ok = response(200, r#"{"url":"https://e.com/a.png","deletion_url":"https://e.com/d/1"}"#);
        let up = example.response("Example", "a.png", &ok).unwrap();
        assert_eq!((up.url.as_str(), up.deletion_url.as_deref()), ("https://e.com/a.png", Some("https://e.com/d/1")));
        let bad = response(400, r#"{"error":"file too big"}"#);
        assert_eq!(example.response("Example", "a.png", &bad).unwrap_err(), "Example answered 400: file too big");

        let catbox = Sxcu::parse(CATBOX).unwrap();
        let up = catbox.response("catbox", "a.png", &response(200, "https://files.catbox.moe/x1y2z3.png")).unwrap();
        assert_eq!(up.url, "https://files.catbox.moe/x1y2z3.png");
        let err = catbox.response("catbox", "a.png", &response(200, "No file uploaded")).unwrap_err();
        assert_eq!(err, "catbox didn't return a link: No file uploaded");
        assert_eq!(catbox.response("catbox", "a", &response(502, "")).unwrap_err(), "catbox answered 502");
        let html = response(403, "<!doctype html><html><title>403 | Forbidden</title><body>…</body></html>");
        assert_eq!(catbox.response("litterbox", "a", &html).unwrap_err(), "litterbox answered 403: 403 | Forbidden");
    }

    #[test]
    fn imported_names_avoid_built_ins() {
        let mut sxcu = Sxcu::parse(CATBOX).unwrap();
        assert_eq!(file_name_for(&sxcu), "catbox.moe");
        sxcu.name = "Catbox".into();
        assert_eq!(file_name_for(&sxcu), "Catbox (custom)");
        sxcu.name = "a/b: c.".into();
        assert_eq!(file_name_for(&sxcu), "a_b_ c");
    }
}
