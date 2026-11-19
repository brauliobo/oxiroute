use std::{
    fmt::Write as _,
    fs::File,
    io::{self, Read as _},
    os::unix::fs::FileExt as _,
    path::Path,
};

use sha2::{Digest as _, Sha256};

/// Longest build id a handshake carries.
pub const MAX_BUILD_ID_BYTES: usize = 64;

const ELF_MAGIC: [u8; 4] = *b"\x7fELF";
const ELF_CLASS_64: u8 = 2;
const ELF_LITTLE_ENDIAN: u8 = 1;
const ELF_HEADER_BYTES: usize = 64;
const PROGRAM_HEADER_BYTES: usize = 56;
const PT_NOTE: u32 = 4;
const NT_GNU_BUILD_ID: u32 = 3;
const MAX_NOTE_SEGMENT_BYTES: u64 = 64 * 1024;
const MAX_BUILD_ID_NOTE_BYTES: usize = 32;
const IMAGE_DIGEST_BYTES: usize = 16;
const READ_BLOCK_BYTES: usize = 1 << 20;

/// The build id of the running executable.
///
/// It is the GNU build id the linker recorded in the image (`gnu:<hex>`), which is a hash of the
/// image and costs a few reads. An image without one, or in an object format this does not parse,
/// is named by the leading 128 bits of its SHA-256 instead (`sha256:<hex>`). It is read from the
/// process's own image, so it names the binary that runs even after the file it was started from
/// was replaced.
///
/// # Errors
///
/// Returns an error when the image cannot be read.
pub fn executable_build_id() -> io::Result<String> {
    build_id_of(Path::new("/proc/self/exe"))
}

/// The build id of the executable file at `path`, named as [`executable_build_id`] names the
/// running one.
///
/// # Errors
///
/// Returns an error when the file cannot be read.
pub fn build_id_of(path: &Path) -> io::Result<String> {
    let image = File::open(path)?;
    match gnu_build_id(&image)? {
        Some(note) => Ok(format!("gnu:{}", hex(&note))),
        None => image_digest(image).map(|digest| format!("sha256:{}", hex(&digest))),
    }
}

fn gnu_build_id(image: &File) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0_u8; ELF_HEADER_BYTES];
    if image.read_exact_at(&mut header, 0).is_err()
        || header[..4] != ELF_MAGIC
        || header[4] != ELF_CLASS_64
        || header[5] != ELF_LITTLE_ENDIAN
    {
        return Ok(None);
    }
    let table = u64::from_le_bytes(header[32..40].try_into().expect("fixed slice"));
    let entry = usize::from(u16::from_le_bytes([header[54], header[55]]));
    let count = u16::from_le_bytes([header[56], header[57]]);
    if entry < PROGRAM_HEADER_BYTES {
        return Ok(None);
    }
    for index in 0..u64::from(count) {
        let mut program = [0_u8; PROGRAM_HEADER_BYTES];
        let at = table.saturating_add(index.saturating_mul(entry as u64));
        image.read_exact_at(&mut program, at)?;
        let kind = u32::from_le_bytes(program[..4].try_into().expect("fixed slice"));
        let offset = u64::from_le_bytes(program[8..16].try_into().expect("fixed slice"));
        let size = u64::from_le_bytes(program[32..40].try_into().expect("fixed slice"));
        if kind != PT_NOTE || size > MAX_NOTE_SEGMENT_BYTES {
            continue;
        }
        let mut notes = vec![0_u8; usize::try_from(size).expect("bounded note segment")];
        image.read_exact_at(&mut notes, offset)?;
        if let Some(id) = parse_build_id_note(&notes) {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// The descriptor of the `GNU` build id note among the ELF notes of one segment.
fn parse_build_id_note(mut notes: &[u8]) -> Option<Vec<u8>> {
    let word = |bytes: &[u8], at: usize| {
        bytes
            .get(at..at + 4)
            .map(|word| u32::from_le_bytes(word.try_into().expect("fixed slice")) as usize)
    };
    while notes.len() >= 12 {
        let name_size = word(notes, 0)?;
        let desc_size = word(notes, 4)?;
        let kind = u32::try_from(word(notes, 8)?).ok()?;
        let name_end = 12 + name_size.next_multiple_of(4);
        let desc_end = name_end + desc_size.next_multiple_of(4);
        let name = notes.get(12..12 + name_size)?;
        let desc = notes.get(name_end..name_end + desc_size)?;
        if kind == NT_GNU_BUILD_ID && name == b"GNU\0" && desc_size <= MAX_BUILD_ID_NOTE_BYTES {
            return Some(desc.to_vec());
        }
        notes = notes.get(desc_end..)?;
    }
    None
}

fn image_digest(mut image: File) -> io::Result<[u8; IMAGE_DIGEST_BYTES]> {
    let mut hasher = Sha256::new();
    let mut block = vec![0_u8; READ_BLOCK_BYTES];
    loop {
        match image.read(&mut block)? {
            0 => break,
            read => hasher.update(&block[..read]),
        }
    }
    let digest = hasher.finalize();
    Ok(digest[..IMAGE_DIGEST_BYTES]
        .try_into()
        .expect("fixed digest prefix"))
}

fn hex(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(text, "{byte:02x}").expect("writing to a String cannot fail");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(name: &[u8], kind: u32, desc: &[u8]) -> Vec<u8> {
        let pad = |bytes: &[u8]| {
            let mut padded = bytes.to_vec();
            padded.resize(bytes.len().next_multiple_of(4), 0);
            padded
        };
        let mut note = Vec::new();
        note.extend_from_slice(&u32::try_from(name.len()).unwrap().to_le_bytes());
        note.extend_from_slice(&u32::try_from(desc.len()).unwrap().to_le_bytes());
        note.extend_from_slice(&kind.to_le_bytes());
        note.extend(pad(name));
        note.extend(pad(desc));
        note
    }

    #[test]
    fn the_gnu_build_id_is_found_among_other_notes() {
        let mut notes = note(b"GNU\0", 1, &[6, 1, 0, 0, 0, 0]);
        notes.extend(note(b"GNU\0", NT_GNU_BUILD_ID, &[0xab; 20]));
        assert_eq!(parse_build_id_note(&notes), Some(vec![0xab; 20]));
    }

    #[test]
    fn notes_without_a_gnu_build_id_name_none() {
        assert_eq!(parse_build_id_note(&[]), None);
        assert_eq!(parse_build_id_note(&note(b"GNU\0", 1, &[0; 4])), None);
        assert_eq!(
            parse_build_id_note(&note(b"OTHER\0", NT_GNU_BUILD_ID, &[1; 20])),
            None
        );
        assert_eq!(
            parse_build_id_note(&note(b"GNU\0", NT_GNU_BUILD_ID, &[1; 64])),
            None
        );
    }

    #[test]
    fn a_truncated_note_names_none() {
        let mut notes = note(b"GNU\0", NT_GNU_BUILD_ID, &[1; 20]);
        notes.truncate(notes.len() - 8);
        assert_eq!(parse_build_id_note(&notes), None);
    }

    #[test]
    fn the_running_image_has_a_stable_id_within_the_handshake_bound() {
        let id = executable_build_id().unwrap();
        assert!(id.starts_with("gnu:") || id.starts_with("sha256:"), "{id}");
        assert!(id.len() <= MAX_BUILD_ID_BYTES);
        assert_eq!(executable_build_id().unwrap(), id);
    }

    #[test]
    fn an_image_that_is_not_elf_is_named_by_its_digest() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image");
        std::fs::write(&path, b"not an elf image").unwrap();
        let image = File::open(&path).unwrap();
        assert_eq!(gnu_build_id(&image).unwrap(), None);
        assert_eq!(image_digest(image).unwrap().len(), IMAGE_DIGEST_BYTES);
    }
}
