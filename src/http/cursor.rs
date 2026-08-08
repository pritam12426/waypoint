/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! Opaque pagination cursor tokens for the HTTP list endpoint.
//!
//! The cursor encodes the `(created_at, id)` of the last row of a page —
//! the keyset bound the next page's query compares against. It is hex of
//! `"{id}:{created_at}"`, which keeps it URL-safe and gives clients no
//! structure to depend on; only the server knows the format. Offset
//! pagination stays as the compatibility path.

const HEX: &[u8; 16] = b"0123456789abcdef";

fn encode_hex(bytes: &[u8]) -> String {
	let mut out = String::with_capacity(bytes.len() * 2);
	for &b in bytes {
		out.push(HEX[(b >> 4) as usize] as char);
		out.push(HEX[(b & 0x0f) as usize] as char);
	}
	out
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
	if !s.len().is_multiple_of(2) {
		return None;
	}
	let mut out = Vec::with_capacity(s.len() / 2);
	let bytes = s.as_bytes();
	for chunk in bytes.chunks_exact(2) {
		let hi = (chunk[0] as char).to_digit(16)?;
		let lo = (chunk[1] as char).to_digit(16)?;
		out.push(((hi << 4) | lo) as u8);
	}
	Some(out)
}

/// Encodes a page's last `(created_at, id)` into a cursor token.
pub fn encode_cursor(id: i64, created_at: &str) -> String {
	encode_hex(format!("{id}:{created_at}").as_bytes())
}

/// Decodes a cursor token back into `(created_at, id)`, or `None` if it is
/// malformed (bad hex, missing separator, non-numeric id).
pub fn decode_cursor(token: &str) -> Option<(String, i64)> {
	if token.len() > 512 {
		// A token that long cannot be a page bound; reject before allocating.
		return None;
	}
	let decoded = decode_hex(token)?;
	let text = String::from_utf8(decoded).ok()?;
	let (id, created_at) = text.split_once(':')?;
	Some((created_at.to_string(), id.parse().ok()?))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roundtrips() {
		let token = encode_cursor(42, "2026-03-01 00:00:00");
		assert_eq!(
			decode_cursor(&token),
			Some(("2026-03-01 00:00:00".to_string(), 42))
		);
	}

	#[test]
	fn rejects_garbage() {
		assert_eq!(decode_cursor(""), None);
		assert_eq!(decode_cursor("zz"), None);
		assert_eq!(decode_cursor("616263"), None); // "abc" — no separator
		assert_eq!(decode_cursor("66:61"), None); // "fa" — id not numeric
		assert_eq!(decode_cursor(&"a".repeat(1000)), None);
	}
}
