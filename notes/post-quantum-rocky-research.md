# Rocky Linux's post-quantum announcement — what it is, and what (if anything) kumaOS does

Status: research written 2026-11-18, two days after Rocky Linux 10.3 GA
(2026-11-16) — the release the user saw reported as "Rocky just added
post-quantum encryption." This note traces the claim to primary sources,
identifies the actual upstream layer, and sizes the implications for a
Fedora bootc base. Labels: CONFIRMED = verified from a primary source on
2026-11-18; REPORTED = secondary source; UNVERIFIED = no source found.

## Verdict

**Nothing to do for 45.x — this is a Rocky/RHEL-story item, and Fedora
already carries the same capability, in most cases months earlier.**
The reasoning in one paragraph:

- What Rocky "just added" is the RHEL 10.x PQC program arriving in
  Rocky's mirror of it: ML-KEM hybrid key exchange in OpenSSH (FIPS
  mode) and libreswan's IKEv2, ML-DSA signatures across the TLS/PKCS#11/
  OpenPGP stack, and a `NO-HMAC-SHA1` crypto-policies subpolicy (§1).
  Every named item comes from `crypto-policies` and the distro
  components above it — and `crypto-policies` is **one upstream shared
  by Fedora and RHEL**; RHEL backports from the same gitlab the Fedora
  packages build from (§2).
- kumaOS's base (`quay.io/fedora/fedora-bootc:45`, config.rs:41) already
  ships OpenSSH 10.5p1 — where ML-KEM hybrid kex has been the *default*
  since OpenSSH 10.0 (April 2025) — and crypto-policies 20260713, whose
  DEFAULT policy lists `mlkem768x25519-sha256` **first** for both ssh
  client and server. The `NO-HMAC-SHA1` subpolicy Rocky 10.3
  "introduces" has been in Fedora's 20260713 snapshot since July 2026.
  PQ key exchange is already default-on for this image's sshd (§3).
- The image's hardening floor pins nothing crypto-algorithmic — sysctls,
  kargs, the firewalld zone, NTS vendors, faillock — so a base
  crypto-policies bump is *not* a two-file change; the floor is
  PQ-neutral. The nearest canary the smoke already carries is the NTS
  check, since NTS rides TLS and crypto-policies governs chrony's TLS
  backend (§3).
- LUKS/dm-crypt has no post-quantum angle at all: the disk key is
  symmetric and unlocked by a passphrase or a PCR-bound secret — there
  is no key exchange to make quantum-vulnerable (§3).

## 1. What Rocky actually shipped

- **Rocky 10.3, GA 2026-11-16** — the release behind the news cycle:
  - `libreswan` 5.4: ML-KEM-768 and ML-KEM-1024 post-quantum key
    exchange in IKEv2, plus multiple key exchanges per RFC 9370
    (https://rockylinux.org/news/rocky-linux-10-3-ga-release,
    https://docs.rockylinux.org/10/releases/release_notes/10_3/).
    CONFIRMED.
  - SSH in **FIPS mode** now supports and prefers the hybrid PQ key
    exchanges `mlkem768nistp256-sha256` and `mlkem1024nistp384-sha384`.
    CONFIRMED (same two sources).
  - `crypto-policies` gains a `NO-HMAC-SHA1` subpolicy (compliance-
    scanner SHA-1-in-HMAC disable). CONFIRMED (same two sources).
  - `rust-sequoia-sqv` 1.5.0 with full PQC per the final OpenPGP
    RFC 9980; `audit` 4.2.1 with TLS 1.3 PQC for remote logging;
    OpenSC 0.27.1 with PKCS#11 3.2 PQC; `golang` 1.27.3 with a
    `crypto/mldsa` package. CONFIRMED (same two sources).
- **Rocky 10.2, GA 2026-05-28** — the broader PQC pass, and the
  sharpest edge in the whole story: the `FUTURE` system-wide crypto
  policy now allows **only** hybrid ML-KEM key exchange (all
  traditional kex removed — Rocky's own note warns this "will break
  connections to endpoints that do not support PQC, including most of
  the public internet", and states it does not affect `DEFAULT`). Also:
  OpenSSH ML-KEM hybrid kex in FIPS mode, libssh PQ/T hybrids, p11-kit
  0.26.1 PQ definitions in PKCS#11 headers, Directory Server ML-DSA
  (44/65/87) TLS certificates, podman-sequoia composite PQ signatures
  (ML-DSA-87+Ed448)
  (https://rockylinux.org/news/rocky-linux-10-2-ga-release,
  https://docs.rockylinux.org/10/releases/release_notes/10_2/).
  CONFIRMED.
- **Rocky 10.1, GA 2025-11-25**: OpenSSL prioritizes PQ over classical
  algorithms; PQ enabled in GnuTLS and more policies
  (https://rockylinux.org/news/rocky-linux-10-1-ga-release). CONFIRMED.
- **Rocky 9.8, GA 2026-05-27**: GnuTLS 3.8.10 with ML-KEM hybrid kex
  and ML-DSA; p11-kit 0.26.1
  (https://rockylinux.org/news/rocky-linux-9-8-ga-release). CONFIRMED.
- **The CIQ press release (2026-02-04)** — likely the other thing the
  user half-remembers: CIQ (Rocky's founding support vendor, not the
  Rocky project) put NSS 3.112 with ML-KEM and ML-DSA through CAVP
  certification for "Rocky Linux from CIQ" 9.6, first Enterprise Linux
  NSS module in NIST's Modules-in-Process list
  (https://ciq.com/press/ciq-nss-module-first-to-achieve-cavp-certification-for-post-quantum-cryptography-algorithms).
  CONFIRMED as CIQ's claim; it is a *commercial support product*
  milestone (FIPS 140-3 validation runway, projected Q2 2027), not
  something stock Rocky ships. The follow-on (2026-04-16) bundles this
  into the RLC Pro compliance platform
  (https://ciq.com/press/ciq-delivers-the-first-enterprise-linux-compliance-platform-for-federal-cryptographic-calidation-and-post-quantum-readiness).
  CONFIRMED (press releases).
- **Mechanism**: Rocky rebuilds RHEL source; these items are RHEL 10.1/
  10.2/10.3 backports wearing Rocky's release notes (the 10.3
  "Security Features" section is RHEL's list verbatim in substance).
  REPORTED (structural — Rocky's rebuild model is documented upstream;
  the item-for-item mapping to RHEL 10.x release notes is visible in
  the docs links above). Nothing here is a Rocky-original feature.

## 2. The upstream layer: one crypto-policies for Fedora and RHEL

- **`crypto-policies` is the delivery vehicle, and it is shared.** The
  gitlab (`redhat-crypto/fedora-crypto-policies`) is the upstream both
  Fedora packages and RHEL engineer from — the F45 changelog and the
  RHEL feature lists name the same commits
  (https://gitlab.com/redhat-crypto/fedora-crypto-policies/-/commits/master,
  https://packages.fedoraproject.org/pkgs/crypto-policies/crypto-policies/fedora-45.html).
  CONFIRMED.
- **The F45 changelog is Rocky 10.3's announcement, dated earlier**:
  - 2026-03-31 (`20260331-1.git4c567b2`): "FIPS: allow NIST hybrid
    kexes for openssh", "libssh: enable NIST ML-KEM hybrids",
    "libssh: add sntrup761x25519-sha512/mlkem768x25519-sha256".
  - 2026-07-08 (`20260710-1.gitb952499`): "policies/modules/
    NO-HMAC-SHA1: introduce" — the exact subpolicy Rocky 10.3 ships
    four months later — and "openssh: add mldsa44-ed25519 signature
    algorithm".
  - 2026-07-13 (`20260713-1`): sequoia SLH-DSA signature algorithms.
  CONFIRMED (Fedora package changelog,
  https://packages.fedoraproject.org/pkgs/crypto-policies/crypto-policies/fedora-45.html).
  **Fedora 45 ships crypto-policies 20260713-2.gitc42da77.fc45.**
  CONFIRMED (same page; matches the pin recorded in
  notes/45.0.0-upgrade-path-research.md §6).
- **PQ is default-on, not opt-in, in Fedora's DEFAULT policy.** The
  generator's pinned test output for `DEFAULT-opensshserver.txt`
  (identical for `FEDORA43-opensshserver.txt`) reads:
  `KexAlgorithms mlkem768x25519-sha256,mlkem768nistp256-sha256,mlkem1024nistp384-sha384,curve25519-sha256,…`
  — the ML-KEM hybrid is **first/preferred**, and the NIST-curve
  hybrids Rocky announces are present too
  (https://gitlab.com/redhat-crypto/fedora-crypto-policies/-/blob/master/tests/outputs/DEFAULT-opensshserver.txt).
  CONFIRMED. Escape hatches exist in the other direction: a `NO-PQ`
  module strips it, `TEST-PQ` widens it (file list,
  https://packages.fedoraproject.org/pkgs/crypto-policies/crypto-policies/fedora-45.html).
  CONFIRMED.
- **FUTURE is the aggressive end, and only on RHEL 10 does it drop
  traditional kex entirely.** Fedora carries `FUTURE.pol` but no
  Fedora release sets it by default; Rocky's 10.2 warning
  ("breaks connections to … most of the public internet") is a
  REL-follower restating the RHEL 10.2 change. CONFIRMED (Rocky note,
  §1) for the RHEL behavior; Fedora's FUTURE.pol content not
  byte-diffed — UNVERIFIED at the byte level, immaterial to kuma.
- **OpenSSH upstream context**: PQ kex default since OpenSSH 9.0
  (April 2022, `sntrup761x25519-sha512`); `mlkem768x25519-sha256`
  added in 9.9 and made the default in 10.0 (April 2025); 10.1 began
  warning when a connection does not use PQ kex
  (https://www.openssh.org/pq.html). **Fedora 45 ships openssh
  10.5p1** (https://packages.fedoraproject.org/pkgs/openssh/openssh/fedora-45.html).
  CONFIRMED. Note the flavor difference: RHEL/FIPS prefers the
  *NIST-curve* hybrids (`mlkem768nistp256-sha256`); Fedora's DEFAULT
  prefers the *x25519* hybrid (`mlkem768x25519-sha256`). Both are
  FIPS 203 ML-KEM hybrids; the classical half differs. CONFIRMED
  (policy outputs + Rocky notes).

## 3. kumaOS-specific implications

- **The base already carries it.** `DEFAULT_BASE =
  "quay.io/fedora/fedora-bootc:45"` (src/config.rs:41), and the tree
  contains **zero** `update-crypto-policies` calls or kex/cipher
  overrides (grep over src/). Every image therefore rides
  crypto-policies DEFAULT on OpenSSH 10.5p1: sshd's offered kex list
  leads with `mlkem768x25519-sha256` (§2). CONFIRMED (tree +
  §2 sources).
- **sshd on the image needs nothing.** The sshd block enables
  `sshd.service` and its only shipped drop-in is
  `40-kuma-keys.conf` — `AuthorizedKeysFile .ssh/authorized_keys
  /etc/kuma/keys/%u` (src/containerfile/blocks.rs:4121-4132, :2074,
  emitted at :4847-4880). No `KexAlgorithms`/`Ciphers` line anywhere,
  so sshd inherits the policy. Key exchange negotiation is transparent
  to authentication: the smoke's `sshpass + BatchMode` pubkey path
  (scripts/smoke.sh:1301-1317) is unaffected by which kex both ends
  pick, and both ends of that lane (host client, guest sshd) are
  PQ-capable, so no negotiation failure is even reachable there. The
  world-readable-private-key scar is a file-permission refusal
  *before* kex matters — orthogonal. CONFIRMED (tree read).
- **The hardening floor is PQ-neutral, so this is NOT a two-file
  change.** The floor ships exactly six files plus one authselect
  flip (blocks.rs:4395-4409): `70-kuma-hardening.conf` (sysctls),
  `05-kuma-hardening.toml` (kargs: `init_on_free=1`,
  `page_alloc.shuffle=1`, `vsyscall=none`, `vdso32=0`,
  `module.sig_enforce=1`, `rd.shell=0`, `rd.emergency=halt`,
  `systemd.ssh_auto=no`, `random.trust_cpu=off`, blocks.rs:4298-4300),
  the public zone, `chrony.conf`, the NM MAC conf, `faillock.conf`.
  None pins a cipher, kex, TLS version, or crypto-policy. The smoke's
  hardening lap mirrors this: a 50-key sysctl read, the kargs on
  /proc/cmdline, the zone's services + slirp rich rule, ≥2 NTS sources
  with both vendors named and no `pool` line, and faillock's PAM
  wiring (scripts/smoke.sh:4430-4534). **No assertion in the smoke
  pins anything a crypto-policies bump could move** — a base policy
  snapshot bump sails through unchanged, as it in fact did at the F45
  rebase (crypto-policies 20251128 → 20260713 rode 45.0.0 with no
  floor edit; notes/45.0.0-upgrade-path-research.md §6, which also
  flagged the one real interaction: the doctor/smoke NTS check, since
  NTS rides TLS and crypto-policies governs chrony's TLS backend).
  CONFIRMED (tree read).
- **LUKS/disk encryption: no PQ angle — stated plainly so it can be
  dismissed.** The image's encrypted root is dm-crypt/LUKS: a
  symmetric volume key unlocked by a passphrase (or a TPM-bound
  secret). There is no key exchange in that path to attack with a
  quantum computer, and no kuma declaration touches one — the smoke's
  LUKS interaction is "root was unlocked" plus the hibernate/swapfile
  kargs (smoke.sh:1378, :765-780). Any future PQ story for disk
  encryption lives in volume-key *distribution* (e.g. the 10.2-era
  `clevis-pin-trustee` remote-attestation pin,
  https://docs.rockylinux.org/10/releases/release_notes/10_2/), which
  kuma does not ship. CONFIRMED (tree read; the crypto reasoning is
  standard — no source needed).
- **NTS time sources**: two vendors, Cloudflare and Netnod, curated in
  `CHRONY_CONF` (blocks.rs:4349-4365) and asserted by the smoke
  (smoke.sh:4513-4528). NTS is TLS 1.3; if either vendor's front end
  offers a PQ TLS group and the base's OpenSSL offers X25519MLKEM768,
  the handshake may already be hybrid — kuma pins the *vendors*, not
  the algorithms, so nothing to change either way. The NTS check is
  the existing detector if a base policy change ever *breaks* these
  handshakes (a FUTURE-style PQ-only policy is the scenario; DEFAULT
  is not). CONFIRMED (tree) / vendor TLS group support UNVERIFIED
  (not our pin to make).
- **`notes/peer-distros-research.md` has no Rocky entry** (grep: zero
  Rocky mentions) — this note is the first. The new fact updates
  nothing there: every §-item in that file stands, and the PQC story
  slots in as a new "Rocky/RHEL tracks what Fedora already shipped"
  data point beside §2's greenboot finding. CONFIRMED (grep).

## 4. Verdict

**Nothing / watch — no 45.x work, no declaration line, no smoke
assertion.**

- The user's framing inverts the actual direction of travel: Rocky
  didn't add a capability Fedora lacks; RHEL (and therefore Rocky)
  finished delivering to its *FIPS/compliance* audience what Fedora
  has shipped by default since April 2025 (ML-KEM kex in OpenSSH) and
  July 2026 (the NO-HMAC-SHA1 subpolicy, the NIST-hybrid FIPS kex
  allowlist). The genuinely RHEL-exclusive part is certification
  (CIQ's CAVP/CMVP runway), which no Fedora-family distro can carry —
  FIPS *validation* is a binary-level federal process, not a config.
- kumaOS rides DEFAULT policy with zero crypto overrides, so the
  image's sshd already negotiates an ML-KEM hybrid kex, its TLS stacks
  already offer PQ groups where the components support them, and the
  F46 rebase will carry whatever crypto-policies ships next without a
  kuma-side edit. The "two-file change" rule for the hardening floor
  does not trigger: the floor pins posture (sysctls, kargs, zone,
  NTS vendors, faillock), not algorithms.
- The watch items, for honesty rather than action:
  1. **If a future Fedora DEFAULT ever tightens the way RHEL's FUTURE
     did** (PQ-only kex), the first symptoms on this image would be
     the smoke's ssh lane failing against non-PQ endpoints and the NTS
     check on a vendor that lags — both already-detected failures, no
     new assertion needed.
  2. **OpenSSH 10.1+'s non-PQ warning** (WarnWeakCrypto) can put a
     "connection is not using a post-quantum key exchange" banner in
     user sessions when sshing to legacy hosts. Cosmetic; if it ever
     generates questions, the answer is a per-host `WarnWeakCrypto
     no-pq-kex`, not a policy change (https://www.openssh.org/pq.html).
     REPORTED (upstream doc; not reproduced on this image).
  3. **RHEL's FUTURE-policy direction** is the signal to watch for
     where the whole ecosystem is headed: PQ-only kex as the *end
     state*, with DEFAULT riding meanwhile. If Fedora's DEFAULT ever
     moves that way, it arrives via a crypto-policies snapshot bump
     and the rebase test plan (notes/45.0.0-upgrade-path-research.md
     §6) already names the ssh/crypto-policies interaction as its
     check.
- What would change the verdict: a Fedora change proposal flipping
  DEFAULT's kex ordering or removing traditional kex (track
  `fedora-crypto-policies` commits and the Fedora change process — the
  2026-03-31 "FIPS: allow NIST hybrid kexes" commit shows how these
  arrive). Until then: this is a Rocky/RHEL compliance story, and the
  actionable answer for kumaOS is a link to this file the next time
  the question comes up.
