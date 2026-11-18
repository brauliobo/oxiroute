use std::fmt::Write as _;

use crate::cli::net::encode;

pub(super) struct Request {
    host: Option<String>,
    path: String,
}

impl Request {
    pub(super) fn parse(target: &str, path: Option<&str>) -> Result<Self, String> {
        match path {
            Some(path) => Ok(Self {
                host: Some(target.to_owned()),
                path: absolute_path(path)?,
            }),
            None if target.starts_with('/') => Ok(Self {
                host: None,
                path: absolute_path(target)?,
            }),
            None => Self::url(target),
        }
    }

    fn url(url: &str) -> Result<Self, String> {
        let (scheme, rest) = url.split_once("://").ok_or(
            "expected a URL such as https://example.com/path, a HOST and a PATH, or a /PATH",
        )?;
        let (authority, path) = rest
            .find(['/', '?', '#'])
            .map_or((rest, ""), |at| rest.split_at(at));
        let host = authority
            .rsplit_once('@')
            .map_or(authority, |(_, host)| host);
        let default_port = match scheme {
            "http" => ":80",
            "https" => ":443",
            _ => "",
        };
        let host = host.strip_suffix(default_port).unwrap_or(host);
        if host.is_empty() {
            return Err(format!("{url} names no host"));
        }
        let path = if path.starts_with('/') { path } else { "/" };
        Ok(Self {
            host: Some(host.to_owned()),
            path: absolute_path(path)?,
        })
    }

    pub(super) fn query(&self, method: &str, listener: Option<&str>) -> String {
        let mut query = format!("method={method}&path={}", encode(&self.path));
        for (name, value) in [("host", self.host.as_deref()), ("listener", listener)] {
            if let Some(value) = value {
                let _ = write!(query, "&{name}={}", encode(value));
            }
        }
        format!("/api/v1/routes/explain?{query}")
    }

    pub(super) fn describe(&self, method: &str) -> String {
        format!(
            "{method} {}{}",
            self.host.as_deref().unwrap_or(""),
            self.path
        )
    }
}

fn absolute_path(path: &str) -> Result<String, String> {
    let path = path.split(['?', '#']).next().unwrap_or_default();
    if path.starts_with('/') {
        Ok(path.to_owned())
    } else {
        Err(format!("the path {path:?} must start with /"))
    }
}
