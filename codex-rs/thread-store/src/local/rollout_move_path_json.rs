//! Lossless JSON encoding for local rollout move paths.
//!
//! Keep ordinary UTF-8 paths as strings so previously written journals and
//! receipts remain readable. Only paths that cannot be strings use a tagged,
//! platform-specific representation.

use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Deserializer;
use serde::Serialize;
use serde::Serializer;
use serde::de::Error as _;

#[derive(Deserialize)]
#[serde(untagged)]
enum StoredPath {
    Utf8(String),
    Native(NativePath),
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NativePath {
    #[cfg(unix)]
    #[serde(rename = "unixHex")]
    unix_hex: String,
    #[cfg(windows)]
    #[serde(rename = "windowsWideHex")]
    windows_wide_hex: String,
}

pub(super) fn serialize<S>(path: &Path, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if let Some(path) = path.to_str() {
        return serializer.serialize_str(path);
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        NativePath {
            unix_hex: encode_hex(path.as_os_str().as_bytes()),
        }
        .serialize(serializer)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;

        let mut bytes = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        NativePath {
            windows_wide_hex: encode_hex(&bytes),
        }
        .serialize(serializer)
    }
}

pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<PathBuf, D::Error>
where
    D: Deserializer<'de>,
{
    match StoredPath::deserialize(deserializer)? {
        StoredPath::Utf8(path) => {
            if path.contains('\0') {
                return Err(D::Error::custom("rollout path contains NUL"));
            }
            Ok(PathBuf::from(path))
        }
        StoredPath::Native(path) => {
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStringExt;

                let bytes = decode_hex(&path.unix_hex).map_err(D::Error::custom)?;
                if bytes.contains(&0) {
                    return Err(D::Error::custom("rollout path contains NUL"));
                }
                Ok(std::ffi::OsString::from_vec(bytes).into())
            }
            #[cfg(windows)]
            {
                use std::os::windows::ffi::OsStringExt;

                let bytes = decode_hex(&path.windows_wide_hex).map_err(D::Error::custom)?;
                let chunks = bytes.chunks_exact(2);
                if !chunks.remainder().is_empty() {
                    return Err(D::Error::custom("invalid Windows rollout path"));
                }
                let units = chunks
                    .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                    .collect::<Vec<_>>();
                if units.contains(&0) {
                    return Err(D::Error::custom("rollout path contains NUL"));
                }
                Ok(std::ffi::OsString::from_wide(&units).into())
            }
        }
    }
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 0xf) as usize] as char);
    }
    encoded
}

fn decode_hex(encoded: &str) -> Result<Vec<u8>, &'static str> {
    let bytes = encoded.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("invalid rollout path hex length");
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let digit = |byte| match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err("invalid rollout path hex digit"),
        };
        decoded.push((digit(pair[0])? << 4) | digit(pair[1])?);
    }
    Ok(decoded)
}

#[cfg(test)]
#[path = "rollout_move_path_json_tests.rs"]
mod tests;
