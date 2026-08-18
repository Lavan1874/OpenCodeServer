# ADR 0020: SettingsWindowController split — pure move, no credential extraction

Date: 2026-08-14

## Status

Accepted. Implemented on branch `refactor/settings-controller-split`
(commit `3fbea72`); all automated gates green. Merged to `main` as merge
commit `1b97d61` (verified 2026-08-16).

## Context

`SettingsWindowController.swift` had grown to 1370 lines holding the
Settings window's entire surface: stored state and injected dependencies,
interface construction with Auto Layout metrics, the credential editor
state machine, the Save flow (including Keychain mutation staging and
Service Management choices), executable discovery, and the live-status
re-render. ADR 0019 established the two-phase discipline for exactly this
shape: a first, deliberately pure-move split for navigability, then
extractions only where a concern has **cohesive private state and a
narrow cross-boundary access**.

## Decision

### Phase 1 — pure move into per-concern extension files

The file became a `SettingsWindowController/` directory: the main file
(stored state, `CredentialEditorState`/`SaveFeedbackContext`, init,
`present`/`reload`, cancel, feedback utilities) plus five
`extension SettingsWindowController` files grouped by concern:

- `SettingsWindowController+Layout.swift` (381 lines) —
  `LayoutMetrics`, `buildInterface`, advanced disclosure, window
  resizing, and the private form-metric helpers (`label`,
  `configureFormGrid`, `horizontalStack`, `fittingWidth`,
  `textFieldWidth`).
- `SettingsWindowController+CredentialEditing.swift` (230) — the
  credential editor state machine: probe, explicit Edit/Copy read,
  render, clear, Show toggle, Edit/Copy/Remove actions, agent
  `grantAccess`, `currentPassword`.
- `SettingsWindowController+SaveFlow.swift` (408) —
  `CredentialMutation`/`CredentialSavePlan`, `save`, `finishSave`,
  restart offer, the tested static decisions, `applyServiceChoices`.
- `SettingsWindowController+ExecutableSelection.swift` (41) —
  candidate popup rebuild, `candidateChanged`, `chooseExecutable`.
- `SettingsWindowController+StatusRefresh.swift` (90) —
  `refreshLiveStatus`, agent-access row, `renderSaveFeedback`,
  `saveFeedbackText`.

Swift resolves methods regardless of defining file, so every `@objc`
action and selector binding is unchanged; the Edit menu still works
through the Responder Chain; decrypt-class Keychain reads stay off the
main thread; the Advanced disclosure behavior is byte-identical.

**The one mechanical deviation, forced by Swift semantics:** `private`
cannot cross file boundaries, so the 66 declarations referenced from
another file in the split drop the keyword and become internal (module)
scope. Swift offers nothing between `fileprivate` and `internal`, so
this is the coarsest available equivalent of the Rust split's
`pub(super)`. Pure-move verification: sorted-line comparison of the
original class body against the concatenated new bodies — 1296 non-empty
lines on both sides, 112 differing lines, every one a pure `private`
keyword removal, zero unexpected additions or deletions. No method body,
property, comment, or selector changed. The Xcode project (hand-maintained
pbxproj) registers the new directory group and six source files.

### Phase 2 — evaluated a CredentialEditorController; declined

The candidate was the credential editing cluster (ADR 0019's stateful-
controller pattern). Both criteria fail:

**Cohesive private state — no.** The cluster owns only two fields
(`credentialEditorState`, `credentialOperationGeneration`). Everything
else it manipulates belongs to other concerns: 8 password-row controls,
`saveButton` (a Save-flow footer control that `renderCredentialEditor`
enables/disables), the agent-access row (`grantAccess` re-renders
`refreshAccessRow`'s controls), `feedbackLabel`/`showError`, `logger`,
the injected Keychain closures, and `loadedAccount`, which is shared
read/write with `reload` and the Save flow. The Rust CredentialController
this pattern comes from moved six dedicated fields; here the dominant
state of the cluster is not its own.

**Narrow boundary — no, wide in both directions.** Inbound:
`save()` switches over all seven `credentialEditorState` cases — the
editor state's whole meaning is "what should Save do" — and
`finishSave`/`reload`/`buildInterface` all call back into the cluster's
render/probe entry points. Outbound: an extraction would need access to
`saveButton`, the feedback surface, the agent-access row, `loadedAccount`,
and the Keychain closures, while Layout must keep measuring the
password-row controls (row height, value-column width) to build the
NSGridView. A child-view-controller shape would move the 8 row controls
in but still cross out for save-enable state, error feedback, and
fitting-size queries.

**Does not make anything testable.** The pure decisions are already
extracted and tested (`saveFeedbackText`, `needsCredentialAuthorization`,
`shouldOfferRestart`, `credentialNotice` statics; `KeychainStore` and
`CredentialMutationCoordinator` behind injected closures). What remains
is NSControl visibility rendering and dialogs — not unit-testable in
either shape without a GUI harness.

Per ADR 0019's stopping criterion — extract only when the boundary is
clean **and** the extraction removes or makes testable real complexity —
this is the honest "stop at the pure move" outcome.

## Acceptance

All gates green on the branch: `cargo fmt --all -- --check`;
`cargo clippy --all-targets --all-features --locked -- -D warnings`;
`cargo test --all --features test-fixture` (125 unit + 65 integration);
full `xcodebuild test` (77 tests, 0 failures, including the 13
AppKitBaselineTests that instantiate the split controller). No Release
build or installation was performed: phase 2 extracted nothing, so no
acceptance candidate left the branch.

## Consequences

- The Settings surface is navigable by concern with unchanged runtime
  behavior; the split is a reorganization, not a simplification — same
  conclusion ADR 0019 recorded for its module split.
- Cross-file members are internal, not private. The module is one app
  target plus `@testable` tests, so the practical exposure is nil, but
  future single-file helpers should keep `private` where callers allow
  it (the Layout metric helpers already do).
- A credential editor extraction is recorded as *evaluated and declined*
  with the coupling evidence above; revisit only if Settings grows a
  real second pane or the editor acquires state of its own — not as
  churn against the current shape.
