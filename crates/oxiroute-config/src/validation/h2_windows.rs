use crate::{ConfigError, Listener, Protocol};

pub(super) fn validate_listener_h2_windows(listener: &Listener) -> Result<(), ConfigError> {
    let (stream, connection) = (
        listener.h2_initial_stream_window,
        listener.h2_initial_connection_window,
    );
    if stream.is_none() && connection.is_none() {
        return Ok(());
    }
    if !matches!(listener.protocol, Protocol::Http | Protocol::ForwardHttp2) {
        return Err(ConfigError::ListenerH2WindowsUnsupported {
            listener: listener.name.clone(),
            protocol: listener.protocol,
        });
    }
    for (field, value, max) in [
        (
            "h2_initial_stream_window",
            stream,
            Listener::MAX_H2_STREAM_WINDOW,
        ),
        (
            "h2_initial_connection_window",
            connection,
            Listener::MAX_H2_CONNECTION_WINDOW,
        ),
    ] {
        if let Some(value) = value
            && !(Listener::MIN_H2_WINDOW..=max).contains(&value)
        {
            return Err(ConfigError::InvalidListenerH2Window {
                listener: listener.name.clone(),
                field,
                value,
                min: Listener::MIN_H2_WINDOW,
                max,
            });
        }
    }
    let windows = listener.effective_h2_windows();
    if windows.connection < windows.stream {
        return Err(ConfigError::ListenerH2ConnectionWindowBelowStream {
            listener: listener.name.clone(),
            stream: windows.stream.into(),
            connection: windows.connection.into(),
        });
    }
    Ok(())
}
