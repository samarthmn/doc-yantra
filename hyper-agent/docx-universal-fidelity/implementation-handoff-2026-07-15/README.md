# DOCX universal fidelity implementation handoff

Date: 2026-07-15  
Branch: `hyper-agent/docx-universal-fidelity`  
Current commit: `69525d0`  
Status: stopped at the owner's request; worktree changes are preserved and uncommitted

> **Continuation (2026-07-15):** the owner decisions recorded here are resolved and
> the next round of work is specified in
> [`../agent-tasks-6-on-demand-fonts.md`](../agent-tasks-6-on-demand-fonts.md)
> (on-demand exact-font acquisition, fontless package → `gdocs-anchors-toc`;
> supersedes the same-day metric-overlay design) and
> [`../agent-tasks-7-table-geometry.md`](../agent-tasks-7-table-geometry.md)
> (vector geometry probe + table edge conventions → `gdocs-tables-longdoc`).
> See the Resolutions section of
> [02-blockers-and-owner-actions.md](./02-blockers-and-owner-actions.md).
> Step 0 everywhere: review, verify, and commit the stopped worktree patch first.

This folder records the state of the `docx-universal-fidelity` implementation,
the two remaining frozen fidelity failures, the unverified remediation in the
worktree, and the decisions needed to continue. It is the permanent handoff;
the files under the repository `tmp/` directory remain diagnostic artifacts.

No fidelity threshold, manifest, source fixture, or ground-truth PDF was changed
to improve a score. No `pdf-core` source file was changed. Nothing was pushed,
released, or merged.

## Read this first

1. [Current status](./01-current-status.md) separates committed work from the
   stopped, uncommitted patch.
2. [Blockers and owner actions](./02-blockers-and-owner-actions.md) lists the
   decisions and assets needed from the owner.
3. [Technical findings and solution path](./03-technical-findings-and-solution-path.md)
   records the LibreOffice-inspired design and the evidence against quick score
   hacks.
4. [Verification and reproduction](./04-verification-and-reproduction.md) lists
   the exact gates, current results, and commands to resume safely.

## Outcome at stop time

The engine now converts all three new Google Docs fixtures with exact Word page
counts. Two of the three new frozen SSIM gates still fail:

| Case | Pages ours/truth | Frozen threshold | Current targeted SSIM | Result |
| --- | ---: | ---: | ---: | --- |
| `gdocs-anchors-toc` | 10/10 | 0.90 | 0.6898937923 | blocked |
| `gdocs-tables-longdoc` | 44/44 | 0.90 | 0.8124519676 | blocked |
| `gdocs-columns` | 3/3 | 0.90 | 0.9280746825 | pass |
| `complex` regression guard | 6/6 | 0.90 | 0.9374006584 | pass |

The latest aggregate 18-case run was not repeated after the final uncommitted
numbering and geometric-wrap changes because the owner stopped execution. The
current-source targeted runs above are the authoritative final measurements.

## Safety boundary

The goal should be interpreted as broad DOCX support with high fidelity, not as
a promise that every arbitrary DOCX can always render. The repository contract
requires fail-closed errors for visible constructs the engine cannot prove safe.
Removing that boundary would trade explicit failures for silently corrupted
documents.
