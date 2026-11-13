//! Bounded Unicode transactions and explicitly selected US-layout scalar fallback.

/// `(UTF-16 unit, down)` events for one Unicode scalar, submitted as one native batch.
pub fn unicode_events(ch: char) -> Vec<(u16, bool)> {
    let mut buffer = [0; 2];
    let units = ch.encode_utf16(&mut buffer);
    units
        .iter()
        .map(|&u| (u, true))
        .chain(units.iter().map(|&u| (u, false)))
        .collect()
}

/// Key-up-only recovery for the successfully inserted prefix. Never replay characters.
pub fn pending_releases(events: &[(u16, bool)], inserted: usize) -> Vec<(u16, bool)> {
    let mut pending = Vec::new();
    for &(unit, down) in events.iter().take(inserted) {
        if down {
            pending.push((unit, false));
        } else if let Some(index) = pending.iter().position(|&(u, _)| u == unit) {
            pending.remove(index);
        }
    }
    pending
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spaces_punctuation_and_croatian_letters_remain_unicode() {
        for (ch, unit) in [
            (' ', 0x20),
            ('+', 0x2b),
            ('@', 0x40),
            ('č', 0x10d),
            ('Ł', 0x141),
        ] {
            assert_eq!(unicode_events(ch), [(unit, true), (unit, false)]);
        }
    }

    #[test]
    fn supplementary_scalar_keeps_both_utf16_units_in_one_batch() {
        assert_eq!(
            unicode_events('😀'),
            [
                (0xd83d, true),
                (0xde00, true),
                (0xd83d, false),
                (0xde00, false)
            ]
        );
    }

    #[test]
    fn partial_insertion_recovers_only_unreleased_downs_without_replaying_text() {
        let events = [
            (0xd83d, true),
            (0xde00, true),
            (0xd83d, false),
            (0xde00, false),
        ];
        assert_eq!(pending_releases(&events, 0), []);
        assert_eq!(pending_releases(&events, 1), [(0xd83d, false)]);
        assert_eq!(
            pending_releases(&events, 2),
            [(0xd83d, false), (0xde00, false)]
        );
        assert_eq!(pending_releases(&events, 3), [(0xde00, false)]);
        assert_eq!(pending_releases(&events, 4), []);
    }
}

/// Completion is native submission evidence, never application consumption. Unsupported
/// means no input was attempted and is the ONLY result permitting another adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Completion {
    Unsupported,
    Completed,
    Partial,
    Failed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packet<K> {
    Key(K, bool),
    Unicode(u16, bool),
    Mask(bool),
}

/// Invalid counts and backends without prefix evidence are uncertain, never retryable.
pub fn count_completion(sent: Option<usize>, expected: usize) -> Completion {
    match sent {
        Some(n) if n == expected => Completion::Completed,
        Some(0) => Completion::Failed,
        Some(n) if n < expected => Completion::Partial,
        _ => Completion::Unknown,
    }
}

pub fn transient<K: Copy>(key: K, held: bool) -> Vec<Packet<K>> {
    let mut events = vec![Packet::Key(key, true)];
    if !held {
        events.push(Packet::Key(key, false));
    }
    events
}

pub fn scalar_packets<K>(ch: char) -> Vec<Packet<K>> {
    unicode_events(ch)
        .into_iter()
        .map(|(u, down)| Packet::Unicode(u, down))
        .collect()
}

/// One atomic scalar transaction, serialized with every other native step. `modifiers`
/// is the current aggregate at THIS step, never a snapshot retained across text.
/// A backend returns an exact accepted prefix, or None for uncertain submission.
/// The only loops are over <=8 modifiers and <=4 scalar packets (fallback <=4).
/// Failed cleanup/restoration requires route invalidation by the future worker.
pub fn transaction<K: Copy + Eq>(
    modifiers: &[K],
    mask: bool,
    payload: &[Packet<K>],
    protected: &[K],
    mut send: impl FnMut(&[Packet<K>]) -> Option<usize>,
) -> Completion {
    use Packet::*;
    let mask_packet = [Mask(true), Mask(false)];
    if mask
        && !matches!(
            checked_payload(&mask_packet, &[], &mut send),
            Completion::Completed
        )
    {
        return Completion::Unknown;
    }
    let lifted: Vec<_> = modifiers.iter().map(|&k| Key(k, false)).collect();
    if !lifted.is_empty() {
        let count = send(&lifted);
        if count != Some(lifted.len()) {
            // Known prefix: restore only what was lifted. Unknown: best effort all.
            let n = count
                .filter(|&n| n <= modifiers.len())
                .unwrap_or(modifiers.len());
            let restore: Vec<_> = modifiers[..n].iter().map(|&k| Key(k, true)).collect();
            if !restore.is_empty() {
                send(&restore);
            }
            if mask {
                checked_payload(&mask_packet, &[], &mut send);
            }
            return Completion::Unknown;
        }
    }
    let result = checked_payload(payload, protected, &mut send);
    let restore: Vec<_> = modifiers.iter().map(|&k| Key(k, true)).collect();
    let restored = restore.is_empty() || send(&restore) == Some(restore.len());
    let masked = !mask || checked_payload(&mask_packet, &[], &mut send) == Completion::Completed;
    if restored && masked {
        result
    } else {
        Completion::Unknown
    }
}

fn checked_payload<K: Copy + Eq>(
    payload: &[Packet<K>],
    protected: &[K],
    send: &mut impl FnMut(&[Packet<K>]) -> Option<usize>,
) -> Completion {
    use Packet::*;
    let count = send(payload);
    let result = count_completion(count, payload.len());
    if result == Completion::Completed {
        return result;
    }
    let Some(n) = count.filter(|&n| n <= payload.len()) else {
        // No prefix evidence. Release only transient downs, never reinsert text.
        let downs: Vec<_> = payload
            .iter()
            .copied()
            .filter(|p| matches!(p, Key(_, true) | Unicode(_, true) | Mask(true)))
            .collect();
        let cleanup = pending_packets(&downs, protected);
        if !cleanup.is_empty() {
            send(&cleanup);
        }
        return Completion::Unknown;
    };
    let cleanup = pending_packets(&payload[..n], protected);
    if !cleanup.is_empty() && send(&cleanup) != Some(cleanup.len()) {
        Completion::Unknown
    } else {
        result
    }
}

fn pending_packets<K: Copy + Eq>(prefix: &[Packet<K>], protected: &[K]) -> Vec<Packet<K>> {
    use Packet::*;
    let mut pending = Vec::new();
    for &event in prefix {
        let (up, down) = match event {
            Key(k, _) if protected.contains(&k) => continue,
            Key(k, d) => (Key(k, false), d),
            Unicode(u, d) => (Unicode(u, false), d),
            Mask(d) => (Mask(false), d),
        };
        if down {
            pending.push(up);
        } else if let Some(i) = pending.iter().position(|&p| p == up) {
            pending.remove(i);
        }
    }
    pending
}

#[cfg(test)]
mod step_tests {
    use super::*;
    #[test]
    fn native_counts_do_not_turn_failure_or_unknown_into_success() {
        for (sent, expected, result) in [
            (Some(0), 1, Completion::Failed),
            (Some(1), 1, Completion::Completed),
            (Some(1), 2, Completion::Partial),
            (None, 1, Completion::Unknown),
            (Some(3), 2, Completion::Unknown),
        ] {
            assert_eq!(count_completion(sent, expected), result);
        }
    }
    #[test]
    fn every_surrogate_prefix_is_cleaned_without_replay() {
        let payload = scalar_packets::<u16>('😀');
        for (prefix, cleanup) in [
            (0, vec![]),
            (1, vec![Packet::Unicode(0xd83d, false)]),
            (
                2,
                vec![
                    Packet::Unicode(0xd83d, false),
                    Packet::Unicode(0xde00, false),
                ],
            ),
            (3, vec![Packet::Unicode(0xde00, false)]),
            (4, vec![]),
        ] {
            let mut calls = Vec::new();
            let result = transaction(&[], false, &payload, &[], |p| {
                calls.push(p.to_vec());
                Some(if calls.len() == 1 { prefix } else { p.len() })
            });
            assert_eq!(
                calls[0],
                [
                    Packet::Unicode(0xd83d, true),
                    Packet::Unicode(0xde00, true),
                    Packet::Unicode(0xd83d, false),
                    Packet::Unicode(0xde00, false)
                ]
            );
            assert_eq!(calls.get(1).cloned().unwrap_or_default(), cleanup);
            assert_eq!(calls.len(), if cleanup.is_empty() { 1 } else { 2 });
            assert_eq!(
                result,
                if prefix == 4 {
                    Completion::Completed
                } else if prefix == 0 {
                    Completion::Failed
                } else {
                    Completion::Partial
                }
            );
        }
    }
    #[test]
    fn newline_preserves_held_enter_even_during_cleanup() {
        for held in [true, false] {
            let payload = transient(0x24, held);
            assert_eq!(
                payload,
                if held {
                    vec![Packet::Key(0x24, true)]
                } else {
                    vec![Packet::Key(0x24, true), Packet::Key(0x24, false)]
                }
            );
            let mut calls = Vec::new();
            transaction(
                &[],
                false,
                &payload,
                if held { &[0x24] } else { &[] },
                |p| {
                    calls.push(p.to_vec());
                    Some(1)
                },
            );
            assert_eq!(calls.len(), if held { 1 } else { 2 });
            if !held {
                assert_eq!(calls[1], [Packet::Key(0x24, false)]);
            }
        }
    }
    #[test]
    fn failed_cleanup_is_unknown_not_completed_or_safe_to_replay() {
        let mut call = 0;
        assert_eq!(
            transaction::<u16>(&[], false, &scalar_packets('😀'), &[], |_| {
                call += 1;
                Some(if call == 1 { 2 } else { 0 })
            }),
            Completion::Unknown
        );
    }
    #[test]
    fn each_step_uses_current_aggregate_and_masks_menu_modifiers() {
        let mut calls = Vec::new();
        for held in [&[0x38, 0x37][..], &[0x37][..], &[][..]] {
            assert_eq!(
                transaction(held, held.contains(&0x37), &scalar_packets('x'), &[], |p| {
                    calls.push(p.to_vec());
                    Some(p.len())
                }),
                Completion::Completed
            );
        }
        assert_eq!(calls[0], [Packet::Mask(true), Packet::Mask(false)]);
        assert_eq!(
            calls[1],
            [Packet::Key(0x38, false), Packet::Key(0x37, false)]
        );
        assert_eq!(calls[3], [Packet::Key(0x38, true), Packet::Key(0x37, true)]);
        assert_eq!(calls[4], [Packet::Mask(true), Packet::Mask(false)]);
        assert_eq!(calls[6], [Packet::Key(0x37, false)]);
        assert_eq!(calls[8], [Packet::Key(0x37, true)]);
        assert_eq!(calls.len(), 11); // last step is just Unicode, no stale restore
    }
    #[test]
    fn failed_modifier_suspension_restores_only_inserted_prefix_and_skips_text() {
        let mut calls = Vec::new();
        let result = transaction(&[1, 2], false, &scalar_packets('x'), &[], |p| {
            calls.push(p.to_vec());
            Some(1)
        });
        assert_eq!(
            calls,
            [
                vec![Packet::Key(1, false), Packet::Key(2, false)],
                vec![Packet::Key(1, true)]
            ]
        );
        assert_eq!(result, Completion::Unknown);
    }
}

/// US-layout fallback, chosen BEFORE any Unicode attempt. All modifiers have been
/// suspended by transaction; the optional Shift here has transient ownership.
pub fn fallback_packets(
    ch: char,
    held: impl Fn(u16) -> bool,
) -> Option<(Vec<Packet<u16>>, Vec<u16>)> {
    let (key, shift) = crate::keys::char_key(ch)?;
    let owned = held(key);
    let mut events = Vec::new();
    if shift {
        events.push(Packet::Key(crate::keys::SHIFT, true));
    }
    events.extend(transient(key, owned));
    if shift {
        events.push(Packet::Key(crate::keys::SHIFT, false));
    }
    Some((events, if owned { vec![key] } else { vec![] }))
}
/// Legacy Mac chunking; the new executor uses exactly one scalar instead.
pub fn unicode_chunks(text: &str) -> Vec<Vec<u16>> {
    let mut chunks = Vec::new();
    let mut chunk = Vec::new();
    for ch in text.chars() {
        if chunk.len() + ch.len_utf16() > 16 {
            chunks.push(std::mem::take(&mut chunk));
        }
        chunk.extend_from_slice(ch.encode_utf16(&mut [0; 2]));
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }
    chunks
}

#[cfg(test)]
mod boundary_tests {
    use super::*;
    #[test]
    fn fallback_owns_only_transient_keys() {
        let (p, protected) = fallback_packets('A', |k| k == 0).unwrap();
        assert_eq!(
            p,
            [
                Packet::Key(0x38, true),
                Packet::Key(0, true),
                Packet::Key(0x38, false)
            ]
        );
        assert_eq!(protected, [0]);
        let (p, protected) = fallback_packets('a', |_| false).unwrap();
        assert_eq!(p, [Packet::Key(0, true), Packet::Key(0, false)]);
        assert!(protected.is_empty());
        assert!(fallback_packets('č', |_| false).is_none());
    }
    #[test]
    fn mac_chunks_preserve_scalar_boundaries_and_content() {
        for text in ["", "😀", "123456789012345😀x", "😀😀😀😀😀😀😀😀😀", "a\nč"]
        {
            let chunks = unicode_chunks(text);
            assert!(chunks.iter().all(|c| c.len() <= 16 && !c.is_empty()));
            let decoded: String = chunks
                .iter()
                .map(|c| String::from_utf16(c).unwrap())
                .collect();
            assert_eq!(decoded, text);
        }
        assert_eq!(
            unicode_chunks("123456789012345😀x")
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            [15, 3]
        );
    }
}

#[cfg(test)]
mod uncertainty_tests {
    use super::*;
    #[test]
    fn unknown_prefix_attempts_only_transient_ups_and_never_replays() {
        let mut calls = Vec::new();
        let result = transaction::<u16>(&[], false, &scalar_packets('😀'), &[], |p| {
            calls.push(p.to_vec());
            if calls.len() == 1 {
                None
            } else {
                Some(p.len())
            }
        });
        assert_eq!(result, Completion::Unknown);
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[1],
            [
                Packet::Unicode(0xd83d, false),
                Packet::Unicode(0xde00, false)
            ]
        );
    }
    #[test]
    fn restoration_failure_and_mask_failure_are_unknown() {
        for failure in [0, 1, 3, 4] {
            let mut i = 0;
            let result = transaction(&[0x3a], true, &scalar_packets('x'), &[], |p| {
                let n = if i == failure { 0 } else { p.len() };
                i += 1;
                Some(n)
            });
            assert_eq!(result, Completion::Unknown);
        }
    }
    #[test]
    fn fallback_unknown_cleanup_does_not_release_a_borrowed_key() {
        let (payload, protected) = fallback_packets('A', |key| key == 0).unwrap();
        let mut calls = Vec::new();
        transaction(&[], false, &payload, &protected, |p| {
            calls.push(p.to_vec());
            None
        });
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1], [Packet::Key(0x38, false)]);
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::*;
    use crate::input_queue::{
        Command, Limits, Operation, Outcome, OwnerClass, Physical, Press, Scheduler,
    };

    #[test]
    fn another_owners_shift_survives_until_final_release_between_scalars() {
        let mut q = Scheduler::new(Limits::default());
        let a = q.open_owner(1, OwnerClass::Viewer).unwrap();
        let b = q.open_owner(2, OwnerClass::Viewer).unwrap();
        let shift = Press {
            physical: Physical::Key(0x38),
            generation: 1,
        };
        let mut held = Vec::new();
        let mut packets = Vec::new();
        let mut run = |q: &mut Scheduler| {
            let step = q.next_step().unwrap();
            match step.operation {
                Operation::Down(Physical::Key(k)) => held.push(k),
                Operation::Up(Physical::Key(k)) => held.retain(|&v| v != k),
                Operation::Scalar(c) => {
                    assert_eq!(
                        transaction(&held, false, &scalar_packets(c), &[], |p| {
                            packets.push(p.to_vec());
                            Some(p.len())
                        }),
                        Completion::Completed
                    );
                }
                Operation::Ownership => (),
                other => panic!("unexpected {other:?}"),
            }
            q.complete(step.token, Outcome::Completed).unwrap();
        };
        q.try_submit(a, Command::Down(shift)).unwrap();
        run(&mut q);
        q.try_submit(b, Command::Down(shift)).unwrap();
        run(&mut q);
        q.try_submit(a, Command::Up(shift)).unwrap();
        run(&mut q);
        q.try_submit(a, Command::Text("xy".into())).unwrap();
        run(&mut q);
        q.try_submit(b, Command::Up(shift)).unwrap();
        run(&mut q);
        run(&mut q);
        assert_eq!(
            packets,
            [
                vec![Packet::Key(0x38, false)],
                vec![Packet::Unicode(0x78, true), Packet::Unicode(0x78, false)],
                vec![Packet::Key(0x38, true)],
                vec![Packet::Unicode(0x79, true), Packet::Unicode(0x79, false)],
            ]
        );
    }
}
