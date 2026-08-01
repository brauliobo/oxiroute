use std::collections::HashMap;

use crate::{Span, haproxy::Directive};

use super::{
    EffectiveServer, EffectiveValue, OccurrenceId, ServerAddress, ServerOption, parse_duration,
    parse_host_port, parse_u32, parse_u64,
};

fn parse_server_address(value: &[u8]) -> Option<ServerAddress> {
    if value.starts_with(b"/") {
        return Some(ServerAddress::Unix {
            path: value.to_vec(),
        });
    }
    let (host, port) = parse_host_port(value)?;
    Some(ServerAddress::Tcp { host, port })
}

pub(super) struct ParsedServer {
    pub(super) server: EffectiveServer,
    pub(super) conflicts: Vec<OptionConflict>,
}

pub(super) struct OptionConflict {
    pub(super) name: Vec<u8>,
    pub(super) current_span: Span,
    pub(super) previous_span: Span,
}

pub(super) fn merge_server_defaults(current: &mut EffectiveServer, incoming: EffectiveServer) {
    if incoming.check.is_some() {
        current.check = incoming.check;
    }
    if incoming.interval.is_some() {
        current.interval = incoming.interval;
    }
    if incoming.fast_interval.is_some() {
        current.fast_interval = incoming.fast_interval;
    }
    if incoming.down_interval.is_some() {
        current.down_interval = incoming.down_interval;
    }
    if incoming.rise.is_some() {
        current.rise = incoming.rise;
    }
    if incoming.fall.is_some() {
        current.fall = incoming.fall;
    }
    if incoming.max_connections.is_some() {
        current.max_connections = incoming.max_connections;
    }
}

pub(super) fn parse_server(
    directive: &Directive,
    occurrence: OccurrenceId,
) -> Option<ParsedServer> {
    let [name, address, options @ ..] = directive.arguments.as_slice() else {
        return None;
    };
    let address_value = parse_server_address(&address.value)?;
    let mut server = EffectiveServer {
        name: EffectiveValue::direct(name.value.clone(), occurrence, name.span),
        address: EffectiveValue::direct(address_value, occurrence, address.span),
        check: None,
        interval: None,
        fast_interval: None,
        down_interval: None,
        rise: None,
        fall: None,
        max_connections: None,
        unsupported_options: Vec::new(),
    };
    let mut seen: HashMap<Vec<u8>, (Vec<Vec<u8>>, Span)> = HashMap::new();
    let mut conflicts = Vec::new();
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        let argument_count = server_option_argument_count(&option.value)?;
        let option_arguments = options.get(index + 1..index + 1 + argument_count)?;
        let arguments = option_arguments
            .iter()
            .map(|argument| argument.value.clone())
            .collect::<Vec<_>>();
        if let Some((previous_arguments, previous_span)) = seen.get(&option.value) {
            if previous_arguments != &arguments {
                conflicts.push(OptionConflict {
                    name: option.value.clone(),
                    current_span: option.span,
                    previous_span: *previous_span,
                });
            }
            index += 1 + argument_count;
            continue;
        }
        seen.insert(option.value.clone(), (arguments.clone(), option.span));

        match option.value.as_slice() {
            b"check" => {
                server.check = Some(EffectiveValue::direct(true, occurrence, option.span));
            }
            b"no-check" => {
                server.check = Some(EffectiveValue::direct(false, occurrence, option.span));
            }
            b"inter" => {
                let value = &option_arguments[0];
                let duration = parse_duration(&value.value)?;
                server.interval = Some(EffectiveValue::direct(duration, occurrence, value.span));
            }
            b"fastinter" => {
                let value = &option_arguments[0];
                let duration = parse_duration(&value.value)?;
                server.fast_interval =
                    Some(EffectiveValue::direct(duration, occurrence, value.span));
            }
            b"downinter" => {
                let value = &option_arguments[0];
                let duration = parse_duration(&value.value)?;
                server.down_interval =
                    Some(EffectiveValue::direct(duration, occurrence, value.span));
            }
            b"rise" => {
                let value = &option_arguments[0];
                server.rise = Some(EffectiveValue::direct(
                    parse_u32(&value.value)?,
                    occurrence,
                    value.span,
                ));
            }
            b"fall" => {
                let value = &option_arguments[0];
                server.fall = Some(EffectiveValue::direct(
                    parse_u32(&value.value)?,
                    occurrence,
                    value.span,
                ));
            }
            b"maxconn" => {
                let value = &option_arguments[0];
                server.max_connections = Some(EffectiveValue::direct(
                    parse_u64(&value.value)?,
                    occurrence,
                    value.span,
                ));
            }
            _ => server.unsupported_options.push(EffectiveValue::direct(
                ServerOption {
                    name: option.value.clone(),
                    arguments,
                },
                occurrence,
                option.span,
            )),
        }
        index += 1 + argument_count;
    }
    Some(ParsedServer { server, conflicts })
}

fn server_option_argument_count(name: &[u8]) -> Option<usize> {
    match name {
        b"agent-check"
        | b"backup"
        | b"check"
        | b"check-send-proxy"
        | b"check-ssl"
        | b"check-via-socks4"
        | b"disabled"
        | b"enabled"
        | b"no-agent-check"
        | b"no-backup"
        | b"no-check"
        | b"no-check-send-proxy"
        | b"no-ssl"
        | b"send-proxy"
        | b"send-proxy-v2"
        | b"ssl" => Some(0),
        b"addr"
        | b"agent-addr"
        | b"agent-inter"
        | b"agent-port"
        | b"agent-send"
        | b"check-alpn"
        | b"check-pool-conn-name"
        | b"check-port"
        | b"check-proto"
        | b"check-sni"
        | b"downinter"
        | b"error-limit"
        | b"fall"
        | b"fastinter"
        | b"init-addr"
        | b"inter"
        | b"maxconn"
        | b"observe"
        | b"on-error"
        | b"on-marked-down"
        | b"on-marked-up"
        | b"pool-conn-name"
        | b"port"
        | b"rise"
        | b"sni"
        | b"source"
        | b"verify"
        | b"weight" => Some(1),
        _ => None,
    }
}
