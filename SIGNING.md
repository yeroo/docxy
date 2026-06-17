# Code signing policy

This document is the published code-signing policy for **Docxy**.

## Current status

Release binaries are **not yet Authenticode-signed**. We have applied to the
[SignPath Foundation](https://signpath.org/) free code-signing program for
open-source software; once the certificate is issued, Windows release binaries
will be signed automatically by the release workflow (see *Enabling SignPath*
below).

Until signing is active, verify downloads using the published **SHA-256
checksums** and the **GitHub build-provenance attestation** that ship with every
release.

## How releases are produced

- Binaries are built **only** by the GitHub Actions release workflow
  (`.github/workflows/release.yml`) on a version tag — never on a developer
  machine.
- Each binary is published with a `.sha256` checksum.
- Each binary carries a cryptographic **build-provenance attestation**
  (`actions/attest-build-provenance`) tying it to the exact commit, workflow,
  and runner that produced it.

### Verifying a download

Checksum (PowerShell):

```
Get-FileHash .\docxy-windows-x86_64.exe -Algorithm SHA256
# compare against docxy-windows-x86_64.exe.sha256
```

Provenance (GitHub CLI):

```
gh attestation verify docxy-windows-x86_64.exe --repo yeroo/docxy
```

## Roles

This project follows SignPath's separation-of-duties model. Roles are currently
held by the maintainer; additional reviewers/approvers will be added as the
project grows.

| Role      | Responsibility                          | Holder |
| --------- | --------------------------------------- | ------ |
| Author    | Writes code / opens pull requests       | yeroo  |
| Reviewer  | Reviews and approves pull requests      | yeroo  |
| Approver  | Authorizes each signing request         | yeroo  |

All maintainer accounts (GitHub and SignPath) have multi-factor authentication
enabled.

## Privacy

Docxy is a local terminal tool. It does not phone home or transmit any user
data. The only outbound action it can take is opening an `http(s)` link you
explicitly click — and only after a confirmation prompt — in your default
browser.

## Enabling SignPath (after approval)

Once the SignPath Foundation issues the certificate:

1. In the SignPath organization, configure the predefined **GitHub.com** Trusted
   Build System and link it to this project; install the
   [SignPath GitHub App](https://github.com/apps/signpath).
2. Add repository **secret** `SIGNPATH_API_TOKEN` (submitter token) and
   repository **variables** `SIGNPATH_ORGANIZATION_ID`, `SIGNPATH_PROJECT_SLUG`,
   `SIGNPATH_SIGNING_POLICY_SLUG`.
3. In `release.yml`, after the unsigned Windows binary is uploaded as an
   artifact, insert the signing step and attach the **signed** result instead of
   the unsigned one:

   ```yaml
   - name: Sign Windows binary (SignPath)
     if: runner.os == 'Windows' && startsWith(github.ref, 'refs/tags/') && vars.SIGNPATH_ORGANIZATION_ID != ''
     uses: signpath/github-action-submit-signing-request@v2
     with:
       api-token: ${{ secrets.SIGNPATH_API_TOKEN }}
       organization-id: ${{ vars.SIGNPATH_ORGANIZATION_ID }}
       project-slug: ${{ vars.SIGNPATH_PROJECT_SLUG }}
       signing-policy-slug: ${{ vars.SIGNPATH_SIGNING_POLICY_SLUG }}
       github-artifact-id: ${{ steps.upload-unsigned.outputs.artifact-id }}
       wait-for-completion: true
       output-artifact-directory: dist
   ```

   (Give the Windows `Upload artifact` step `id: upload-unsigned` so the signing
   step can reference its `artifact-id`.)

## Acknowledgement

When signing is active, this section will read: *"Free code signing provided by
[SignPath.io](https://signpath.io), certificate by [SignPath Foundation](https://signpath.org/)."*
