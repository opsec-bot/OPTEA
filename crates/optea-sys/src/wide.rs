//! UTF-16 conversion helpers.

use windows::core::PCWSTR;

/// A NUL-terminated UTF-16 buffer that owns its storage.
///
/// Keep the `WideString` alive for as long as the `PCWSTR` is in use — the
/// pointer borrows from this buffer.
pub struct WideString(Vec<u16>);

impl WideString {
    pub fn new(s: &str) -> Self {
        let mut v: Vec<u16> = s.encode_utf16().collect();
        v.push(0);
        WideString(v)
    }

    pub fn as_pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }
}

pub fn wide(s: &str) -> WideString {
    WideString::new(s)
}

/// Decode a UTF-16 buffer, stopping at the first NUL.
pub fn from_wide_nul(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// Decode a REG_MULTI_SZ style double-NUL-terminated sequence.
pub fn from_wide_multi(buf: &[u16]) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0usize;
    for (i, &c) in buf.iter().enumerate() {
        if c == 0 {
            if i == start {
                break; // empty string == end of sequence
            }
            out.push(String::from_utf16_lossy(&buf[start..i]));
            start = i + 1;
        }
    }
    out
}

/// Encode strings into a double-NUL-terminated REG_MULTI_SZ buffer.
pub fn to_wide_multi(items: &[String]) -> Vec<u16> {
    let mut v = Vec::new();
    for s in items {
        v.extend(s.encode_utf16());
        v.push(0);
    }
    v.push(0);
    v
}
