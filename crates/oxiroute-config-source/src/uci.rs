use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use serde_json::{Map, Value};

use crate::ConfigSourceError;
use crate::limits::{
    BoundedOutput, MAX_NODES, MAX_STRUCTURAL_DEPTH, check_string, source_text, validate_value,
};
use crate::native::{
    NativeDirective, decode_apache, decode_haproxy, decode_nginx, decode_squid, decode_varnish,
};

include!("uci/document.rs");
include!("uci/native.rs");
include!("uci/records.rs");
include!("uci/tokenizer.rs");
