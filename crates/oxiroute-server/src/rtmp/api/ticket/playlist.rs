use super::TICKET_PARAMETER;

const HLS_CONTENT_TYPE: &str = "application/vnd.apple.mpegurl";
const DASH_CONTENT_TYPE: &str = "application/dash+xml";
const HLS_URI_ATTRIBUTES: [&str; 1] = ["URI"];
const DASH_URI_ATTRIBUTES: [&str; 2] = ["sourceURL", "media"];

pub(in super::super) fn carry_ticket(content_type: &str, body: Vec<u8>, ticket: &str) -> Vec<u8> {
    let carry: fn(&str, &str) -> String = match content_type {
        HLS_CONTENT_TYPE => hls,
        DASH_CONTENT_TYPE => dash,
        _ => return body,
    };
    match String::from_utf8(body) {
        Ok(text) => carry(&text, ticket).into_bytes(),
        Err(error) => error.into_bytes(),
    }
}

fn hls(playlist: &str, ticket: &str) -> String {
    let mut output = String::with_capacity(playlist.len() + 64);
    for line in playlist.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let ending = &line[content.len()..];
        if content.starts_with('#') {
            output.push_str(&rewrite_attributes(content, &HLS_URI_ATTRIBUTES, ticket));
        } else if content.is_empty() {
            output.push_str(content);
        } else {
            output.push_str(&with_ticket(content, ticket));
        }
        output.push_str(ending);
    }
    output
}

fn dash(manifest: &str, ticket: &str) -> String {
    rewrite_attributes(manifest, &DASH_URI_ATTRIBUTES, ticket)
}

fn rewrite_attributes(text: &str, names: &[&str], ticket: &str) -> String {
    let mut output = String::with_capacity(text.len() + 64);
    let mut rest = text;
    while let Some(value_start) = next_attribute_value(rest, names) {
        let Some(length) = rest[value_start..].find('"') else {
            break;
        };
        let value_end = value_start + length;
        output.push_str(&rest[..value_start]);
        output.push_str(&with_ticket(&rest[value_start..value_end], ticket));
        rest = &rest[value_end..];
    }
    output.push_str(rest);
    output
}

fn next_attribute_value(text: &str, names: &[&str]) -> Option<usize> {
    names
        .iter()
        .filter_map(|name| attribute_value_start(text, name))
        .min()
}

fn attribute_value_start(text: &str, name: &str) -> Option<usize> {
    let needle = format!("{name}=\"");
    let mut from = 0;
    while let Some(found) = text[from..].find(&needle) {
        let start = from + found;
        let boundary = text[..start]
            .chars()
            .next_back()
            .is_none_or(|previous| !previous.is_ascii_alphanumeric() && previous != '-');
        if boundary {
            return Some(start + needle.len());
        }
        from = start + needle.len();
    }
    None
}

fn with_ticket(reference: &str, ticket: &str) -> String {
    if is_absolute(reference) {
        return reference.to_owned();
    }
    let (target, fragment) = reference
        .split_once('#')
        .map_or((reference, ""), |(target, _)| {
            (target, &reference[target.len()..])
        });
    let separator = if target.contains('?') { '&' } else { '?' };
    format!("{target}{separator}{TICKET_PARAMETER}={ticket}{fragment}")
}

fn is_absolute(reference: &str) -> bool {
    if reference.starts_with("//") {
        return true;
    }
    reference.split_once(':').is_some_and(|(scheme, _)| {
        scheme.starts_with(|first: char| first.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "+-.".contains(character))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_references_carry_the_ticket_and_absolute_ones_do_not() {
        let playlist = "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"keys/key-1.bin\",IV=0x01\n#EXTINF:2.000,\nseg-1.ts\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.example.com/k.bin\"\n#EXTINF:2.000,\nseg-2.ts?x=1\r\n#EXT-X-ENDLIST\n";
        assert_eq!(
            hls(playlist, "T"),
            "#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"keys/key-1.bin?ticket=T\",IV=0x01\n#EXTINF:2.000,\nseg-1.ts?ticket=T\n#EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.example.com/k.bin\"\n#EXTINF:2.000,\nseg-2.ts?x=1&ticket=T\r\n#EXT-X-ENDLIST\n"
        );
    }

    #[test]
    fn master_playlist_variant_references_carry_the_ticket() {
        assert_eq!(
            hls("#EXT-X-STREAM-INF:BANDWIDTH=1\nhigh/index.m3u8\n", "T"),
            "#EXT-X-STREAM-INF:BANDWIDTH=1\nhigh/index.m3u8?ticket=T\n"
        );
    }

    #[test]
    fn dash_references_carry_the_ticket() {
        let manifest = "<SegmentList><Initialization sourceURL=\"init.mp4\"/><SegmentURL media=\"seg-0.m4s\" t=\"0\" d=\"1000\"/><SegmentURL media=\"seg-1.m4s\" t=\"1000\" d=\"1000\"/></SegmentList><AdaptationSet mimeType=\"video/mp4\">";
        assert_eq!(
            dash(manifest, "T"),
            "<SegmentList><Initialization sourceURL=\"init.mp4?ticket=T\"/><SegmentURL media=\"seg-0.m4s?ticket=T\" t=\"0\" d=\"1000\"/><SegmentURL media=\"seg-1.m4s?ticket=T\" t=\"1000\" d=\"1000\"/></SegmentList><AdaptationSet mimeType=\"video/mp4\">"
        );
    }

    #[test]
    fn other_content_is_untouched() {
        let body = b"seg-1.ts\n".to_vec();
        assert_eq!(carry_ticket("video/mp2t", body.clone(), "T"), body);
    }
}
