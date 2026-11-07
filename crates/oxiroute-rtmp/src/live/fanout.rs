#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_cache_size(
    limits: LiveHubLimits,
    event: &MediaEvent,
) -> Result<(), LiveHubError> {
    let maximum = match event.kind() {
        MediaEventKind::Metadata => limits.max_cached_metadata_bytes,
        MediaEventKind::AacSequenceHeader
        | MediaEventKind::AvcSequenceHeader
        | MediaEventKind::HevcSequenceHeader
        | MediaEventKind::Av1SequenceHeader => limits.max_cached_codec_header_bytes,
        MediaEventKind::Audio
        | MediaEventKind::VideoKeyframe
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => return Ok(()),
    };
    if event.payload_len() > maximum {
        return Err(LiveHubError::CachedEventTooLarge {
            kind: event.kind(),
            size: event.payload_len(),
            maximum,
        });
    }
    Ok(())
}

pub(super) fn fanout_event(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    event: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    match subscriber.state {
        SubscriberState::AwaitingMedia => match event.kind() {
            MediaEventKind::Audio => {
                enqueue_audio_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
            }
            MediaEventKind::VideoKeyframe => {
                enqueue_mixed_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
            }
            MediaEventKind::Metadata
            | MediaEventKind::AacSequenceHeader
            | MediaEventKind::AvcSequenceHeader
            | MediaEventKind::HevcSequenceHeader
            | MediaEventKind::Av1SequenceHeader
            | MediaEventKind::VideoInterframe
            | MediaEventKind::VideoDisposable => {}
        },
        SubscriberState::AwaitingKeyframe => {
            if event.kind() == MediaEventKind::VideoKeyframe {
                enqueue_mixed_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
            }
        }
        SubscriberState::Audio => {
            fanout_audio_event(subscriber, cache, event, limits, fanout_bytes, report);
        }
        SubscriberState::Mixed => {
            fanout_mixed_event(subscriber, cache, event, limits, fanout_bytes, report);
        }
    }
}

pub(super) fn apply_video_transition(
    subscriber: &mut SubscriberQueue,
    transition: VideoTrackTransition,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    match transition {
        VideoTrackTransition::None => {}
        VideoTrackTransition::Absent => match subscriber.state {
            SubscriberState::AwaitingKeyframe => {
                subscriber.state = SubscriberState::AwaitingMedia;
            }
            SubscriberState::Mixed => {
                *fanout_bytes -= clear_queue(subscriber);
                subscriber.state = SubscriberState::AwaitingMedia;
                report.viewers_resynchronized += 1;
            }
            SubscriberState::AwaitingMedia | SubscriberState::Audio => {}
        },
        VideoTrackTransition::Present { codec_changed } => match subscriber.state {
            SubscriberState::AwaitingMedia => {
                subscriber.state = SubscriberState::AwaitingKeyframe;
            }
            SubscriberState::Mixed if codec_changed => {
                *fanout_bytes -= clear_queue(subscriber);
                subscriber.state = SubscriberState::AwaitingKeyframe;
                report.viewers_resynchronized += 1;
            }
            SubscriberState::AwaitingKeyframe | SubscriberState::Audio | SubscriberState::Mixed => {
            }
        },
    }
}

fn fanout_audio_event(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    event: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    match event.kind() {
        MediaEventKind::VideoKeyframe => {
            let bootstrap = cache.video_bootstrap(event);
            if enqueue(subscriber, &bootstrap, limits, fanout_bytes) {
                subscriber.state = SubscriberState::Mixed;
                report.queued_events += bootstrap.len();
                return;
            }
            *fanout_bytes -= clear_queue(subscriber);
            subscriber.state = SubscriberState::AwaitingKeyframe;
            report.dropped_events += 1;
            report.viewers_resynchronized += 1;
            enqueue_mixed_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
        }
        MediaEventKind::AvcSequenceHeader
        | MediaEventKind::HevcSequenceHeader
        | MediaEventKind::Av1SequenceHeader
        | MediaEventKind::VideoInterframe
        | MediaEventKind::VideoDisposable => {}
        MediaEventKind::Metadata | MediaEventKind::AacSequenceHeader | MediaEventKind::Audio => {
            if enqueue(
                subscriber,
                std::slice::from_ref(event),
                limits,
                fanout_bytes,
            ) {
                report.queued_events += 1;
                return;
            }
            *fanout_bytes -= clear_queue(subscriber);
            subscriber.state = SubscriberState::AwaitingMedia;
            report.dropped_events += 1;
            report.viewers_resynchronized += 1;
            if event.kind() == MediaEventKind::Audio {
                enqueue_audio_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
            }
        }
    }
}

fn fanout_mixed_event(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    event: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    if enqueue(
        subscriber,
        std::slice::from_ref(event),
        limits,
        fanout_bytes,
    ) {
        report.queued_events += 1;
        return;
    }
    if event.kind() == MediaEventKind::VideoDisposable {
        report.dropped_events += 1;
        return;
    }

    *fanout_bytes -= clear_queue(subscriber);
    subscriber.state = SubscriberState::AwaitingKeyframe;
    report.dropped_events += 1;
    report.viewers_resynchronized += 1;
    if event.kind() == MediaEventKind::VideoKeyframe {
        enqueue_mixed_bootstrap(subscriber, cache, event, limits, fanout_bytes, report);
    }
}

fn enqueue_audio_bootstrap(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    audio: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    let bootstrap = cache.audio_bootstrap(audio);
    if enqueue(subscriber, &bootstrap, limits, fanout_bytes) {
        subscriber.state = SubscriberState::Audio;
        report.queued_events += bootstrap.len();
    } else {
        report.dropped_events += 1;
    }
}

fn enqueue_mixed_bootstrap(
    subscriber: &mut SubscriberQueue,
    cache: &MediaCache,
    keyframe: &MediaEvent,
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
    report: &mut PublishReport,
) {
    let bootstrap = cache.mixed_bootstrap(keyframe);
    if enqueue(subscriber, &bootstrap, limits, fanout_bytes) {
        subscriber.state = SubscriberState::Mixed;
        report.queued_events += bootstrap.len();
    } else {
        report.dropped_events += 1;
    }
}

fn enqueue(
    subscriber: &mut SubscriberQueue,
    events: &[MediaEvent],
    limits: LiveHubLimits,
    fanout_bytes: &mut usize,
) -> bool {
    let Some(message_count) = subscriber.events.len().checked_add(events.len()) else {
        return false;
    };
    let Some(batch_bytes) = events.iter().try_fold(0_usize, |total, event| {
        total.checked_add(event.payload_len())
    }) else {
        return false;
    };
    let Some(queue_bytes) = subscriber.bytes.checked_add(batch_bytes) else {
        return false;
    };
    let Some(total_fanout_bytes) = fanout_bytes.checked_add(batch_bytes) else {
        return false;
    };
    if message_count > limits.max_queue_messages_per_subscriber
        || queue_bytes > limits.max_queue_bytes_per_subscriber
        || total_fanout_bytes > limits.max_fanout_bytes
    {
        return false;
    }

    subscriber.events.extend(events.iter().cloned());
    subscriber.bytes = queue_bytes;
    *fanout_bytes = total_fanout_bytes;
    true
}

pub(super) fn reset_subscribers(stream: &mut LiveStream) -> usize {
    stream
        .subscribers
        .values_mut()
        .map(|subscriber| {
            subscriber.state = SubscriberState::AwaitingMedia;
            clear_queue(subscriber)
        })
        .sum()
}

fn clear_queue(subscriber: &mut SubscriberQueue) -> usize {
    subscriber.events.clear();
    std::mem::take(&mut subscriber.bytes)
}
