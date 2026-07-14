# GitHub Governance Bootstrap

The JSON files in this directory describe desired state. They are not evidence that the live repository has been configured. Apply them only after the bootstrap commit is pushed and the initial checks have completed successfully.

## Preconditions

1. Authenticate `gh` as an administrator of `Lamentis-operating-systems`.
2. Stop if `Lamentis-operating-systems/naome-memory` already exists; do not invent a different name.
3. Install the DCO GitHub App for this repository and enable web commit sign-off. If organization policy prevents installation, stop rather than omitting the `DCO` gate.
4. Enable GitHub Private Vulnerability Reporting.
5. Verify that the committed SSH public key in `allowed_signers` is registered as the maintainer's GitHub signing key before creating release tags.

Create the public repository from the existing source:

```console
gh repo create Lamentis-operating-systems/naome-memory \
  --public \
  --source=. \
  --remote=origin \
  --push \
  --description="Experimental deterministic memory consolidation and episodic-retention engine for the NAOME autonomous agent kernel."
```

Apply repository settings, topics, Actions permissions, security features, labels, milestone, and slice issues from `repository-settings.json` and `project-bootstrap.json` through `gh api`. Security endpoints vary by organization plan; read each response and verify the resulting UI/API state rather than treating a successful command as proof.

## Main ruleset

Do not install `main-protection.json` until real completed checks establish the exact contexts:

- `CI / required`
- `DCO`
- `CodeQL / Analyze (actions)`
- `CodeQL / Analyze (rust)`
- `Dependency Review / Dependency Review`

Then create the repository ruleset with the contents of `main-protection.json`. It has no bypass actor and requires pull requests with zero approvals, resolved review threads, strict up-to-date status checks, linear history, squash-only merging, no deletion, and no force push.

## Live verification

After installation, use API reads and a disposable `task/governance-probe` pull request to verify:

- the ruleset targets `main` and is active;
- a direct push to `main` is rejected;
- force push and branch deletion are rejected;
- CODEOWNERS has no syntax error;
- all five required contexts report on the probe pull request;
- a squash merge succeeds only when every context passes and threads are resolved;
- workflow tokens are read-only and cannot approve pull requests;
- issues are on while Projects, Wiki, Discussions, merge commits, rebase merges, auto-merge, and Downloads are off where the API exposes them;
- Dependabot alerts/security updates, secret scanning push protection, and Private Vulnerability Reporting are active.

Record the API responses and probe pull request URL in the bootstrap issue. Configuration files alone are not live-governance proof.
