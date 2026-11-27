# Changelog

All notable changes are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-11-27

First public version, under the GNU Affero General Public License v3.0 or later.

- Stand-in Hue bridge for Hue Sync clients: discovery (mDNS, SSDP), pairing, the real bridge's
  resources under the relay's identity, and the entertainment stream (DTLS 1.2, PSK).
- Per-channel brightness levels (`gains`) per entertainment area, with live tuning through
  `gains.json`.
- `outside_sync`: the levels also apply between syncs, to the lights' own state.
- Commands: `run`, `pair-bridge`, `list-areas`, `stream-test`.
- Known to work with the LG webOS Hue Sync app (webOS 24), Hue Bridge software 1978688000 and a Hue
  Flux light strip.

[0.1.0]: https://github.com/m-torun/hue-relay/releases/tag/v0.1.0
