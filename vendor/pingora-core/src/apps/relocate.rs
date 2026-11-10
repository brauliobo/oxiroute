//! Moving a keep-alive connection to another worker of its listener while it waits for its next request.

use crate::protocols::Stream;

/// With a runtime that scales its workers (`pingora_runtime::RuntimeOpts::worker_scaling`), moves the connection to a
/// lighter active worker when its own is released or holds more than its share: the task continues there and the
/// socket is registered with that worker's I/O driver. Call it only where the connection waits between two requests and
/// nothing of it is parked. Does nothing otherwise (no scaling, TLS, balanced workers). An error means the socket
/// could not be registered again: drop the connection.
///
/// # Errors
///
/// When the socket cannot be registered with the new worker's I/O driver.
pub async fn relocate_idle_connection(stream: &mut Stream) -> std::io::Result<()> {
    if !pingora_runtime::wants_relocation()
        || !stream.can_move_to_current_runtime()
        || !pingora_runtime::relocate_connection().await
    {
        return Ok(());
    }
    stream.move_to_current_runtime()
}
