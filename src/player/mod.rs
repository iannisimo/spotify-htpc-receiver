//! The player daemon (Soloist): its wire protocol, and later the client that talks to it.

mod protocol;

pub use protocol::{ParseError, WireCommand, encode_command, parse_message};
