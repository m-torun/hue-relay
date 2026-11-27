# Contributing to hue-relay

Thanks for your interest. Bug reports, fixes, documentation and reports of what works with your
setup (bridge, lights, Hue Sync client) are all welcome.

Everyone taking part follows the [Code of Conduct](CODE_OF_CONDUCT.md). Security problems go through
the [security policy](SECURITY.md), not public issues.

## Scope

hue-relay does one job: per-channel brightness levels for Hue entertainment areas, applied to a Hue
Sync client's stream and, if enabled, to the lights' own state between syncs. Changes that make it
do that better (more clients, more lights, robustness, clarity) fit. Features beyond that job, or
specific to one home, are better built next to it (in a home automation system, for example), and
pull requests for them will usually be declined. If you are unsure, open an issue first and ask.

## Reporting a bug

Open an issue with the bug template. The most useful details are: the hue-relay version
(`hue-relay --version`), how you run it (Docker or the binary), your bridge's software version, the
Hue Sync client (device, app version), the lights in the area, and the relay's log around the
problem. Remove keys and anything else private from logs before posting.

## Making a change

1. For anything larger than a small fix, open an issue first so we can agree on the approach.
2. Fork the repository and create a branch from `main`, named after the change
   (`fix-handshake-timeout`, `docs-macvlan`).
3. Make the change. Keep each pull request to one topic.
4. Run the checks CI runs:

   ```
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test
   ```

5. If you can, test with real hardware: `hue-relay list-areas` and `hue-relay stream-test` help, and
   the relay logs every request and a sample of each stream. Say in the pull request what you tested
   with (bridge software, client, lights).
6. Update the README (and `CHANGELOG.md`) when behaviour or configuration changes.
7. Open a pull request against `main` and fill in the template.

## Commit messages

- One logical change per commit; a commit should build and pass the tests on its own.
- A subject line in the imperative ("Fix …", "Add …", "Retry …"), up to about 72 characters, no
  trailing full stop.
- A blank line, then a body that explains why the change is needed and what it changes, wrapped
  at about 72 characters. Mention what you tested, and reference issues ("Fixes #12").

```
Retry the bridge handshake after a takeover

The bridge refuses a new DTLS session for a few seconds after another
stream on the same area ends. Retry with backoff instead of giving up
after one attempt. Tested with a Hue Bridge 1.78 and a TV's Hue Sync
app. Fixes #12.
```

## Code style

- `rustfmt` with the repository's `rustfmt.toml`; no `clippy` warnings.
- Comments explain why something is done, not what the next line does.
- Log messages and documentation are plain, specific sentences: say what happened and to which
  device or area.
- New behaviour gets unit tests where it can be tested without hardware (frame handling, gains,
  colours, state rendering).

## Licence of contributions

hue-relay is licensed under the [GNU Affero General Public License, version 3 or later](LICENSE).
By contributing you agree that your contribution is licensed under the same terms.
