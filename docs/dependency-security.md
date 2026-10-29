# Dependency security policy

This library does not commit `Cargo.lock`. Updating an ignored local lockfile
alone does not protect its consumers, so the published manifest carries the
following minimum versions. Optional constraints are enabled only alongside the
backend that brings in the dependency; they disable their own default features.
The ordinary backend dependencies continue to choose the required TLS provider.

| Dependency | Minimum | Reason |
| --- | --- | --- |
| `time` | 0.3.47 | [RUSTSEC-2026-0009](https://rustsec.org/advisories/RUSTSEC-2026-0009.html) |
| `sqlx` | 0.8.6 | Includes the protocol fix from 0.8.1, [RUSTSEC-2024-0363](https://rustsec.org/advisories/RUSTSEC-2024-0363.html); keeps the audited 0.8 API |
| `event-listener` | 5.4.2 | [RUSTSEC-2026-0221](https://rustsec.org/advisories/RUSTSEC-2026-0221.html), through `async-lock` and SQLx |
| `tokio` | 1.53.3 | [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html), service-backed stores and tests |
| `ctutils` | 0.4.4 | [RUSTSEC-2026-0385](https://rustsec.org/advisories/RUSTSEC-2026-0385.html), MongoDB digest dependencies |
| `rustls` | 0.23.46 | [RUSTSEC-2026-0285](https://rustsec.org/advisories/RUSTSEC-2026-0285.html), [0346](https://rustsec.org/advisories/RUSTSEC-2026-0346.html), [0348](https://rustsec.org/advisories/RUSTSEC-2026-0348.html), [0349](https://rustsec.org/advisories/RUSTSEC-2026-0349.html), SQLx/MongoDB TLS |
| `rustls-webpki` | 0.103.16 | [RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382.html), certificate verification |
| `webpki-roots` | 1.0.10 | [RUSTSEC-2026-0381](https://rustsec.org/advisories/RUSTSEC-2026-0381.html), root certificates; also constrains the 1.x dependency re-exported by 0.26.11 |

These constraints cover the compatible versions in saysion's graph. A downstream
application may also resolve other dependency versions and must audit its own
lockfile. Run `cargo update` before auditing an existing application lockfile.
CI generates a fresh full graph, runs `cargo audit --deny warnings` on pull
requests and weekly, and fails on new vulnerabilities, unsoundness notices,
unmaintained packages, or yanked versions. The single exception below is explicit
and must be reviewed separately; it does not suppress other RSA advisories.

## Scoped RSA advisory exception

Reviewed on **2026-10-29**; review again by **2027-01-29** or on any SQLx/RSA
upgrade, whichever comes first. Maintainers are responsible for this review.

[RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)
reports timing leakage of RSA **private-key** operations. It currently has no
patched version, including RSA 0.9.10. In this repository, RSA is introduced by
the optional `sqlx-mysql` backend through `sqlx-mysql 0.8.6`.

The [audited driver authentication implementation](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-mysql/src/connection/auth.rs)
imports `RsaPublicKey`, parses a public key supplied by the MySQL server, and calls
`RsaPublicKey::encrypt` with OAEP for non-TLS authentication. It does not generate,
load, or use an RSA private key. For TLS connections it sends the password inside
the TLS channel instead. A source search of the driver's `src` directory found no
RSA private-key/decryption call. Saysion itself has no RSA call sites. Consequently
the private-key timing path described in this advisory is not reached through
this audited use. `.cargo/audit.toml` ignores **only this advisory ID** for this
repository; downstream applications with other RSA uses must assess them.

SQLx 0.9 makes non-TLS RSA authentication optional, but also changes dynamic query
APIs and SQLite behavior; see the [upstream migration notes](https://github.com/launchbadge/sqlx/blob/v0.9.0/CHANGELOG.md#090---2026-05-06).
That upgrade is a separate compatibility change, rather than a prerequisite for
this narrowly scoped exception. Prefer authenticated TLS for database connections.
Remove or reassess the exception when a patched RSA release is available, when
upgrading the driver, or if the repository adds any RSA private-key operation.
