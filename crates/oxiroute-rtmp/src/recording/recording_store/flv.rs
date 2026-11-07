use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
};

use super::RecordingStoreError;

pub(super) fn inspect_tail(file: &mut File, length: u64) -> Result<(u8, u32), RecordingStoreError> {
    if length < 28 {
        return Err(RecordingStoreError::ResumeInvalid);
    }
    let mut header = [0_u8; 9];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut header))
        .map_err(|_| RecordingStoreError::ResumeInvalid)?;
    if &header[..4] != b"FLV\x01" || header[5..9] != [0, 0, 0, 9] {
        return Err(RecordingStoreError::ResumeInvalid);
    }
    let mut previous_tag_size = [0_u8; 4];
    file.seek(SeekFrom::End(-4))
        .and_then(|_| file.read_exact(&mut previous_tag_size))
        .map_err(|_| RecordingStoreError::ResumeInvalid)?;
    let tag_size = u64::from(u32::from_be_bytes(previous_tag_size));
    let tag_start = length
        .checked_sub(4)
        .and_then(|end| end.checked_sub(tag_size))
        .filter(|start| *start >= 13)
        .ok_or(RecordingStoreError::ResumeInvalid)?;
    let mut tag_header = [0_u8; 11];
    file.seek(SeekFrom::Start(tag_start))
        .and_then(|_| file.read_exact(&mut tag_header))
        .map_err(|_| RecordingStoreError::ResumeInvalid)?;
    let data_size = u32::from_be_bytes([0, tag_header[1], tag_header[2], tag_header[3]]);
    if tag_size != u64::from(data_size) + 11 {
        return Err(RecordingStoreError::ResumeInvalid);
    }
    let timestamp_ms =
        u32::from_be_bytes([tag_header[7], tag_header[4], tag_header[5], tag_header[6]]);
    file.seek(SeekFrom::End(0))
        .map_err(|_| RecordingStoreError::ResumeInvalid)?;
    Ok((header[4], timestamp_ms))
}
