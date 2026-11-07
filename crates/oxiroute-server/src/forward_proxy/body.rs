#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct TimedBody<B> {
    idle: Pin<Box<Sleep>>,
    idle_timeout: Duration,
    inner: B,
    lifetime: Pin<Box<Sleep>>,
}

pub(super) struct RelayedRequestBody {
    inner: mpsc::Receiver<Frame<Bytes>>,
}

impl Body for RelayedRequestBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.inner)
            .poll_recv(context)
            .map(|frame| frame.map(Ok))
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_closed() && self.inner.is_empty()
    }
}

pub(super) fn relay_request_body(
    body: Incoming,
    limit: usize,
    idle_timeout: Duration,
    lifetime_deadline: Instant,
) -> (
    RelayedRequestBody,
    oneshot::Receiver<Result<(), RequestFailure>>,
) {
    let (frames, receiver) = mpsc::channel(1);
    let (mut completion, completed) = oneshot::channel();
    tokio::spawn(async move {
        let mut body = Limited::new(body, limit);
        let idle = tokio::time::sleep(idle_timeout);
        let lifetime = tokio::time::sleep_until(lifetime_deadline);
        tokio::pin!(idle, lifetime);
        let outcome = loop {
            let frame = tokio::select! {
                () = completion.closed() => return,
                () = &mut idle => break Err(RequestFailure::GatewayTimeout),
                () = &mut lifetime => break Err(RequestFailure::GatewayTimeout),
                frame = poll_fn(|context| Pin::new(&mut body).poll_frame(context)) => frame,
            };
            let Some(frame) = frame else {
                break Ok(());
            };
            let frame = match frame {
                Ok(frame) => frame,
                Err(error) if error.is::<http_body_util::LengthLimitError>() => {
                    break Err(RequestFailure::PayloadTooLarge);
                }
                Err(_) => break Err(RequestFailure::BadRequest),
            };
            idle.as_mut().reset(Instant::now() + idle_timeout);
            tokio::select! {
                () = completion.closed() => return,
                () = &mut idle => break Err(RequestFailure::GatewayTimeout),
                () = &mut lifetime => break Err(RequestFailure::GatewayTimeout),
                result = frames.send(frame) => {
                    let _ = result;
                },
            }
        };
        let _ = completion.send(outcome);
    });
    (RelayedRequestBody { inner: receiver }, completed)
}

impl<B> TimedBody<B> {
    pub(super) fn new(inner: B, idle_timeout: Duration, lifetime_deadline: Instant) -> Self {
        Self {
            idle: Box::pin(tokio::time::sleep(idle_timeout)),
            idle_timeout,
            inner,
            lifetime: Box::pin(tokio::time::sleep_until(lifetime_deadline)),
        }
    }
}

impl<B> Body for TimedBody<B>
where
    B: Body<Data = Bytes> + Unpin,
    B::Error: Error + Send + Sync + 'static,
{
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        context: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if self.lifetime.as_mut().poll(context).is_ready() {
            return Poll::Ready(Some(Err(Box::new(ForwardBodyTimeout::Lifetime))));
        }
        if self.idle.as_mut().poll(context).is_ready() {
            return Poll::Ready(Some(Err(Box::new(ForwardBodyTimeout::Idle))));
        }
        match Pin::new(&mut self.inner).poll_frame(context) {
            Poll::Ready(Some(Ok(frame))) => {
                let deadline = Instant::now() + self.idle_timeout;
                self.idle.as_mut().reset(deadline);
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(error))) => Poll::Ready(Some(Err(Box::new(error)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        self.inner.size_hint()
    }
}

#[derive(Debug, thiserror::Error)]
enum ForwardBodyTimeout {
    #[error("forward response body exceeded its idle timeout")]
    Idle,
    #[error("forward response body exceeded its lifetime timeout")]
    Lifetime,
}
