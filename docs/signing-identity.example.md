# OpenCodeServer Signing Identity Runbook (template)

This is the in-repo, value-free template for the signing-identity runbook.
The real runbook is maintainer-local and deliberately outside the
repository: the tracked tree must contain no concrete identity values
(developer account, real name, Team ID, leaf id, certificate fingerprints,
serials). Where the real values live:

- `~/.config/opencodeserver/signing-identity` — one line, the exact
  identity name (for example `Apple Development: developer@example.com
  (LEAFIDXXXX)`). Consumed by `scripts/build.sh` (Release default) and
  `scripts/install.sh` (required leaf authority); overridable with
  `SIGNING_IDENTITY` / `OPENCODESERVER_SIGNING_AUTHORITY`.
- `~/Documents/OpenCodeServer-References/signing-identity.md` — the live
  runbook copy of this template, kept in sync by the maintainer.
- `~/.config/opencodeserver/sensitive-patterns` — the fixed strings the
  public-mirror sync gate refuses to publish.

Fill in a copy of the sections below in the maintainer-local runbook; never
commit the filled-in values.

## Current state

| Property | Value |
|---|---|
| Identity name | `Apple Development: <developer-account> (<leaf-id>)` |
| Team ID | `<team-id>` |
| Certificate SHA-1 | `<certificate-sha1>` |
| Certificate SHA-256 | `<certificate-sha256>` |
| Subject | `UID=<uid>, CN=<identity-name>, OU=<team-id>, O=<developer-name>, C=<country>` |
| Issuer | `CN=Apple Worldwide Developer Relations Certification Authority, OU=G3, O=Apple Inc., C=US` |
| Serial | `<certificate-serial>` |
| Validity | `<not-before> → <not-after>` |
| Location | Login keychain of the designated build Mac |
| Signing authority | `<identity-name>` |
| Signed product components | `OpenCodeServer`, `OpenCodeServerAgent`, `opencodeserverctl` |

Discover the identity at runtime with:

```bash
security find-identity -v -p codesigning
```

## Signing policy

- Release builds use the Apple Development identity and `--timestamp=none`.
- Debug, test, and CI builds retain the ad hoc default from
  `Config/Base.xcconfig` and `Config/Release.xcconfig`; the concrete
  identity reaches a Release build only through `scripts/build.sh`
  (`SIGNING_IDENTITY` or the local identity file).
- The Apple Development private key stays in the login keychain on the
  designated build Mac. It must not be exported, copied, or committed; no
  `.p12` backup.
- Sign nested code before the outer `OpenCodeServer` app and verify the
  complete bundle with `codesign --verify --deep --strict`.
- Never weaken a Designated Requirement to a bundle-identifier-only rule.

## Keychain access and certificate expiry

On a first Release signing operation, approve the macOS Keychain ACL prompt
with **Always Allow**; do not pre-authorize by exporting the private key or
weakening the keychain ACL. An identity change requires the ADR 0016
`access_pending` and explicit “Allow Keychain Access…” flow to be
revalidated during post-migration acceptance.

## Verification commands

```bash
security find-identity -v -p codesigning
security find-certificate -c "<identity-name>" -p \
  | openssl x509 -noout -subject -issuer -serial -startdate -enddate
codesign --display --verbose=4 /Applications/OpenCodeServer.app
codesign -d -r- /Applications/OpenCodeServer.app
codesign --verify --deep --strict /Applications/OpenCodeServer.app
```

## Trust scope and recovery

The private key exists only on the designated build Mac. Other test Macs
may receive already-signed builds and the public certificate, but never the
private key. If the certificate is lost or expires, use the normal Xcode
reissue flow from the same Apple team; record the new public certificate
facts, Designated Requirement, and any TCC or Keychain reauthorization in
the local runbook and the relevant test record.

## Xcode reissue procedure and yearly cadence

Apple Development certificates have a yearly validity window. When a
reissue is authorized, use Xcode's account and signing-certificate
management for the same team, keep the private key in the login keychain,
and record the replacement's public facts, the new Designated Requirement,
and TCC/Keychain reauthorization results before installation.

A reissued certificate changes the leaf suffix (`(<leaf-id>)`) and
therefore the leaf authority string. Update
`~/.config/opencodeserver/signing-identity` — the single source consumed by
`scripts/build.sh` Release defaults and `scripts/install.sh`'s
`required_signing_authority` — together with the identity facts table
above, and re-run the ADR 0021 identity-change installer gate.
