pub(super) fn has_glob(value: &[u8]) -> bool {
    value.iter().any(|byte| matches!(byte, b'*' | b'?' | b'['))
}

pub(super) fn glob_matches(pattern: &[u8], value: &[u8]) -> bool {
    if value.first() == Some(&b'.') && pattern.first() != Some(&b'.') {
        return false;
    }
    if pattern.len() > 4_096 || value.len() > 4_096 {
        return false;
    }
    let mut memo = vec![vec![None; value.len() + 1]; pattern.len() + 1];
    glob_matches_at(pattern, value, 0, 0, &mut memo)
}

fn glob_matches_at(
    pattern: &[u8],
    value: &[u8],
    pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    if let Some(result) = memo[pattern_index][value_index] {
        return result;
    }
    let result = match pattern.get(pattern_index) {
        None => value_index == value.len(),
        Some(b'*') => {
            glob_matches_at(pattern, value, pattern_index + 1, value_index, memo)
                || (value_index < value.len()
                    && glob_matches_at(pattern, value, pattern_index, value_index + 1, memo))
        }
        Some(b'?') => {
            value_index < value.len()
                && glob_matches_at(pattern, value, pattern_index + 1, value_index + 1, memo)
        }
        Some(b'[') => class_match_at(pattern, value, pattern_index, value_index, memo),
        Some(literal) => {
            value.get(value_index) == Some(literal)
                && glob_matches_at(pattern, value, pattern_index + 1, value_index + 1, memo)
        }
    };
    memo[pattern_index][value_index] = Some(result);
    result
}

fn class_match_at(
    pattern: &[u8],
    value: &[u8],
    pattern_index: usize,
    value_index: usize,
    memo: &mut [Vec<Option<bool>>],
) -> bool {
    let Some(close) = pattern[pattern_index + 1..]
        .iter()
        .position(|byte| *byte == b']')
        .map(|offset| pattern_index + 1 + offset)
    else {
        return value.get(value_index) == Some(&b'[')
            && glob_matches_at(pattern, value, pattern_index + 1, value_index + 1, memo);
    };
    let Some(candidate) = value.get(value_index) else {
        return false;
    };
    let class = &pattern[pattern_index + 1..close];
    let negated = class
        .first()
        .is_some_and(|byte| matches!(byte, b'!' | b'^'));
    let class = if negated { &class[1..] } else { class };
    let mut matched = false;
    let mut index = 0;
    while index < class.len() {
        if index + 2 < class.len() && class[index + 1] == b'-' {
            matched |= class[index] <= *candidate && *candidate <= class[index + 2];
            index += 3;
        } else {
            matched |= class[index] == *candidate;
            index += 1;
        }
    }
    matched != negated && glob_matches_at(pattern, value, close + 1, value_index + 1, memo)
}
