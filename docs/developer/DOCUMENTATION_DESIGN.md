# Documentation Design and Evidence Standards

This document records the research behind the README and documentation structure, and the standards
for maintaining it. Product behavior belongs in the versioned specifications; this document governs
how that behavior is explained and supported with evidence. Research sources were reviewed on
2026-11-14.

## Product Findings

OxiRoute has three different kinds of truth that need different presentation:

1. **Operational truth:** active listeners, pools, health, generations, RTMP sessions, counters, and
   readiness. Operators need commands, evidence, and recovery actions.
2. **Configuration truth:** typed objects, source formats, revisions, previews, composition, and
   validation. Integrators need examples and exact contracts without reading Rust types.
3. **Implementation truth:** crate ownership, runtime planning, state machines, test layers, and
   unsupported behavior. Developers need architecture and failure-path context.

The hierarchy separates these needs while preserving links to the normative specifications. A reader
should be able to evaluate the product, complete a task, or find an exact contract without first
learning the implementation vocabulary.

## Research Foundations

These sources inform editorial choices. They do not establish OxiRoute capabilities, readiness, or
performance; those claims require repository evidence.

| Primary source | Finding | Application |
| --- | --- | --- |
| [Diátaxis](https://diataxis.fr/) | Tutorials, how-to guides, reference, and explanation serve different reader needs. | Keep first-run learning separate from operational tasks, exact contracts, and architecture. |
| [Diátaxis how-to guides](https://diataxis.fr/how-to-guides/) | A how-to guide starts from the reader's problem and desired result. | Use task headings such as “Reload configuration” instead of internal subsystem names. |
| [GitHub README guidance](https://docs.github.com/en/repositories/managing-your-repositorys-settings-and-features/customizing-your-repository/about-readmes) | A README explains usefulness, capabilities, and how to start. | Make the README an evaluation and navigation page; link complete contracts. |
| [Google technical writing](https://developers.google.com/tech-writing/one/clear-sentences) | Specific verbs and objective data improve clarity. | Describe observable behavior and limits instead of unsupported adjectives. |
| [Caddy reverse-proxy quickstart](https://caddyserver.com/docs/quick-starts/reverse-proxy) | Prerequisites, one proxy task, and a verification request form a short learning path. | Start with one local HTTP outcome before introducing optional protocols and management. |
| [Envoy documentation organization](https://www.envoyproxy.io/docs/envoy/latest/about_docs) | Introduction, configuration, operations, extension, and API reference have separate entry points. | Give operators, API integrators, and developers direct routes to their work. |
| [Pingora upstream](https://github.com/cloudflare/pingora) | Pingora provides a Rust framework for networked systems. | Explain the framework relationship without inheriting its traffic figures or production evidence. |

## Language and Page Standards

Use direct English, active verbs, and concrete outcomes. Explain a term when it first becomes
necessary. Preserve exact CLI names and configuration keys in code formatting. Keep limitations next
to the affected feature rather than hiding them in a distant disclaimer.

| Avoid | Prefer |
| --- | --- |
| “Safe configuration changes” | “A rejected candidate leaves the active generation serving.” |
| “Revision-checked configuration writes” | “Configuration saves reject stale revisions.” |
| “Health-aware pools” | “Route requests to healthy upstream servers.” |
| “Fast proxy” | A measured result with its scenario, version, and limitations. |
| “Five-minute setup” | “Run a local HTTP proxy,” unless setup time has actually been measured. |

Tutorials state prerequisites, guide one achievable outcome, show expected results, and explain how
to stop or clean up. Operational guides state the task, required access, steps, verification, and
recovery action. Reference pages document types, defaults, bounds, errors, and availability.
Architecture pages explain ownership, data flow, and the reasons behind important decisions.

A build command is not an installation promise. State whether instructions assume a checkout, a
prebuilt binary, or a package. Separate required tools from optional dashboard or protocol tools.
Examples use generic data and must match the checked-in CLI and configuration syntax.

## Audience Lanes

| Lane | Primary question | Entry point | Success signal |
| --- | --- | --- | --- |
| Operator | Can I run and safely change this instance? | `docs/user/GETTING_STARTED.md` | A verified local request and a readable runtime state |
| Existing proxy operator | What will migrate, and what will not? | `docs/user/MIGRATION.md` | Report reviewed, preview finalized, native source preserved |
| Media operator | Can I publish, observe, and record this stream? | `docs/user/RTMP.md` | Live catalog and recorder phase match the configured policy |
| API integrator | What endpoint, auth, revision, or number type should I expect? | `docs/reference/API.md` | Client handles success, stale state, and redacted failure accurately |
| Rust/UI developer | Where does this behavior belong, and how is it proved? | `docs/developer/ARCHITECTURE.md` | Owning abstraction, focused test, compatibility update |
| Release maintainer | Can this claim and artifact be shipped reproducibly? | `docs/developer/RELEASING.md` | Gates, docs, media, package, and source archive agree |

## Information Architecture

```text
README.md
  -> docs/README.md
      -> docs/user/*
      -> docs/reference/*
      -> docs/developer/*
      -> existing normative specifications
  -> website/index.html
      -> role tabs
      -> capability tabs
      -> dashboard recordings
      -> versioned GitHub source docs
```

The README answers identity, current status, features, first run, measured performance, boundaries,
and where to go next. User pages answer one task at a time. Reference pages answer exact shape questions. Normative specs hold the
complete contract. Developer pages explain ownership and contribution. The website compresses the
same hierarchy into a navigable visual overview.

## Website and Media Standards

The website summarizes the same contracts as the repository documentation. Navigation should lead
with tasks and audiences. Keep capability boundaries visible and preserve a readable keyboard and
no-JavaScript path. Expandable detail should not conceal prerequisites or limitations.

Dashboard images and recordings use generic fixtures, match shipped labels, and include useful text
descriptions. Media illustrates behavior; it does not establish protocol conformance or reliability.

## Source Of Truth

| Information | Authoritative file |
| --- | --- |
| Current feature status | `docs/COMPATIBILITY.md`; README summarizes, release notes record changes |
| Complete configuration behavior | `docs/CONFIG_SPEC.md`, `docs/CONFIG_FORMATS.md` |
| Management/API behavior | `docs/API_UI_SPEC.md`, `docs/MANAGEMENT_CLI.md` |
| Operations and lifecycle | `docs/OPERATIONS.md` |
| Native import boundary | `docs/IMPORT_SPEC.md` |
| RTMP behavior | `docs/RTMP_SPEC.md` |
| Test/release gates | `docs/TEST_STRATEGY.md`, `docs/developer/RELEASING.md` |
| Public orientation | `website/index.html`, linked back to the files above |

When code changes a public behavior, update the authoritative contract first, then the relevant user
page and website status. Never make the website more optimistic than the compatibility matrix.

## Claims and Versions

Feature-specific support and whole-product readiness are different facts. Use the support labels
from [the compatibility matrix](../COMPATIBILITY.md); do not turn component tests or planned work
into active runtime support. Do not infer a product maturity label from an individual capability.

Anchor release and toolchain statements to checked-in metadata and release documentation. Distinguish
the current release from historical measurements and experimental changes. Never relabel a result
from an older release or a development build as a measurement of the current release.

Feature summaries describe what a reader can accomplish and the important boundary. Complete
protocol conformance, drop-in migration compatibility, production readiness, and security guarantees
require specific evidence; dependency selection alone establishes none of them.

## Benchmark Publication Standards

The [benchmark guide](../BENCHMARKS.md) is the evaluation entry point. Developer reports and raw
artifacts retain the detail needed to inspect historical measurements. A short summary links back
to its source and states the measured version and workload alongside its numbers. These standards
apply to new summaries and benchmark publications; historical reports may have gaps that must be
identified rather than silently filled with assumptions.

Classify evidence availability: checked-in raw artifacts are inspectable from a checkout; local-only
artifacts are unavailable to other readers; a retained narrative without raw data is historical
reported evidence. Do not call a result independently reproducible merely because a harness exists,
or link an ignored report directory as if it were published. A newly reproduced measurement is a
new series, not a replacement identity for an old result.

| Required context | What to disclose |
| --- | --- |
| Identity | Measurement date, release or commit, competitor versions, and build profile. |
| Environment | CPU class, OS/kernel, worker allocation, affinity, SMT, relevant memory limits, and shared-host interference. |
| Workload | Protocol, request/response sizes, connection reuse, concurrency, origin behavior, routes, TLS provider/cipher/resumption, and enabled features. |
| Procedure | Harness and command, warm-up, measurement duration, repetitions, run ordering, exclusion criteria, and aggregation. |
| Results | Successful throughput, errors/timeouts, latency percentiles and units, and explicitly defined CPU/memory metrics. |
| Evidence | Reproduction script, configuration, raw result artifact, observed variability, and unmeasured cases. |

Keep throughput, CPU efficiency, latency, memory, startup, and reload measurements separate. A
microbenchmark does not establish end-to-end capacity. A closed-loop saturation test describes
latency at its fixed concurrency; it does not establish latency under a fixed external arrival
rate. Do not average unlike workloads into a single performance score or present a noisy small
change as a proven improvement.

Comparison settings should perform equivalent work. Disclose differences that remain, including
logging, headers, caching, TLS, worker counts, and connection reuse. Report errors and rejected runs
rather than silently removing unfavorable results. Keep missing data visibly missing.

Methodology sources and their limits:

- [wrk2](https://github.com/giltene/wrk2) explains coordinated omission and scheduled-rate latency
  measurement, and documents roughly millisecond timing granularity. Its methodology motivates
  distinguishing load models; it is not a recommendation for every microsecond-scale test.
- [oha](https://github.com/hatoo/oha) documents scheduled-time latency correction when a request rate
  or burst schedule is configured. This is an option for future fixed-rate tests, not evidence that
  existing repository measurements use that model.
- [RFC 9411](https://www.rfc-editor.org/rfc/rfc9411.html) provides testbed, traffic, and reporting
  disciplines for network security devices. Borrowing those disciplines does not make the project's
  loopback proxy measurements RFC-conformant tests.

Publish only generic benchmark fixtures and sanitized environment descriptions. Retain enough
hardware and allocation information to interpret results without including hostnames, private
addresses, credentials, user paths, or real traffic.

## Maintenance Checklist

- Can a new operator find a local run and its expected result directly from the README?
- Do visible capability claims agree with the compatibility matrix and its maturity vocabulary?
- Does each example use the current CLI/config/API shape?
- Are secrets, roots, stream queries, and production data absent from prose and media?
- Do tabs and expanders preserve a useful keyboard and no-JavaScript path?
- Do dashboard GIFs still match shipped labels and controls?
- Do local links resolve, examples match the current syntax, and Markdown files use LF line endings?
- Does each benchmark summary match its raw artifact, measured version, units, and workload?
- Have focused checks verified changed commands or configuration examples?
- Have the required test and release gates verified any accompanying runtime changes?

First successful request, verified reload, successful API call, and first focused developer test are
useful reader-validation outcomes. They are acceptance goals, not measured usability results unless
an actual study records its method and observations.
