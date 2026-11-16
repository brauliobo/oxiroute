mod session;
mod stage;
mod transport;

use std::sync::Arc;

use rustls::{ServerConfig, server::ClientHello};

pub(crate) use session::{Finish, Progress, Session};
pub(crate) use transport::Transport;

pub trait TlsRouter: Send + Sync + 'static {
    fn config(&self, hello: &ClientHello<'_>) -> Option<Arc<ServerConfig>>;
}
