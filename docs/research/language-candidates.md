# Language candidates for the metsuke metrics agent/server

Comparison of Haskell, Rust, Crystal, and Go for a security-critical, statically-compiled
Cardano SPO metrics agent and collection server. Findings are grounded in current library/tooling
state (checked August 2026); this document is an input to the language decision (musashi-ping-sak.7),
not the decision itself.

## Comparison table

| Criterion | Rust | Haskell | Go | Crystal |
|---|---|---|---|---|
| Ed25519 | mature, several audited-quality impls (`ed25519-dalek`, ring) | `cardano-crypto-class` (IOG-maintained, used in cardano-node) | stdlib `crypto/ed25519`, first-class | `spider-gazelle/ed25519` shard or `monocypher.cr` binding; small ecosystem |
| CBOR | mature: `minicbor` (used by Pallas), `ciborium`, `serde_cbor` | mature: `cborg`/`serialise`, IOG-maintained, used throughout cardano-node | decent: `fxamacker/cbor` widely used | shard-level only, less battle-tested |
| COSE_Sign1 | general-purpose `cose-rust` exists, not Cardano-native; would need adaptation | **no maintained COSE package found** on Hackage; would build on `cborg` from scratch | `veraison/go-cose` — actively maintained, spec-focused, explicit Ed25519 support | **no COSE shard found**; build on CBOR + Ed25519 primitives |
| Cardano-specific libs | **Pallas** (TxPipe, modular, `pallas-codec`/`pallas-crypto`/`pallas-addresses`) and Emurgo's `cardano-serialization-lib`; both actively maintained | `cardano-api`, `cardano-crypto-class`, `ouroboros-consensus-cardano` — canonical, IOG's own node stack | `gouroboros` (blinklabs-io) — NtC LocalStateQuery client support, Catalyst-funded, active | none found |
| Ouroboros NtC / LocalStateQuery | via Pallas's ouroboros mini-protocol modules | native — this *is* the node's own codebase | `gouroboros` has a documented `LocalStateQuery` handler over the local UNIX socket | none — would require hand-rolling the mini-protocol |
| journald reading | `sd-journal`/`systemd` crates — FFI wrappers over libsystemd, active but small | `libsystemd-journal` (Hackage) — read support since v1.3, follow-forward/backward supported | `coreos/go-systemd/v22/sdjournal` — mature, cgo-based, widely used (CoreOS/k8s heritage) | **no shard found**; would need to hand-write FFI bindings |
| Static compilation | strong: musl target + `-C target-feature=+crt-static`, well-documented crane/naersk recipes | possible but fiddly: needs static gmp/zlib, `--disable-executable-dynamic`, GPL-avoidance requires switching off GMP | best-in-class: `CGO_ENABLED=0` gives static binaries for free when no cgo deps; needs musl+cgo path once cgo library (sdjournal) enters the build | possible via musl target but community reports linker gaps (`cannot find -lgc`) on aarch64 cross-builds |
| Cross-compile x86_64/aarch64 | good: `pkgsCross.aarch64-multiplatform-musl.pkgsStatic` pattern documented, one known nixpkgs bug (`getauxval` on musl aarch64, NixOS/nixpkgs#264687) | supported via `haskell.nix` cross-compilation tutorial and community templates (`sambnt/haskell-cross`); more moving parts | straightforward for CGO_ENABLED=0 builds (`GOARCH=arm64`); once cgo (sdjournal) is in the mix, needs explicit cross-linker (`aarch64-linux-gnu-gcc`) | reported friction; Zig-based toolchain is the smoother path than Nix today |
| Nix build tooling | excellent: `crane`/`naersk`, both flake-parts friendly, well-trodden musl recipes | `haskell.nix` is powerful but heavyweight (own binary cache, evaluation-heavy); flake-parts fit is workable but non-trivial | `buildGoModule` is built into nixpkgs directly (no external gomod2nix needed for a simple flake-parts module); cross via `pkgsCross` works for pure-Go builds | `crystal` is packaged in nixpkgs (v1.19.1) but built from upstream binary release, not source-bootstrapped; less flake-parts tooling precedent |
| Lint/format/static analysis | best-in-class: `clippy` + `rustfmt` are effectively mandatory-quality tools, IDE-integrated | strong for a smaller pool: `hlint` (mature) + `fourmolu` (mature) + `weeder` (mature, dead-code) + `stan` (explicitly beta) | strong and official-toolchain-backed: `staticcheck`, `gofumpt`, `govulncheck` (official, reachability-aware CVE scanner) | present but thin: `ameba` (linter) + `crystal tool format` (formatter); no dedicated vuln/supply-chain scanner found |
| Supply-chain / security tooling | `cargo-audit` (RustSec advisories) + `cargo-deny` (licenses/bans/sources) + `cargo-geiger` (unsafe-code detection); layered, well-documented 2026 best practice | no dedicated Hackage vulnerability database/scanner found; supply-chain story weaker | `govulncheck` — official, reachability-aware, binary-scan mode; strongest out-of-the-box story here | no supply-chain scanner found |
| Memory safety | compile-time ownership, no GC; NSA/CISA list Rust as memory-safe; `unsafe` blocks are the auditable attack surface | GC'd, no manual memory management; not commonly benchmarked for CVE history in network daemons, more associated with formal verification | GC'd; NSA/CISA list Go as memory-safe; GC introduces latency variance | GC'd; small ecosystem, no CVE track record to draw on for security-critical daemons |
| Security-critical daemon track record | growing (Linux kernel accepting Rust, many new network tools) | none found relevant to a *standalone SPO-facing daemon*; the node itself is Haskell, but that's a different risk/ops profile | strong (etcd, containerd, Kubernetes control plane, CoreOS tooling) | none found |

## Per-language notes

### Rust
Best coverage of the Cardano-native building blocks (Pallas is explicitly maintained for this
purpose and already ships `pallas-crypto`, `pallas-codec`), the strongest static/cross-compile
recipes via crane or naersk, and the most mature supply-chain tooling (`cargo-audit` +
`cargo-deny`). The one gap is COSE_Sign1: no Cardano-native COSE crate exists, so `cose-rust`
(general-purpose, OpenSSL-backed) would need adaptation or a small custom COSE_Sign1 encoder built
on `pallas-codec`/`minicbor`. journald bindings are FFI-based but standard (`sd-journal` crate).

### Go
Strongest official-toolchain security story (`govulncheck` is unmatched — reachability-aware,
official, binary-scannable) and the most proven track record for security-critical network
daemons (etcd, containerd, k8s control plane). `gouroboros` gives a real, funded, actively
maintained NtC/LocalStateQuery client and `veraison/go-cose` covers COSE_Sign1 directly. Static
compilation is trivial for pure-Go code but the moment `sdjournal` (cgo-based journal reader)
enters the build, static linking needs explicit musl/cgo cross-compilation flags — solvable, but
loses Go's "just works" story. Weakest cell: no cgo-free/pure-Go journal reader exists (the pure-Go
`journal` package only *writes* to journald over the socket; reading still needs cgo `sdjournal`).

### Haskell
The Cardano-native cell is unbeatable — `cardano-api`/`cardano-crypto-class` are the same
libraries IOG maintains for cardano-node itself, so protocol compatibility risk is close to zero.
journald reading is covered (`libsystemd-journal`, read support since v1.3). The weak points are
COSE (no maintained package found — would be built from scratch on `cborg`) and supply-chain
tooling (no Hackage-wide vulnerability scanner comparable to `cargo-audit`/`govulncheck`). Static
linking works but is the most operationally fiddly of the four (manual static gmp/zlib, GPL
avoidance requires disabling GMP for the bignum backend). `haskell.nix` is powerful but
evaluation-heavy compared to `crane`/`naersk`.

### Crystal
Weakest ecosystem fit for this specific project. No COSE library, no journald-reading shard (would
require hand-written FFI bindings to `libsystemd`), no Cardano NtC/LocalStateQuery library, and no
supply-chain/vulnerability scanner. Ed25519 and CBOR exist as small shards but are not
battle-tested at Cardano scale. Static/cross-compilation to aarch64-musl has open community-
reported linker issues. Ameba (linter) and `crystal tool format` are solid but the language's small
contributor pool means less assurance on long-term maintenance of any dependency used here.

## Recommendation ranking (input to the decision, not the decision)

1. **Rust** — best combination of Cardano-native libraries (Pallas), static/cross-compile
   maturity, and supply-chain tooling. Only real gap is COSE_Sign1, which is a small, well-scoped
   piece of code to write directly on top of existing CBOR primitives.
2. **Go** — best static-compile ergonomics and best-in-class official vulnerability tooling and
   daemon track record, and it has both the Cardano NtC client (`gouroboros`) and COSE_Sign1
   (`go-cose`) covered. Held back only by the cgo dependency for journald reading, which
   complicates the "fully static" story.
3. **Haskell** — unmatched Cardano-protocol fidelity (same libraries as cardano-node) but weaker
   supply-chain tooling and no COSE library; appropriate if protocol-correctness risk is judged the
   dominant concern over operational/security tooling maturity.
4. **Crystal** — not recommended for this project. Every domain-specific building block (COSE,
   journald reading, Cardano NtC) is missing and would need to be built and maintained in-house,
   with no comparable security-tooling ecosystem to lean on.
