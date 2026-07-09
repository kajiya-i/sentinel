# Security Policy

## Supported Versions

| Version | Supported |
|---|---|
| 0.1.x | Yes |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

If you discover a security vulnerability, report it privately via
[GitHub Security Advisories](https://github.com/kajiya-i/sentinel/security/advisories/new).

Please include:

- A description of the vulnerability and its potential impact
- Steps to reproduce or a minimal proof-of-concept
- Affected crate(s) / component(s) and version(s)
- Any suggested mitigations if known

You can expect an acknowledgment within **7 days** and a resolution or status update
within **30 days**.

## Scope

Sentinel drives a headless browser against target URLs and sends collected evidence to an
AI provider. Security-relevant areas include:

- **Target-URL handling / SSRF** — Sentinel opens URLs; server-side deployments must guard
  against internal/metadata endpoints.
- **Secrets** — the AI provider API key (`ANTHROPIC_API_KEY`) must never be logged or leaked.
- **Captured data** — screenshots / DOM may contain PII; retention and access matter.

Vulnerabilities in third-party dependencies (chromiumoxide, reqwest, the browser itself,
etc.) should generally be reported upstream; issues in Sentinel's own handling of them
belong here.
