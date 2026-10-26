# DocKosha Security Overview

This document describes the **security posture** of the DocKosha platform.

## Encryption

All data is encrypted *in transit* (TLS 1.3) and *at rest* (AES-256).

## Access Controls

- Owner-level workspace control
- Per-surface access levels: `documents_access`, `data_rooms_access_all`
- Explicit per-room grants

### Link protections

1. Email OTP verification
2. Allowlist enforcement
3. NDA gating with signature capture

## Code Example

```
const link = await createLink({ otp: true, watermark: "CONFIDENTIAL" });
```

---

For questions contact security@dockosha.com.
