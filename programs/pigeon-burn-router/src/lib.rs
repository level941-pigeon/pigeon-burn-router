// ============================================================
// $PIGEON BurnRouter v2 — Final Corrected Build
// Atomic sell-to-burn router on Meteora DAMM v2
//
// MEV PROTECTION ARCHITECTURE:
//   Layer 1 (on-chain):  caller-supplied min_buyback_out, enforced atomically.
//   Layer 2 (client):    Jito bundle bypasses public mempool.
//   Layer 3 (pending):   Pyth floor — requires PIGEON/SOL feed to be listed.
//
// DEPLOY CHECKLIST:
//   1. Replace REPLACE_WITH_YOUR_PROGRAM_ID
//   2. Query pool baseFeeMode. If == 2, set has_rate_limiter=true on init.
//   3. Verify METEORA_SWAP_DISCRIMINATOR:
//        anchor idl fetch cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG \
//          | jq '.instructions[] | select(.name=="swap") | .discriminant'
//   4. anchor build && anchor deploy --provider.cluster mainnet-beta
//   5. Call initialize_router_atas
//   6. Call initialize (burn_bps=150, migration_window=now+30days, has_rate_limiter=<from step 2>)
//   7. Transfer authority to Squads multisig
//   8. solana program set-upgrade-authority <PROGRAM_ID> --final
//
// PYTH UPGRADE PATH:
//   When PIGEON/SOL is listed on Pyth:
//   1. cargo add pyth-solana-receiver-sdk
//   2. Add Account<'info, PriceUpdateV2> to ExecuteSellBurn
//   3. Call price_update.get_price_no_older_than(&Clock::get()?, PYTH_MAX_AGE_SECONDS, &feed_id)?
//   4. Derive pyth_floor, use pyth_floor.max(min_buyback_out) as effective floor
//
// SUPPLY FLOOR:
//   941 tokens (941_000_000 raw units at 6 decimals) will always exist.
//   When burn would drop supply below floor, burn is reduced to hit floor exactly.
//   When supply is already at floor, burn step is skipped. Sell still completes.
// ============================================================

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::solana_program::pubkey;
use anchor_lang::solana_program::sysvar::instructions::ID as INSTRUCTIONS_SYSVAR_ID;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, Burn, Mint, Token, TokenAccount, Transfer};

declare_id!("REPLACE_WITH_YOUR_PROGRAM_ID");

// ============================================================
// CONSTANTS
// ============================================================

pub const PIGEON_MINT: Pubkey =
    pubkey!("4fSWEw2wbYEUCcMtitzmeGUfqinoafXxkhqZrA9Gpump");
pub const METEORA_DAMM_V2_PROGRAM: Pubkey =
    pubkey!("cpamdpZCGKUy5JxQXB4dcpGPiikHawvSWAd6mEn1sGG");
pub const WSOL_MINT: Pubkey =
    pubkey!("So11111111111111111111111111111111111111112");

pub const MIN_BURN_BPS: u16 = 100;
pub const MAX_BURN_BPS: u16 = 200;
pub const DEFAULT_BURN_BPS: u16 = 150;

pub const CONFIG_SEED: &[u8] = b"pigeon_burn_config";

pub const DUST_SWEEP_CAP: u64 = 1_000_000;
pub const MIGRATION_WINDOW_SECONDS: i64 = 30 * 24 * 60 * 60;

/// 941 tokens * 10^6 (6 decimals) = 941_000_000 raw units.
/// The burn will never reduce supply below this floor.
/// Verify PIGEON decimals on Solscan before deploy.
pub const MIN_SUPPLY_FLOOR: u64 = 941_000_000;

pub const PYTH_PIGEON_SOL_FEED_HEX: &str =
    "0x0000000000000000000000000000000000000000000000000000000000000000";
pub const PYTH_MAX_AGE_SECONDS: u64 = 60;
pub const PYTH_MAX_CONF_RATIO_BPS: u64 = 200;
pub const PYTH_FLOOR_BPS: u64 = 9_500;

/// Meteora DAMM v2 swap discriminator: sha256("global:swap")[0..8]
/// VERIFY BEFORE DEPLOY via anchor idl fetch
pub const METEORA_SWAP_DISCRIMINATOR: [u8; 8] =
    [248, 198, 158, 145, 225, 117, 135, 200];

// ============================================================
// PROGRAM
// ============================================================

#[program]
pub mod pigeon_burn_router {
    use super::*;

    pub fn initialize(
        ctx: Context<Initialize>,
        burn_bps: u16,
        migration_window_open_until: i64,
        has_rate_limiter: bool,
    ) -> Result<()> {
        require!(
            burn_bps >= MIN_BURN_BPS && burn_bps <= MAX_BURN_BPS,
            BurnRouterError::InvalidBurnBps
        );
        let clock = Clock::get()?;
        require!(
            migration_window_open_until > clock.unix_timestamp,
            BurnRouterError::InvalidMigrationWindow
        );

        let config = &mut ctx.accounts.config;
        config.burn_bps = burn_bps;
        config.authority = ctx.accounts.authority.key();
        config.pigeon_mint = ctx.accounts.pigeon_mint.key();
        config.meteora_pool = ctx.accounts.meteora_pool.key();
        config.pigeon_vault = ctx.accounts.pigeon_vault.key();
        config.wsol_vault = ctx.accounts.wsol_vault.key();
        config.paused = false;
        config.total_burned = 0;
        config.migration_window_open_until = migration_window_open_until;
        config.migration_completed = false;
        config.has_rate_limiter = has_rate_limiter;
        config.bump = ctx.bumps.config;

        emit!(RouterInitialized {
            burn_bps,
            authority: config.authority,
            pigeon_mint: config.pigeon_mint,
            meteora_pool: config.meteora_pool,
            migration_window_open_until,
            has_rate_limiter,
        });
        Ok(())
    }

    pub fn initialize_router_atas(_ctx: Context<InitializeRouterAtas>) -> Result<()> {
        Ok(())
    }

    pub fn update_burn_bps(ctx: Context<UpdateConfig>, new_burn_bps: u16) -> Result<()> {
        require!(
            new_burn_bps >= MIN_BURN_BPS && new_burn_bps <= MAX_BURN_BPS,
            BurnRouterError::InvalidBurnBps
        );
        require!(!ctx.accounts.config.paused, BurnRouterError::RouterPaused);
        let old = ctx.accounts.config.burn_bps;
        ctx.accounts.config.burn_bps = new_burn_bps;
        emit!(BurnBpsUpdated {
            old_bps: old,
            new_bps: new_burn_bps,
            authority: ctx.accounts.authority.key(),
        });
        Ok(())
    }

    pub fn set_paused(ctx: Context<UpdateConfig>, paused: bool) -> Result<()> {
        ctx.accounts.config.paused = paused;
        emit!(RouterPauseChanged { paused });
        Ok(())
    }

    pub fn migrate_pool(
        ctx: Context<MigratePool>,
        new_pool: Pubkey,
        new_pigeon_vault: Pubkey,
        new_wsol_vault: Pubkey,
        new_has_rate_limiter: bool,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let config = &mut ctx.accounts.config;
        require!(
            clock.unix_timestamp < config.migration_window_open_until,
            BurnRouterError::MigrationWindowClosed
        );
        require!(!config.migration_completed, BurnRouterError::MigrationAlreadyDone);
        require!(new_pool != Pubkey::default(), BurnRouterError::InvalidPool);

        let old_pool = config.meteora_pool;
        config.meteora_pool = new_pool;
        config.pigeon_vault = new_pigeon_vault;
        config.wsol_vault = new_wsol_vault;
        config.has_rate_limiter = new_has_rate_limiter;
        config.migration_completed = true;

        emit!(PoolMigrated {
            old_pool,
            new_pool,
            timestamp: clock.unix_timestamp,
        });
        Ok(())
    }

    pub fn sweep_dust(ctx: Context<SweepDust>) -> Result<()> {
        let dust = ctx.accounts.router_pigeon_burn_account.amount;
        require!(dust > 0, BurnRouterError::NoDust);

        let sweep_amount = dust.min(DUST_SWEEP_CAP);
        let config = &ctx.accounts.config;
        let signer_seeds: &[&[&[u8]]] = &[&[CONFIG_SEED, &[config.bump]]];

        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.router_pigeon_burn_account.to_account_info(),
                    to: ctx.accounts.burn_treasury.to_account_info(),
                    authority: ctx.accounts.config.to_account_info(),
                },
                signer_seeds,
            ),
            sweep_amount,
        )?;

        emit!(DustSwept {
            amount: sweep_amount,
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }

    pub fn execute_sell_burn(
        ctx: Context<ExecuteSellBurn>,
        token_amount_in: u64,
        min_sol_out: u64,
        min_buyback_out: u64,
        a_to_b: bool,
    ) -> Result<()> {
        require!(!ctx.accounts.config.paused, BurnRouterError::RouterPaused);
        require_gt!(token_amount_in, 0, BurnRouterError::ZeroAmount);
        require_gt!(min_sol_out, 0, BurnRouterError::SlippageNotSet);
        require_gt!(min_buyback_out, 0, BurnRouterError::SlippageNotSet);
        require_gte!(
            ctx.accounts.seller_pigeon_account.amount,
            token_amount_in,
            BurnRouterError::InsufficientBalance
        );

        require_keys_eq!(
            ctx.accounts.pigeon_vault.key(),
            ctx.accounts.config.pigeon_vault,
            BurnRouterError::VaultMismatch
        );
        require_keys_eq!(
            ctx.accounts.wsol_vault.key(),
            ctx.accounts.config.wsol_vault,
            BurnRouterError::VaultMismatch
        );

        let has_rate_limiter = ctx.accounts.config.has_rate_limiter;
        if has_rate_limiter {
            require!(
                !ctx.remaining_accounts.is_empty(),
                BurnRouterError::MissingInstructionsSysvar
            );
            require_keys_eq!(
                ctx.remaining_accounts[0].key(),
                INSTRUCTIONS_SYSVAR_ID,
                BurnRouterError::WrongInstructionsSysvar
            );
        }

        let config = &ctx.accounts.config;
        let burn_bps = config.burn_bps as u64;
        let signer_seeds: &[&[&[u8]]] = &[&[CONFIG_SEED, &[config.bump]]];

        // STEP 1: SELL — PIGEON -> wSOL
        let wsol_before = ctx.accounts.router_wsol_account.amount;

        let sell_accounts = [
            ctx.accounts.meteora_pool.to_account_info(),
            ctx.accounts.seller.to_account_info(),
            ctx.accounts.seller_pigeon_account.to_account_info(),
            ctx.accounts.router_wsol_account.to_account_info(),
            ctx.accounts.pigeon_vault.to_account_info(),
            ctx.accounts.wsol_vault.to_account_info(),
            ctx.accounts.pigeon_mint.to_account_info(),
            ctx.accounts.wsol_mint.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ctx.accounts.clock.to_account_info(),
        ];

        invoke_meteora_swap(
            &ctx.accounts.meteora_program.to_account_info(),
            &sell_accounts,
            token_amount_in,
            min_sol_out,
            a_to_b,
            &[],
            false,
            &[],
        )?;

        // STEP 2: Measure wSOL received
        ctx.accounts.router_wsol_account.reload()?;
        let wsol_received = ctx
            .accounts
            .router_wsol_account
            .amount
            .checked_sub(wsol_before)
            .ok_or(BurnRouterError::MathOverflow)?;
        require_gte!(wsol_received, min_sol_out, BurnRouterError::SlippageExceeded);

        // STEP 3: Split
        let burn_wsol = wsol_received
            .checked_mul(burn_bps)
            .ok_or(BurnRouterError::MathOverflow)?
            .checked_div(10_000)
            .ok_or(BurnRouterError::MathOverflow)?;
        let seller_wsol = wsol_received
            .checked_sub(burn_wsol)
            .ok_or(BurnRouterError::MathOverflow)?;
        require_gt!(burn_wsol, 0, BurnRouterError::BurnAmountTooSmall);
        require_gt!(seller_wsol, 0, BurnRouterError::SellerAmountTooSmall);

        // STEP 4: BUYBACK — wSOL -> PIGEON
        let pigeon_before = ctx.accounts.router_pigeon_burn_account.amount;

        let buyback_accounts = [
            ctx.accounts.meteora_pool.to_account_info(),
            ctx.accounts.config.to_account_info(),
            ctx.accounts.router_wsol_account.to_account_info(),
            ctx.accounts.router_pigeon_burn_account.to_account_info(),
            ctx.accounts.wsol_vault.to_account_info(),
            ctx.accounts.pigeon_vault.to_account_info(),
            ctx.accounts.wsol_mint.to_account_info(),
            ctx.accounts.pigeon_mint.to_account_info(),
            ctx.accounts.token_program.to_account_info(),
            ctx.accounts.system_program.to_account_info(),
            ctx.accounts.clock.to_account_info(),
        ];

        let rate_limiter_remaining: Vec<AccountInfo> = if has_rate_limiter {
            vec![ctx.remaining_accounts[0].clone()]
        } else {
            vec![]
        };

        invoke_meteora_swap(
            &ctx.accounts.meteora_program.to_account_info(),
            &buyback_accounts,
            burn_wsol,
            min_buyback_out,
            !a_to_b,
            signer_seeds,
            has_rate_limiter,
            &rate_limiter_remaining,
        )?;

        // STEP 5: BURN with 941 token supply floor
        // Burn never reduces supply below 941_000_000 raw units (941 tokens at 6 decimals).
        // If supply is already at or below floor, burn is skipped entirely.
        // Sell completes and seller receives SOL regardless.
        ctx.accounts.router_pigeon_burn_account.reload()?;
        let to_burn_raw = ctx
            .accounts
            .router_pigeon_burn_account
            .amount
            .checked_sub(pigeon_before)
            .ok_or(BurnRouterError::MathOverflow)?;
        require_gt!(to_burn_raw, 0, BurnRouterError::NothingToBurn);
        require_gte!(to_burn_raw, min_buyback_out, BurnRouterError::SlippageExceeded);

        let current_supply = ctx.accounts.pigeon_mint.supply;
        let effective_burn = if current_supply <= MIN_SUPPLY_FLOOR {
            0
        } else if current_supply.saturating_sub(to_burn_raw) < MIN_SUPPLY_FLOOR {
            current_supply.saturating_sub(MIN_SUPPLY_FLOOR)
        } else {
            to_burn_raw
        };

        if effective_burn > 0 {
            token::burn(
                CpiContext::new_with_signer(
                    ctx.accounts.token_program.to_account_info(),
                    Burn {
                        mint: ctx.accounts.pigeon_mint.to_account_info(),
                        from: ctx.accounts.router_pigeon_burn_account.to_account_info(),
                        authority: ctx.accounts.config.to_account_info(),
                    },
                    signer_seeds,
                ),
                effective_burn,
            )?;

            ctx.accounts.config.total_burned = ctx
                .accounts
                .config
                .total_burned
                .checked_add(effective_burn)
                .ok_or(BurnRouterError::MathOverflow)?;
        }

        // STEP 6: PAY SELLER
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                Transfer {
                    from: ctx.accounts.router_wsol_account.to_account_info(),
                    to: ctx.accounts.seller_wsol_account.to_account_info(),
                    authority: ctx.accounts.config.to_account_info(),
                },
                signer_seeds,
            ),
            seller_wsol,
        )?;

        emit!(BurnExecuted {
            seller: ctx.accounts.seller.key(),
            tokens_sold: token_amount_in,
            sol_received: wsol_received,
            burn_sol: burn_wsol,
            seller_sol: seller_wsol,
            pigeon_burned: effective_burn,
            total_burned: ctx.accounts.config.total_burned,
            burn_bps: config.burn_bps,
        });

        Ok(())
    }
}

// ============================================================
// METEORA DAMM v2 CPI
// ============================================================

#[allow(clippy::too_many_arguments)]
fn invoke_meteora_swap<'info>(
    program: &AccountInfo<'info>,
    accounts: &[AccountInfo<'info>],
    amount_in: u64,
    min_amount_out: u64,
    a_to_b: bool,
    signer_seeds: &[&[&[u8]]],
    has_rate_limiter: bool,
    remaining: &[AccountInfo<'info>],
) -> Result<()> {
    require_keys_eq!(
        program.key(),
        METEORA_DAMM_V2_PROGRAM,
        BurnRouterError::WrongProgram
    );

    let mut data = Vec::with_capacity(25);
    data.extend_from_slice(&METEORA_SWAP_DISCRIMINATOR);
    data.extend_from_slice(&amount_in.to_le_bytes());
    data.extend_from_slice(&min_amount_out.to_le_bytes());
    data.push(a_to_b as u8);

    let mut account_metas = vec![
        AccountMeta::new(accounts[0].key(), false),
        AccountMeta::new_readonly(accounts[1].key(), true),
        AccountMeta::new(accounts[2].key(), false),
        AccountMeta::new(accounts[3].key(), false),
        AccountMeta::new(accounts[4].key(), false),
        AccountMeta::new(accounts[5].key(), false),
        AccountMeta::new_readonly(accounts[6].key(), false),
        AccountMeta::new_readonly(accounts[7].key(), false),
        AccountMeta::new_readonly(accounts[8].key(), false),
        AccountMeta::new_readonly(accounts[9].key(), false),
        AccountMeta::new_readonly(accounts[10].key(), false),
    ];

    let mut all_accounts: Vec<AccountInfo> = accounts.to_vec();
    if has_rate_limiter && !remaining.is_empty() {
        account_metas.push(AccountMeta::new_readonly(remaining[0].key(), false));
        all_accounts.push(remaining[0].clone());
    }

    let ix = Instruction {
        program_id: program.key(),
        accounts: account_metas,
        data,
    };

    if signer_seeds.is_empty() {
        anchor_lang::solana_program::program::invoke(&ix, &all_accounts)
    } else {
        invoke_signed(&ix, &all_accounts, signer_seeds)
    }
    .map_err(|e| {
        msg!("Meteora DAMM v2 swap failed: {:?}", e);
        BurnRouterError::MeteoraSwapFailed.into()
    })
}

use anchor_lang::solana_program::instruction::AccountMeta;

// ============================================================
// ACCOUNTS
// ============================================================

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = authority,
        space = 8 + RouterConfig::INIT_SPACE,
        seeds = [CONFIG_SEED],
        bump
    )]
    pub config: Account<'info, RouterConfig>,
    #[account(mut)]
    pub authority: Signer<'info>,
    #[account(constraint = pigeon_mint.key() == PIGEON_MINT @ BurnRouterError::WrongMint)]
    pub pigeon_mint: Account<'info, Mint>,
    /// CHECK: DAMM v2 pool
    pub meteora_pool: UncheckedAccount<'info>,
    /// CHECK: PIGEON vault
    pub pigeon_vault: UncheckedAccount<'info>,
    /// CHECK: wSOL vault
    pub wsol_vault: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeRouterAtas<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, RouterConfig>,
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = wsol_mint,
        associated_token::authority = config
    )]
    pub router_wsol_account: Account<'info, TokenAccount>,
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = pigeon_mint,
        associated_token::authority = config
    )]
    pub router_pigeon_burn_account: Account<'info, TokenAccount>,
    #[account(constraint = wsol_mint.key() == WSOL_MINT @ BurnRouterError::WrongMint)]
    pub wsol_mint: Account<'info, Mint>,
    #[account(constraint = pigeon_mint.key() == PIGEON_MINT @ BurnRouterError::WrongMint)]
    pub pigeon_mint: Account<'info, Mint>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = authority @ BurnRouterError::Unauthorized
    )]
    pub config: Account<'info, RouterConfig>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct MigratePool<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = authority @ BurnRouterError::Unauthorized
    )]
    pub config: Account<'info, RouterConfig>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct SweepDust<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, RouterConfig>,
    #[account(
        mut,
        associated_token::mint = pigeon_mint,
        associated_token::authority = config
    )]
    pub router_pigeon_burn_account: Account<'info, TokenAccount>,
    #[account(mut, token::mint = pigeon_mint)]
    pub burn_treasury: Account<'info, TokenAccount>,
    #[account(constraint = pigeon_mint.key() == PIGEON_MINT @ BurnRouterError::WrongMint)]
    pub pigeon_mint: Account<'info, Mint>,
    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ExecuteSellBurn<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, RouterConfig>,
    #[account(mut)]
    pub seller: Signer<'info>,
    #[account(
        mut,
        associated_token::mint = pigeon_mint,
        associated_token::authority = seller
    )]
    pub seller_pigeon_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = wsol_mint,
        associated_token::authority = seller
    )]
    pub seller_wsol_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        constraint = pigeon_mint.key() == PIGEON_MINT @ BurnRouterError::WrongMint
    )]
    pub pigeon_mint: Account<'info, Mint>,
    #[account(
        mut,
        associated_token::mint = wsol_mint,
        associated_token::authority = config
    )]
    pub router_wsol_account: Account<'info, TokenAccount>,
    #[account(
        mut,
        associated_token::mint = pigeon_mint,
        associated_token::authority = config
    )]
    pub router_pigeon_burn_account: Account<'info, TokenAccount>,
    #[account(constraint = wsol_mint.key() == WSOL_MINT @ BurnRouterError::WrongMint)]
    pub wsol_mint: Account<'info, Mint>,
    #[account(
        constraint = meteora_pool.key() == config.meteora_pool @ BurnRouterError::WrongPool
    )]
    /// CHECK: validated against config
    pub meteora_pool: UncheckedAccount<'info>,
    #[account(mut)]
    /// CHECK: binding enforced in body
    pub pigeon_vault: UncheckedAccount<'info>,
    #[account(mut)]
    /// CHECK: binding enforced in body
    pub wsol_vault: UncheckedAccount<'info>,
    #[account(
        constraint = meteora_program.key() == METEORA_DAMM_V2_PROGRAM @ BurnRouterError::WrongProgram
    )]
    /// CHECK: program ID validated
    pub meteora_program: UncheckedAccount<'info>,
    pub clock: Sysvar<'info, Clock>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

// ============================================================
// STATE
// ============================================================

#[account]
#[derive(InitSpace)]
pub struct RouterConfig {
    pub authority: Pubkey,
    pub pigeon_mint: Pubkey,
    pub meteora_pool: Pubkey,
    pub pigeon_vault: Pubkey,
    pub wsol_vault: Pubkey,
    pub burn_bps: u16,
    pub paused: bool,
    pub total_burned: u64,
    pub migration_window_open_until: i64,
    pub migration_completed: bool,
    pub has_rate_limiter: bool,
    pub bump: u8,
}

// ============================================================
// ERRORS
// ============================================================

#[error_code]
pub enum BurnRouterError {
    #[msg("Invalid burn BPS: must be 100-200")]
    InvalidBurnBps,
    #[msg("Router is paused")]
    RouterPaused,
    #[msg("Unauthorized")]
    Unauthorized,
    #[msg("Amount must be > 0")]
    ZeroAmount,
    #[msg("Slippage protection required")]
    SlippageNotSet,
    #[msg("Slippage exceeded")]
    SlippageExceeded,
    #[msg("Math overflow")]
    MathOverflow,
    #[msg("Burn wSOL split too small")]
    BurnAmountTooSmall,
    #[msg("Seller wSOL split too small")]
    SellerAmountTooSmall,
    #[msg("Nothing to burn after buyback")]
    NothingToBurn,
    #[msg("Insufficient seller PIGEON balance")]
    InsufficientBalance,
    #[msg("Vault mismatch")]
    VaultMismatch,
    #[msg("Wrong mint")]
    WrongMint,
    #[msg("Wrong pool")]
    WrongPool,
    #[msg("Wrong program")]
    WrongProgram,
    #[msg("Meteora swap CPI failed")]
    MeteoraSwapFailed,
    #[msg("Migration window closed")]
    MigrationWindowClosed,
    #[msg("Migration already completed")]
    MigrationAlreadyDone,
    #[msg("Invalid pool address")]
    InvalidPool,
    #[msg("Migration window must be in the future")]
    InvalidMigrationWindow,
    #[msg("No dust in router ATA")]
    NoDust,
    #[msg("Rate limiter pool requires SYSVAR_INSTRUCTIONS in remaining_accounts[0]")]
    MissingInstructionsSysvar,
    #[msg("remaining_accounts[0] must be SYSVAR_INSTRUCTIONS_PUBKEY")]
    WrongInstructionsSysvar,
}

// ============================================================
// EVENTS
// ============================================================

#[event]
pub struct RouterInitialized {
    pub burn_bps: u16,
    pub authority: Pubkey,
    pub pigeon_mint: Pubkey,
    pub meteora_pool: Pubkey,
    pub migration_window_open_until: i64,
    pub has_rate_limiter: bool,
}

#[event]
pub struct BurnBpsUpdated {
    pub old_bps: u16,
    pub new_bps: u16,
    pub authority: Pubkey,
}

#[event]
pub struct RouterPauseChanged {
    pub paused: bool,
}

#[event]
pub struct PoolMigrated {
    pub old_pool: Pubkey,
    pub new_pool: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct DustSwept {
    pub amount: u64,
    pub timestamp: i64,
}

#[event]
pub struct BurnExecuted {
    pub seller: Pubkey,
    pub tokens_sold: u64,
    pub sol_received: u64,
    pub burn_sol: u64,
    pub seller_sol: u64,
    pub pigeon_burned: u64,
    pub total_burned: u64,
    pub burn_bps: u16,
}
