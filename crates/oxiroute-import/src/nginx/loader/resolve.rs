#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct Resolution {
    pub(super) paths: Vec<PathBuf>,
    pub(super) truncated: bool,
}

pub(super) enum ResolveFailure {
    Missing,
    Io(io::Error),
    UnsupportedGlob,
    GlobWorkLimit,
}

pub(super) fn resolve_pattern(
    prefix: &Path,
    pattern: &[u8],
    max_matches: usize,
    max_work: usize,
) -> Result<Resolution, ResolveFailure> {
    validate_glob_pattern(pattern).map_err(|()| ResolveFailure::UnsupportedGlob)?;
    let pattern_path = PathBuf::from(OsString::from_vec(pattern.to_vec()));
    if !has_glob_meta(pattern) {
        let literal_path = PathBuf::from(OsString::from_vec(unescape_glob_bytes(pattern)));
        let path = if pattern_path.is_absolute() {
            literal_path
        } else {
            prefix.join(literal_path)
        };
        return match fs::symlink_metadata(&path) {
            Ok(_) => Ok(Resolution {
                paths: vec![path],
                truncated: false,
            }),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(ResolveFailure::Missing),
            Err(error) => Err(ResolveFailure::Io(error)),
        };
    }

    expand_glob(prefix, &pattern_path, max_matches, max_work)
}

pub(super) fn canonical_path_changed(requested: &Path, expected: &Path) -> bool {
    match fs::canonicalize(requested) {
        Ok(path) => path != expected,
        Err(_) => true,
    }
}

fn expand_glob(
    prefix: &Path,
    pattern: &Path,
    max_matches: usize,
    max_work: usize,
) -> Result<Resolution, ResolveFailure> {
    let mut candidates = if pattern.is_absolute() {
        vec![PathBuf::from("/")]
    } else {
        vec![prefix.to_path_buf()]
    };
    let mut truncated = false;
    let mut work = 0;

    for component in pattern.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                for candidate in &mut candidates {
                    candidate.push("..");
                }
            }
            Component::Normal(component) if has_glob_meta(component.as_bytes()) => {
                let mut next = Vec::new();
                for candidate in candidates {
                    let entries = match fs::read_dir(&candidate) {
                        Ok(entries) => entries,
                        Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(ResolveFailure::Io(error)),
                    };
                    for entry in entries {
                        if work == max_work {
                            return Err(ResolveFailure::GlobWorkLimit);
                        }
                        work += 1;
                        let entry = entry.map_err(ResolveFailure::Io)?;
                        if glob_matches(component.as_bytes(), entry.file_name().as_bytes()) {
                            push_bounded_path(&mut next, entry.path(), max_matches, &mut truncated);
                        }
                    }
                }
                candidates = next;
            }
            Component::Normal(component) => {
                for candidate in &mut candidates {
                    candidate.push(OsString::from_vec(unescape_glob_bytes(
                        component.as_bytes(),
                    )));
                }
            }
            Component::Prefix(_) => unreachable!("Unix paths do not have prefixes"),
        }
    }

    let mut paths = Vec::new();
    for candidate in candidates {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => paths.push(candidate),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ResolveFailure::Io(error)),
        }
    }
    paths.sort_by(|left, right| {
        left.as_os_str()
            .as_bytes()
            .cmp(right.as_os_str().as_bytes())
    });
    Ok(Resolution { paths, truncated })
}

fn validate_glob_pattern(pattern: &[u8]) -> Result<(), ()> {
    let mut index = 0;
    while let Some(byte) = pattern.get(index).copied() {
        match byte {
            b'\\' => {
                if index + 1 == pattern.len() {
                    return Err(());
                }
                index += 2;
            }
            b'[' => {
                index = validate_glob_class(pattern, index)?;
            }
            b']' => return Err(()),
            _ => index += 1,
        }
    }
    Ok(())
}

fn validate_glob_class(pattern: &[u8], opening: usize) -> Result<usize, ()> {
    let mut index = opening + 1;
    if pattern.get(index) == Some(&b'^') {
        return Err(());
    }
    if pattern.get(index) == Some(&b'!') {
        index += 1;
    }
    let content_start = index;
    while let Some(byte) = pattern.get(index).copied() {
        match byte {
            b'[' | b'\\' => return Err(()),
            b']' => {
                if index == content_start {
                    return Err(());
                }
                let content = &pattern[content_start..index];
                let mut content_index = 0;
                while content_index < content.len() {
                    if content_index + 2 < content.len() && content[content_index + 1] == b'-' {
                        if content[content_index] > content[content_index + 2] {
                            return Err(());
                        }
                        content_index += 3;
                    } else {
                        content_index += 1;
                    }
                }
                return Ok(index + 1);
            }
            _ => index += 1,
        }
    }
    Err(())
}

fn push_bounded_path(
    paths: &mut Vec<PathBuf>,
    path: PathBuf,
    max_paths: usize,
    truncated: &mut bool,
) {
    if paths.len() < max_paths {
        paths.push(path);
        return;
    }
    *truncated = true;
    let Some((largest_index, largest)) =
        paths.iter().enumerate().max_by(|(_, left), (_, right)| {
            left.as_os_str()
                .as_bytes()
                .cmp(right.as_os_str().as_bytes())
        })
    else {
        return;
    };
    if path.as_os_str().as_bytes() < largest.as_os_str().as_bytes() {
        paths[largest_index] = path;
    }
}

fn unescape_glob_bytes(pattern: &[u8]) -> Vec<u8> {
    let mut literal = Vec::with_capacity(pattern.len());
    let mut index = 0;
    while let Some(byte) = pattern.get(index).copied() {
        if byte == b'\\' && index + 1 < pattern.len() {
            literal.push(pattern[index + 1]);
            index += 2;
        } else {
            literal.push(byte);
            index += 1;
        }
    }
    literal
}

fn has_glob_meta(pattern: &[u8]) -> bool {
    let mut escaped = false;
    for byte in pattern {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if matches!(*byte, b'*' | b'?' | b'[') {
            return true;
        }
    }
    false
}

fn glob_matches(pattern: &[u8], name: &[u8]) -> bool {
    if name.first() == Some(&b'.') && pattern_first_literal(pattern) != Some(b'.') {
        return false;
    }
    let mut memo = vec![vec![None; name.len() + 1]; pattern.len() + 1];
    glob_matches_from(pattern, name, 0, 0, &mut memo)
}

fn glob_matches_from(
    pattern: &[u8],
    name: &[u8],
    pattern_index: usize,
    name_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(result) = memo[pattern_index][name_index] {
        return result;
    }
    let result = match pattern.get(pattern_index).copied() {
        None => name_index == name.len(),
        Some(b'*') => {
            glob_matches_from(pattern, name, pattern_index + 1, name_index, memo)
                || (name_index < name.len()
                    && glob_matches_from(pattern, name, pattern_index, name_index + 1, memo))
        }
        Some(b'?') => {
            name_index < name.len()
                && glob_matches_from(pattern, name, pattern_index + 1, name_index + 1, memo)
        }
        Some(b'[') => class_match(&pattern[pattern_index..], name.get(name_index).copied())
            .is_some_and(|(matches, consumed)| {
                matches
                    && glob_matches_from(
                        pattern,
                        name,
                        pattern_index + consumed,
                        name_index + 1,
                        memo,
                    )
            }),
        Some(b'\\') if pattern_index + 1 < pattern.len() => {
            name.get(name_index) == pattern.get(pattern_index + 1)
                && glob_matches_from(pattern, name, pattern_index + 2, name_index + 1, memo)
        }
        Some(byte) => {
            name.get(name_index) == Some(&byte)
                && glob_matches_from(pattern, name, pattern_index + 1, name_index + 1, memo)
        }
    };
    memo[pattern_index][name_index] = Some(result);
    result
}

fn class_match(pattern: &[u8], byte: Option<u8>) -> Option<(bool, usize)> {
    let byte = byte?;
    let closing = pattern.iter().position(|candidate| *candidate == b']')?;
    if closing < 2 {
        return None;
    }
    let mut index = 1;
    let negated = matches!(pattern.get(index), Some(b'!' | b'^'));
    if negated {
        index += 1;
    }
    let mut matched = false;
    while index < closing {
        if index + 2 < closing && pattern[index + 1] == b'-' {
            matched |= pattern[index] <= byte && byte <= pattern[index + 2];
            index += 3;
        } else {
            matched |= pattern[index] == byte;
            index += 1;
        }
    }
    Some((matched != negated, closing + 1))
}

fn pattern_first_literal(pattern: &[u8]) -> Option<u8> {
    match pattern {
        [b'\\', literal, ..] => Some(*literal),
        [literal, ..] if !matches!(*literal, b'*' | b'?' | b'[') => Some(*literal),
        _ => None,
    }
}
