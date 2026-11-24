use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use mlua::Lua;

use super::stdlib::check_str_limit;

pub(super) fn register(lua: &Lua) -> mlua::Result<()> {
    let net_tbl = lua.create_table()?;

    let join_host_port_fn = lua.create_function(|_, (host, port): (String, i64)| {
        check_str_limit(&host)?;
        if host.is_empty() {
            return Err(mlua::Error::runtime(
                "net.join_host_port() host must not be empty",
            ));
        }
        if !(1..=65_535).contains(&port) {
            return Err(mlua::Error::runtime(format!(
                "net.join_host_port() port must be in 1..=65535, got {port}"
            )));
        }
        if host.parse::<Ipv6Addr>().is_ok() {
            return Ok(format!("[{host}]:{port}"));
        }
        if let Some(inner) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']'))
            && inner.parse::<Ipv6Addr>().is_ok()
        {
            return Ok(format!("{host}:{port}"));
        }
        if host.contains(':') || host.chars().any(char::is_whitespace) {
            return Err(mlua::Error::runtime(format!(
                "net.join_host_port() invalid host `{host}`"
            )));
        }
        Ok(format!("{host}:{port}"))
    })?;
    net_tbl.set("join_host_port", join_host_port_fn)?;

    let is_ipv4_fn = lua.create_function(|_, text: String| {
        check_str_limit(&text)?;
        Ok(text.parse::<Ipv4Addr>().is_ok())
    })?;
    net_tbl.set("is_ipv4", is_ipv4_fn)?;

    let is_ipv6_fn = lua.create_function(|_, text: String| {
        check_str_limit(&text)?;
        Ok(text.parse::<Ipv6Addr>().is_ok())
    })?;
    net_tbl.set("is_ipv6", is_ipv6_fn)?;

    let is_ip_fn = lua.create_function(|_, text: String| {
        check_str_limit(&text)?;
        Ok(text.parse::<IpAddr>().is_ok())
    })?;
    net_tbl.set("is_ip", is_ip_fn)?;

    let is_cidr_fn = lua.create_function(|_, text: String| {
        check_str_limit(&text)?;
        Ok(oxiroute_config::cidr_contains(&text, IpAddr::V4(Ipv4Addr::UNSPECIFIED)).is_some())
    })?;
    net_tbl.set("is_cidr", is_cidr_fn)?;

    let cidr_contains_fn = lua.create_function(|_, (cidr, ip): (String, String)| {
        check_str_limit(&cidr)?;
        check_str_limit(&ip)?;
        let addr: IpAddr = ip.parse().map_err(|_| {
            mlua::Error::runtime(format!("net.cidr_contains() invalid IP address `{ip}`"))
        })?;
        oxiroute_config::cidr_contains(&cidr, addr).ok_or_else(|| {
            mlua::Error::runtime(format!("net.cidr_contains() invalid CIDR `{cidr}`"))
        })
    })?;
    net_tbl.set("cidr_contains", cidr_contains_fn)?;

    let normalize_host_fn = lua.create_function(|_, host: String| {
        check_str_limit(&host)?;
        let lowered = host.to_ascii_lowercase();
        let normalized = lowered.strip_suffix('.').unwrap_or(&lowered);
        Ok(normalized.to_owned())
    })?;
    net_tbl.set("normalize_host", normalize_host_fn)?;

    lua.globals().set("net", net_tbl)?;
    Ok(())
}
