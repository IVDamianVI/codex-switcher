# Security policy

## Reporting a vulnerability

Do not report security vulnerabilities in a public issue and never attach a
Codex authentication file, access token, refresh token, API key, or other
credential.

Use GitHub's private vulnerability reporting feature for this repository. If it
is not enabled, contact the maintainer privately through a verified contact
method listed on the maintainer's GitHub profile.

Include the affected version, operating system, reproduction steps, and impact.
Replace account identifiers, filesystem paths, tokens, and other personal data
with clearly marked placeholders.

## Scope

Security-sensitive areas include:

- credential file permissions and lifecycle;
- profile switching and atomic file replacement;
- temporary authentication directories;
- command execution and environment handling;
- accidental disclosure of authentication or account information.

Please allow reasonable time for investigation before publishing details.

