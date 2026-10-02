# INC-2291 timeline (UTC, 2026-08-14)

- 09:12 Checkout API starts returning TLS handshake errors.
- 09:20 On-call paged.
- 09:41 Cause found: the certificate on the API gateway expired at 09:12.
- 09:59 New certificate deployed; errors stop.
Outage: 09:12 to 09:59, 47 minutes.
