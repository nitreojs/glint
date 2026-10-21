use serde_json::Value;

use super::http::Response;
use super::{base64, random_u64, url_encode};

pub(crate) struct Context<'a> {
    pub file_name: &'a str,
    pub url_encode: bool,
    pub response: Option<&'a Response>,
    pub regex_list: &'a [String],
}

impl Context<'_> {
    pub(crate) fn parse(&self, text: &str) -> Result<String, String> {
        let chars: Vec<char> = text.chars().collect();
        Ok(self.parse_at(&chars, false, 0)?.0)
    }

    fn parse_at(&self, text: &[char], function: bool, start: usize) -> Result<(String, usize), String> {
        let mut out = String::new();
        let mut escape = false;
        let mut i = start;
        while i < text.len() {
            let c = text[i];
            if !escape {
                match c {
                    '{' => {
                        let (parsed, end) = self.parse_at(text, true, i + 1)?;
                        out += &parsed;
                        i = end + 1;
                        continue;
                    }
                    '}' | '|' => break,
                    '\\' => {
                        escape = true;
                        i += 1;
                        continue;
                    }
                    ':' if function => {
                        let mut params = Vec::new();
                        loop {
                            let (param, end) = self.parse_at(text, false, i + 1)?;
                            params.push(param);
                            i = end;
                            if text.get(i) != Some(&'|') {
                                break;
                            }
                        }
                        return Ok((self.call(&out, &params)?, i));
                    }
                    _ => {}
                }
            }
            escape = false;
            out.push(c);
            i += 1;
        }
        if function {
            return Ok((self.call(&out, &[])?, i));
        }
        Ok((out, i))
    }

    fn response(&self) -> Result<&Response, String> {
        self.response.ok_or_else(|| "the response isn't known yet".to_string())
    }

    fn call(&self, name: &str, params: &[String]) -> Result<String, String> {
        let min = |n: usize| {
            if params.len() < n { Err(format!("{{{name}}} needs at least {n} parameter(s)")) } else { Ok(()) }
        };
        let interactive = || Err(format!("{{{name}:…}} asks for input while uploading, which glint can't do"));
        match name.to_ascii_lowercase().as_str() {
            "" => Err("empty {} in the uploader".into()),
            "response" => Ok(self.response()?.text.clone()),
            "responseurl" => Ok(self.response()?.url.clone()),
            "header" => {
                min(1)?;
                Ok(self.response()?.header(&params[0]).unwrap_or_default().to_string())
            }
            "json" => {
                min(1)?;
                let (input, path) = match params {
                    [input, path, ..] => (input.as_str(), path.as_str()),
                    [path] => (self.response()?.text.as_str(), path.as_str()),
                    [] => unreachable!(),
                };
                json(input, path)
            }
            "regex" => {
                min(1)?;
                let (input, pattern, group) = match params {
                    [input, pattern, group, ..] => (input.as_str(), pattern.as_str(), group.as_str()),
                    [pattern, group] => (self.response()?.text.as_str(), pattern.as_str(), group.as_str()),
                    [pattern] => (self.response()?.text.as_str(), pattern.as_str(), ""),
                    [] => unreachable!(),
                };
                let legacy = pattern.parse::<usize>().ok().and_then(|n| self.regex_list.get(n.checked_sub(1)?));
                regex(input, legacy.map_or(pattern, String::as_str), group)
            }
            "filename" if self.url_encode => Ok(url_encode(self.file_name)),
            "filename" => Ok(self.file_name.to_string()),
            "input" => Ok(String::new()),
            "random" => {
                min(2)?;
                Ok(params[(random_u64() % params.len() as u64) as usize].clone())
            }
            "base64" => {
                min(1)?;
                Ok(base64(params[0].as_bytes()))
            }
            "outputbox" => Ok(String::new()),
            "xml" => Err("{xml:…} in an uploader isn't supported by glint".into()),
            "select" | "inputbox" | "prompt" => interactive(),
            _ => Err(format!("unknown function {{{name}}} in the uploader")),
        }
    }
}

fn json(input: &str, path: &str) -> Result<String, String> {
    if input.is_empty() || path.is_empty() {
        return Ok(String::new());
    }
    let root: Value =
        serde_json::from_str(input).map_err(|_| format!("the server didn't answer with JSON: {}", short(input)))?;
    let path = path.strip_prefix('$').unwrap_or(path);
    let mut node = &root;
    let mut rest = path.trim_start_matches('.');
    let missing = || Ok(String::new());
    while !rest.is_empty() {
        if let Some(inner) = rest.strip_prefix('[') {
            let Some(end) = inner.find(']') else { return Err(format!("bad JSON path {path}")) };
            let key = &inner[..end];
            rest = inner[end + 1..].trim_start_matches('.');
            let quoted = key.strip_prefix(['\'', '"']).and_then(|k| k.strip_suffix(['\'', '"']));
            node = match (quoted, key.parse::<usize>()) {
                (Some(name), _) => match node.get(name) {
                    Some(n) => n,
                    None => return missing(),
                },
                (None, Ok(i)) => match node.get(i) {
                    Some(n) => n,
                    None => return missing(),
                },
                (None, Err(_)) => return Err(format!("bad JSON path {path}")),
            };
        } else {
            let end = rest.find(['.', '[']).unwrap_or(rest.len());
            node = match node.get(&rest[..end]) {
                Some(n) => n,
                None => return missing(),
            };
            rest = rest[end..].trim_start_matches('.');
        }
    }
    Ok(match node {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    })
}

fn regex(input: &str, pattern: &str, group: &str) -> Result<String, String> {
    if input.is_empty() || pattern.is_empty() {
        return Ok(String::new());
    }
    let re = regex_lite::Regex::new(pattern).map_err(|e| format!("regex {pattern} isn't supported: {e}"))?;
    let Some(caps) = re.captures(input) else { return Ok(String::new()) };
    let m = match group {
        "" => caps.get(0),
        g => match g.parse::<usize>() {
            Ok(n) => caps.get(n),
            Err(_) => caps.name(g),
        },
    };
    Ok(m.map(|m| m.as_str().to_string()).unwrap_or_default())
}

pub(crate) fn short(text: &str) -> String {
    let mut text = text.trim();
    if text.starts_with('<')
        && let Some((_, rest)) = text.split_once("<title>")
        && let Some((title, _)) = rest.split_once("</title>")
    {
        text = title.trim();
    }
    match text.char_indices().nth(200) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(text: &str) -> Response {
        Response {
            status: 200,
            text: text.into(),
            headers: vec![("Location".into(), "https://h/x".into())],
            url: "https://h/final".into(),
        }
    }

    fn eval(template: &str, r: &Response) -> Result<String, String> {
        Context { file_name: "a b.png", url_encode: true, response: Some(r), regex_list: &[] }.parse(template)
    }

    #[test]
    fn json_paths_pick_values() {
        let r = response(
            r#"{"success":true,"files":[{"url":"https://u/1.png","size":12}],"data":{"a b":{"c":[null,"z"]}}}"#,
        );
        assert_eq!(eval("{json:files[0].url}", &r).unwrap(), "https://u/1.png");
        assert_eq!(eval("{json:$.files[0].size}", &r).unwrap(), "12");
        assert_eq!(eval(r#"{json:\{"k":"v"\}|k}"#, &r).unwrap(), "v");
        assert_eq!(eval("{json:{response}|files[0].size}", &r).unwrap(), "12");
        assert_eq!(eval("{json:success}", &r).unwrap(), "true");
        assert_eq!(eval("{json:nope.deeper}", &r).unwrap(), "");
        assert!(eval("{json:a}", &response("<html>")).unwrap_err().contains("<html>"));
    }

    #[test]
    fn regex_groups_by_number_and_name() {
        let r = response("0,https://puu.sh/abc.png,123,0");
        assert_eq!(eval(r"{regex:,(https?://[^,]+),|1}", &r).unwrap(), "https://puu.sh/abc.png");
        assert_eq!(eval(r"{regex:,(?<u>h[^,]+)|u}", &r).unwrap(), "https://puu.sh/abc.png");
        assert_eq!(eval(r"{regex:\\d+$}", &r).unwrap(), "0");
        assert_eq!(eval("{regex:zzz}", &r).unwrap(), "");
        assert_eq!(eval(r"{regex:id=42|id=(\\d+)|1}", &r).unwrap(), "42");
        let list = [r"(.+)\n(.+)".to_string()];
        let ctx = Context { file_name: "", url_encode: false, response: Some(&response("k1\nd2")), regex_list: &list };
        assert_eq!(ctx.parse("https://j/f.php?h={regex:1|1}&d={regex:1|2}").unwrap(), "https://j/f.php?h=k1&d=d2");
    }

    #[test]
    fn response_header_filename_and_escapes() {
        let r = response("https://u/plain\n");
        assert_eq!(eval("{response}", &r).unwrap(), "https://u/plain\n");
        assert_eq!(eval("{responseurl}", &r).unwrap(), "https://h/final");
        assert_eq!(eval("{header:location}", &r).unwrap(), "https://h/x");
        assert_eq!(eval("{filename}", &r).unwrap(), "a%20b.png");
        assert_eq!(eval(r"\{literal\} {base64:user:pass}", &r).unwrap(), "{literal} dXNlcjpwYXNz");
        assert!(["x", "y"].contains(&eval("{random:x|y}", &r).unwrap().as_str()));
        assert_eq!(eval("{input}", &r).unwrap(), "");
        assert!(eval("{select:a|b}", &r).unwrap_err().contains("can't"));
        assert!(eval("{xml:/a}", &r).is_err() && eval("{bogus}", &r).is_err());
    }
}
