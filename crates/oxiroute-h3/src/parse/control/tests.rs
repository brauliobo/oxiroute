use super::*;

fn frame(kind: u64, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    frame::write_header(&mut out, kind, payload.len() as u64);
    out.extend_from_slice(payload);
    out
}

fn settings_frame() -> Vec<u8> {
    let mut out = Vec::new();
    settings::encode(&mut out, 1000);
    out
}

fn id(value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    varint::encode(&mut out, value);
    out
}

fn feed(control: &mut Control, bytes: &[u8]) -> Result<bool, Violation> {
    control.feed(Bytes::copy_from_slice(bytes))
}

#[test]
fn settings_first_then_goaway_and_unknown_frames() {
    let mut control = Control::new();
    assert_eq!(feed(&mut control, &settings_frame()), Ok(false));
    assert_eq!(
        control.settings().unwrap().max_field_section_size,
        Some(1000)
    );
    assert_eq!(
        feed(&mut control, &frame(0x1f * 4 + 0x21, &[1; 300])),
        Ok(false)
    );
    assert_eq!(
        feed(&mut control, &frame(frame::MAX_PUSH_ID, &id(4))),
        Ok(false)
    );
    assert_eq!(feed(&mut control, &frame(frame::GOAWAY, &id(8))), Ok(true));
    assert_eq!(feed(&mut control, &frame(frame::GOAWAY, &id(8))), Ok(true));
}

#[test]
fn bytes_may_arrive_one_at_a_time() {
    let mut control = Control::new();
    let mut all = settings_frame();
    all.extend(frame(0x40, &[0; 20]));
    all.extend(frame(frame::GOAWAY, &id(0)));
    let mut goaway = false;
    for byte in all {
        goaway |= feed(&mut control, &[byte]).unwrap();
    }
    assert!(goaway);
}

#[test]
fn first_frame_other_than_settings_is_missing_settings() {
    for first in [
        frame(frame::GOAWAY, &id(0)),
        frame(0x40, &[]),
        frame(frame::MAX_PUSH_ID, &id(0)),
    ] {
        let mut control = Control::new();
        assert_eq!(
            feed(&mut control, &first).unwrap_err().0,
            Code::H3_MISSING_SETTINGS
        );
    }
}

#[test]
fn forbidden_and_repeated_frames_are_unexpected() {
    for kind in [
        frame::DATA,
        frame::HEADERS,
        frame::PUSH_PROMISE,
        0x2,
        0x6,
        0x8,
        0x9,
        frame::SETTINGS,
    ] {
        let mut control = Control::new();
        feed(&mut control, &settings_frame()).unwrap();
        assert_eq!(
            feed(&mut control, &frame(kind, &[])).unwrap_err().0,
            Code::H3_FRAME_UNEXPECTED,
            "{kind:#x}"
        );
    }
}

#[test]
fn identifier_frames_are_validated() {
    let mut control = Control::new();
    feed(&mut control, &settings_frame()).unwrap();
    feed(&mut control, &frame(frame::GOAWAY, &id(8))).unwrap();
    assert_eq!(
        feed(&mut control, &frame(frame::GOAWAY, &id(12)))
            .unwrap_err()
            .0,
        Code::H3_ID_ERROR
    );

    let mut control = Control::new();
    feed(&mut control, &settings_frame()).unwrap();
    feed(&mut control, &frame(frame::MAX_PUSH_ID, &id(8))).unwrap();
    assert_eq!(
        feed(&mut control, &frame(frame::MAX_PUSH_ID, &id(4)))
            .unwrap_err()
            .0,
        Code::H3_ID_ERROR
    );

    let mut control = Control::new();
    feed(&mut control, &settings_frame()).unwrap();
    assert_eq!(
        feed(&mut control, &frame(frame::CANCEL_PUSH, &id(0)))
            .unwrap_err()
            .0,
        Code::H3_ID_ERROR
    );

    for payload in [&[][..], &[0x40], &[0x01, 0x01], &[0; 9]] {
        let mut control = Control::new();
        feed(&mut control, &settings_frame()).unwrap();
        assert_eq!(
            feed(&mut control, &frame(frame::GOAWAY, payload))
                .unwrap_err()
                .0,
            Code::H3_FRAME_ERROR
        );
    }
}

#[test]
fn settings_errors_carry_their_codes() {
    let mut control = Control::new();
    assert_eq!(
        feed(&mut control, &frame(frame::SETTINGS, &[0x02, 0x00]))
            .unwrap_err()
            .0,
        Code::H3_SETTINGS_ERROR
    );
    let mut control = Control::new();
    assert_eq!(
        feed(&mut control, &frame(frame::SETTINGS, &[0x01]))
            .unwrap_err()
            .0,
        Code::H3_FRAME_ERROR
    );
    let mut control = Control::new();
    let mut huge = Vec::new();
    frame::write_header(&mut huge, frame::SETTINGS, MAX_SETTINGS_PAYLOAD + 1);
    assert_eq!(
        feed(&mut control, &huge).unwrap_err().0,
        Code::H3_EXCESSIVE_LOAD
    );
}

#[test]
fn unknown_frames_are_skipped_without_buffering() {
    let mut control = Control::new();
    feed(&mut control, &settings_frame()).unwrap();
    let mut header = Vec::new();
    frame::write_header(&mut header, 0x1f * 9 + 0x21, 1 << 40);
    feed(&mut control, &header).unwrap();
    for _ in 0..16 {
        feed(&mut control, &[0u8; 4096]).unwrap();
    }
    assert!(control.payload.capacity() <= MAX_SETTINGS_PAYLOAD as usize);
}
