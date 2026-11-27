# Security policy

## Supported versions

Fixes go into `main` and the latest release. Please check that a problem still occurs there before
reporting it.

## Reporting a vulnerability

Please do not open a public issue. Use GitHub's private reporting instead: on this repository's
**Security** tab, choose **Report a vulnerability**. Describe the problem, how to reproduce it, and
what an attacker could do with it. You will get an answer as soon as the maintainer can look at it;
please allow some time, this is a spare-time project.

## What to keep in mind

hue-relay is meant for trusted home networks. By design:

- it answers like a Hue bridge to anyone on the network, and while pairing (`link_open`) is open
  anyone on the network can pair with it;
- it holds an application key for your real bridge, which grants control of your lights;
- its HTTPS certificate is self-signed, as Hue clients expect.

Reports about ways past these boundaries (for example pairing while `link_open` is off, reading the
bridge key, or controlling lights without a pairing) are very welcome.
