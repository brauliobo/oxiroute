#[allow(clippy::wildcard_imports)]
use super::*;

impl Resolver<'_> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn resolve_rtmp_block(
        &mut self,
        directive: &ExpandedDirective,
        root_auto_push: &Policy,
    ) -> EffectiveRtmp {
        self.resolve_block_header(directive, DirectiveContext::NginxMain, "rtmp");
        let children = directive.children.as_deref().unwrap_or_default();
        let mut policy =
            self.resolve_local_policy(children, DirectiveContext::RtmpMain, Policy::default());
        policy.auto_push = root_auto_push.auto_push.clone();
        policy.auto_push_reconnect_ms = root_auto_push.auto_push_reconnect_ms.clone();
        policy.auto_push_socket_dir = root_auto_push.auto_push_socket_dir.clone();
        let mut servers = Vec::new();
        let mut outbound_chunk_size = 4_096;
        let mut chunk_size_origin = None;
        let mut max_inbound_message_size = DEFAULT_NGINX_MAX_MESSAGE_SIZE;
        let mut max_message_origin = None;
        let mut ack_window_size = DEFAULT_NGINX_ACK_WINDOW_SIZE;
        let mut ack_window_origin = None;
        let mut access_log_disabled = false;
        let mut access_log_path = None;
        let mut access_log_origin = None;

        for child in children {
            if child.directive.name.value == b"server" {
                let server = self.resolve_server(child, &policy);
                if let Some(server_chunk_size) = server.outbound_chunk_size {
                    if chunk_size_origin.is_some() && outbound_chunk_size != server_chunk_size {
                        self.block(
                            server
                                .chunk_size_origin
                                .as_ref()
                                .expect("server chunk size retains its origin")
                                .occurrence,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "nginx-RTMP servers use different chunk sizes but canonical policy is service-wide",
                        );
                    } else {
                        outbound_chunk_size = server_chunk_size;
                        chunk_size_origin.clone_from(&server.chunk_size_origin);
                    }
                }
                servers.push(server);
            } else if child.directive.name.value == b"chunk_size" {
                if self
                    .validate_registered(child, DirectiveContext::RtmpMain)
                    .is_ok()
                    && child.directive.children.is_none()
                {
                    match parse_u32(&child.directive.arguments[0].value) {
                        Some(value) if (1..=MAX_OUTBOUND_CHUNK_SIZE).contains(&value) => {
                            outbound_chunk_size = value;
                            chunk_size_origin = Some(Self::origin(child));
                            self.resolved(child.occurrence);
                        }
                        _ => self.block(
                            child.occurrence,
                            E_INVALID_VALUE,
                            "chunk_size is outside canonical RTMP outbound chunk bounds",
                        ),
                    }
                }
            } else if child.directive.name.value == b"max_message" {
                if let Some((value, origin)) =
                    self.resolve_max_message_size(child, DirectiveContext::RtmpMain)
                {
                    max_inbound_message_size = value;
                    max_message_origin = Some(origin);
                }
            } else if child.directive.name.value == b"ack_window" {
                if let Some((value, origin)) =
                    self.resolve_ack_window_size(child, DirectiveContext::RtmpMain)
                {
                    ack_window_size = value;
                    ack_window_origin = Some(origin);
                }
            } else if child.directive.name.value == b"access_log" {
                if self
                    .validate_registered(child, DirectiveContext::RtmpMain)
                    .is_ok()
                    && child.directive.children.is_none()
                    && (child.directive.arguments.len() == 1
                        || (child.directive.arguments.len() == 2
                            && child.directive.arguments[1].value == b"combined"))
                {
                    if child.directive.arguments.len() == 1
                        && child.directive.arguments[0].value == b"off"
                    {
                        access_log_disabled = true;
                        access_log_origin = Some(Self::origin(child));
                        self.resolved(child.occurrence);
                    } else if (child.directive.arguments.len() == 1
                        || (child.directive.arguments.len() == 2
                            && child.directive.arguments[1].value == b"combined"))
                        && std::str::from_utf8(&child.directive.arguments[0].value)
                            .is_ok_and(|path| path.starts_with('/'))
                    {
                        let path = std::str::from_utf8(&child.directive.arguments[0].value)
                            .expect("UTF-8 access_log path was checked above");
                        access_log_path = Some(PathBuf::from(path));
                        access_log_origin = Some(Self::origin(child));
                        self.resolved(child.occurrence);
                    } else {
                        self.block(
                            child.occurrence,
                            E_UNSUPPORTED_FEATURE,
                            "RTMP access_log requires an absolute path with the optional combined format",
                        );
                    }
                }
            } else if child.directive.name.value == b"hls_muxdelay" {
                self.resolve_source_noop(child, DirectiveContext::RtmpMain);
            } else if !is_supported_policy(&child.directive.name.value) {
                self.resolve_unsupported_subtree(
                    child,
                    DirectiveContext::RtmpMain,
                    unsupported_rtmp_reason(&child.directive.name.value),
                );
            }
        }

        let mut effective_max_message_size = None;
        let mut effective_max_message_origin = None;
        let mut effective_ack_window_size = None;
        let mut effective_ack_window_origin = None;
        for server in &servers {
            let server_max_message_size = server
                .max_inbound_message_size
                .unwrap_or(max_inbound_message_size);
            let server_max_message_origin = server
                .max_message_origin
                .clone()
                .or_else(|| max_message_origin.clone());
            if let Some(effective) = effective_max_message_size {
                if effective != server_max_message_size {
                    self.block(
                        server
                            .max_message_origin
                            .clone()
                            .unwrap_or_else(|| server.origin.clone())
                            .occurrence,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx-RTMP servers use different max_message values but canonical policy is service-wide",
                    );
                } else if server_max_message_origin.is_some() {
                    effective_max_message_origin = server_max_message_origin;
                }
            } else {
                effective_max_message_size = Some(server_max_message_size);
                effective_max_message_origin = server_max_message_origin;
            }

            let server_ack_window_size = server.ack_window_size.unwrap_or(ack_window_size);
            let server_ack_window_origin = server
                .ack_window_origin
                .clone()
                .or_else(|| ack_window_origin.clone());
            if let Some(effective) = effective_ack_window_size {
                if effective != server_ack_window_size {
                    self.block(
                        server
                            .ack_window_origin
                            .clone()
                            .unwrap_or_else(|| server.origin.clone())
                            .occurrence,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "nginx-RTMP servers use different ack_window values but canonical policy is service-wide",
                    );
                } else if server_ack_window_origin.is_some() {
                    effective_ack_window_origin = server_ack_window_origin;
                }
            } else {
                effective_ack_window_size = Some(server_ack_window_size);
                effective_ack_window_origin = server_ack_window_origin;
            }
        }
        if let Some(value) = effective_max_message_size {
            max_inbound_message_size = value;
            max_message_origin = effective_max_message_origin;
        }
        if let Some(value) = effective_ack_window_size {
            ack_window_size = value;
            ack_window_origin = effective_ack_window_origin;
        }

        if servers.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "rtmp requires at least one server block",
            );
        }
        let auto_push_socket_dir =
            if policy.auto_push.value && policy.auto_push_socket_dir.origin.is_none() {
                PathBuf::from("/tmp/oxiroute-rtmp")
            } else {
                policy.auto_push_socket_dir.value
            };
        EffectiveRtmp {
            origin: Self::origin(directive),
            outbound_chunk_size,
            chunk_size_origin,
            max_inbound_message_size,
            max_message_origin,
            ack_window_size,
            ack_window_origin,
            auto_push: policy.auto_push.value,
            auto_push_origin: policy.auto_push.origin,
            auto_push_reconnect_ms: policy.auto_push_reconnect_ms.value,
            auto_push_reconnect_origin: policy.auto_push_reconnect_ms.origin,
            auto_push_socket_dir,
            auto_push_socket_origin: policy.auto_push_socket_dir.origin,
            access_log_disabled,
            access_log_path,
            access_log_origin,
            servers,
        }
    }

    fn resolve_server(
        &mut self,
        directive: &ExpandedDirective,
        inherited: &Policy,
    ) -> EffectiveRtmpServer {
        self.resolve_block_header(directive, DirectiveContext::RtmpMain, "server");
        let children = directive.children.as_deref().unwrap_or_default();
        let policy =
            self.resolve_local_policy(children, DirectiveContext::RtmpServer, inherited.clone());
        let mut listens = Vec::new();
        let mut applications = Vec::new();
        let mut application_names = HashMap::new();
        let mut outbound_chunk_size = None;
        let mut chunk_size_origin = None;
        let mut max_inbound_message_size = None;
        let mut max_message_origin = None;
        let mut ack_window_size = None;
        let mut ack_window_origin = None;

        for child in children {
            match child.directive.name.value.as_slice() {
                b"listen" => listens.push(self.resolve_listen(child)),
                b"chunk_size" => {
                    if let Some(value) = self.resolve_server_chunk_size(child) {
                        outbound_chunk_size = Some(value);
                        chunk_size_origin = Some(Self::origin(child));
                    }
                }
                b"max_message" => {
                    if let Some((value, origin)) =
                        self.resolve_max_message_size(child, DirectiveContext::RtmpServer)
                    {
                        max_inbound_message_size = Some(value);
                        max_message_origin = Some(origin);
                    }
                }
                b"ack_window" => {
                    if let Some((value, origin)) =
                        self.resolve_ack_window_size(child, DirectiveContext::RtmpServer)
                    {
                        ack_window_size = Some(value);
                        ack_window_origin = Some(origin);
                    }
                }
                b"application" => {
                    let application = self.resolve_application(child, &policy);
                    if let Some(name) = &application.name {
                        if let Some(first) = application_names.get(&name.value).copied() {
                            self.block_related(
                                child.occurrence,
                                E_DUPLICATE_IDENTITY,
                                "duplicate nginx-RTMP application name in one server",
                                first,
                            );
                        } else {
                            application_names.insert(name.value.clone(), child.occurrence);
                        }
                    }
                    applications.push(application);
                }
                b"hls_muxdelay" => self.resolve_source_noop(child, DirectiveContext::RtmpServer),
                name if is_supported_policy(name) => {}
                _ => self.resolve_unsupported_subtree(
                    child,
                    DirectiveContext::RtmpServer,
                    unsupported_rtmp_reason(&child.directive.name.value),
                ),
            }
        }

        if listens.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP server requires an explicit socket listen",
            );
        }
        if applications.is_empty() {
            self.block(
                directive.occurrence,
                E_INVALID_VALUE,
                "nginx-RTMP server requires at least one application",
            );
        }
        EffectiveRtmpServer {
            origin: Self::origin(directive),
            outbound_chunk_size,
            chunk_size_origin,
            max_inbound_message_size,
            max_message_origin,
            ack_window_size,
            ack_window_origin,
            listens,
            applications,
        }
    }

    fn resolve_server_chunk_size(&mut self, directive: &ExpandedDirective) -> Option<u32> {
        if self
            .validate_registered(directive, DirectiveContext::RtmpServer)
            .is_err()
            || directive.directive.children.is_some()
            || directive.directive.arguments.len() != 1
        {
            return None;
        }
        match parse_u32(&directive.directive.arguments[0].value) {
            Some(value) if (1..=MAX_OUTBOUND_CHUNK_SIZE).contains(&value) => {
                self.resolved(directive.occurrence);
                Some(value)
            }
            _ => {
                self.block(
                    directive.occurrence,
                    E_INVALID_VALUE,
                    "chunk_size is outside canonical RTMP outbound chunk bounds",
                );
                None
            }
        }
    }

    fn resolve_max_message_size(
        &mut self,
        directive: &ExpandedDirective,
        context: DirectiveContext,
    ) -> Option<(u64, DirectiveOrigin)> {
        if self.validate_registered(directive, context).is_err()
            || directive.directive.children.is_some()
            || directive.directive.arguments.len() != 1
        {
            return None;
        }
        match parse_nginx_size(&directive.directive.arguments[0].value) {
            Some(value) if (1..=MAX_INBOUND_MESSAGE_SIZE).contains(&value) => {
                let origin = Self::origin(directive);
                self.resolved(directive.occurrence);
                Some((value, origin))
            }
            _ => {
                self.block(
                    directive.occurrence,
                    E_INVALID_VALUE,
                    "max_message is outside canonical RTMP assembled-message bounds",
                );
                None
            }
        }
    }

    fn resolve_ack_window_size(
        &mut self,
        directive: &ExpandedDirective,
        context: DirectiveContext,
    ) -> Option<(u32, DirectiveOrigin)> {
        if self.validate_registered(directive, context).is_err()
            || directive.directive.children.is_some()
            || directive.directive.arguments.len() != 1
        {
            return None;
        }
        match parse_u32(&directive.directive.arguments[0].value) {
            Some(value) if value != 0 => {
                let origin = Self::origin(directive);
                self.resolved(directive.occurrence);
                Some((value, origin))
            }
            _ => {
                self.block(
                    directive.occurrence,
                    E_INVALID_VALUE,
                    "ack_window must be a nonzero RTMP acknowledgement window",
                );
                None
            }
        }
    }

    fn resolve_listen(&mut self, directive: &ExpandedDirective) -> EffectiveRtmpListen {
        let value = directive
            .directive
            .arguments
            .first()
            .map(|word| self.value(word));
        let registered = self.validate_registered(directive, DirectiveContext::RtmpServer);
        let address = value
            .as_ref()
            .and_then(|value| parse_rtmp_socket(&value.value));
        let outcome = if directive.directive.children.is_some() {
            Some((E_INVALID_VALUE, "listen must end with a semicolon"))
        } else if registered.is_err() {
            None
        } else if directive.directive.arguments.len() != 1 {
            Some((
                E_UNSUPPORTED_FEATURE,
                "nginx-RTMP listen options are not represented canonically",
            ))
        } else if address.is_none() {
            Some((
                E_INVALID_VALUE,
                "nginx-RTMP listen must be a numeric socket address with a nonzero port",
            ))
        } else {
            None
        };
        self.finish_occurrence(directive.occurrence, outcome);
        EffectiveRtmpListen {
            origin: Self::origin(directive),
            value,
            address,
        }
    }

    pub(super) fn reject_overlapping_listens(&mut self, blocks: &[EffectiveRtmp]) {
        let listens = blocks
            .iter()
            .flat_map(|block| &block.servers)
            .flat_map(|server| &server.listens)
            .filter_map(|listen| {
                listen
                    .address
                    .map(|address| (address, listen.origin.occurrence))
            })
            .collect::<Vec<_>>();
        for (index, (first_address, first_origin)) in listens.iter().enumerate() {
            for (second_address, second_origin) in &listens[index + 1..] {
                if sockets_overlap(*first_address, *second_address) {
                    self.block_related(
                        *second_origin,
                        E_DUPLICATE_IDENTITY,
                        "nginx-RTMP listen sockets overlap in canonical listener semantics",
                        *first_origin,
                    );
                }
            }
        }
    }
}
