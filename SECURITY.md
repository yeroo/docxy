# Security Policy

## Supported versions

The latest release is supported. Fixes land on the default branch and ship in the
next tagged release.

## Reporting a vulnerability

Please report security issues **privately** using GitHub's
**Security → Report a vulnerability** (private advisory) on this repository,
rather than opening a public issue. If you prefer email, contact
boris.kudryashov@gmail.com. We aim to respond within a few days.

## A note on antivirus false positives

The prebuilt Windows binaries are occasionally flagged as a **false positive** by
Microsoft Defender and similar engines. This is a well-known heuristic issue with
small, low-reputation native executables — **not** an indication of malware:

- The complete source is in this repository and the builds are reproducible
  through the CI / release workflows.
- The tool reads and writes files (`.docx`/`.pdf`), which is the kind of behavior
  some heuristics over-weight.
- We are pursuing Authenticode code signing (via the SignPath Foundation free OSS
  program) to resolve this for good — see [SIGNING.md](SIGNING.md).

If you encounter it, you can build from source or verify the binary against the
CI build. **This is not a security vulnerability — please do not report it as
one.** Suspected real malware in a release artifact, on the other hand, is very
much in scope; report it privately as above.
