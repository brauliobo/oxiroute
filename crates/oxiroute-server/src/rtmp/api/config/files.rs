use oxiroute_config_source::{ConfigFormat, render_config, resolve_source_with_format};
use pingora::protocols::http::ServerSession;
use serde::Deserialize;
use serde_json::json;

use super::{
    ApiResponse, ConfigApiState, config_revision_header, read_config_body,
    require_json_content_type,
    source::{REDACTION_POLICY, SourceFormat, comparisons, validation_diagnostics},
};
use crate::{
    config::{
        coordinator::{AuthoredRevision, WorkspaceFileError},
        diff::unified,
        resource_patch::{ResourcePatch, ResourcePatchError, apply_patch},
    },
    rtmp::api::response::system_time_ms,
};

#[derive(Clone, Copy)]
pub(in crate::rtmp::api) enum FilesRoute<'a> {
    List,
    Validate,
    Item(&'a str),
}

pub(super) fn match_files_route(path: &str) -> Option<FilesRoute<'_>> {
    match path {
        "/api/v1/config/files" => Some(FilesRoute::List),
        "/api/v1/config/files/validate" => Some(FilesRoute::Validate),
        _ => path
            .strip_prefix("/api/v1/config/files/")
            .filter(|rel| !rel.is_empty())
            .map(FilesRoute::Item),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileValidateRequest {
    path: String,
    text: String,
    #[serde(default)]
    text_diff: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileSaveRequest {
    text: String,
    #[serde(default)]
    create: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ConvertRequest {
    target_format: ConfigFormat,
    #[serde(default)]
    source_format: Option<SourceFormat>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    view: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PatchRequest {
    patch: ResourcePatch,
    #[serde(default)]
    dry_run: bool,
}

impl ConfigApiState {
    pub(super) async fn handle_files(
        &self,
        route: FilesRoute<'_>,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        match (route, method) {
            (FilesRoute::List, "GET") => self.files_list_response(),
            (FilesRoute::List, _) => ApiResponse::method_not_allowed("GET"),
            (FilesRoute::Validate, "POST") => match read_json::<FileValidateRequest>(session).await
            {
                Ok(request) => self.file_validate_response(&request),
                Err(response) => response,
            },
            (FilesRoute::Validate, _) => ApiResponse::method_not_allowed("POST"),
            (FilesRoute::Item(raw_path), "GET") => match decode_rel_path(raw_path) {
                Ok(rel_path) => self.file_read_response(&rel_path),
                Err(response) => response,
            },
            (FilesRoute::Item(raw_path), "PUT") => {
                let rel_path = match decode_rel_path(raw_path) {
                    Ok(path) => path,
                    Err(response) => return response,
                };
                let request = match read_json::<FileSaveRequest>(session).await {
                    Ok(request) => request,
                    Err(response) => return response,
                };
                let expected = if request.create {
                    None
                } else {
                    match config_revision_header(session) {
                        Ok(revision) => Some(revision),
                        Err(response) => return response,
                    }
                };
                self.file_save_response(&rel_path, expected.as_ref(), &request)
            }
            (FilesRoute::Item(_), _) => ApiResponse::method_not_allowed("GET, PUT"),
        }
    }

    pub(super) async fn handle_convert(
        &self,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        if method != "POST" {
            return ApiResponse::method_not_allowed("POST");
        }
        let request = match read_json::<ConvertRequest>(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let authority = self.authority();
        let validated = if let Some(raw_text) = &request.text {
            let restored = match authority.restored(raw_text) {
                Ok(text) => text,
                Err(response) => return response,
            };
            let format = request
                .source_format
                .map_or_else(|| self.coordinator.format(), SourceFormat::config_format);
            match resolve_source_with_format(
                self.coordinator.canonical_path(),
                restored.as_bytes(),
                format,
            ) {
                Ok(resolved) => resolved.config,
                Err(error) => {
                    return ApiResponse::error(422, "invalid_source", error.to_string());
                }
            }
        } else if request.view.as_deref() == Some("active") {
            let Some(active) = &authority.active else {
                return ApiResponse::error(
                    503,
                    "active_config_unavailable",
                    "no configuration generation is active",
                );
            };
            active.config().as_ref().clone()
        } else {
            match &authority.disk {
                Ok(doc) => doc.validated_config.clone(),
                Err(rejection) => return self.source_unavailable(rejection),
            }
        };

        let rendered = match render_config(request.target_format, &validated) {
            Ok(text) => text,
            Err(error) => {
                return ApiResponse::error(422, "render_failed", error.to_string());
            }
        };
        let secrets = authority.secrets_with(validated.as_draft());
        let withheld = !secrets.can_redact();
        let redacted = secrets.redact(&rendered);
        ApiResponse::json(
            200,
            &json!({
                "schemaVersion": 1,
                "format": request.target_format,
                "text": if withheld { None } else { Some(redacted.text) },
                "redaction": {
                    "policy": REDACTION_POLICY,
                    "redacted": redacted.replaced,
                    "withheld": withheld,
                },
            }),
        )
    }

    pub(super) async fn handle_patch(
        &self,
        method: &str,
        session: &mut ServerSession,
    ) -> ApiResponse {
        if method != "POST" {
            return ApiResponse::method_not_allowed("POST");
        }
        let now_unix_ms = match system_time_ms() {
            Ok(now) => now,
            Err(response) => return response,
        };
        let request = match read_json::<PatchRequest>(session).await {
            Ok(request) => request,
            Err(response) => return response,
        };
        let expected = if request.dry_run {
            None
        } else {
            match config_revision_header(session) {
                Ok(revision) => Some(revision),
                Err(response) => return response,
            }
        };

        let document = match self.coordinator.load_shared() {
            Ok(doc) => doc,
            Err(rejection) => return self.source_unavailable(&rejection),
        };
        let mut draft = document.validated_config.to_draft();
        draft = match apply_patch(&draft, request.patch) {
            Ok(draft) => draft,
            Err(ResourcePatchError::NotFound(msg)) => {
                return ApiResponse::error(404, "resource_not_found", &msg);
            }
            Err(ResourcePatchError::Conflict(msg)) => {
                return ApiResponse::error(409, "resource_conflict", &msg);
            }
            Err(ResourcePatchError::Invalid(msg)) => {
                return ApiResponse::error(422, "invalid_resource_patch", &msg);
            }
        };
        if let Err(response) = self.merge_redacted_rtmp_secrets(&mut draft) {
            return response;
        }

        if request.dry_run {
            let prepared = match self.prepare_candidate(draft, now_unix_ms, None) {
                Ok(prepared) => prepared,
                Err(response) => return response,
            };
            let authority = self.authority();
            let diagnostics = validation_diagnostics(&prepared, self.mode);
            return ApiResponse::json(
                200,
                &json!({
                    "schemaVersion": 1,
                    "dryRun": true,
                    "valid": true,
                    "format": prepared.draft.format(),
                    "candidateRevision": prepared.draft.effective_revision(),
                    "activeRevision": self.active_revision(),
                    "restartRequired": !prepared.startup_only.is_empty(),
                    "restartReasons": prepared.startup_only,
                    "diagnostics": diagnostics,
                    "diff": comparisons(&authority, prepared.draft.validated_config()),
                }),
            );
        }

        let expected = expected.expect("checked above when !dry_run");
        self.persist(&expected, |mutation| {
            self.prepare_candidate(draft, now_unix_ms, mutation)
        })
    }

    fn files_list_response(&self) -> ApiResponse {
        match self.coordinator.list_workspace_files() {
            Ok(files) => ApiResponse::json(
                200,
                &json!({
                    "schemaVersion": 1,
                    "rootFormat": self.coordinator.format(),
                    "configWritable": self.coordinator.write_block().is_none(),
                    "writeBlock": self.coordinator.write_block(),
                    "files": files,
                }),
            ),
            Err(rejection) => self.source_unavailable(&rejection),
        }
    }

    fn file_read_response(&self, rel_path: &str) -> ApiResponse {
        let file = match self.coordinator.read_workspace_file(rel_path) {
            Ok(file) => file,
            Err(error) => return workspace_file_error_response(&error),
        };
        let authority = self.authority();
        let secrets = authority.secrets();
        let withheld = !secrets.can_redact();
        let redacted = secrets.redact(&file.text);
        ApiResponse::json(
            200,
            &json!({
                "schemaVersion": 1,
                "path": file.path,
                "kind": file.kind,
                "format": file.format,
                "fileRevision": file.revision,
                "isRoot": file.is_root,
                "text": if withheld { None } else { Some(redacted.text) },
                "redaction": {
                    "policy": REDACTION_POLICY,
                    "redacted": redacted.replaced,
                    "withheld": withheld,
                },
                "configWritable": self.coordinator.write_block().is_none(),
                "writeBlock": self.coordinator.write_block(),
            }),
        )
    }

    fn file_validate_response(&self, request: &FileValidateRequest) -> ApiResponse {
        let now_unix_ms = match system_time_ms() {
            Ok(now) => now,
            Err(response) => return response,
        };
        let authority = self.authority();
        let restored = match authority.restored(&request.text) {
            Ok(text) => text,
            Err(response) => return response,
        };
        let (outcome, disk_text, _is_root) = match self
            .coordinator
            .prepare_workspace_file(&request.path, &restored)
        {
            Ok(result) => result,
            Err(error) => return workspace_file_error_response(&error),
        };
        let prepared = match self.preflight(outcome, now_unix_ms, None) {
            Ok(prepared) => prepared,
            Err(response) => return response,
        };
        let secrets = authority.secrets_with(prepared.draft.validated_config().as_draft());
        let text_diff = if request.text_diff && secrets.can_redact() {
            let old = secrets.redact(disk_text.as_deref().unwrap_or("")).text;
            let new = secrets.redact(&restored).text;
            Some(unified(&old, &new, &request.path, "candidate"))
        } else {
            None
        };
        ApiResponse::json(
            200,
            &json!({
                "schemaVersion": 1,
                "valid": true,
                "path": request.path,
                "candidateRevision": prepared.draft.effective_revision(),
                "activeRevision": self.active_revision(),
                "compositional": prepared.draft.compositional(),
                "dependencyCount": prepared.draft.dependencies().len(),
                "diagnostics": validation_diagnostics(&prepared, self.mode),
                "restartRequired": !prepared.startup_only.is_empty(),
                "restartReasons": prepared.startup_only,
                "topology": prepared.topology,
                "diff": comparisons(&authority, prepared.draft.validated_config()),
                "textDiff": text_diff,
                "configWritable": self.coordinator.write_block().is_none(),
                "writeBlock": self.coordinator.write_block(),
            }),
        )
    }

    fn file_save_response(
        &self,
        rel_path: &str,
        expected: Option<&AuthoredRevision>,
        request: &FileSaveRequest,
    ) -> ApiResponse {
        let now_unix_ms = match system_time_ms() {
            Ok(now) => now,
            Err(response) => return response,
        };
        let authority = self.authority();
        let restored = match authority.restored(&request.text) {
            Ok(text) => text,
            Err(response) => return response,
        };
        let (outcome, _disk_text, is_root) =
            match self.coordinator.prepare_workspace_file(rel_path, &restored) {
                Ok(result) => result,
                Err(error) => return workspace_file_error_response(&error),
            };

        if is_root {
            let Some(expected) = expected else {
                return ApiResponse::error(
                    428,
                    "precondition_required",
                    "If-Config-Revision header is required when saving the root configuration",
                );
            };
            return self.persist(expected, |mutation| {
                self.preflight(outcome, now_unix_ms, mutation)
            });
        }

        let prepared = match self.preflight(outcome, now_unix_ms, None) {
            Ok(prepared) => prepared,
            Err(response) => return response,
        };
        let file_revision = match self
            .coordinator
            .save_workspace_file(rel_path, expected, &restored)
        {
            Ok(revision) => revision,
            Err(error) => return workspace_file_error_response(&error),
        };
        let restart_required = !prepared.startup_only.is_empty();
        ApiResponse::json(
            200,
            &json!({
                "schemaVersion": 1,
                "path": rel_path,
                "fileRevision": file_revision,
                "candidateRevision": prepared.draft.effective_revision(),
                "activeRevision": self.active_revision(),
                "written": true,
                "outcome": if restart_required { "saved_restart_required" } else { "saved_pending_activation" },
                "activationState": if restart_required { "restart_required" } else { "pending" },
                "restartRequired": restart_required,
                "restartReasons": prepared.startup_only,
                "diagnostics": validation_diagnostics(&prepared, self.mode),
            }),
        )
    }
}

fn workspace_file_error_response(error: &WorkspaceFileError) -> ApiResponse {
    match error {
        WorkspaceFileError::InvalidPath(msg) => ApiResponse::error(400, "invalid_config_path", msg),
        WorkspaceFileError::NotFound(msg) => ApiResponse::error(404, "config_file_not_found", msg),
        WorkspaceFileError::TooLarge => {
            ApiResponse::error(413, "config_file_too_large", error.to_string())
        }
        WorkspaceFileError::InvalidUtf8 => {
            ApiResponse::error(422, "invalid_utf8", error.to_string())
        }
        WorkspaceFileError::Conflict { current_revision } => ApiResponse::json(
            409,
            &json!({
                "schemaVersion": 1,
                "fileRevision": current_revision,
                "error": {
                    "code": "revision_conflict",
                    "message": "the file changed on disk since it was read",
                },
            }),
        ),
        WorkspaceFileError::Io(msg) => ApiResponse::error(500, "write_failed", msg),
    }
}

fn decode_rel_path(raw: &str) -> Result<String, ApiResponse> {
    let mut bytes = Vec::with_capacity(raw.len());
    let input = raw.as_bytes();
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' {
            if i + 2 >= input.len() {
                return Err(ApiResponse::error(
                    400,
                    "invalid_config_path",
                    "malformed percent-encoding in file path",
                ));
            }
            let hex = std::str::from_utf8(&input[i + 1..i + 3]).map_err(|_| {
                ApiResponse::error(
                    400,
                    "invalid_config_path",
                    "malformed percent-encoding in file path",
                )
            })?;
            let decoded = u8::from_str_radix(hex, 16).map_err(|_| {
                ApiResponse::error(
                    400,
                    "invalid_config_path",
                    "malformed percent-encoding in file path",
                )
            })?;
            bytes.push(decoded);
            i += 3;
        } else {
            bytes.push(input[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).map_err(|_| {
        ApiResponse::error(400, "invalid_config_path", "file path must be valid UTF-8")
    })
}

async fn read_json<T: serde::de::DeserializeOwned>(
    session: &mut ServerSession,
) -> Result<T, ApiResponse> {
    require_json_content_type(session)?;
    let body = read_config_body(session).await?;
    serde_json::from_slice(&body).map_err(|err| {
        ApiResponse::error(
            400,
            "invalid_request",
            format!("malformed JSON request body: {err}"),
        )
    })
}
