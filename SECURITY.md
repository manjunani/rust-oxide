# Security Policy

## Supported Versions

The `main` branch is the only supported version at this time. Tagged
releases follow.

| Version | Supported          |
|---------|--------------------|
| main    | :white_check_mark: |
| < 0.1.x | :x:                |

## Reporting a Vulnerability

**Do not file public GitHub issues for security vulnerabilities.** Use the
GitHub private vulnerability reporting channel instead:

1. Go to <https://github.com/manjunani/rust-oxide/security/advisories/new>
   (requires a GitHub account; the link is open to all logged-in users).
2. Fill in the advisory form. Maintainers are auto-notified.
3. The maintainer (`@manjunani`) will acknowledge within **72 hours** and
   coordinate disclosure timing with you.

If GitHub private reporting is unavailable for any reason, email the
repository owner directly via the address in their GitHub profile. PGP key
on request.

When reporting, include:

* A description of the issue, severity, and impact (CVSS optional but
  helpful).
* A minimal reproducer (test, script, `curl` invocation, or commit hash).
* Affected crate(s) and the commit / tag you observed the bug on.
* Your preferred public credit (handle + URL) once the fix ships.

### Disclosure timeline

| Stage | Target SLA |
|-------|-----------|
| Acknowledgement | 72 hours |
| Triage + severity assessment | 7 days |
| Patch landed on `main` | 30 days (sooner for criticals) |
| Coordinated public disclosure | when fix released, or at reporter's request |

### Scope

In-scope crates: every workspace member under `crates/`. Out-of-scope:
issues in transitive dependencies that we cannot patch upstream — file those
with the original maintainer, then open a `[security] update X` issue here
so we can bump the pin.
