<h1 align="center">saysion</h1>
<div align="center">
  <strong>
    Session support with pluggable middleware
  </strong>
</div>

<br />

<div align="center">
  <h3>
    <a href="https://docs.rs/saysion">
      API Docs
    </a>
    <span> | </span>
    <a href="https://github.com/salvo-rs/saysion/releases">
      Releases
    </a>
    <span> | </span>
    <a href="https://github.com/salvo-rs/saysion/blob/main/.github/CONTRIBUTING.md">
      Contributing
    </a>
  </h3>
</div>


*Fork from: https://github.com/http-rs/async-session*

## Installation (0.3.0 unreleased)

The current `main` branch targets **0.3.0**, which has not been published to
crates.io. It requires **Rust 1.95 or newer**. Published 0.2.0 does not contain
`SignedCookieStore`, `SessionConflict`, or the new persistence guarantees.
Use the development source until 0.3.0 is released:

```toml
[dependencies]
saysion = { git = "https://github.com/salvo-rs/saysion", branch = "main" }
```

Commit your application's `Cargo.lock` to retain its resolved Git revision;
update the dependency deliberately when following this moving branch. Add the
backend feature you need, such as `features = ["redis"]`.
See the [0.3 migration guide](docs/migration-0.3.md) and [changelog](CHANGELOG.md).

## Session stores

Backend setup and feature flags: [English](docs/en/stores.md) |
[中文](docs/zh/stores.md).

## Safety
This crate uses ``#![deny(unsafe_code)]`` to ensure everything is implemented in
100% Safe Rust.

## Contributing
Want to join us? Check out our ["Contributing" guide][contributing] and take a
look at some of these issues:

## Acknowledgements

This work is based on the work initiated by
[@chrisdickinson](https://github.com/chrisdickinson) in
[tide#266](https://github.com/http-rs/tide/pull/266).

## License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br/>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>
