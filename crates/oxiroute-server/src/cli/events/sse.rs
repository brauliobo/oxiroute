use std::io;

#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct Frame {
    pub(super) id: Option<u64>,
    pub(super) name: String,
    pub(super) data: String,
}

pub(super) struct Frames<I> {
    lines: I,
}

pub(super) fn frames<I: Iterator<Item = io::Result<String>>>(lines: I) -> Frames<I> {
    Frames { lines }
}

impl<I: Iterator<Item = io::Result<String>>> Iterator for Frames<I> {
    type Item = io::Result<Frame>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut frame = Frame::default();
        let mut started = false;
        loop {
            let line = match self.lines.next()? {
                Ok(line) => line,
                Err(error) => return Some(Err(error)),
            };
            if line.is_empty() {
                if started {
                    return Some(Ok(frame));
                }
                continue;
            }
            let Some((field, value)) = line.split_once(':') else {
                continue;
            };
            if field.is_empty() {
                continue;
            }
            started = true;
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "id" => frame.id = value.parse().ok(),
                "event" => value.clone_into(&mut frame.name),
                "data" => {
                    if !frame.data.is_empty() {
                        frame.data.push('\n');
                    }
                    frame.data.push_str(value);
                }
                _ => {}
            }
        }
    }
}
