//! Weighted rendezvous hashing over the selectable servers of a pool.
//!
//! A server's score for a key is `-ln(u) / weight`, where `u` is a hash of the key and the server
//! in `(0, 1)`; the lowest score wins. Servers share no state, so a key moves only when its own
//! server leaves, and the share of keys is proportional to the weight. The server side of the hash
//! is computed once per view, so a request hashes its key once and mixes it with each server's hash.

use super::selectable::SelectableEntry;

const NONE: u32 = u32::MAX;
const WEIGHTS: usize = 101;

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The splitmix64 finalizer: every input bit affects every output bit.
fn mix(mut bits: u64) -> u64 {
    bits ^= bits >> 30;
    bits = bits.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    bits ^= bits >> 27;
    bits = bits.wrapping_mul(0x94d0_49bb_1331_11eb);
    bits ^ (bits >> 31)
}

/// The stable hash of a server name; the same on every worker and across reloads.
pub(super) fn name_hash(name: &str) -> u64 {
    mix(fnv1a(name.as_bytes()))
}

/// `-ln(u) / weight` for `u` taken from the top 53 bits of `score`, which an `f64` holds exactly.
#[allow(clippy::cast_precision_loss)]
fn cost(weight: u16, score: u64) -> f64 {
    let unit = ((score >> 11) as f64 + 0.5) / 9_007_199_254_740_992.0;
    -unit.ln() / f64::from(weight)
}

/// The membership position of the server that owns `key`.
///
/// Within one weight the cost falls as the score rises, so only the highest score of each weight
/// needs a logarithm; at most 100 are taken however many servers there are.
pub(super) fn winner(entries: &[SelectableEntry], key: &[u8]) -> Option<u32> {
    let key = mix(fnv1a(key));
    let mut best = [(0_u64, NONE); WEIGHTS];
    for entry in entries {
        let score = mix(key ^ entry.hash);
        let slot = &mut best[usize::from(entry.weight)];
        if slot.1 == NONE || score > slot.0 {
            *slot = (score, entry.index);
        }
    }
    (0_u16..)
        .zip(&best)
        .filter(|(_, (_, index))| *index != NONE)
        .min_by(|(a, (a_score, _)), (b, (b_score, _))| {
            cost(*a, *a_score).total_cmp(&cost(*b, *b_score))
        })
        .map(|(_, (_, index))| *index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn farm(weights: &[u16]) -> Vec<SelectableEntry> {
        weights
            .iter()
            .enumerate()
            .map(|(index, weight)| SelectableEntry {
                index: u32::try_from(index).expect("small farm"),
                weight: *weight,
                hash: name_hash(&format!("s{index}")),
            })
            .collect()
    }

    fn owners(servers: &[SelectableEntry]) -> Vec<u32> {
        (0..1000)
            .map(|key| winner(servers, format!("key-{key}").as_bytes()).expect("server"))
            .collect()
    }

    #[test]
    fn a_key_always_maps_to_the_same_server() {
        let servers = farm(&[1, 1, 1]);
        assert_eq!(owners(&servers), owners(&servers));
    }

    #[test]
    fn removing_a_server_remaps_only_its_own_keys() {
        for weights in [&[1_u16, 1, 1, 1][..], &[3, 1, 2, 1, 3, 2]] {
            let full = farm(weights);
            let before = owners(&full);
            let after = owners(&full[..full.len() - 1]);
            let removed = u32::try_from(full.len() - 1).expect("small farm");
            for (old, new) in before.iter().zip(&after) {
                assert!(*old == *new || *old == removed, "{old} moved to {new}");
            }
            assert!(before.contains(&removed));
        }
    }

    #[test]
    fn weights_scale_the_share_of_keys() {
        let counts = owners(&farm(&[3, 1]))
            .iter()
            .fold([0_u32; 2], |mut counts, owner| {
                counts[*owner as usize] += 1;
                counts
            });
        assert!(
            (650..850).contains(&counts[0]),
            "heavy server got {}",
            counts[0]
        );
    }

    #[test]
    fn the_per_weight_maximum_is_the_lowest_cost_of_every_server() {
        let weights = (0..300_u16)
            .map(|index| 1 + index * 7 % 100)
            .collect::<Vec<_>>();
        let servers = farm(&weights);
        for key in 0..300 {
            let key = format!("session-{key}");
            let hashed = mix(fnv1a(key.as_bytes()));
            let expected = servers
                .iter()
                .min_by(|a, b| {
                    cost(a.weight, mix(hashed ^ a.hash))
                        .total_cmp(&cost(b.weight, mix(hashed ^ b.hash)))
                })
                .map(|entry| entry.index);
            assert_eq!(winner(&servers, key.as_bytes()), expected);
        }
    }

    #[test]
    fn no_servers_own_no_keys() {
        assert_eq!(winner(&[], b"key"), None);
    }
}
