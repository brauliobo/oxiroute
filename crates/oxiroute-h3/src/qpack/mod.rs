//! QPACK (RFC 9204) for a peer that never gets a dynamic table: this server advertises capacity 0 and no blocked
//! streams, so request sections decode from the static table and literals, and responses are encoded the same way.
//! The Huffman coder and prefix integers are those of HPACK, shared from `oxiroute-h2`.

pub mod decode;
pub mod encode;
pub mod static_table;
