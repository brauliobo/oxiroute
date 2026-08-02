use std::{io, os::fd::AsFd};

use rustix::net::{SendFlags, send};

const HEADER_SIZE: usize = 52;

fn main() -> Result<(), io::Error> {
    let mut frame = [0_u8; HEADER_SIZE];
    frame[0..4].copy_from_slice(b"OXSP");
    frame[4..6].copy_from_slice(&1_u16.to_be_bytes());
    frame[6..8].copy_from_slice(&77_u16.to_be_bytes());
    frame[20..28].copy_from_slice(&1_u64.to_be_bytes());
    frame[28..44].copy_from_slice(b"spawned-worker01");
    frame[44..52].copy_from_slice(&1_u64.to_be_bytes());

    let sent = send(io::stdin().as_fd(), &frame, SendFlags::NOSIGNAL)
        .map_err(|error| io::Error::from_raw_os_error(error.raw_os_error()))?;
    if sent != frame.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            "partial seqpacket fixture send",
        ));
    }
    Ok(())
}
