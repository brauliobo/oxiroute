use std::{
    fmt::Write as _,
    io::{BufReader, Write as _},
};

use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::{Endpoint, Response, Timeouts};
use crate::cli::error::CliError;

pub(crate) struct Client {
    pub(super) endpoint: Endpoint,
    pub(super) token: Option<Zeroizing<String>>,
    timeouts: Timeouts,
}

impl Client {
    pub(crate) const fn new(
        endpoint: Endpoint,
        token: Option<Zeroizing<String>>,
        timeouts: Timeouts,
    ) -> Self {
        Self {
            endpoint,
            token,
            timeouts,
        }
    }

    pub(crate) const fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub(crate) fn get(&self, path: &str, authenticated: bool) -> Result<Response, CliError> {
        self.request("GET", path, &[], &[], authenticated)
    }

    pub(crate) fn json(
        &self,
        method: &str,
        path: &str,
        value: &Value,
        headers: &[(&str, &str)],
    ) -> Result<Response, CliError> {
        let body = serde_json::to_vec(value).map_err(|error| CliError::Local(error.to_string()))?;
        self.request(method, path, &body, headers, true)
    }

    pub(crate) fn mutation_json(
        &self,
        method: &str,
        path: &str,
        value: &Value,
    ) -> Result<Response, CliError> {
        let revision = self.active_revision()?;
        let mut value = value.clone();
        value
            .as_object_mut()
            .ok_or_else(|| CliError::Local("mutation body must be an object".into()))?
            .insert("expectedActiveRevision".into(), json!(revision));
        self.json(method, path, &value, &[])
    }

    pub(crate) fn generation_json(
        &self,
        method: &str,
        path: &str,
        value: &Value,
    ) -> Result<Response, CliError> {
        let revision = self.active_revision()?;
        self.json(
            method,
            path,
            value,
            &[("If-Generation-Revision", &revision)],
        )
    }

    fn active_revision(&self) -> Result<String, CliError> {
        self.get("/api/v1/generations", true)?
            .json()?
            .pointer("/generation/activeRevision")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| CliError::Local("generation response has no active revision".into()))
    }

    pub(crate) fn request(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        headers: &[(&str, &str)],
        authenticated: bool,
    ) -> Result<Response, CliError> {
        self.exchange(method, path, body, headers, authenticated)?
            .into_result()
    }

    /// The answer whatever its status, for the probes whose failure statuses carry a body worth showing.
    pub(crate) fn get_unchecked(&self, path: &str) -> Result<Response, CliError> {
        self.exchange("GET", path, &[], &[], false)
    }

    fn exchange(
        &self,
        method: &str,
        path: &str,
        body: &[u8],
        headers: &[(&str, &str)],
        authenticated: bool,
    ) -> Result<Response, CliError> {
        if !path.starts_with('/') || path.contains(['\r', '\n']) {
            return Err(CliError::Local("request path is invalid".into()));
        }
        let token = if authenticated && !self.endpoint.is_unix() {
            Some(self.token.as_deref().ok_or_else(|| {
                CliError::Local(
                    "no management token file: pass --token-file, set OXIROUTE_MANAGEMENT_TOKEN_FILE, \
                     or put it in /etc/oxiroute/management.token"
                        .into(),
                )
            })?)
        } else {
            None
        };
        let mut head = Zeroizing::new(format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.endpoint.host(),
            body.len()
        ));
        if !body.is_empty() {
            head.push_str("Content-Type: application/json\r\n");
        }
        if let Some(token) = token {
            let _ = write!(head, "Authorization: Bearer {token}\r\n");
        }
        for (name, value) in headers {
            if name.contains(['\r', '\n']) || value.contains(['\r', '\n']) {
                return Err(CliError::Local("request header is invalid".into()));
            }
            let _ = write!(head, "{name}: {value}\r\n");
        }
        head.push_str("\r\n");
        let mut stream = self.endpoint.connect(self.timeouts)?;
        stream
            .write_all(head.as_bytes())
            .and_then(|()| stream.write_all(body))
            .map_err(|error| self.endpoint.transport(CliError::Io(error), self.timeouts))?;
        Response::read(BufReader::new(stream))
            .map_err(|error| self.endpoint.transport(error, self.timeouts))
    }
}
