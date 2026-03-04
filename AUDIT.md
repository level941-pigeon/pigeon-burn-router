# BurnRouter v2 Audit Record

This document records all three rounds of security review conducted on the PIGEON BurnRouter v2 program. Every finding is listed with its severity, the root cause, and how it was resolved. Nothing has been removed or minimized.

The goal of this document is simple: anyone evaluating this program for integration or investment should be able to verify that it was subjected to serious scrutiny and that the team did not hide or dismiss what was found.

-----

## Audit Round 1: Architecture Review

**Scope:** Initial program design, mechanism logic, and economic assumptions.

**Method:** Line-by-line review of the original v4 source file against the published mechanism specification.

-----

### Finding 1.1: MEV Sandwich Vulnerability

**Severity:** Critical

**Description:** The original design had no on-chain economic floor on the buyback step. A sandwich bot could front-run the PIGEON buyback, inflate the price, and cause the router to buy back significantly less PIGEON than expected. The seller would receive their wSOL correctly, but the burn amount would be near zero.

**Resolution:** Two-layer MEV protection added. Layer one: all transactions are submitted as Jito bundles through a client-side helper, bypassing the public mempool entirely. Layer two: a `min_buyback_out` parameter is enforced on-chain inside `execute_sell_burn`. If the buyback leg would return fewer tokens than the caller specified, the entire transaction reverts. Both layers must fail simultaneously for a sandwich to succeed.

-----

### Finding 1.2: No Pool Migration Path

**Severity:** Critical

**Description:** The original design had no mechanism to handle pool deprecation or migration. If Meteora deprecated the DAMM v2 pool the router was pointed at, the router would be permanently bricked with no path to recovery. This contradicted the publicly disclosed “one-time 30-day migration window.”

**Resolution:** `migrate_pool` instruction added. The `RouterConfig` account stores a `migration_window_open_until` Unix timestamp set at initialization. The `migrate_pool` instruction enforces two guards: the current time must be before the timestamp, and a `migration_completed` boolean flag must be false. After one successful migration the flag is set permanently and the instruction can never execute again. After the window closes the pool address is frozen forever.

-----

### Finding 1.3: Dust Accumulation

**Severity:** Medium

**Description:** Rounding on the wSOL split could leave small amounts of wSOL stranded in the router’s vault after each transaction. Over thousands of transactions these amounts compound. With no sweep mechanism, the funds would be permanently locked.

**Resolution:** `sweep_dust` instruction added. The instruction is permissionless, meaning anyone can call it. Each call moves up to `DUST_SWEEP_CAP` (1,000,000 lamports) from the router vault to a designated recipient. The cap prevents any single call from draining more than intended, and the permissionless design means no admin action is required for dust to be recovered.

-----

### Finding 1.4: Fee Numbers Incorrect in Comments

**Severity:** Low

**Description:** Source comments stated Jupiter charges “less than 0.3%” and trading terminals charge “2.5%+.” These numbers were reversed. Jupiter’s effective fee is under 0.3% and is accurate. Terminal fees are typically 1% on Photon and Axiom, not 2.5%.

**Resolution:** Comments corrected to reflect accurate market rates. This had no impact on program logic but was corrected for accuracy.

-----

## Audit Round 2: Code Correctness and Implementation Review

**Scope:** Full implementation audit of the merged v2 program including all resolved findings from Round 1. Cross-check of all recommendations against primary sources.

**Method:** Each instruction reviewed for correctness of account constraints, CPI call structure, arithmetic, and guard ordering. Auditor recommendations verified against Meteora documentation, Pyth documentation, and Anchor documentation before acceptance.

-----

### Finding 2.1: Wrong Pyth Crate Recommended

**Severity:** Critical (finding in auditor recommendation, not in program)

**Description:** An external reviewer recommended adding `pyth-sdk-solana = "0.1"` to resolve Pyth integration. This recommendation contained two errors. First, version 0.1 does not exist on crates.io. The crate version series is 0.10.x. Second, and more critically, the Pyth documentation explicitly states that `pyth-sdk-solana` contains internal implementation details for Pythnet and should not be used for Solana programs. The correct crate for Solana integration is `pyth-solana-receiver-sdk`.

**Resolution:** Recommendation rejected. `pyth-solana-receiver-sdk = "0.6.1"` is the correct dependency, confirmed against current Pyth documentation. The upgrade path using this crate is documented in the program header for future implementation.

-----

### Finding 2.2: Wrong instructions_sysvar Pattern Recommended

**Severity:** High (finding in auditor recommendation, not in program)

**Description:** An external reviewer recommended adding `SYSVAR_INSTRUCTIONS_PUBKEY` as a named struct field in the `ExecuteSellBurn` accounts struct. This pattern would require every caller to pass the sysvar regardless of whether the pool has a rate limiter, breaking swaps on non-rate-limiter pools and requiring a separate accounts struct per pool type.

**Resolution:** Recommendation rejected. Meteora documentation and CHANGELOG both confirm the correct pattern: include `SYSVAR_INSTRUCTIONS_PUBKEY` in the `remaining_accounts` of the swap instruction. The CHANGELOG further confirms that the rate limiter only applies to the buy direction, not the sell direction. The sell leg (PIGEON to wSOL) never requires the sysvar. The buyback leg (wSOL to PIGEON, buying PIGEON) requires it only when `config.has_rate_limiter == true`. The program implements this correctly via `ctx.remaining_accounts[0]` validated against `INSTRUCTIONS_SYSVAR_ID`, passed to the buyback CPI only when the flag is set.

-----

### Finding 2.3: PIGEON/SOL Pyth Price Feed Does Not Exist

**Severity:** Critical (program design decision)

**Description:** The v2 design included an on-chain Pyth MEV guard that derived a price floor from a live PIGEON/SOL Pyth feed before executing any swap. Pyth only lists established assets. PIGEON is a pump.fun memecoin. No PIGEON/SOL feed exists on Pyth and none is likely to be created.

**Resolution:** On-chain Pyth guard removed from the production program. The `min_buyback_out` parameter passed by the caller serves as the on-chain slippage floor. Jito bundle submission remains as the primary MEV protection layer. The Pyth upgrade path is documented in the program header: when a PIGEON/SOL feed is listed, add `pyth-solana-receiver-sdk`, add `Account<'info, PriceUpdateV2>` to `ExecuteSellBurn`, and replace the stub comment with a live price check using `get_feed_id_from_hex`. This is a named technical debt item, not a hidden gap.

-----

### Finding 2.4: Meteora CPI Account Order Unverified

**Severity:** Medium

**Description:** The raw CPI to Meteora’s `swap` instruction listed 11 accounts plus clock. Some Meteora DAMM v2 pools with `baseFeeMode == 2` (rate limiter) require an additional `SYSVAR_INSTRUCTIONS_PUBKEY` account. If the pool used at deployment has `baseFeeMode == 2` and the sysvar is not passed, the CPI will fail at runtime.

**Resolution:** `has_rate_limiter` flag added to `RouterConfig`, set at initialization. When true, the buyback CPI includes the sysvar via `remaining_accounts`. The deploy script includes a `POOL_HAS_RATE_LIMITER` environment variable that must be set correctly before deploy. The discriminator verification step in the deploy script instructs the deployer to confirm the Meteora swap discriminator against the live IDL before building.

-----

### Finding 2.5: Test Suite Gaps

**Severity:** Medium

**Description:** Seven gaps identified in the original test suite: (1) test file header described a different program than what was being tested, (2) migration window tested via flag only, not by advancing clock time, (3) no MEV guard test, (4) no Pyth staleness test, (5) no complete happy-path integration test, (6) ATA creation used deprecated pattern, (7) minor arithmetic precision gaps.

**Resolution:** Full test suite rewritten. ATAs use `getAssociatedTokenAddressSync()`. Dust cap tested with exact equality. MEV guard test expects `ZeroMinOut` error. Migration window test uses environment variable injection for devnet timestamp testing. Happy-path integration test added covering all six steps of `execute_sell_burn`. 20+ test cases total covering all guards and instruction paths.

-----

## Audit Round 3: Final Pre-Deploy Review

**Scope:** Verification that all Round 1 and Round 2 findings were correctly resolved in the final merged program. Review of deploy script for correctness and safety.

**Method:** Final merged file reviewed against resolved finding list. Deploy script reviewed for completeness and correctness of the initialization sequence.

-----

### Finding 3.1: Deploy Script Out of Sync with Program Interface

**Severity:** High

**Description:** The deploy script was written against an earlier version of the program interface and did not include the `has_rate_limiter` flag or the `pigeon_vault` and `wsol_vault` explicit account fields added during Round 2 resolution.

**Resolution:** Deploy script fully rewritten. The updated script includes 11 gated steps, each requiring explicit confirmation before any destructive action. Required configuration variables are listed at the top: `METEORA_POOL_ADDRESS`, `PIGEON_VAULT_ADDRESS`, `WSOL_VAULT_ADDRESS`, `POOL_HAS_RATE_LIMITER`, `POOL_PIGEON_IS_TOKEN_A`, and `SQUADS_MULTISIG_ADDRESS`. The upgrade authority revocation step requires the operator to type `REVOKE_AUTHORITY` manually before executing. Final verification reads on-chain state before declaring the deploy complete.

-----

### Finding 3.2: No Explicit Program Key Safety Check in CPI

**Severity:** Low

**Description:** The raw CPI to Meteora did not include an explicit check verifying that the program being called matched `METEORA_DAMM_V2_PROGRAM`. The account constraint on the program account provided some protection but was not redundant with an explicit key equality check inside the function body.

**Resolution:** `require_keys_eq!(program.key(), METEORA_DAMM_V2_PROGRAM)` added inside `invoke_meteora_swap`. Redundant with the account constraint but provides defense in depth.

-----

## Summary Table

|Round|Finding                          |Severity|Status                                                             |
|-----|---------------------------------|--------|-------------------------------------------------------------------|
|1    |MEV sandwich vulnerability       |Critical|Resolved: two-layer protection                                     |
|1    |No pool migration path           |Critical|Resolved: migrate_pool instruction                                 |
|1    |Dust accumulation                |Medium  |Resolved: permissionless sweep_dust                                |
|1    |Fee comment inaccuracies         |Low     |Resolved: comments corrected                                       |
|2    |Wrong Pyth crate recommended     |Critical|Resolved: recommendation rejected                                  |
|2    |Wrong instructions_sysvar pattern|High    |Resolved: remaining_accounts pattern confirmed against Meteora docs|
|2    |PIGEON/SOL Pyth feed missing     |Critical|Resolved: guard removed, upgrade path documented                   |
|2    |Meteora CPI account order        |Medium  |Resolved: has_rate_limiter flag added                              |
|2    |Test suite gaps (7 items)        |Medium  |Resolved: full test suite rewritten                                |
|3    |Deploy script out of sync        |High    |Resolved: script fully rewritten                                   |
|3    |No explicit program key check    |Low     |Resolved: require_keys_eq! added                                   |

**Total findings:** 11
**Resolved:** 11
**Outstanding:** 0

-----

## Known Technical Debt

**Pyth on-chain guard:** Not implemented because no PIGEON/SOL feed exists on Pyth. The current on-chain protection is `min_buyback_out` enforced at the instruction level. When a Pyth feed for PIGEON is listed, the upgrade path is documented in the program header. This is a named gap, not a hidden one.

**Typed Meteora SDK:** The program currently calls Meteora via raw `invoke_signed` with a manually constructed discriminator. When Meteora publishes a `damm-v2` crate with CPI features, the raw call can be replaced with a typed SDK call. The discriminator verification step in the deploy script ensures the current raw call is correct against the live IDL at deploy time.

-----

## Deployment Checklist Status

- [ ] Discriminator verified against live Meteora IDL
- [ ] `anchor build` completed with no warnings
- [ ] Devnet smoke test: all six steps of `execute_sell_burn` confirmed
- [ ] `initialize_router_atas` called
- [ ] `initialize` called with correct burn_bps and migration window
- [ ] Authority transferred to Squads multisig
- [ ] Upgrade authority revoked: program immutable
- [ ] On-chain state verified post-deploy

Items will be checked as deployment progresses. This document will be updated with the mainnet program ID and transaction signatures when deployment is complete.
