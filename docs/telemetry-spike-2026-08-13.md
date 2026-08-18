# GT-22 spike: how anything would leave the machine, where it would land, and how to prove it does not

- Research date: 2026-08-13
- Branch: `master`, commit `111cce0`
- Mode: dependency inspection of this tree plus external research. No network
  code was written and no account was created anywhere.
- Question from the card: the acceptance criterion is *"a user who has not
  consented produces not a single network packet, and that is visible from the
  code, not from a setting."* This document is about how to satisfy that
  literally, and about what the options actually cost if the answer ever
  changes.
- Companion document: [diagnostic-bundle-spike-2026-08-13.md](diagnostic-bundle-spike-2026-08-13.md)
  (GT-83 — the local file that would be the payload).

## Verdict

**Build no transport. Ship the local bundle and let the user attach it
themselves. Then make the absence of network code checkable rather than
promised.**

Three findings drive that.

1. **The baseline is genuinely clean, and it is worth protecting.** Verified
   today against `crates/app/Cargo.toml`, `crates/core/Cargo.toml`,
   `Cargo.lock` and `cargo tree`: **no async runtime, no HTTP client, no TLS
   stack**. A grep for `reqwest|hyper|tokio|rustls|native-tls|openssl|ureq|curl|
   schannel|webpki|sentry|posthog` returns nothing. The `windows` crate is
   present but its enabled features cover foundation, globalization, security,
   console, system information, threading, shell, windowing, filesystem and
   IOCTL — none of them touch WinHTTP, WinINet or ws2_32.
2. **Every option where the app performs the send adds an HTTP and TLS stack to
   the shipped binary unconditionally**, consent state notwithstanding, unless
   that code is excluded from the build entirely. The only transports that add
   literally nothing are the ones where a human does the upload.
3. **This tool's peer group has no telemetry at all**, and that is the
   expectation any addition would be judged against — see the prior-art
   section. Zero telemetry is the norm here, not an achievement to advertise.

## What other programs do

### Telemetry

| App | Opt-in / opt-out | What is collected | Shown to the user? | Destination | Outcome |
|---|---|---|---|---|---|
| **Firefox / Glean** | tiered: categories 1–2 default on, 3–4 default off with explicit consent | classified into four categories by identifiability — technical, interaction, stored content, highly sensitive | every metric is publicly documented per probe; each collection change needs a published data review | Mozilla's own pipeline | the reference-grade framework: Glean makes collecting an *undeclared* metric technically impossible |
| **Syncthing** | **opt-in, and "undecided" is a real third state** | rotating id for daily dedup, version, platform, folder and device *counts* — not contents | preview described in the GUI before submission (verified from forum discussion, not from the official docs) | aggregates published publicly | closest existing model to what this card describes |
| **Homebrew** | opt-out | package and tap names, install options, arch, OS, command names; explicitly excludes user ids, IPs, build logs | a notice before the first send, not a payload preview | InfluxDB, EU-hosted, 365-day retention | re-litigated in GitHub issues from 2016 through 2024 — a permanent low-grade trust tax from the default alone |
| **VS Code** | opt-out, four levels (`off`/`crash`/`error`/`all`) | crash reports, errors, usage | yes — `code --telemetry` dumps every possible event; a live trace command exists | Microsoft | spawned VSCodium, a fork whose entire purpose is stripping default-on telemetry |
| **Godot** | none shipped | — | — | — | an opt-in proposal was floated and never advanced; nothing shipped |
| **Krita** | none shipped | official statement: collects nothing whatsoever | — | — | KDE's separate opt-in `KUserFeedback` framework exists; Krita does not use it |
| **Audacity (2021)** | proposed opt-out | hashed IP for 24 h, OS and CPU for optional crash reports | **no** — that was the complaint | Google Analytics and Yandex Metrica | **the cautionary tale**: ~3 500 thumbs-down, "spyware" coverage, 50+ forks including Tenacity |
| **ShareX** | none | *"does not collect data of any kind"* | — | — | clean category exemplar |
| **Rufus** | none of its own | — | — | — | ships an option to help users skip *Windows'* telemetry prompts |
| WizTree, TreeSize, Everything, 7-Zip | **unverified** — no official privacy or telemetry statement was reached | — | — | — | absence of evidence, not evidence of absence; flagged rather than filled in |

Audacity is the important row and it is widely misread. The data was mild by
industry standards. What triggered the revolt was a legalistic privacy policy
appearing with no notice, bundled with a law-enforcement disclosure clause,
landing on top of an already-burned trust reserve from a separate CLA fight.
The same data, opt-in and previewed from day one, would very likely have passed
without incident. **It is a process and framing failure, not a data-volume
failure.**

Homebrew is the other half of the lesson: nothing ever blew up, but eight-plus
years of recurring complaints show that an opt-out default generates perpetual
friction even when the data is genuinely benign.

### What this means here

The peer group GameTrimmer will be judged by — WizTree-class Windows
utilities — treats zero telemetry as the floor. Any collection added later is a
real deviation from the category's implicit contract, not a routine update.
That does not close the door: Syncthing and Homebrew both show opt-in,
well-scoped telemetry coexisting with a trust-sensitive audience. It does mean
that if it is ever added, the Syncthing shape — undecided default, preview
before send, aggregate-only destination, small enumerable schema — is the fit,
and the VS Code and Homebrew shape is not.

## Transport options

| Option | Async runtime? | TLS stack? | Rust SDK? | Added to the shipped binary |
|---|---|---|---|---|
| `ureq` + `rustls`, hand-rolled POST | no (blocking) | yes | n/a | smallest of the networked options |
| `reqwest`, default features | yes (tokio) | yes | n/a | largest generic-HTTP option |
| Sentry (`sentry` crate) | maybe — the reqwest transport can pull tokio | yes | yes, official | reqwest-class, plus envelope machinery |
| PostHog (`posthog-rs`) | yes (async is the default) | yes | yes, official, actively maintained | reqwest-class, plus batching and event queue |
| Countly, Aptabase | whatever you hand-roll | whatever you pick | **no official Rust SDK** | same cost as a hand-rolled POST, plus maintaining wire-format compatibility |
| Plausible, Umami | — | — | no | **wrong shape entirely** — web pageview tools; named only to close the door |
| GitHub issue or `mailto:` via shell-open | no | no | n/a | **zero** |

Notes that matter for this card specifically:

- **Sentry** is the most capable option for crash handling and scrubbing —
  `send_default_pii` is off by default and `before_send` can redact fields —
  but `before_send` is a scrub hook, not a consent gate. The card's "show the
  exact payload before sending" requirement needs a preview step built in front
  of any SDK regardless of which one is chosen. Windows minidump support is a
  separate crate that re-launches the executable as a crash-reporter
  subprocess, which is a second binary path to reason about for the guarantee.
- **PostHog, Aptabase and Countly are architecturally wrong for this**, not
  merely heavy. All three assume a continuous behavioural event stream —
  always-on batching, session identity, funnels — which is a different consent
  and minimization shape from "one user-reviewed file, sent when the user
  chooses". Adopting one would mean fighting its defaults for no gain over a
  plain POST that would have to be written correctly either way.
- **GlitchTip and Bugsink** are the credible self-hosted targets if Sentry-style
  tooling is ever wanted: both speak the Sentry DSN protocol, so the Rust SDK
  points at them unmodified. Bugsink in particular runs on SQLite with no
  Redis or Celery, which matches this project's existing infrastructure
  instincts. Self-hosting Sentry proper (ClickHouse, Kafka, Postgres, Redis,
  Snuba) is not appropriate for a solo developer — GlitchTip and Bugsink exist
  precisely because it is not.
- **A bare POST endpoint is the most demanding option to run honestly**, not
  the simplest. It needs a TLS backend with Windows cert-store access, an
  offline spool with retry and dedupe, a payload size cap, and rate limiting —
  scanners find open endpoints fast. That is exactly the kind of code that
  becomes an unmaintained edge case in a hobby project.

## Where it would land

| Option | Cost (2026, verified today) | Retention control | Ops burden | Posture |
|---|---|---|---|---|
| SaaS free tier | $0 to the cap — Sentry 5 000 errors/mo, 30-day retention; PostHog 1 M events/mo | vendor-set | lowest | the developer is still the data controller; the vendor is a processor, and a DPA becomes part of the story |
| Self-hosted VPS | Hetzner CX-class ≈ €5.49–8/mo; DigitalOcean from ≈ $4/mo | full | real — patching, certs, backups, uptime, and a plan for when payment stops | controller and processor in one; simplest legally, heaviest operationally |
| Object storage + presigned upload | Cloudflare R2: $0.015/GB-month, **$0 egress**, free tier 10 GB and 1 M writes/mo | full, via lifecycle rules | low, but something must still mint the URL | cheapest credible "real" backend if one is ever needed |
| Private git repo of bundles | $0 | awkward — git does not forget, and deletion on request means rewriting history (this project already knows that cost from the 2026-08-03 rewrite) | very low | simple, but the deletion story is the sharp edge |
| **No server — the user attaches the file** | $0 | the user chooses where it lands | **zero** | **cleanest available**: the app never makes a network call, so there is no transmission moment inside it at all |

One failure mode is easy to miss. For any option where the app dials a URL, an
abandoned endpoint means either silent upload failures forever or — worse — an
expired domain that someone else can re-register and start receiving bundles
at. The "human sends it" options have no endpoint to abandon. A SaaS free tier
at least degrades visibly rather than dangerously.

GitHub's attachment limit for non-image files is 25 MB via the browser
uploader, comfortably above the bundle sizes the companion document projects.

## Making the guarantee provable

This is the part the card actually hinges on. A boolean check in front of a
`post()` call is a setting, not a guarantee: the TLS stack is still linked and
still reachable from any path that forgets the check, now or after a future
edit.

### The architecture

```
crates/
  core/        # unchanged
  app/         # unchanged default dependencies
  telemetry/   # NEW — the only crate allowed an HTTP or TLS dependency
```

```toml
# crates/app/Cargo.toml
[dependencies]
gametrimmer-telemetry = { path = "../telemetry", optional = true }

[features]
telemetry = ["dep:gametrimmer-telemetry"]
```

The default feature set ships with `telemetry` off. **This project already has
the pattern**: `headless` and `cli-apply` are off by default and documented as
excluded from the release build. Every call site is behind
`#[cfg(feature = "telemetry")]`, so a shipped build does not contain the code
to call — not code that chooses not to call. Runtime consent still gates
behaviour *within* a telemetry-enabled build, but as a second independent gate,
never as the only one.

The crate boundary buys more than a feature flag alone: no HTTP or TLS crate
appears in `cargo tree` for the default build, no such symbols get linked, and
reviewing "does this touch the network" collapses to "is `crates/telemetry` in
this build's dependency graph".

### How to check it

1. **`cargo tree` grep** against the default-feature build for
   `reqwest|hyper|tokio|rustls|native-tls|ureq` and their transitives. This is
   exactly the check run for this document; it is cheap and deterministic.
2. **`cargo deny`** with an explicit ban list, so a future PR that makes an HTTP
   crate non-optional fails CI rather than relying on a human skimming a diff.
3. **Import-table inspection of the shipped `.exe`** — `dumpbin /imports`, or
   the open-source `Dependencies.exe`, confirming `WININET.dll`, `WINHTTP.dll`
   and `WS2_32.dll` are absent. This is the most convincing check available
   because it inspects the artifact the user is about to run, not source they
   would have to trust was what built it.
4. **A CI job** asserting both, so "the default build has no network code"
   becomes a red badge the moment it stops being true.
5. Publish commands 1 and 3 as a documented check anyone can run against a
   downloaded release.

`cargo geiger` is worth a mention as a weak second signal — it would show a new
cluster of unsafe-heavy crates if an HTTP client ever arrived transitively
through something unexpected — but it is not the primary tool here.

## Staged path

**Stage 0 — now, through the local-bundle milestone.** No transport code
anywhere, `crates/telemetry` does not exist. The only "upload" is the user
attaching the bundle to a GitHub issue themselves, with the app doing nothing
more than `explorer.exe /select,<bundle>.zip`. Zero network code in the
repository, not merely in the default build — the strongest form of the
guarantee, and free.

*Graduate only when* there is a real, recurring case of someone wanting to
report something and the manual attach is the thing actually blocking them —
not a hypothetical.

**Stage 1 — first automated send, if that friction turns out to be real.** Add
`crates/telemetry` as the gated, off-by-default crate above, using `ureq` +
`rustls` (smallest maintained HTTP+TLS combination) against either a cheap VPS
or a presigned R2 upload. R2's free tier and zero egress remove the
surprise-bill risk that a SaaS overage carries. **Ship the CI assertion and the
binary-inspection check in the same PR that adds the capability** — the
guarantee and the capability land together, never the capability first.

*Graduate only when* Stage 0 has run long enough that the volume estimate is
measured rather than guessed, and there is still appetite to own an endpoint's
uptime indefinitely.

**Stage 2 — only if scrubbing or volume outgrows a hand-rolled endpoint.** Swap
the transport *inside* `crates/telemetry` for a self-hosted GlitchTip, which is
a DSN change rather than a rewrite. Named now only so Stage 1's crate boundary
is drawn where it will not have to move.

## Consequences for GameTrimmer

1. **GT-22 stays parked, but for a documented reason now rather than a
   deferred one.** The card's own framing — "first see whether there are users
   at all" — survives this research intact and is reinforced by the peer-group
   finding.
2. **One deliverable can be built today with no transport at all**: the
   compile-time guarantee and its CI check. It costs nothing, it is worth
   having before any temptation arrives, and it converts the About text's
   restraint into something a sceptic can verify. Worth its own card.
3. **When the local bundle ships, the delivery affordance is "reveal the file
   in Explorer" plus a link to the issue tracker** — not an upload button.
4. **If telemetry is ever built, the shape is fixed by this document**: opt-in
   with a real undecided state, payload preview before send, aggregate-only
   destination, small enumerable schema, and an announcement that precedes the
   capability rather than accompanying it. That last point is the Audacity
   lesson and it is the one that actually damages projects.
5. **Do not adopt a product-analytics SDK at any stage.** Wrong architecture,
   not merely heavy.

## Sources (all accessed 2026-08-13)

- [Mozilla — Firefox data collection categories](https://wiki.mozilla.org/Firefox/Data_Collection) and [telemetry review guidelines](https://firefox-source-docs.mozilla.org/toolkit/components/telemetry/internals/review.html)
- [Syncthing — configuration docs (`urAccepted`)](https://docs.syncthing.net/users/config.html)
- [Homebrew — Analytics](https://docs.brew.sh/Analytics); [issue #142](https://github.com/Homebrew/brew/issues/142), [issue #18479](https://github.com/Homebrew/brew/issues/18479)
- [VS Code — Telemetry](https://code.visualstudio.com/docs/configure/telemetry); [VSCodium](https://github.com/VSCodium/vscodium)
- [Godot proposals — discussion #10295](https://github.com/godotengine/godot-proposals/discussions/10295)
- [Krita — Privacy statement](https://krita.org/en/privacy-statement/); [KDE KUserFeedback](https://github.com/KDE/kuserfeedback)
- Audacity: [TechRadar on the reversal](https://www.techradar.com/news/audacity-reverses-opt-in-telemetry-plans-following-user-revolt); [the fork discussion](https://github.com/audacity/audacity/discussions/1225)
- [ShareX — Privacy policy](https://getsharex.com/privacy-policy)
- [Sentry Rust SDK features](https://lib.rs/crates/sentry/features); [issue #598 on `send_default_pii`](https://github.com/getsentry/sentry-rust/issues/598); [sentry-rust-minidump](https://docs.rs/sentry-rust-minidump)
- [GlitchTip vs self-hosted Sentry](https://danubedata.ro/blog/self-host-sentry-glitchtip-error-tracking-2026); [Bugsink](https://github.com/bugsink/bugsink)
- [posthog-rs](https://github.com/PostHog/posthog-rs); [Countly SDK list](https://support.countly.com/hc/en-us/articles/360037236571-SDK-Repos-and-Features); [Aptabase](https://github.com/aptabase/aptabase)
- [rustls-native-certs](https://github.com/rustls/rustls-native-certs/blob/main/README.md)
- [cargo-deny](https://www.rustfaq.org/en/how-to-use-cargo-deny-for-dependency-auditing/); [Dependencies.exe](https://github.com/lucasg/Dependencies)
- [GitHub attachment size limits](https://github.com/orgs/community/discussions/146417); [AWS presigned upload pattern](https://docs.aws.amazon.com/AmazonS3/latest/userguide/PresignedUrlUploadObject.html)
- [Cloudflare R2 pricing](https://egresscost.com/cloudflare/); [Hetzner / DigitalOcean 2026 pricing](https://betterstack.com/community/guides/web-servers/hetzner-cloud-review/)
- Local verification: `crates/app/Cargo.toml`, `crates/core/Cargo.toml`, `Cargo.lock`, `cargo tree -p gametrimmer`

Prices, free-tier limits and crate maintenance status are volatile; the date
above marks when each was checked, not when it stops being true. Re-verify
before committing to any vendor.

## Explicit uncertainties

- Syncthing's exact preview-before-send UI is described in forum discussion,
  not in the official documentation; the mechanism is corroborated but the
  screenshot-level detail is not.
- WizTree, TreeSize, Everything and 7-Zip have no official privacy statement
  that could be reached. Their rows say "unverified" rather than "no
  telemetry", and the peer-group conclusion rests on the products that *were*
  verified.
- Godot's telemetry discussion is open and unresolved, not a documented
  rejection.
- Binary-size figures for the HTTP options are qualitative. Nothing was
  compiled for this spike, so the table ranks the options rather than
  measuring them.
