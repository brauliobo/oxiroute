use std::net::{SocketAddr, SocketAddrV4};

use rustix::net::{
    AddressFamily, RecvFlags, SendFlags, SocketType, netlink::SOCK_DIAG,
    netlink::SocketAddrNetlink, recv, sendto, socket,
};

const SOCK_DIAG_BY_FAMILY: u16 = 20;
const NLM_F_REQUEST_DUMP: u16 = 0x301;
const NLMSG_DONE: u16 = 3;
const NLMSG_ERROR: u16 = 2;
const TCP_LISTEN_STATES: u32 = 1 << 10;
const IPPROTO_TCP: u8 = 6;
const HEADER_LEN: usize = 16;
const REQUEST_LEN: usize = HEADER_LEN + 56;
const REQUEST_PORT: usize = HEADER_LEN + 8;
const REPLY_PORT: usize = HEADER_LEN + 4;
const REPLY_ADDRESS: usize = HEADER_LEN + 8;
const REPLY_RECEIVE_QUEUE: usize = HEADER_LEN + 56;

fn request(port: u16) -> [u8; REQUEST_LEN] {
    let mut message = [0; REQUEST_LEN];
    message[0..4].copy_from_slice(
        &u32::try_from(REQUEST_LEN)
            .expect("a short request")
            .to_ne_bytes(),
    );
    message[4..6].copy_from_slice(&SOCK_DIAG_BY_FAMILY.to_ne_bytes());
    message[6..8].copy_from_slice(&NLM_F_REQUEST_DUMP.to_ne_bytes());
    message[HEADER_LEN] = 2;
    message[HEADER_LEN + 1] = IPPROTO_TCP;
    message[HEADER_LEN + 4..HEADER_LEN + 8].copy_from_slice(&TCP_LISTEN_STATES.to_ne_bytes());
    message[REQUEST_PORT..REQUEST_PORT + 2].copy_from_slice(&port.to_be_bytes());
    message
}

fn field<const N: usize>(bytes: &[u8], at: usize) -> [u8; N] {
    bytes[at..at + N]
        .try_into()
        .expect("a field inside its message")
}

fn queued_for(message: &[u8], address: SocketAddrV4) -> Option<u64> {
    let local = field::<4>(message, REPLY_ADDRESS);
    let listens_on_address = local == [0; 4] || local == address.ip().octets();
    let listens_on_port = u16::from_be_bytes(field(message, REPLY_PORT)) == address.port();
    (listens_on_address && listens_on_port)
        .then(|| u64::from(u32::from_ne_bytes(field(message, REPLY_RECEIVE_QUEUE))))
}

/// The accept queue of the listening socket that serves `address`, read by asking the kernel's socket diagnostics for
/// the listeners of its port, so the cost does not grow with the connections other processes hold open.
pub fn pending_accepts(address: SocketAddr) -> Option<u64> {
    let SocketAddr::V4(address) = address else {
        return None;
    };
    let diagnostics = socket(AddressFamily::NETLINK, SocketType::RAW, Some(SOCK_DIAG))
        .expect("socket diagnostics");
    sendto(
        &diagnostics,
        &request(address.port()),
        SendFlags::empty(),
        &SocketAddrNetlink::new(0, 0),
    )
    .expect("ask the kernel for the listeners");
    let mut reply = vec![0; 1 << 16];
    let mut found = None;
    loop {
        let (_, length) =
            recv(&diagnostics, reply.as_mut_slice(), RecvFlags::empty()).expect("listener reply");
        let mut at = 0;
        while at + HEADER_LEN <= length {
            let size =
                usize::try_from(u32::from_ne_bytes(field(&reply, at))).expect("a message length");
            match u16::from_ne_bytes(field(&reply, at + 4)) {
                NLMSG_DONE => return found,
                NLMSG_ERROR => panic!("the kernel refused the listener query"),
                _ => found = found.or(queued_for(&reply[at..at + size], address)),
            }
            at += size.next_multiple_of(4);
        }
    }
}
