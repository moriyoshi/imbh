//! Identifier newtypes (ARCHITECTURE.md §10.4).

/// A 16-byte OTel trace id. Under the `serde` feature it (de)serializes as a 32-char lowercase-hex
/// string (not a byte array) — see the impls below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TraceId(pub [u8; 16]);

impl TraceId {
    /// Build from a byte slice, accepting only exactly 16 bytes.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        <[u8; 16]>::try_from(b).ok().map(TraceId)
    }

    /// Parse 32 lowercase/uppercase hex chars.
    pub fn from_hex(s: &str) -> Option<Self> {
        hex_decode::<16>(s).map(TraceId)
    }

    /// Lowercase hex, 32 chars.
    pub fn to_hex(&self) -> String {
        hex_lower(&self.0)
    }
}

/// An 8-byte OTel span id. Under the `serde` feature it (de)serializes as a 16-char lowercase-hex
/// string (see the impls below).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SpanId(pub [u8; 8]);

impl SpanId {
    /// Build from a byte slice, accepting only exactly 8 bytes.
    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        <[u8; 8]>::try_from(b).ok().map(SpanId)
    }

    /// Parse 16 lowercase/uppercase hex chars.
    pub fn from_hex(s: &str) -> Option<Self> {
        hex_decode::<8>(s).map(SpanId)
    }

    /// Lowercase hex, 16 chars.
    pub fn to_hex(&self) -> String {
        hex_lower(&self.0)
    }
}

fn hex_decode<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = [0u8; N];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = hex_val(bytes[2 * i])?;
        let lo = hex_val(bytes[2 * i + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

fn hex_val(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

/// WAL log-sequence number (durability tracking, ARCHITECTURE.md §7). Monotonic per DB and
/// **always ≥ 1** — 0 is not a valid LSN. The absence of a position ("nothing durable yet" for the
/// watermark, "not yet written" for a queued ingest receipt) is expressed as `Option<Lsn>` = `None`
/// rather than an in-band `0` sentinel, so the never-valid zero is ruled out by the type via
/// `NonZero`. Construct with [`Lsn::new`] (returns `None` on 0) and read with [`Lsn::get`].
pub type Lsn = std::num::NonZero<u64>;

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

/// Trace/span ids serialize as lowercase-hex **strings** (the OTel wire form), not byte arrays, so
/// JSON DTOs read naturally. Deserialization reuses the same length-checked `from_hex` parser, so a
/// malformed id is a serde error rather than a silently-truncated array.
#[cfg(feature = "serde")]
mod serde_hex {
    use std::borrow::Cow;

    use super::{SpanId, TraceId};
    use serde::de::{Error as _, Unexpected};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    impl Serialize for TraceId {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&self.to_hex())
        }
    }

    impl<'de> Deserialize<'de> for TraceId {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            // `Cow` accepts both borrowed and owned strings, so it works on any deserializer.
            let s = Cow::<'de, str>::deserialize(d)?;
            TraceId::from_hex(&s).ok_or_else(|| {
                D::Error::invalid_value(Unexpected::Str(&s), &"32-char hex trace id")
            })
        }
    }

    impl Serialize for SpanId {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&self.to_hex())
        }
    }

    impl<'de> Deserialize<'de> for SpanId {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            let s = Cow::<'de, str>::deserialize(d)?;
            SpanId::from_hex(&s)
                .ok_or_else(|| D::Error::invalid_value(Unexpected::Str(&s), &"16-char hex span id"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_length() {
        assert_eq!(TraceId([0xab; 16]).to_hex(), "ab".repeat(16));
        assert_eq!(SpanId([0x01; 8]).to_hex(), "01".repeat(8));
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(TraceId::from_bytes(&[0u8; 15]).is_none());
        assert!(TraceId::from_bytes(&[0u8; 16]).is_some());
        assert!(SpanId::from_bytes(&[0u8; 9]).is_none());
    }

    #[cfg(feature = "serde")]
    #[test]
    fn ids_serialize_as_hex_strings() {
        // TraceId/SpanId serialize as quoted lowercase-hex strings, not byte arrays, and round-trip.
        let tid = TraceId([0xab; 16]);
        let json = serde_json::to_string(&tid).unwrap();
        assert_eq!(json, format!("\"{}\"", "ab".repeat(16)));
        assert_eq!(serde_json::from_str::<TraceId>(&json).unwrap(), tid);

        let sid = SpanId([0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]);
        let json = serde_json::to_string(&sid).unwrap();
        assert_eq!(json, "\"0123456789abcdef\"");
        assert_eq!(serde_json::from_str::<SpanId>(&json).unwrap(), sid);

        // Malformed hex (wrong length / non-hex char) is a deserialization error, not a silent value.
        assert!(serde_json::from_str::<TraceId>("\"abc\"").is_err());
        assert!(serde_json::from_str::<SpanId>("\"zz23456789abcdef\"").is_err());
    }
}
