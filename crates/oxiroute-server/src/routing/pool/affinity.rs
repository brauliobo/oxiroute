#[allow(clippy::wildcard_imports)]
use super::*;

/// Opaque cookie value naming one server of one pool: the first eight bytes of
/// `SHA-256("oxiroute-affinity" 0 pool 0 server)`. Stable across reloads and workers.
fn cookie_id(pool: &str, server: &str) -> u64 {
    let digest = openssl::sha::sha256(format!("oxiroute-affinity\0{pool}\0{server}").as_bytes());
    u64::from_be_bytes(digest[..8].try_into().expect("eight bytes"))
}

impl EndpointPool {
    /// The selectable server that weighted rendezvous hashing assigns to `key`.
    pub(crate) fn rendezvous_server(&self, key: &[u8]) -> Option<String> {
        let members = self.members();
        let view = self.health.selectable_view(&members, now_unix_ms());
        let index = rendezvous::winner(&view.entries, key)?;
        Some(members.endpoints[index as usize].name.clone())
    }

    /// `name`, if that server is selectable now.
    pub(crate) fn selectable_server(&self, name: &str) -> Option<String> {
        let server = self.server_endpoint(name)?;
        server
            .selectable_at(now_unix_ms())
            .then(|| server.name.clone())
    }

    /// The selectable server whose cookie value is `id`.
    pub(crate) fn server_with_cookie_id(&self, id: u64) -> Option<String> {
        let members = self.members();
        let view = self.health.selectable_view(&members, now_unix_ms());
        view.rotation(0)
            .map(|index| &members.endpoints[index])
            .find(|server| self.cookie_id_of(server) == id)
            .map(|server| server.name.clone())
    }

    /// The cookie value naming `name`, hashed once per server.
    pub(crate) fn cookie_id(&self, name: &str) -> Option<u64> {
        self.server_endpoint(name)
            .map(|server| self.cookie_id_of(&server))
    }

    fn cookie_id_of(&self, server: &PoolEndpoint) -> u64 {
        *server
            .cookie_id
            .get_or_init(|| cookie_id(&self.name, &server.name))
    }
}
