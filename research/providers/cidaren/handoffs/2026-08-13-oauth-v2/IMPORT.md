# Import record

Imported: 2026-08-13

Source artifact: `cidaren-oauth-v2-handoff-20260813.zip` (user supplied)

Archive SHA-256:

```text
5c09f74d5c73df6339ad4daac48f51dff218f10219c9cdf417d9f2aa12384f70
```

## Imported scope

This directory preserves the handoff's Markdown documentation, evidence index,
and original `SHA256SUMS.txt`. The large analysis reports, browser probes,
Python reference implementation, and requirements file remain in the external
source archive and are not copied into the repository.

All 16 entries named by the source manifest were verified successfully before
this documentation slice was imported. A bounded credential scan of the
documentation and manifest found no live OAuth code, UserToken, JWT, or session
value. The manifest intentionally names files that are not present in this
directory because it describes the complete external archive.

## Evidence role

This is user-supplied first-party protocol research and authorized live-device
validation evidence. It is a Reference, not donor code. In particular, it
establishes a simpler assisted OAuth bootstrap than the earlier XWeb callback
capture hypothesis:

1. Core creates a short-lived pending login with independent random
   `state` and `authorize_marker` values; the marker must not equal `"2"`.
2. The same WeChat OAuth URL may be opened in the current WeChat environment,
   copied to desktop WeChat, or rendered as a QR code.
3. The user returns the final Cidaren callback URL through WebUI or another
   authenticated Asterism surface.
4. Core strictly validates and atomically consumes the callback code once.
5. The Provider performs the already-audited V2 P-256 ECDH, HKDF-SHA256 and
   AES-GCM exchange, persists the resulting session, and verifies it with a
   fresh `Student/Main` read.

Importing these documents does not by itself claim the Core pending-login API
or WebUI flow is implemented. Those remain implementation and verification
work.
