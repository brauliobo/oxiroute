use oxiroute_uring::{Config, Missing, Ring, probe};
use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

const RINGS: usize = 3;

fn limit_memlock(bytes: u64) {
    let Rlimit { maximum, .. } = getrlimit(Resource::Memlock);
    let limit = Rlimit {
        current: Some(bytes),
        maximum,
    };
    setrlimit(Resource::Memlock, limit).expect("lower the locked memory limit");
}

fn refused_naming_the_limit(config: &Config) {
    let refusal = probe(config).expect_err("no ring fits a locked memory limit of zero");
    let Missing::Locked { limit, ring, .. } = refusal else {
        panic!("not a locked memory refusal: {refusal}");
    };
    assert_eq!((limit, ring), (0, config.ring_bytes()));
    assert!(
        refusal.to_string().contains("RLIMIT_MEMLOCK of 0 KiB"),
        "{refusal}"
    );
}

fn charged_exactly_the_computed_bytes(config: &Config) {
    limit_memlock(RINGS as u64 * config.ring_bytes());
    let rings: Vec<Ring> = std::iter::from_fn(|| Ring::new(config).ok())
        .take(RINGS + 1)
        .collect();
    assert_eq!(
        rings.len(),
        RINGS,
        "one ring is {} bytes",
        config.ring_bytes()
    );
}

#[test]
fn a_kernel_that_accounts_ring_memory_charges_the_computed_bytes_and_the_refusal_names_the_limit() {
    let config = Config::default();
    limit_memlock(0);
    if Ring::new(&config).is_ok() {
        return;
    }
    refused_naming_the_limit(&config);
    charged_exactly_the_computed_bytes(&config);
}

#[test]
fn a_ring_costs_what_its_entries_need_and_a_larger_ring_costs_more() {
    let ring = |entries| {
        Config {
            entries,
            ..Config::default()
        }
        .ring_bytes()
    };
    assert_eq!(ring(2048), 217_088);
    assert_eq!(ring(2000), ring(2048));
    assert!(ring(4096) > ring(2048) && ring(2048) > ring(256));
}
