use std::net::SocketAddr;

use mlua::{Lua, LuaSerdeExt as _, Table, Value as LuaValue};

pub(super) fn register(lua: &Lua) -> mlua::Result<()> {
    let oxi = lua.create_table()?;

    oxi.set("config", lua.create_function(fn_config)?)?;
    oxi.set("certbot", lua.create_function(fn_certbot)?)?;
    oxi.set("cert_files", lua.create_function(fn_cert_files)?)?;
    oxi.set(
        "cert_self_signed",
        lua.create_function(fn_cert_self_signed)?,
    )?;
    oxi.set("tls_profile", lua.create_function(fn_tls_profile)?)?;
    oxi.set("upstream", lua.create_function(fn_upstream)?)?;
    oxi.set("host", lua.create_function(fn_host)?)?;
    oxi.set("path", lua.create_function(fn_path)?)?;
    oxi.set("proxy", lua.create_function(fn_proxy)?)?;
    oxi.set("redirect", lua.create_function(fn_redirect)?)?;
    oxi.set("fixed_response", lua.create_function(fn_fixed_response)?)?;
    oxi.set("static_files", lua.create_function(fn_static_files)?)?;
    oxi.set("route", lua.create_function(fn_route)?)?;
    oxi.set("http_service", lua.create_function(fn_http_service)?)?;
    oxi.set("listener", lua.create_function(fn_listener)?)?;

    let globals = lua.globals();
    globals.set("oxi", &oxi)?;
    globals.set("oxiroute", oxi)?;
    Ok(())
}

fn new_array_table(lua: &Lua) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    table.set_metatable(Some(lua.array_metatable()))?;
    Ok(table)
}

fn copy_table_entries(src: &Table, dst: &Table) -> mlua::Result<()> {
    for pair in src.pairs::<LuaValue, LuaValue>() {
        let (key, value) = pair?;
        dst.set(key, value)?;
    }
    Ok(())
}

fn merge_opts(target: &Table, opts: Option<&Table>) -> mlua::Result<()> {
    if let Some(opts) = opts {
        copy_table_entries(opts, target)?;
    }
    Ok(())
}

fn ensure_array_if_empty(lua: &Lua, table: &Table) -> mlua::Result<Table> {
    if table.pairs::<LuaValue, LuaValue>().next().is_none() {
        return new_array_table(lua);
    }
    Ok(table.clone())
}

fn string_or_list(
    lua: &Lua,
    value: LuaValue,
    fn_name: &str,
    arg_name: &str,
) -> mlua::Result<(Table, Vec<String>)> {
    match value {
        LuaValue::String(text) => {
            let owned = text.to_str()?.to_owned();
            let array = new_array_table(lua)?;
            array.set(1, owned.as_str())?;
            Ok((array, vec![owned]))
        }
        LuaValue::Table(table) => {
            let mut indexed: Vec<(i64, String)> = Vec::new();
            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, val) = pair?;
                let LuaValue::Integer(index) = key else {
                    return Err(mlua::Error::runtime(format!(
                        "{fn_name}(): {arg_name} must be a string or list of strings"
                    )));
                };
                let LuaValue::String(item) = val else {
                    return Err(mlua::Error::runtime(format!(
                        "{fn_name}(): {arg_name} entries must be strings"
                    )));
                };
                indexed.push((index, item.to_str()?.to_owned()));
            }
            indexed.sort_by_key(|(index, _)| *index);

            let array = new_array_table(lua)?;
            let mut items = Vec::with_capacity(indexed.len());
            for (offset, (index, item)) in indexed.into_iter().enumerate() {
                let expected = i64::try_from(offset + 1)
                    .map_err(|_| mlua::Error::runtime(format!("{fn_name}(): list too large")))?;
                if index != expected {
                    return Err(mlua::Error::runtime(format!(
                        "{fn_name}(): {arg_name} list must be sequential starting at 1"
                    )));
                }
                array.set(expected, item.as_str())?;
                items.push(item);
            }
            Ok((array, items))
        }
        _ => Err(mlua::Error::runtime(format!(
            "{fn_name}(): {arg_name} must be a string or list of strings"
        ))),
    }
}

fn convert_headers(lua: &Lua, value: LuaValue, fn_name: &str) -> mlua::Result<Table> {
    match value {
        LuaValue::Nil => new_array_table(lua),
        LuaValue::Table(table) => {
            let mut int_entries: Vec<(i64, Table)> = Vec::new();
            let mut str_entries: Vec<(String, String)> = Vec::new();

            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, val) = pair?;
                match key {
                    LuaValue::Integer(index) => {
                        let LuaValue::Table(entry) = val else {
                            return Err(mlua::Error::runtime(format!(
                                "{fn_name}(): header list entries must be tables"
                            )));
                        };
                        int_entries.push((index, entry));
                    }
                    LuaValue::String(name) => {
                        let LuaValue::String(header_value) = val else {
                            return Err(mlua::Error::runtime(format!(
                                "{fn_name}(): header map values must be strings"
                            )));
                        };
                        str_entries.push((
                            name.to_str()?.to_owned(),
                            header_value.to_str()?.to_owned(),
                        ));
                    }
                    _ => {
                        return Err(mlua::Error::runtime(format!(
                            "{fn_name}(): headers must be a list of header tables or a string map"
                        )));
                    }
                }
            }

            if !int_entries.is_empty() && !str_entries.is_empty() {
                return Err(mlua::Error::runtime(format!(
                    "{fn_name}(): headers cannot mix list entries and key-value pairs"
                )));
            }

            let out = new_array_table(lua)?;
            if !int_entries.is_empty() {
                int_entries.sort_by_key(|(index, _)| *index);
                for (offset, (index, entry)) in int_entries.into_iter().enumerate() {
                    let expected = i64::try_from(offset + 1).map_err(|_| {
                        mlua::Error::runtime(format!("{fn_name}(): header list too large"))
                    })?;
                    if index != expected {
                        return Err(mlua::Error::runtime(format!(
                            "{fn_name}(): header list must be sequential starting at 1"
                        )));
                    }
                    let copied = lua.create_table()?;
                    copy_table_entries(&entry, &copied)?;
                    out.set(expected, copied)?;
                }
            } else if !str_entries.is_empty() {
                str_entries.sort_by(|(left, _), (right, _)| left.cmp(right));
                for (offset, (name, header_value)) in str_entries.into_iter().enumerate() {
                    let index = i64::try_from(offset + 1).map_err(|_| {
                        mlua::Error::runtime(format!("{fn_name}(): header map too large"))
                    })?;
                    let entry = lua.create_table()?;
                    entry.set("name", name)?;
                    entry.set("value", header_value)?;
                    out.set(index, entry)?;
                }
            }

            Ok(out)
        }
        _ => Err(mlua::Error::runtime(format!(
            "{fn_name}(): headers must be a table or nil"
        ))),
    }
}

fn fn_config(lua: &Lua, spec: Option<Table>) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    if let Some(spec) = spec {
        copy_table_entries(&spec, &out)?;
    }
    if matches!(out.get::<LuaValue>("version")?, LuaValue::Nil) {
        out.set("version", 1_u32)?;
    }
    match out.get::<LuaValue>("listeners")? {
        LuaValue::Nil => {
            out.set("listeners", new_array_table(lua)?)?;
        }
        LuaValue::Table(listeners) => {
            out.set("listeners", ensure_array_if_empty(lua, &listeners)?)?;
        }
        _ => {}
    }
    Ok(out)
}

fn fn_certbot(
    lua: &Lua,
    (name, domains, opts): (String, LuaValue, Option<Table>),
) -> mlua::Result<Table> {
    let (dns_names, domain_list) = string_or_list(lua, domains, "oxi.certbot", "domains")?;

    let root = match opts
        .as_ref()
        .map(|tbl| tbl.get::<Option<String>>("root"))
        .transpose()?
        .flatten()
    {
        Some(custom) => custom.trim_end_matches('/').to_owned(),
        None => "/etc/letsencrypt".to_owned(),
    };

    let explicit_live = opts
        .as_ref()
        .map(|tbl| tbl.get::<Option<String>>("live_directory_path"))
        .transpose()?
        .flatten();
    let explicit_archive = opts
        .as_ref()
        .map(|tbl| tbl.get::<Option<String>>("archive_directory_path"))
        .transpose()?
        .flatten();

    let primary = || -> mlua::Result<&str> {
        domain_list
            .iter()
            .find(|domain| !domain.starts_with("*."))
            .map(String::as_str)
            .or_else(|| {
                domain_list
                    .first()
                    .map(|domain| domain.strip_prefix("*.").unwrap_or(domain.as_str()))
            })
            .ok_or_else(|| mlua::Error::runtime("oxi.certbot(): domains must not be empty"))
    };

    let live_dir = match explicit_live {
        Some(path) => path,
        None => format!("{root}/live/{}", primary()?),
    };
    let archive_dir = match explicit_archive {
        Some(path) => path,
        None => format!("{root}/archive/{}", primary()?),
    };

    let source = lua.create_table()?;
    source.set("type", "certbot")?;
    source.set("live_directory_path", live_dir)?;
    source.set("archive_directory_path", archive_dir)?;

    let out = lua.create_table()?;
    out.set("name", name)?;
    out.set("dns_names", dns_names)?;
    out.set("source", source)?;
    Ok(out)
}

fn fn_cert_files(
    lua: &Lua,
    (name, domains, cert_path, key_path): (String, LuaValue, String, String),
) -> mlua::Result<Table> {
    let (dns_names, _) = string_or_list(lua, domains, "oxi.cert_files", "domains")?;

    let source = lua.create_table()?;
    source.set("type", "files")?;
    source.set("certificate_chain_path", cert_path)?;
    source.set("private_key_path", key_path)?;

    let out = lua.create_table()?;
    out.set("name", name)?;
    out.set("dns_names", dns_names)?;
    out.set("source", source)?;
    Ok(out)
}

fn fn_cert_self_signed(
    lua: &Lua,
    (name, domains, opts): (String, LuaValue, Option<Table>),
) -> mlua::Result<Table> {
    let (dns_names, _) = string_or_list(lua, domains, "oxi.cert_self_signed", "domains")?;

    let source = lua.create_table()?;
    source.set("type", "self_signed_development")?;
    merge_opts(&source, opts.as_ref())?;

    let out = lua.create_table()?;
    out.set("name", name)?;
    out.set("dns_names", dns_names)?;
    out.set("source", source)?;
    Ok(out)
}

fn fn_tls_profile(
    lua: &Lua,
    (name, certificates, opts): (String, LuaValue, Option<Table>),
) -> mlua::Result<Table> {
    let (certs_array, cert_list) =
        string_or_list(lua, certificates, "oxi.tls_profile", "certificates")?;

    let out = lua.create_table()?;
    out.set("name", name)?;
    out.set("certificates", certs_array)?;
    merge_opts(&out, opts.as_ref())?;

    if matches!(out.get::<LuaValue>("default_certificate")?, LuaValue::Nil) {
        let first = cert_list.first().ok_or_else(|| {
            mlua::Error::runtime("oxi.tls_profile(): certificates must not be empty")
        })?;
        out.set("default_certificate", first.as_str())?;
    }

    Ok(out)
}

fn parse_upstream_endpoint(lua: &Lua, spec: &str) -> mlua::Result<Table> {
    let endpoint = lua.create_table()?;
    if let Some(path) = spec.strip_prefix("unix:") {
        endpoint.set("type", "unix")?;
        endpoint.set("path", path)?;
        return Ok(endpoint);
    }
    if spec.starts_with('/') {
        endpoint.set("type", "unix")?;
        endpoint.set("path", spec)?;
        return Ok(endpoint);
    }
    if spec.parse::<SocketAddr>().is_ok() {
        endpoint.set("type", "socket")?;
        endpoint.set("address", spec)?;
        return Ok(endpoint);
    }
    if let Some((host, port_str)) = spec.rsplit_once(':')
        && !host.is_empty()
        && let Ok(port) = port_str.parse::<u16>()
    {
        endpoint.set("type", "dns")?;
        endpoint.set("host", host)?;
        endpoint.set("port", port)?;
        return Ok(endpoint);
    }
    Err(mlua::Error::runtime(format!(
        "oxi.upstream(): invalid server endpoint `{spec}` (expected socket `ip:port`, DNS `host:port`, or unix `/path`)"
    )))
}

fn build_upstream_server(lua: &Lua, value: LuaValue, index: usize) -> mlua::Result<Table> {
    let default_name = format!("s{index}");
    match value {
        LuaValue::String(spec) => {
            let endpoint = parse_upstream_endpoint(lua, &spec.to_str()?)?;
            let server = lua.create_table()?;
            server.set("name", default_name)?;
            server.set("endpoint", endpoint)?;
            Ok(server)
        }
        LuaValue::Table(src) => {
            let server = lua.create_table()?;
            copy_table_entries(&src, &server)?;
            if matches!(server.get::<LuaValue>("name")?, LuaValue::Nil) {
                server.set("name", default_name)?;
            }
            if let LuaValue::String(spec) = server.get::<LuaValue>("endpoint")? {
                let endpoint = parse_upstream_endpoint(lua, &spec.to_str()?)?;
                server.set("endpoint", endpoint)?;
            }
            Ok(server)
        }
        _ => Err(mlua::Error::runtime(
            "oxi.upstream(): each server must be a string or table",
        )),
    }
}

fn fn_upstream(
    lua: &Lua,
    (name, servers, opts): (String, LuaValue, Option<Table>),
) -> mlua::Result<Table> {
    let raw_servers: Vec<LuaValue> = match servers {
        LuaValue::String(_) => vec![servers],
        LuaValue::Table(table) => {
            let mut int_entries: Vec<(i64, LuaValue)> = Vec::new();
            let mut has_string_keys = false;

            for pair in table.pairs::<LuaValue, LuaValue>() {
                let (key, val) = pair?;
                match key {
                    LuaValue::Integer(index) => int_entries.push((index, val)),
                    LuaValue::String(_) => has_string_keys = true,
                    _ => {
                        return Err(mlua::Error::runtime(
                            "oxi.upstream(): servers must be a string, server table, or list",
                        ));
                    }
                }
            }

            if has_string_keys && !int_entries.is_empty() {
                return Err(mlua::Error::runtime(
                    "oxi.upstream(): servers table cannot mix list items and named fields",
                ));
            }

            if has_string_keys {
                vec![LuaValue::Table(table)]
            } else {
                int_entries.sort_by_key(|(index, _)| *index);
                let mut list = Vec::with_capacity(int_entries.len());
                for (offset, (index, item)) in int_entries.into_iter().enumerate() {
                    let expected = i64::try_from(offset + 1).map_err(|_| {
                        mlua::Error::runtime("oxi.upstream(): server list too large")
                    })?;
                    if index != expected {
                        return Err(mlua::Error::runtime(
                            "oxi.upstream(): server list must be sequential starting at 1",
                        ));
                    }
                    list.push(item);
                }
                list
            }
        }
        _ => {
            return Err(mlua::Error::runtime(
                "oxi.upstream(): servers must be a string, server table, or list",
            ));
        }
    };

    let servers_array = new_array_table(lua)?;
    for (offset, item) in raw_servers.into_iter().enumerate() {
        let index = offset + 1;
        let server_tbl = build_upstream_server(lua, item, index)?;
        let lua_index = i64::try_from(index)
            .map_err(|_| mlua::Error::runtime("oxi.upstream(): server list too large"))?;
        servers_array.set(lua_index, server_tbl)?;
    }

    let out = lua.create_table()?;
    out.set("name", name)?;
    out.set("servers", servers_array)?;
    merge_opts(&out, opts.as_ref())?;
    Ok(out)
}

fn build_host(lua: &Lua, spec: LuaValue) -> mlua::Result<Table> {
    match spec {
        LuaValue::Table(table) => Ok(table),
        LuaValue::String(text) => {
            let text = text.to_str()?;
            let out = lua.create_table()?;
            if let Some(suffix) = text.strip_prefix("*.") {
                out.set("kind", "nginx_leading_wildcard")?;
                out.set("value", suffix)?;
            } else if let Some(suffix) = text.strip_prefix('.') {
                out.set("kind", "nginx_leading_dot")?;
                out.set("value", suffix)?;
            } else {
                out.set("kind", "normalized_host")?;
                out.set("value", text.as_ref())?;
            }
            Ok(out)
        }
        _ => Err(mlua::Error::runtime(
            "oxi.host(): spec must be a string or table",
        )),
    }
}

fn fn_host(lua: &Lua, spec: LuaValue) -> mlua::Result<Table> {
    build_host(lua, spec)
}

fn build_path(lua: &Lua, spec: LuaValue) -> mlua::Result<Table> {
    match spec {
        LuaValue::Table(table) => Ok(table),
        LuaValue::String(text) => {
            let text = text.to_str()?;
            let out = lua.create_table()?;
            if let Some(exact) = text.strip_prefix('=') {
                out.set("kind", "exact")?;
                out.set("value", exact)?;
            } else if let Some(raw) = text.strip_prefix('^') {
                out.set("kind", "raw_prefix")?;
                out.set("value", raw)?;
            } else {
                out.set("kind", "segment_prefix")?;
                out.set("value", text.as_ref())?;
            }
            Ok(out)
        }
        _ => Err(mlua::Error::runtime(
            "oxi.path(): spec must be a string or table",
        )),
    }
}

fn fn_path(lua: &Lua, spec: LuaValue) -> mlua::Result<Table> {
    build_path(lua, spec)
}

fn fn_proxy(lua: &Lua, (upstream_pool, policy): (String, Option<Table>)) -> mlua::Result<Table> {
    let policy_tbl = match policy {
        Some(tbl) => tbl,
        None => lua.create_table()?,
    };
    let out = lua.create_table()?;
    out.set("type", "proxy")?;
    out.set("upstream_pool", upstream_pool)?;
    out.set("policy", policy_tbl)?;
    Ok(out)
}

fn fn_redirect(lua: &Lua, (location, opts): (LuaValue, Option<Table>)) -> mlua::Result<Table> {
    let mut consumed_fallback = false;
    let location_tbl = match location {
        LuaValue::Table(table) => table,
        LuaValue::String(text) => {
            let text = text.to_str()?;
            let loc = lua.create_table()?;
            let is_template = text.contains("$scheme")
                || text.contains("$host")
                || text.contains("$request_uri");
            if is_template {
                loc.set("kind", "request_template")?;
                loc.set("value", text.as_ref())?;
                if let Some(opts) = opts.as_ref() {
                    let fallback: LuaValue = opts.get("nginx_host_fallback")?;
                    if !matches!(fallback, LuaValue::Nil) {
                        loc.set("nginx_host_fallback", fallback)?;
                        consumed_fallback = true;
                    }
                }
            } else {
                loc.set("kind", "literal")?;
                loc.set("value", text.as_ref())?;
            }
            loc
        }
        _ => {
            return Err(mlua::Error::runtime(
                "oxi.redirect(): location must be a string or table",
            ));
        }
    };

    let out = lua.create_table()?;
    out.set("type", "redirect")?;
    out.set("status", 308_u16)?;
    out.set("location", location_tbl)?;

    if let Some(opts) = opts {
        for pair in opts.pairs::<LuaValue, LuaValue>() {
            let (key, val) = pair?;
            if let LuaValue::String(ref key_str) = key {
                let key_name = key_str.to_str()?;
                if key_name == "nginx_host_fallback" && consumed_fallback {
                    continue;
                }
                if key_name == "headers" {
                    out.set("headers", convert_headers(lua, val, "oxi.redirect")?)?;
                    continue;
                }
            }
            out.set(key, val)?;
        }
    }

    Ok(out)
}

fn fn_fixed_response(
    lua: &Lua,
    (status, body, headers): (u16, Option<String>, LuaValue),
) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    out.set("type", "fixed_response")?;
    out.set("status", status)?;
    out.set("body", body.unwrap_or_default())?;
    out.set("headers", convert_headers(lua, headers, "oxi.fixed_response")?)?;
    Ok(out)
}

fn fn_static_files(
    lua: &Lua,
    (root_directory, opts): (String, Option<Table>),
) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    out.set("type", "static_files")?;
    out.set("root_directory", root_directory)?;

    if let Some(opts) = opts {
        for pair in opts.pairs::<LuaValue, LuaValue>() {
            let (key, val) = pair?;
            if let LuaValue::String(ref key_str) = key
                && key_str.to_str()? == "headers"
            {
                out.set("headers", convert_headers(lua, val, "oxi.static_files")?)?;
                continue;
            }
            out.set(key, val)?;
        }
    }

    Ok(out)
}

fn fn_route(
    lua: &Lua,
    (path, action, opts): (LuaValue, Table, Option<Table>),
) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    out.set("path", build_path(lua, path)?)?;
    out.set("action", action)?;

    if let Some(opts) = opts {
        for pair in opts.pairs::<LuaValue, LuaValue>() {
            let (key, val) = pair?;
            if let LuaValue::String(ref key_str) = key {
                let key_name = key_str.to_str()?;
                if key_name == "host" {
                    if !matches!(val, LuaValue::Nil) {
                        out.set("host", build_host(lua, val)?)?;
                    }
                    continue;
                }
                if key_name == "methods" {
                    if !matches!(val, LuaValue::Nil) {
                        let (methods_arr, _) =
                            string_or_list(lua, val, "oxi.route", "opts.methods")?;
                        out.set("methods", methods_arr)?;
                    }
                    continue;
                }
            }
            out.set(key, val)?;
        }
    }

    Ok(out)
}

fn fn_http_service(
    lua: &Lua,
    (name, routes, opts): (String, Table, Option<Table>),
) -> mlua::Result<Table> {
    let out = lua.create_table()?;
    out.set("name", name)?;
    out.set("routes", ensure_array_if_empty(lua, &routes)?)?;
    merge_opts(&out, opts.as_ref())?;
    Ok(out)
}

fn fn_listener(
    lua: &Lua,
    (name, bind, service, opts): (String, LuaValue, Option<String>, Option<Table>),
) -> mlua::Result<Table> {
    let bind_tbl = match bind {
        LuaValue::Table(table) => table,
        LuaValue::String(text) => {
            let text = text.to_str()?;
            let tbl = lua.create_table()?;
            if let Some(address) = text.strip_prefix("udp://") {
                tbl.set("type", "udp")?;
                tbl.set("address", address)?;
            } else if let Some(path) = text.strip_prefix("unix:") {
                tbl.set("type", "unix")?;
                tbl.set("path", path)?;
            } else if text.starts_with('/') {
                tbl.set("type", "unix")?;
                tbl.set("path", text.as_ref())?;
            } else {
                tbl.set("type", "socket")?;
                tbl.set("address", text.as_ref())?;
            }
            tbl
        }
        _ => {
            return Err(mlua::Error::runtime(
                "oxi.listener(): bind must be a string or table",
            ));
        }
    };

    let out = lua.create_table()?;
    out.set("name", name)?;
    out.set("bind", bind_tbl)?;
    if let Some(service) = service {
        out.set("service", service)?;
    }
    merge_opts(&out, opts.as_ref())?;
    if matches!(out.get::<LuaValue>("protocol")?, LuaValue::Nil) {
        out.set("protocol", "http")?;
    }
    Ok(out)
}
