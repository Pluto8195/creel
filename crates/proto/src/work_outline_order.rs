//! Compact base-62 fractional order keys for Work Outline siblings.
//!
//! This is the public-domain `fractional-indexing` construction (rocicorp):
//! a key is an *integer part* whose head character encodes its length
//! (`a..=z` positive, `A..=Z` negative) followed by an optional *fractional
//! part* of base-62 digits that never ends in `0`. The alphabet `0-9A-Za-z`
//! is in ASCII order, so plain byte comparison of two keys orders them.
//!
//! Rules fixed by the registry audit (`docs/plans/work-outline/m0-registry-persistence.md`
//! §9, `m0-contract.md` §3.3):
//! - keys are 1..=[`MAX_ORDER_KEY_LEN`] bytes and sort by byte order; siblings
//!   sort `(orderKey, id)`, so equal keys minted by two devices are fine;
//! - the first key is `a0`; sequential appends bump the integer part, so
//!   10^4 appends stay under 5 characters;
//! - no nonce and no jitter;
//! - overflowing the length cap is an error ([`OrderKeyError::TooLong`]); the
//!   caller (not this module) rebalances the sibling group with
//!   `order_keys_between(None, None, n)`;
//! - readers treat an invalid stored key as "sort last by id" plus a repair
//!   marker (see `work_outline::WorkOutlineRepair::BadOrderKey`).
use std::fmt;

/// Longest permitted key, in bytes (keys are ASCII, so also characters).
pub const MAX_ORDER_KEY_LEN: usize = 64;

const DIGITS: &[u8; 62] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
/// `A` followed by 26 zeroes: the smallest integer part, which has no
/// predecessor and so is never a valid key by itself.
const SMALLEST_INTEGER: &str = "A00000000000000000000000000";

/// Why a key could not be produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKeyError {
    /// A bound is not a [`valid_order_key`].
    InvalidKey,
    /// `lower >= upper`: no key lies strictly between.
    OutOfOrder,
    /// The result would exceed [`MAX_ORDER_KEY_LEN`]; rebalance the group.
    TooLong,
    /// The integer range is exhausted (practically unreachable).
    Exhausted,
}

impl fmt::Display for OrderKeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidKey => "invalid work outline order key",
            Self::OutOfOrder => "order key bounds are not strictly increasing",
            Self::TooLong => "work outline order key is too long",
            Self::Exhausted => "work outline order key range is exhausted",
        })
    }
}

impl std::error::Error for OrderKeyError {}

fn digit_value(byte: u8) -> Option<usize> {
    DIGITS.iter().position(|d| *d == byte)
}

/// Length of the integer part announced by its head character.
fn integer_len(head: u8) -> Option<usize> {
    match head {
        b'a'..=b'z' => Some((head - b'a') as usize + 2),
        b'A'..=b'Z' => Some((b'Z' - head) as usize + 2),
        _ => None,
    }
}

/// Split a key into `(integer part, fractional part)`; `None` if the head is
/// not a letter or the key is shorter than its integer part.
fn split(key: &str) -> Option<(&str, &str)> {
    let len = integer_len(*key.as_bytes().first()?)?;
    if len > key.len() {
        return None;
    }
    Some(key.split_at(len))
}

/// A key a writer may store: 1..=64 base-62 ASCII characters, a well-formed
/// integer part, a fractional part that does not end in `0`, and not the
/// minimum integer sentinel.
pub fn valid_order_key(key: &str) -> bool {
    if key.is_empty() || key.len() > MAX_ORDER_KEY_LEN || key == SMALLEST_INTEGER {
        return false;
    }
    if !key.bytes().all(|b| digit_value(b).is_some()) {
        return false;
    }
    match split(key) {
        Some((_, fraction)) => !fraction.ends_with('0'),
        None => false,
    }
}

/// The digit string midway between `a` and `b` (`None` = unbounded above).
/// Both are fractional parts without a trailing zero and `a < b`.
fn midpoint(a: &[u8], b: Option<&[u8]>) -> String {
    if let Some(b) = b {
        // Strip the longest common prefix, padding `a` with zeroes as we go.
        let mut n = 0;
        while b
            .get(n)
            .is_some_and(|bd| *bd == a.get(n).copied().unwrap_or(DIGITS[0]))
        {
            n += 1;
        }
        if n > 0 {
            let rest = midpoint(a.get(n..).unwrap_or(&[]), Some(&b[n..]));
            return format!("{}{rest}", String::from_utf8_lossy(&b[..n]));
        }
    }
    let digit_a = a.first().and_then(|d| digit_value(*d)).unwrap_or(0);
    let digit_b = match b {
        Some(b) => digit_value(b[0]).unwrap_or(DIGITS.len()),
        None => DIGITS.len(),
    };
    if digit_b - digit_a > 1 {
        let mid = (digit_a + digit_b).div_ceil(2);
        return (DIGITS[mid] as char).to_string();
    }
    // The first digits are consecutive.
    if let Some(b) = b.filter(|b| b.len() > 1) {
        return (b[0] as char).to_string();
    }
    let rest = midpoint(a.get(1..).unwrap_or(&[]), None);
    format!("{}{rest}", DIGITS[digit_a] as char)
}

fn increment_integer(int: &str) -> Option<String> {
    let bytes = int.as_bytes();
    let head = bytes[0];
    let mut digits: Vec<u8> = bytes[1..].to_vec();
    let mut carry = true;
    for d in digits.iter_mut().rev() {
        let next = digit_value(*d)? + 1;
        if next == DIGITS.len() {
            *d = DIGITS[0];
        } else {
            *d = DIGITS[next];
            carry = false;
            break;
        }
    }
    if carry {
        if head == b'Z' {
            return Some(format!("a{}", DIGITS[0] as char));
        }
        if head == b'z' {
            return None;
        }
        let head = head + 1;
        if head > b'a' {
            digits.push(DIGITS[0]);
        } else {
            digits.pop();
        }
        digits.insert(0, head);
    } else {
        digits.insert(0, head);
    }
    String::from_utf8(digits).ok()
}

fn decrement_integer(int: &str) -> Option<String> {
    let bytes = int.as_bytes();
    let head = bytes[0];
    let mut digits: Vec<u8> = bytes[1..].to_vec();
    let mut borrow = true;
    for d in digits.iter_mut().rev() {
        let value = digit_value(*d)?;
        if value == 0 {
            *d = DIGITS[DIGITS.len() - 1];
        } else {
            *d = DIGITS[value - 1];
            borrow = false;
            break;
        }
    }
    if borrow {
        if head == b'a' {
            return Some(format!("Z{}", DIGITS[DIGITS.len() - 1] as char));
        }
        if head == b'A' {
            return None;
        }
        let head = head - 1;
        if head < b'Z' {
            digits.push(DIGITS[DIGITS.len() - 1]);
        } else {
            digits.pop();
        }
        digits.insert(0, head);
    } else {
        digits.insert(0, head);
    }
    String::from_utf8(digits).ok()
}

fn split_checked(key: &str) -> Result<(&str, &str), OrderKeyError> {
    split(key).ok_or(OrderKeyError::InvalidKey)
}

fn checked(key: &str) -> Result<&str, OrderKeyError> {
    if valid_order_key(key) {
        Ok(key)
    } else {
        Err(OrderKeyError::InvalidKey)
    }
}

fn between_unbounded(lower: Option<&str>, upper: Option<&str>) -> Result<String, OrderKeyError> {
    let lower = lower.map(checked).transpose()?;
    let upper = upper.map(checked).transpose()?;
    if lower.zip(upper).is_some_and(|(a, b)| a >= b) {
        return Err(OrderKeyError::OutOfOrder);
    }
    match (lower, upper) {
        (None, None) => Ok(format!("a{}", DIGITS[0] as char)),
        (None, Some(b)) => {
            let (ib, fb) = split_checked(b)?;
            if ib == SMALLEST_INTEGER {
                return Ok(format!("{ib}{}", midpoint(b"", Some(fb.as_bytes()))));
            }
            if ib < b {
                return Ok(ib.to_owned());
            }
            decrement_integer(ib).ok_or(OrderKeyError::Exhausted)
        }
        (Some(a), None) => {
            let (ia, fa) = split_checked(a)?;
            Ok(match increment_integer(ia) {
                Some(next) => next,
                None => format!("{ia}{}", midpoint(fa.as_bytes(), None)),
            })
        }
        (Some(a), Some(b)) => {
            let (ia, fa) = split_checked(a)?;
            let (ib, fb) = split_checked(b)?;
            if ia == ib {
                return Ok(format!(
                    "{ia}{}",
                    midpoint(fa.as_bytes(), Some(fb.as_bytes()))
                ));
            }
            let next = increment_integer(ia).ok_or(OrderKeyError::Exhausted)?;
            if next.as_str() < b {
                Ok(next)
            } else {
                Ok(format!("{ia}{}", midpoint(fa.as_bytes(), None)))
            }
        }
    }
}

/// A key strictly between `lower` and `upper` (`None` = open end). With both
/// ends open the key is `a0`.
pub fn order_key_between(
    lower: Option<&str>,
    upper: Option<&str>,
) -> Result<String, OrderKeyError> {
    let key = between_unbounded(lower, upper)?;
    if key.len() > MAX_ORDER_KEY_LEN {
        return Err(OrderKeyError::TooLong);
    }
    Ok(key)
}

/// `n` strictly increasing keys, all strictly between the bounds, evenly
/// spread (bisected) when both are given so rebalancing and multi-insert do
/// not degrade into one long chain.
pub fn order_keys_between(
    lower: Option<&str>,
    upper: Option<&str>,
    n: usize,
) -> Result<Vec<String>, OrderKeyError> {
    match n {
        0 => return Ok(Vec::new()),
        1 => return Ok(vec![order_key_between(lower, upper)?]),
        _ => {}
    }
    match (lower, upper) {
        (_, None) => {
            let mut keys = Vec::with_capacity(n);
            let mut prev = order_key_between(lower, None)?;
            keys.push(prev.clone());
            for _ in 1..n {
                prev = order_key_between(Some(&prev), None)?;
                keys.push(prev.clone());
            }
            Ok(keys)
        }
        (None, Some(_)) => {
            let mut keys = Vec::with_capacity(n);
            let mut next = order_key_between(None, upper)?;
            keys.push(next.clone());
            for _ in 1..n {
                next = order_key_between(None, Some(&next))?;
                keys.push(next.clone());
            }
            keys.reverse();
            Ok(keys)
        }
        (Some(_), Some(_)) => {
            let mid = n / 2;
            let c = order_key_between(lower, upper)?;
            let mut keys = order_keys_between(lower, Some(&c), mid)?;
            keys.push(c.clone());
            keys.extend(order_keys_between(Some(&c), upper, n - mid - 1)?);
            Ok(keys)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tiny deterministic PRNG so the property tests need no dependency.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    fn assert_strictly_between(lower: Option<&str>, upper: Option<&str>, key: &str) {
        assert!(valid_order_key(key), "{key} is not valid");
        if let Some(lower) = lower {
            assert!(lower < key, "{lower} !< {key}");
        }
        if let Some(upper) = upper {
            assert!(key < upper, "{key} !< {upper}");
        }
    }

    #[test]
    fn first_key_is_a0_and_valid() {
        let key = order_key_between(None, None).unwrap();
        assert_eq!(key, "a0");
        assert!(valid_order_key(&key));
    }

    #[test]
    fn known_vectors_match_fractional_indexing() {
        let k = |a, b| order_key_between(a, b).unwrap();
        assert_eq!(k(None, Some("a0")), "Zz");
        assert_eq!(k(Some("a0"), None), "a1");
        assert_eq!(k(Some("a0"), Some("a1")), "a0V");
        assert_eq!(k(Some("a0V"), Some("a1")), "a0l");
        assert_eq!(k(Some("Zz"), Some("a0")), "ZzV");
        assert_eq!(k(Some("a0"), Some("a0V")), "a0G");
        assert_eq!(k(None, Some("Y00")), "Xzzz");
        assert_eq!(k(Some("bzz"), None), "c000");
        assert_eq!(k(Some("a0"), Some("a0V")), "a0G");
        assert_eq!(
            k(Some("zzzzzzzzzzzzzzzzzzzzzzzzzzy"), None),
            "zzzzzzzzzzzzzzzzzzzzzzzzzzz"
        );
    }

    #[test]
    fn invalid_keys_are_rejected() {
        let long = format!("a0{}", "1".repeat(MAX_ORDER_KEY_LEN));
        for key in [
            "",    // empty
            "0",   // head is not a letter
            "a",   // shorter than its integer part
            "a00", // fraction ends in 0
            "a0!", // not base62
            "a0 ", // whitespace
            "é0",  // non-ASCII
            "b0",  // integer part needs 3 characters
            SMALLEST_INTEGER,
            long.as_str(), // 66 characters
        ] {
            assert!(!valid_order_key(key), "{key:?} should be invalid");
            assert_eq!(
                order_key_between(Some(key), None),
                Err(OrderKeyError::InvalidKey),
                "{key:?} as lower bound"
            );
            assert_eq!(
                order_key_between(None, Some(key)),
                Err(OrderKeyError::InvalidKey),
                "{key:?} as upper bound"
            );
        }
        assert!(valid_order_key("a0"));
        assert!(valid_order_key("a0V"));
        assert!(valid_order_key(&format!(
            "a0{}",
            "1".repeat(MAX_ORDER_KEY_LEN - 2)
        )));
    }

    #[test]
    fn unordered_bounds_are_rejected() {
        assert_eq!(
            order_key_between(Some("a1"), Some("a0")),
            Err(OrderKeyError::OutOfOrder)
        );
        assert_eq!(
            order_key_between(Some("a1"), Some("a1")),
            Err(OrderKeyError::OutOfOrder)
        );
        assert_eq!(
            order_keys_between(Some("a1"), Some("a0"), 3),
            Err(OrderKeyError::OutOfOrder)
        );
    }

    #[test]
    fn between_is_strict_for_every_pair_in_a_dense_set() {
        let mut keys = order_keys_between(None, None, 40).unwrap();
        // Densify with a few rounds of midpoints so fractions of varying
        // length are exercised.
        for _ in 0..3 {
            let mut denser = Vec::new();
            for pair in keys.windows(2) {
                denser.push(pair[0].clone());
                denser.push(order_key_between(Some(&pair[0]), Some(&pair[1])).unwrap());
            }
            denser.push(keys.last().unwrap().clone());
            keys = denser;
        }
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        for (i, a) in keys.iter().enumerate().step_by(7) {
            for b in keys.iter().skip(i + 1).step_by(5) {
                let mid = order_key_between(Some(a), Some(b)).unwrap();
                assert_strictly_between(Some(a), Some(b), &mid);
            }
            assert_strictly_between(Some(a), None, &order_key_between(Some(a), None).unwrap());
            assert_strictly_between(None, Some(a), &order_key_between(None, Some(a)).unwrap());
        }
    }

    #[test]
    fn n_keys_are_strictly_monotone_for_every_bound_shape() {
        let shapes: [(Option<&str>, Option<&str>); 5] = [
            (None, None),
            (Some("a0"), None),
            (None, Some("a0")),
            (Some("a0"), Some("a1")),
            (Some("a0V"), Some("a0W")),
        ];
        for (lower, upper) in shapes {
            for n in [0usize, 1, 2, 3, 7, 62, 100, 500] {
                let keys = order_keys_between(lower, upper, n).unwrap();
                assert_eq!(keys.len(), n, "{lower:?} {upper:?} n={n}");
                for key in &keys {
                    assert_strictly_between(lower, upper, key);
                }
                assert!(
                    keys.windows(2).all(|w| w[0] < w[1]),
                    "{lower:?} {upper:?} n={n} not monotone"
                );
            }
        }
    }

    #[test]
    fn a_bisected_batch_between_close_keys_stays_short() {
        let keys = order_keys_between(Some("a0"), Some("a1"), 2000).unwrap();
        assert_eq!(keys.len(), 2000);
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        assert!(
            keys.iter().all(|k| k.len() <= 6),
            "bisection should stay compact"
        );
    }

    #[test]
    fn rebalance_of_a_full_sibling_group_is_distinct_and_compact() {
        let keys = order_keys_between(None, None, 2000).unwrap();
        assert_eq!(keys.len(), 2000);
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        assert!(keys.iter().all(|k| valid_order_key(k) && k.len() <= 4));
    }

    #[test]
    fn ten_thousand_sequential_appends_stay_under_eight_characters() {
        let mut last = order_key_between(None, None).unwrap();
        let mut longest = last.len();
        for _ in 0..10_000 {
            let next = order_key_between(Some(&last), None).unwrap();
            assert!(last < next);
            longest = longest.max(next.len());
            last = next;
        }
        assert!(longest < 8, "longest key was {longest} characters");
    }

    #[test]
    fn ten_thousand_sequential_prepends_stay_under_eight_characters() {
        let mut first = order_key_between(None, None).unwrap();
        let mut longest = first.len();
        for _ in 0..10_000 {
            let prev = order_key_between(None, Some(&first)).unwrap();
            assert!(prev < first);
            longest = longest.max(prev.len());
            first = prev;
        }
        assert!(longest < 8, "longest key was {longest} characters");
    }

    #[test]
    fn repeated_insert_at_one_spot_grows_slowly_and_then_errors_cleanly() {
        // Always insert immediately before the same upper key: the worst
        // case for fractional keys. Growth is about one character per six
        // inserts, and the 64-character cap surfaces as `TooLong`.
        let upper = "a1";
        let mut lower = "a0".to_owned();
        let mut inserts = 0usize;
        let err = loop {
            match order_key_between(Some(&lower), Some(upper)) {
                Ok(key) => {
                    assert_strictly_between(Some(&lower), Some(upper), &key);
                    assert!(key.len() <= MAX_ORDER_KEY_LEN);
                    lower = key;
                    inserts += 1;
                    assert!(inserts < 10_000, "cap never reached");
                }
                Err(error) => break error,
            }
        };
        assert_eq!(err, OrderKeyError::TooLong);
        assert!(inserts > 100, "only {inserts} inserts before the cap");
    }

    #[test]
    fn random_inserts_preserve_order() {
        let mut rng = Lcg(0x5EED);
        let mut keys: Vec<String> = vec![order_key_between(None, None).unwrap()];
        for _ in 0..3_000 {
            let slot = rng.below(keys.len() + 1);
            let lower = slot.checked_sub(1).map(|i| keys[i].as_str());
            let upper = keys.get(slot).map(String::as_str);
            match order_key_between(lower, upper) {
                Ok(key) => {
                    assert_strictly_between(lower, upper, &key);
                    keys.insert(slot, key);
                }
                // A pathological run may hit the cap; the caller rebalances.
                Err(OrderKeyError::TooLong) => {
                    keys = order_keys_between(None, None, keys.len()).unwrap();
                }
                Err(other) => panic!("unexpected {other:?}"),
            }
            if keys.len() % 500 == 0 {
                assert!(keys.windows(2).all(|w| w[0] < w[1]));
            }
        }
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        assert!(keys.iter().all(|k| valid_order_key(k)));
    }

    #[test]
    fn random_batch_inserts_preserve_order() {
        let mut rng = Lcg(42);
        let mut keys: Vec<String> = Vec::new();
        for _ in 0..200 {
            let slot = rng.below(keys.len() + 1);
            let count = 1 + rng.below(5);
            let lower = slot.checked_sub(1).map(|i| keys[i].clone());
            let upper = keys.get(slot).cloned();
            let batch = order_keys_between(lower.as_deref(), upper.as_deref(), count).unwrap();
            for (offset, key) in batch.into_iter().enumerate() {
                keys.insert(slot + offset, key);
            }
        }
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn ties_sort_deterministically_by_id() {
        // Two devices minting between the same neighbors may produce equal
        // keys; `(orderKey, id)` is a total order, so both orders agree.
        let a = order_key_between(Some("a0"), Some("a1")).unwrap();
        let b = order_key_between(Some("a0"), Some("a1")).unwrap();
        assert_eq!(a, b);
        let mut one = vec![(a.clone(), "w-2"), (b.clone(), "w-1")];
        let mut two = vec![(b, "w-1"), (a, "w-2")];
        one.sort();
        two.sort();
        assert_eq!(one, two);
    }
}
