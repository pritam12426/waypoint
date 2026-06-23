/*
 * Copyright (c) 2026 Pritam
 *
 * SPDX-License-Identifier: MIT
 */

//! SSRF hardening shared by every outbound fetcher in the binary.
//!
//! The [`SsrfResolver`] is a ureq [`Resolver`] that filters every resolved
//! address and refuses to connect to loopback, private, link-local, or
//! unique-local networks. Because ureq consults the resolver for *every*
//! connect — including each redirect hop — a malicious page cannot redirect
//! a fetch onto `127.0.0.1`, the metadata IP `169.254.169.254`, or an
//! internal RFC 1918 host. The check runs on the very addresses the
//! connection will use, so there is no resolve-then-connect race to exploit.
//!
//! Both outbound engines wire this resolver into their ureq agent:
//!
//! * `fetch` — the media scrape engine
//! * `checker` — the link-liveness checker
//!
//! Keeping the guard here instead of inside each agent means every URL a
//! user can save is probed under the same policy, and the blocked-network
//! rules can never drift between the two engines.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use ureq::config::Config;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::NextTimeout;
use ureq::{self, http::Uri};

/// True when `host` (already lowercased, brackets stripped) names a local
/// machine that must never be fetched. These fail fast *before* DNS; the
/// address-level check in [`is_blocked_ip`] is the real guard, this is
/// defense in depth for hosts whose names imply local scope (`localhost`,
/// mDNS `.local`, RFC 8375 `.home.arpa`, ICANN-reserved `.internal`).
fn is_blocked_host(host: &str) -> bool {
	let host = host
		.trim_start_matches('[')
		.trim_end_matches(']')
		.to_ascii_lowercase();
	host == "localhost"
		|| host.ends_with(".localhost")
		|| host.ends_with(".local")
		|| host.ends_with(".internal")
		|| host.ends_with(".home.arpa")
}

/// True when an IPv4 address is a blocked (non-public) network: unspecified,
/// loopback, RFC 1918 private, link-local (incl. the `169.254.169.254` cloud
/// metadata IP), CGNAT (RFC 6598), and the IANA special-purpose ranges
/// (RFC 6890) that no real server ever lives on.
fn is_blocked_v4(ip: &Ipv4Addr) -> bool {
	let [a, b, c, _] = ip.octets();
	a == 0 // 0.0.0.0/8 — "this network"
		|| a == 10 // 10/8 — RFC 1918
		|| a == 127 // 127/8 — loopback
		|| (a == 169 && b == 254) // 169.254/16 — link-local / metadata
		|| (a == 172 && (16..=31).contains(&b)) // 172.16/12 — RFC 1918
		|| (a == 192 && b == 168) // 192.168/16 — RFC 1918
		|| (a == 100 && (64..=127).contains(&b)) // 100.64/10 — CGNAT
		|| (a == 192 && b == 0 && c == 0) // 192.0.0/24 — IETF assignments
		|| (a == 192 && b == 0 && c == 2) // 192.0.2/24 — TEST-NET-1
		|| (a == 198 && b == 18) // 198.18/15 — benchmarking
		|| (a == 198 && b == 51 && c == 100) // 198.51.100/24 — TEST-NET-2
		|| (a == 203 && b == 0 && c == 113) // 203.0.113/24 — TEST-NET-3
		|| (a >= 240) // 240/4 + 255.255.255.255 broadcast
}

/// True when an IPv6 address is a blocked (non-public) network.
fn is_blocked_v6(ip: &Ipv6Addr) -> bool {
	let s = ip.segments();
	if ip.is_unspecified() // ::
		|| ip.is_loopback() // ::1
		|| ip.is_multicast() // ff00::/8 — never a unicast target
		|| (s[0] & 0xfe00) == 0xfc00 // fc00::/7 — unique-local
		|| (s[0] & 0xffc0) == 0xfe80 // fe80::/10 — link-local
		|| (s[0] & 0xffc0) == 0xfec0
	// fec0::/10 — site-local (deprecated RFC 3879)
	{
		return true;
	}
	// IPv4-mapped `::ffff:a.b.c.d` — the real target is the embedded v4.
	if let Some(v4) = ip.to_ipv4_mapped() {
		return is_blocked_v4(&v4);
	}
	// Deprecated IPv4-compatible `::a.b.c.d` — same treatment.
	if s[..6].iter().all(|&x| x == 0) {
		let v4 = Ipv4Addr::new(
			(s[6] >> 8) as u8,
			(s[6] & 0xff) as u8,
			(s[7] >> 8) as u8,
			(s[7] & 0xff) as u8,
		);
		return is_blocked_v4(&v4);
	}
	false
}

/// True when `ip` must never be a fetch target.
