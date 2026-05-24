# aii-onboarding

First-run hardware probe + Tier recommendation for the AII client
(per 《04 架构设计文档》§14.4 + 《05 共识机制详细设计》§9.5).

```rust
let profile = aii_onboarding::detect();
let score   = aii_onboarding::score(&profile);
let tier    = aii_onboarding::recommend_tier(score);
println!("→ recommended {:?} (score {})", tier, score);
```

The probe is read-only and runs locally — no network calls, no DNS,
no remote endpoints. Bootstrap-latency / public-IP / upstream-Mbps
detection are stubbed as `None` in v0.0.10; concrete network probes
ship in a follow-up.
