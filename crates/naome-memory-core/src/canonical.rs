use crate::{MemoryError, Result};
use serde::de::{Error as DeError, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::str::FromStr;

/// Produce the authority bytes used by hashes in this crate.
///
/// The format is not JSON text. It is a small typed binary tree with explicit
/// tags, big-endian lengths, and lexicographically sorted object keys. The
/// Serde data model is only used to visit domain values; floating point numbers
/// are rejected.
pub trait CanonicalBytes: Serialize {
    fn canonical_bytes(&self) -> Result<Vec<u8>> {
        canonical_binary(self)
    }
}

impl<T: Serialize> CanonicalBytes for T {}

pub fn canonical_binary<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value)
        .map_err(|error| MemoryError::CanonicalEncoding(error.to_string()))?;
    let mut output = Vec::new();
    encode_value(&value, &mut output)?;
    Ok(output)
}

fn frame(bytes: &[u8], output: &mut Vec<u8>) -> Result<()> {
    let length = u64::try_from(bytes.len())
        .map_err(|_| MemoryError::CanonicalEncoding("field exceeds u64 length".to_owned()))?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}

fn encode_value(value: &Value, output: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => output.push(0),
        Value::Bool(false) => output.push(1),
        Value::Bool(true) => output.push(2),
        Value::Number(number) => {
            if let Some(unsigned) = number.as_u64() {
                output.push(3);
                output.extend_from_slice(&unsigned.to_be_bytes());
            } else if let Some(signed) = number.as_i64() {
                output.push(4);
                output.extend_from_slice(&signed.to_be_bytes());
            } else {
                return Err(MemoryError::CanonicalEncoding(
                    "floating point values are forbidden".to_owned(),
                ));
            }
        }
        Value::String(string) => {
            output.push(5);
            frame(string.as_bytes(), output)?;
        }
        Value::Array(values) => {
            output.push(6);
            let count = u64::try_from(values.len()).map_err(|_| {
                MemoryError::CanonicalEncoding("array exceeds u64 length".to_owned())
            })?;
            output.extend_from_slice(&count.to_be_bytes());
            for value in values {
                let mut encoded = Vec::new();
                encode_value(value, &mut encoded)?;
                frame(&encoded, output)?;
            }
        }
        Value::Object(values) => {
            output.push(7);
            let count = u64::try_from(values.len()).map_err(|_| {
                MemoryError::CanonicalEncoding("object exceeds u64 length".to_owned())
            })?;
            output.extend_from_slice(&count.to_be_bytes());
            let mut entries: Vec<_> = values.iter().collect();
            entries.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
            for (key, value) in entries {
                frame(key.as_bytes(), output)?;
                let mut encoded = Vec::new();
                encode_value(value, &mut encoded)?;
                frame(&encoded, output)?;
            }
        }
    }
    Ok(())
}

/// A SHA-256 digest rendered as lowercase hexadecimal in JSON.
#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Digest32(pub [u8; 32]);

impl Digest32 {
    pub const ZERO: Self = Self([0; 32]);

    pub fn hash_prefixed(prefix: &[u8], payload: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(prefix);
        hasher.update(payload);
        Self(hasher.finalize().into())
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(64);
        for byte in self.0 {
            output.push(char::from(HEX[usize::from(byte >> 4)]));
            output.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        output
    }
}

impl fmt::Display for Digest32 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl fmt::Debug for Digest32 {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Digest32")
            .field(&self.to_hex())
            .finish()
    }
}

impl FromStr for Digest32 {
    type Err = MemoryError;

    fn from_str(value: &str) -> Result<Self> {
        if value.len() != 64 || !value.is_ascii() {
            return Err(MemoryError::InvalidDigest);
        }
        let mut bytes = [0_u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = from_hex(chunk[0]).ok_or(MemoryError::InvalidDigest)?;
            let low = from_hex(chunk[1]).ok_or(MemoryError::InvalidDigest)?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

const fn from_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

impl Serialize for Digest32 {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

struct DigestVisitor;

impl Visitor<'_> for DigestVisitor {
    type Value = Digest32;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a 64-character lowercase hexadecimal SHA-256 digest")
    }

    fn visit_str<E: DeError>(self, value: &str) -> std::result::Result<Self::Value, E> {
        Digest32::from_str(value).map_err(E::custom)
    }
}

impl<'de> Deserialize<'de> for Digest32 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_str(DigestVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::{CanonicalBytes as _, Digest32};
    use std::collections::BTreeMap;
    use std::str::FromStr as _;

    #[test]
    fn digest_hex_round_trip_is_strict() {
        let digest = Digest32([0xab; 32]);
        let encoded = digest.to_hex();
        assert_eq!(Digest32::from_str(&encoded), Ok(digest));
        assert!(Digest32::from_str(&encoded.to_uppercase()).is_err());
    }

    #[test]
    fn object_encoding_is_key_ordered_and_length_framed() {
        let mut left = BTreeMap::new();
        left.insert("b", 2_u64);
        left.insert("a", 1_u64);
        let mut right = BTreeMap::new();
        right.insert("a", 1_u64);
        right.insert("b", 2_u64);
        assert_eq!(left.canonical_bytes(), right.canonical_bytes());
        assert_ne!(
            left.canonical_bytes(),
            BTreeMap::from([("a", 12_u64)]).canonical_bytes()
        );
    }
}
