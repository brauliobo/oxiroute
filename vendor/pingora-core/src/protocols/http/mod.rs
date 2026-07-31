// Copyright 2026 Cloudflare, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! HTTP/1.x and HTTP/2 implementation APIs

pub mod body_buffer;
pub mod bridge;
pub mod client;
pub mod compression;
pub mod conditional_filter;
pub mod custom;
pub mod date;
pub mod error_resp;
pub mod server;
pub mod subrequest;
pub mod v1;
pub mod v2;

pub use server::Session as ServerSession;

/// The Pingora server name string
pub const SERVER_NAME: &[u8; 7] = b"Pingora";

/// The number of response tasks stored inline in an [`HttpTaskBatch`].
pub const HTTP_TASK_BATCH_CAPACITY: usize = 4;

/// A response-task batch with inline storage for the common case.
pub type HttpTaskBatch = smallvec::SmallVec<[HttpTask; HTTP_TASK_BATCH_CAPACITY]>;

/// An enum to hold all possible HTTP response events.
#[derive(Debug)]
pub enum HttpTask {
    /// the response header and the boolean end of response flag
    Header(Box<pingora_http::ResponseHeader>, bool),
    /// A piece of request or response body and the end of request/response boolean flag.
    Body(Option<bytes::Bytes>, bool),
    /// Request or response body bytes that have been upgraded on H1.1, and EOF bool flag.
    UpgradedBody(Option<bytes::Bytes>, bool),
    /// HTTP response trailer
    Trailer(Option<Box<http::HeaderMap>>),
    /// Signal that the response is already finished
    Done,
    /// Signal that the reading of the response encountered errors.
    Failed(pingora_error::BError),
}

impl HttpTask {
    /// Whether this [`HttpTask`] means the end of the response.
    pub fn is_end(&self) -> bool {
        match self {
            HttpTask::Header(_, end) => *end,
            HttpTask::Body(_, end) => *end,
            HttpTask::UpgradedBody(_, end) => *end,
            HttpTask::Trailer(_) => true,
            HttpTask::Done => true,
            HttpTask::Failed(_) => true,
        }
    }

    /// The [`HttpTask`] type as string.
    pub fn type_str(&self) -> &'static str {
        match self {
            HttpTask::Header(..) => "Header",
            HttpTask::Body(..) => "Body",
            HttpTask::UpgradedBody(..) => "UpgradedBody",
            HttpTask::Trailer(_) => "Trailer",
            HttpTask::Done => "Done",
            HttpTask::Failed(_) => "Failed",
        }
    }
}

#[cfg(test)]
mod task_batch_tests {
    use bytes::Bytes;

    use super::*;

    fn body_task(value: u8) -> HttpTask {
        HttpTask::Body(Some(Bytes::from(vec![value])), false)
    }

    #[test]
    fn inline_capacity_covers_one_through_four_tasks() {
        for len in 1..=HTTP_TASK_BATCH_CAPACITY {
            let tasks: HttpTaskBatch = (0..len as u8).map(body_task).collect();

            assert_eq!(tasks.len(), len);
            assert!(!tasks.spilled(), "batch of {len} tasks spilled");
        }
    }

    #[test]
    fn batch_spills_safely_beyond_inline_capacity() {
        let tasks: HttpTaskBatch = (0..=HTTP_TASK_BATCH_CAPACITY as u8)
            .map(body_task)
            .collect();

        assert!(tasks.spilled());
        assert_eq!(
            tasks
                .iter()
                .map(|task| match task {
                    HttpTask::Body(Some(body), false) => body[0],
                    _ => panic!("unexpected task"),
                })
                .collect::<Vec<_>>(),
            (0..=HTTP_TASK_BATCH_CAPACITY as u8).collect::<Vec<_>>()
        );
    }
}
