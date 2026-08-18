# External-machine distribution acceptance — curl and Homebrew tap

Date: 2026-08-18
Target: clean test VM ("testvm"), Apple Silicon, macOS 26
Scope: pre-notarization distribution channels (ADR 0021, 2026-08-18
amendment) on a machine other than the designated build Mac.
Operator-reported results for the install flows; no credentials or
secrets were involved.

## Channels tested

### curl + ditto (channel A) — PASS

```text
curl -LO .../releases/latest/download/OpenCodeServer-latest.zip   1494k downloaded
ditto -x -k OpenCodeServer-latest.zip .
mv OpenCodeServer.app /Applications
open /Applications/OpenCodeServer.app
```

- No Gatekeeper dialog appeared (curl downloads carry no quarantine
  attribute) — the app launched directly, as designed.
- OpenCodeServerAgent registration and OpenCode detection/launch behaved
  normally on the external machine.

### Homebrew tap (channel C) — PASS, with expected friction

- `brew tap lavan1874/opencodeserver` succeeded.
- `brew install --cask opencodeserver` was initially refused until the
  operator ran `brew trust lavan1874/opencodeserver` (current Homebrew
  requires explicit trust before loading third-party tap packages).
- First open was quarantined and blocked; one System Settings → Privacy
  & Security → "Open Anyway" approval, after which the app and agent
  behaved normally, including OpenCode detection/registration.

## Defects found and fixed during this test

1. Published tap install instructions omitted the `brew trust` step
   required by current Homebrew. Fixed in the public README, the tap
   README, and the Release v86 notes (commit "Document the brew trust
   step required by current Homebrew").
2. Published curl install block omitted `ditto -x -k`'s required
   destination argument (`.`), producing "ditto: No destination". Fixed
   in the public README and the Release v86 notes (commit "Fix ditto
   extraction command: destination argument is required"). The corrected
   four-line block was then verified verbatim on the build Mac —
   download, extraction, and `codesign --verify --deep --strict` all
   passed — before republishing.

Process rule adopted: command blocks in published install documentation
must be executed verbatim before release.

## Not covered (remain open)

- FDA tri-state probe, Local Network prompt attribution, and the TCC
  `AttributionChain` dump on the external machine.
- Version-to-version upgrade on the external machine (brew upgrade and
  manual replacement) — including whether the team-anchored registration
  transaction stays crash-dialog-free there.
- The Keychain password/consent flow on the external machine.
