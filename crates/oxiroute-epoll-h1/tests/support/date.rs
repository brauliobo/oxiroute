use std::cell::Cell;

const DAYS: [&[u8; 3]; 7] = [b"Thu", b"Fri", b"Sat", b"Sun", b"Mon", b"Tue", b"Wed"];
const MONTHS: [&[u8; 3]; 12] = [
    b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec",
];

fn civil(days: i64) -> (i64, usize, i64) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month + 2) / 5 + 1;
    let (month, year_shift) = if month < 10 {
        (month + 3, 0)
    } else {
        (month - 9, 1)
    };
    (
        year_of_era + era * 400 + year_shift,
        month as usize - 1,
        day,
    )
}

fn digits(out: &mut [u8], value: i64) {
    let mut rest = value;
    for slot in out.iter_mut().rev() {
        *slot = b'0' + (rest % 10) as u8;
        rest /= 10;
    }
}

thread_local! {
    static LAST: Cell<(u64, [u8; 29])> = const { Cell::new((u64::MAX, [0; 29])) };
}

pub fn cached_http_date(seconds: u64) -> [u8; 29] {
    LAST.with(|last| {
        let (at, text) = last.get();
        if at == seconds {
            return text;
        }
        let text = http_date(seconds);
        last.set((seconds, text));
        text
    })
}

pub fn http_date(seconds: u64) -> [u8; 29] {
    let days = (seconds / 86_400) as i64;
    let rest = (seconds % 86_400) as i64;
    let (year, month, day) = civil(days);
    let mut out = *b"Thu, 01 Jan 1970 00:00:00 GMT";
    out[..3].copy_from_slice(DAYS[(days % 7) as usize]);
    digits(&mut out[5..7], day);
    out[8..11].copy_from_slice(MONTHS[month]);
    digits(&mut out[12..16], year);
    digits(&mut out[17..19], rest / 3600);
    digits(&mut out[20..22], rest % 3600 / 60);
    digits(&mut out[23..25], rest % 60);
    out
}

#[cfg(test)]
mod tests {
    use super::http_date;

    #[test]
    fn formats_the_http_date_of_known_instants() {
        assert_eq!(&http_date(0), b"Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(&http_date(784_111_777), b"Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(&http_date(1_709_164_800), b"Thu, 29 Feb 2024 00:00:00 GMT");
    }
}
