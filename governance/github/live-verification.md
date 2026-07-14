# Live GitHub Governance Verification

This file records observed GitHub state. It is not a substitute for the
desired-state JSON files in this directory.

## 2026-07-15 bootstrap verification

- Repository: `Lamentis-operating-systems/naome-memory`
- Ruleset: [`main-protection` (18954445)](https://github.com/Lamentis-operating-systems/naome-memory/rules/18954445)
- Enforcement: active on the default branch, with no bypass actors
- Current administrator bypass: `never`
- Merge method: squash only; merge commits and rebase merges disabled
- Required contexts:
  - `CI / required`
  - `DCO`
  - `CodeQL / Analyze (actions)`
  - `CodeQL / Analyze (rust)`
  - `Dependency Review / Dependency Review`
- Required rule types observed for `main`: pull request, linear history,
  deletion denial, non-fast-forward denial, required status checks, and
  CodeQL alert thresholds
- CODEOWNERS API errors: none
- Default workflow token: read-only; pull-request approval disabled
- Private Vulnerability Reporting: enabled
- Secret scanning and push protection: enabled
- Dependabot security updates: enabled
- Issues: enabled
- Projects, Wiki, Discussions, Downloads, auto-merge, merge commits, and
  rebase merges: disabled

The first two complete pull-request gate chains were
[`#11`](https://github.com/Lamentis-operating-systems/naome-memory/pull/11)
and [`#12`](https://github.com/Lamentis-operating-systems/naome-memory/pull/12).
Both completed every repository check successfully before squash merge.

The probe commit carrying this file was pushed directly to `main` and GitHub
rejected it with `GH013`. The response required a pull request, reported all
five expected status checks, and required CodeQL results for the probe commit.

The effective-rules API for `main` reports both `non_fast_forward` and
`deletion` from ruleset `18954445`. Destructive force-push and default-branch
deletion mutations were not executed as probes; their live enforcement is
therefore verified as configured state, not as a destructive rejection test.
