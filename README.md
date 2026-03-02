# $PIGEON BurnRouter v2

Trustless atomic sell-to-burn router for $PIGEON on Meteora DAMM v2.

Every sell through the router permanently destroys 1.5% of the proceeds as PIGEON. No multisig can turn it off. No admin can redirect funds. The burn is physics, not promises.

## How It Works

1. Seller sends PIGEON to the router
2. Router sells PIGEON for wSOL on Meteora DAMM v2
3. Router splits wSOL: 98.5% to seller, 1.5% reserved for burn
4. Router buys PIGEON back with the 1.5%
5. Router calls BurnChecked — PIGEON is permanently destroyed
6. All six steps are atomic. If any step fails, the entire transaction reverts.

## Program Addresses

| Network | Program ID |
|---------|-----------|
| Mainnet | REPLACE_WITH_YOUR_PROGRAM_ID |
| Token | 4fSWEw2wbYEUCcMtitzmeGUfqinoafXxkhqZrA9Gpump |
| Pool | Meteora DAMM v2 |

## Security

- Upgrade authority revoked at deploy: immutable forever
- Authority transferred to Squads multisig before revocation
- Burn BPS bounded 100-200 (1-2%), governance-controlled
- Vault addresses bound at initialization, cannot be redirected
- MEV protection: Jito bundle (client) + min_buyback_out floor (on-chain)
- Rate limiter pools supported via remaining_accounts pattern

## Integration

Any terminal or bot can route PIGEON sells through the router with one instruction call.

```typescript
const tx = await program.methods
  .executeSellBurn(
    new BN(tokenAmountIn),
    new BN(minSolOut),
    new BN(minBuybackOut),
    true // a_to_b: true if PIGEON is token_a in the pool
  )
  .accounts({ /* see IDL */ })
  .remainingAccounts(
    hasRateLimiter ? [{ pubkey: SYSVAR_INSTRUCTIONS_PUBKEY, isSigner: false, isWritable: false }] : []
  )
  .transaction();
