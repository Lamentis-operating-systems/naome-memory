# Security Policy

## Supported versions

NAOME Memory is pre-1.0 experimental software. Security fixes are applied to the latest commit on `main`; no released version currently carries a production-support promise.

## Reporting a vulnerability

Please use GitHub's **Private vulnerability reporting** feature for this repository: open the repository's Security tab, choose **Advisories**, and select **Report a vulnerability**. Do not file a public issue for a suspected vulnerability.

Include a minimal synthetic reproduction, affected commit or version, impact, and any suggested mitigation. Do not send real memory data, personal information, credentials, or third-party confidential material. Maintainers will acknowledge a complete report when available and coordinate validation and disclosure in the private advisory.

If Private Vulnerability Reporting is not visible, do not publish details. Contact the repository owner through the owning GitHub organization and ask for a private reporting channel.

## Scope and current non-claims

The PoC is intended for local, single-host, synthetic data. It does not claim encryption at rest, production PII safety, hostile multi-tenant isolation, distributed consistency, or protection from an attacker who controls the host. Security reports about integrity boundaries, receipt verification, cross-memory-space isolation, artifact traversal, SQLite ingestion, workflow supply chain, or release provenance are in scope.
