use std::{
    collections::{BTreeSet, HashMap, HashSet},
    ffi::{OsStr, OsString},
    fmt::Write as _,
    fs::File,
    io::{Read as _, Write as _},
    os::unix::ffi::OsStringExt as _,
    path::{Component, Path},
    sync::{
        Arc, Mutex, OnceLock,
        mpsc::{self, SyncSender, TrySendError},
    },
    thread::JoinHandle,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use http::{
    HeaderMap, HeaderName, HeaderValue, Method,
    header::{
        AUTHORIZATION, IF_MATCH, IF_MODIFIED_SINCE, IF_NONE_MATCH, IF_RANGE, IF_UNMODIFIED_SINCE,
        RANGE,
    },
};
use openssl::{
    hash::{Hasher, MessageDigest},
    memcmp,
    sha::sha256,
};
use oxiroute_config::{
    AccessLogPolicy, HttpAccessPolicy, HttpCookieAttributePolicy, HttpCookiePathRewrite,
    HttpGzipMinimumVersion, HttpGzipPolicy, HttpLiteralHeader, HttpPoolSelect,
    HttpProxyPathRewrite, HttpProxyPolicy, HttpProxyTimeouts, HttpRedirectLocation,
    HttpRequestHeaderMutation, HttpRequestHeaderValue, HttpResponseHeaderMutation,
    HttpRetryBodySafety, HttpRetryMethodSafety, HttpRetryTarget, HttpRetryTrigger, HttpRoutePolicy,
    HttpSelectInvalid, HttpSelectSpill, HttpStaticPathMapping, HttpStaticTryFile, HttpUpstreamHost,
};
use rustix::{
    fd::OwnedFd,
    fs::{self as rustix_fs, AtFlags, Dir, FileType, Mode, OFlags},
    io::Errno,
};
use zeroize::Zeroizing;

use crate::{
    http_cache::HttpCachePlan,
    secure_bearer::{HeaderCardinality, SecureBearerToken, single_header},
    upstream_peer::UpstreamPlan,
};

mod access;
mod access_log;
mod plan;
mod pool_select;
mod proxy_policy;
mod static_files;
mod static_serve;

pub(crate) use access::*;
pub(crate) use access_log::*;
pub(crate) use plan::*;
pub(crate) use pool_select::*;
pub(crate) use proxy_policy::*;
pub(crate) use static_files::*;
pub(crate) use static_serve::*;

#[cfg(test)]
mod static_request_tests;

#[cfg(test)]
mod retry_tests;

#[cfg(test)]
mod access_log_tests;
