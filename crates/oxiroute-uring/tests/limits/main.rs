use oxiroute_uring::{Config, Missing, probe};
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

const HEADROOM: u64 = 1 << 20;

fn mapped_bytes() -> u64 {
    let statm = std::fs::read_to_string("/proc/self/statm").expect("process memory");
    let pages = statm
        .split_whitespace()
        .next()
        .and_then(|pages| pages.parse::<u64>().ok());
    pages.expect("mapped pages") * 4096
}

#[test]
fn a_ring_whose_buffers_do_not_fit_the_address_space_is_refused_before_it_starts() {
    let Rlimit { maximum, .. } = getrlimit(Resource::As);
    let limit = Rlimit {
        current: Some(mapped_bytes() + HEADROOM),
        maximum,
    };
    setrlimit(Resource::As, limit).expect("lower the address space limit");
    let config = Config {
        buffers: 4096,
        buffer_size: 1 << 20,
        large_buffer_size: 1 << 20,
        ..Config::default()
    };
    let refusal = probe(&config).expect_err("the receive buffers need more than the headroom");
    assert!(matches!(refusal, Missing::Memory(_)), "{refusal}");
    assert!(refusal.to_string().contains("receive buffers"), "{refusal}");
}
