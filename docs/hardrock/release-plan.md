# Release plan: offering the Hardrock panel to SDRoxide

What has to happen before the panel goes to SDRoxide's author (dividebysandwich/sdroxide), and why. Nothing is sent until Rich (AJ3G) says so.

## 1. Finish testing

Run the current checklist in [test-log.md](test-log.md) to the end, on the latest installer. Nothing goes upstream with an open item. The user manual's screenshot (`docs/images/amp-window.png`) was replaced on 18 Oct with one from dev-231be5b; replace it again if the window changes before release.

## 2. Clean history

This repo has about 70 small commits, many of them test-log updates. Upstream gets a fresh branch from their latest `main`, carrying the same code as a few commits that each build and pass their tests:

1. **The amplifier link.** `crates/sdroxide-hardrock` (protocol and link), the types in `sdroxide-types/src/amp.rs`, the engine hooks, and the settings tab.
2. **The AMP window.** Meters, readings, controls, performance, pop-out.
3. **The tuner.** The network drawing, TUNE and ON/BYP, and the L/C estimate.
4. **Docs.** The user manual section (2.23) and `docs/hardrock` (less the excluded files below).

Offer them as one pull request in three or four commits, or as separate pull requests, whichever the author prefers.

## 3. Leave out

| What | Why |
|---|---|
| `.github/workflows/windows-msi.yml` changes | Only for Rich's nightly installers (the short-sha `resolve` job). |
| `crates/sdroxide-rade/build.rs` changes | A local build tweak, not part of the feature. |
| `docs/hardrock/references/` (the PDFs, schematics and bench photos) | Kept out of an upstream pull request to keep it small. HobbyPCB's documents are included in this repository with Jim Veatch's permission (see [references.md](references.md)); the bench logs and VNA photos are Rich's. |
| `docs/hardrock/test-log.md`, `release-plan.md` | Working notes for this project. A short summary of what was tested goes in the pull request text instead. |

## 4. Say up front

- **Tested on the HR500 under Windows 11 only.** Linux and macOS are untested.
- **HR500 only.** The HR50 is selectable and marked untested. Its limits are scaled from the HR500's. The authors plan to test with an HR50.
- **Safety.** The panel is off until a port is set. It never switches a relay with RF on it. Each block is marked `SAFETY INTERLOCK` in the code and explained in [safety-interlocks.md](safety-interlocks.md).
- **Estimates.** The tuner's L and C are an estimate, good to about 10%, shown with a "~" and the step number. How they were reached is in [atu-values.md](atu-values.md).
- **AI-assisted.** Most of the code was written with Claude (Anthropic), directed and bench-tested by Rich. Check SDRoxide's contributing notes for a policy on this before sending, and say so in the pull request either way.

## 5. Order of steps

1. Finish the checklist (step 1 above).
2. Open a short issue on dividebysandwich/sdroxide describing the panel, with a screenshot, and ask whether they want it.
3. If yes, prepare the clean branch (steps 2 and 3), let Rich check it, then open the pull request.

## 6. Would make it easier to accept

Decided with Rich, 18 Oct 2026:

1. **A simulated amp.** To be built after development is finished. A built-in demo amplifier that fakes the serial replies (keying, a trip, a tune), so the author and reviewers can try the panel without a Hardrock, and the tests can cover the link end to end.
2. **Second tester and HobbyPCB's blessing.** Jim Veatch (WA2EUJ), HobbyPCB's owner, is likely to test it, which covers both an independent test and the manufacturer's view. Ask him also whether the schematics may be shared. He may not use SDRoxide, so he will need the standalone version of the panel (see the README's "Planned: standalone app"). The panel's layout is loosely based on his own HR500 Control Panel.
3. **Operating systems.** Tested on Windows 11 only. Say so in the issue and the pull request; Linux and macOS are untested.
4. **A video.** Rich will record a short one of a real transmission and a tune.
5. **Maintenance.** Rich will maintain the panel and handle HR500 bug reports. Say so in the pull request.
