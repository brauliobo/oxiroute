use serde::{Deserialize, Serialize};

use crate::{
    rtmp::api::{export_media_ticket_secret, import_media_ticket_secret},
    tls::ticket_keys::{TicketKeys, TicketKeysState},
};

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProcessSecrets {
    #[serde(default)]
    tls_tickets: Vec<TicketKeysState>,
    #[serde(default)]
    media_ticket: Option<String>,
}

impl ProcessSecrets {
    #[must_use]
    pub fn export() -> Self {
        Self {
            tls_tickets: TicketKeys::export_all(),
            media_ticket: Some(export_media_ticket_secret()),
        }
    }

    pub fn import(&self) {
        TicketKeys::import_all(&self.tls_tickets);
        if let Some(secret) = &self.media_ticket
            && !import_media_ticket_secret(secret)
        {
            log::error!("the media ticket key of the replaced process is malformed");
        }
    }
}
