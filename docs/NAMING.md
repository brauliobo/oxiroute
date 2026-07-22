# Naming candidates

The current working name is **OxiRoute**. Exact-name Cargo and GitHub repository searches
were performed on 2026-07-22; they are not trademark, domain, organization, or package-name
reservations.

| Candidate | Rationale | Search note |
| --- | --- | --- |
| **FerrumEdge** | Rust/iron association plus edge traffic management; broad enough for HTTP and L4. | No exact Cargo crate or GitHub repository found. |
| **RelaySmith** | Conveys building and operating protocol relays without claiming firewall behavior. | No exact Cargo crate or GitHub repository found. |
| **OxiRoute** | Short, Rust-associated, and directly describes routing. | No exact match, but `OxiRouter` exists and creates some confusion risk. |
| **IronConduit** | Memorable systems name for carrying multiple protocols. | No exact Cargo crate or GitHub repository found. |

Names discarded after the same basic search:

- `Muxlane`: an exact GitHub project exists.
- `Gateweaver`: an exact archived API gateway exists.
- `PortWeave`: several networking and port-management projects exist.
- `FerrumGate`: an established zero-trust networking organization exists.
- `RouteForge`: several exact GitHub projects exist.
- `FluxHarbor`: exact GitHub projects exist.

**Recommendation:** use `FerrumEdge` for the public project if trademark and domain checks
are clean; otherwise retain `OxiRoute` and clearly distinguish it from `OxiRouter`.
