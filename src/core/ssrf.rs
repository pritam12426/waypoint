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
fn is_blocked_ip(ip: &IpAddr) -> bool {
	match ip {
		IpAddr::V4(v4) => is_blocked_v4(v4),
		IpAddr::V6(v6) => is_blocked_v6(v6),
	}
}

/// Resolver that refuses to hand ureq a private, loopback, or link-local
/// address. Wraps the OS [`DefaultResolver`] (same DNS, same timeout
/// behavior) and filters the resolved set down to public addresses.
///
/// ureq calls this once per `connect()`, and every redirect hop re-enters
/// `connect()` — so the guard applies to the *final* destination of a
/// redirect chain, not just the URL the caller asked for.
#[derive(Debug, Default)]
pub(crate) struct SsrfResolver(DefaultResolver);

impl Resolver for SsrfResolver {
	fn resolve(
		&self,
		uri: &Uri,
		config: &Config,
		timeout: NextTimeout,
	) -> Result<ResolvedSocketAddrs, ureq::Error> {
		// Fail fast on hostnames that name a local machine.
		if let Some(host) = uri.host()
			&& is_blocked_host(host)
		{
			return Err(ureq::Error::BadUri(format!(
				"refusing to connect to blocked host {host:?}"
			)));
		}
		let addrs = self.0.resolve(uri, config, timeout)?;
		let mut public =
			ResolvedSocketAddrs::from_fn(|_| SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0));
		for addr in &addrs {
			if !is_blocked_ip(&addr.ip()) {
				public.push(*addr);
			}
		}
		if public.is_empty() {
			return Err(ureq::Error::BadUri(
				"refusing to connect: destination resolves to a private or loopback address"
					.to_string(),
			));
		}
		Ok(public)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn parse(ip: &str) -> IpAddr {
		ip.parse().unwrap()
	}

	// Every network class the fetch engine must never touch.
	#[test]
	fn blocked_ipv4_ranges() {
		for ip in [
			"0.0.0.0",
			"10.0.0.1",
			"10.255.255.255",
			"127.0.0.1",
			"127.8.8.8",
			"169.254.169.254",
			"169.254.1.1",
			"172.16.0.1",
			"172.31.255.255",
			"192.168.0.1",
			"192.168.255.255",
			"100.64.0.1",
			"100.127.255.255",
			"192.0.0.1",
			"192.0.2.1",
			"198.18.0.1",
			"198.51.100.1",
			"203.0.113.1",
			"240.0.0.1",
			"255.255.255.255",
		] {
			assert!(is_blocked_ip(&parse(ip)), "{ip} should be blocked");
		}
	}

	#[test]
	fn blocked_ipv6_ranges() {
		for ip in [
			"::",
			"::1",
			"fe80::1",
			"fec0::1",
			"fc00::1",
			"fd12::1",
			"ff02::1",
			"::ffff:127.0.0.1",
			"::ffff:10.1.2.3",
			"::ffff:169.254.169.254",
			"::10.0.0.1",
		] {
			assert!(is_blocked_ip(&parse(ip)), "{ip} should be blocked");
		}
	}

	// Public addresses must always survive the filter.
	#[test]
	fn allowed_public_addresses() {
		for ip in [
			"8.8.8.8",
			"1.1.1.1",
			"93.184.216.34",
			"2001:4860:4860::8888",
			"2606:4700::6810:84e5",
		] {
			assert!(!is_blocked_ip(&parse(ip)), "{ip} should be allowed");
		}
	}

	#[test]
	fn blocked_hostnames() {
		for host in [
			"localhost",
			"foo.localhost",
			"printer.local",
			"db.internal",
			"router.home.arpa",
		] {
			assert!(is_blocked_host(host), "{host} should be blocked");
		}
		for host in [
			"example.com",
			"example.com.localhost.evil.com",
			"cdn.example",
		] {
			assert!(!is_blocked_host(host), "{host} should be allowed");
		}
	}

	// The bracket form used in IPv6 literals must be stripped before the
	// hostname check (it is not a hostname, but must not be rejected either).
	#[test]
	fn blocked_host_brackets() {
		assert!(!is_blocked_host("[2001:db8::1]"));
		assert!(!is_blocked_host("[::1]"));
	}
}
