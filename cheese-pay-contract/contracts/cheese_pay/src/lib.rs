#![no_std]

mod test;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    token, Address, BytesN, Env, String, Vec, symbol_short,
};

// =============================================================================
// TTL CONSTANTS
// Stellar produces ~1 ledger per 5 seconds
// =============================================================================
const LEDGER_TTL_THRESHOLD:  u32 = 518_400;   // ~30 days before we extend
const LEDGER_TTL_EXTEND_TO:  u32 = 1_036_800; // extend to ~60 days
const PAYLINK_TTL_THRESHOLD: u32 = 103_680;   // ~6 days before we extend
const PAYLINK_TTL_EXTEND_TO: u32 = 207_360;   // extend to ~12 days
const DEPOSIT_ID_TTL:        u32 = 518_400;   // ~30 days fixed — no auto-extend

// =============================================================================
// VALIDATION CONSTANTS
// =============================================================================
const USERNAME_MIN_LEN: u32 = 1;
const USERNAME_MAX_LEN: u32 = 32;
const TOKEN_ID_MIN_LEN: u32 = 1;
const TOKEN_ID_MAX_LEN: u32 = 64;
const NOTE_MAX_LEN:     u32 = 256; // max bytes for a paylink note
const BATCH_MAX_SIZE:   u32 = 50;  // max entries per batch call

// =============================================================================
// ERRORS
// =============================================================================
#[contracterror]
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    AlreadyInitialized      = 1,
    NotInitialized          = 2,
    ContractPaused          = 3,
    InsufficientBalance     = 5,
    InvalidAmount           = 6,
    PayLinkNotFound         = 7,
    PayLinkAlreadyPaid      = 8,
    PayLinkCancelled        = 9,
    PayLinkAlreadyExists    = 10,
    PayLinkExpired          = 11,
    NotPayLinkCreator       = 12,
    FeeTooHigh              = 13,
    UsernameTaken           = 14,
    UsernameNotFound        = 15,
    UserAlreadyRegistered   = 16,
    UserNotFound            = 17,
    SelfTransfer            = 18,
    NonZeroBalance          = 19,
    InvalidUsername         = 20,
    InvalidTokenId          = 21,
    BelowMinWithdrawal      = 22,
    DepositAlreadyProcessed = 23,
    NoPendingAdmin          = 24,
    BatchLengthMismatch     = 25,
    NoteTooLong             = 26,
    BatchTooLarge           = 27,
}

// =============================================================================
// STORAGE KEYS
// =============================================================================
#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    // ── Instance (global config) ──────────────────────────────────────────────
    Admin,
    UsdcToken,
    FeeRateBps,    // u32
    FeeTreasury,
    Paused,
    TotalBalance,  // i128 — running sum of all internal user balances
    MinWithdrawal, // i128 — minimum single withdrawal in stroops (0 = no min)
    PendingAdmin,  // Address — staged candidate for two-step admin transfer

    // ── Persistent (per-user) ─────────────────────────────────────────────────
    Balance(String),          // username        → i128
    UsernameToAddr(String),   // username        → Address
    AddrToUsername(Address),  // Address         → username

    // ── Persistent (per-paylink) ──────────────────────────────────────────────
    PayLink(String),          // token_id        → PayLinkData

    // ── Persistent (idempotency) ──────────────────────────────────────────────
    ProcessedDeposit(String), // deposit_id      → bool  (expires in ~30 days)
}

// =============================================================================
// DATA STRUCTURES
// =============================================================================

/// On-chain payment request.
///
/// `expiration_ledger` — last ledger at which this link can be paid.
/// Backend default: current_ledger_sequence + 103_680  (~6 days)
///
/// `payer_username` / `paid_at_ledger` — populated when `pay_paylink` succeeds.
#[derive(Clone)]
#[contracttype]
pub struct PayLinkData {
    pub creator_username:  String,
    pub amount:            i128,
    pub note:              String,
    pub paid:              bool,
    pub cancelled:         bool,
    pub expiration_ledger: u32,
    pub created_at_ledger: u32,
    pub payer_username:    Option<String>,
    pub paid_at_ledger:    Option<u32>,
}

// =============================================================================
// CONTRACT
// =============================================================================
#[contract]
pub struct CheesePay;

#[contractimpl]
impl CheesePay {

    // =========================================================================
    // INIT
    // =========================================================================

    pub fn initialize(
        env:          Env,
        admin:        Address,
        usdc_token:   Address,
        fee_rate_bps: u32,
        fee_treasury: Address,
    ) -> Result<(), Error> {
        // Require admin's signature to prevent front-running between deploy
        // and initialization — without this anyone could call first and
        // set themselves as admin.
        admin.require_auth();

        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        if fee_rate_bps > 500 {
            return Err(Error::FeeTooHigh);
        }

        env.storage().instance().set(&DataKey::Admin,        &admin);
        env.storage().instance().set(&DataKey::UsdcToken,    &usdc_token);
        env.storage().instance().set(&DataKey::FeeRateBps,   &fee_rate_bps);
        env.storage().instance().set(&DataKey::FeeTreasury,  &fee_treasury);
        env.storage().instance().set(&DataKey::Paused,       &false);
        env.storage().instance().set(&DataKey::TotalBalance, &0_i128);
        env.storage().instance().set(&DataKey::MinWithdrawal,&0_i128);
        Self::extend_instance_ttl(&env);
        Ok(())
    }

    // =========================================================================
    // USER REGISTRATION
    //
    // Called by the Cheese backend the moment a user completes signup.
    // No KYC gate — registration is instant at account creation.
    //
    // `username`        — the user's chosen @handle e.g. "temi"
    // `stellar_address` — Stellar address Cheese generated for this user.
    //                     Cheese holds the private key. User never sees it.
    // =========================================================================

    pub fn register_user(
        env:             Env,
        username:        String,
        stellar_address: Address,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::validate_username(&username)?;

        let u_key = DataKey::UsernameToAddr(username.clone());
        if env.storage().persistent().has(&u_key) {
            return Err(Error::UsernameTaken);
        }

        let a_key = DataKey::AddrToUsername(stellar_address.clone());
        if env.storage().persistent().has(&a_key) {
            return Err(Error::UserAlreadyRegistered);
        }

        env.storage().persistent().set(&u_key, &stellar_address);
        env.storage().persistent().set(&a_key, &username);
        let b_key = DataKey::Balance(username.clone());
        env.storage().persistent().set(&b_key, &0_i128);
        Self::extend_user_keys(&env, &username);

        env.events().publish(
            (symbol_short!("reg_user"), username.clone()),
            stellar_address,
        );

        Ok(())
    }

    // =========================================================================
    // BATCH REGISTER USERS
    //
    // Registers multiple users atomically — if any entry fails the entire
    // batch reverts. `usernames` and `addresses` must have equal length.
    // =========================================================================

    pub fn batch_register_users(
        env:       Env,
        usernames: Vec<String>,
        addresses: Vec<Address>,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;

        if usernames.len() != addresses.len() {
            return Err(Error::BatchLengthMismatch);
        }
        if usernames.len() > BATCH_MAX_SIZE {
            return Err(Error::BatchTooLarge);
        }

        let len = usernames.len();
        let mut i = 0u32;
        while i < len {
            let username = usernames.get(i).unwrap();
            let address  = addresses.get(i).unwrap();

            Self::validate_username(&username)?;

            let u_key = DataKey::UsernameToAddr(username.clone());
            if env.storage().persistent().has(&u_key) {
                return Err(Error::UsernameTaken);
            }
            let a_key = DataKey::AddrToUsername(address.clone());
            if env.storage().persistent().has(&a_key) {
                return Err(Error::UserAlreadyRegistered);
            }

            env.storage().persistent().set(&u_key, &address);
            env.storage().persistent().set(&a_key, &username);
            let b_key = DataKey::Balance(username.clone());
            env.storage().persistent().set(&b_key, &0_i128);
            Self::extend_user_keys(&env, &username);

            env.events().publish(
                (symbol_short!("reg_user"), username.clone()),
                address,
            );

            i += 1;
        }

        Ok(())
    }

    // =========================================================================
    // DEREGISTER USER
    //
    // Removes all on-chain state for a user. Requires zero balance — the backend
    // must fully withdraw before calling this. Frees the username for reuse.
    // =========================================================================

    pub fn deregister_user(
        env:      Env,
        username: String,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;

        let u_key = DataKey::UsernameToAddr(username.clone());
        let stellar_address: Address = env.storage().persistent()
            .get(&u_key)
            .ok_or(Error::UsernameNotFound)?;

        let b_key   = DataKey::Balance(username.clone());
        let balance: i128 = env.storage().persistent().get(&b_key).unwrap_or(0);
        if balance != 0 {
            return Err(Error::NonZeroBalance);
        }

        env.storage().persistent().remove(&u_key);
        env.storage().persistent().remove(&DataKey::AddrToUsername(stellar_address.clone()));
        env.storage().persistent().remove(&b_key);

        env.events().publish(
            (symbol_short!("dereg"), username.clone()),
            stellar_address,
        );

        Ok(())
    }

    // =========================================================================
    // UPDATE ADDRESS
    //
    // Rotates the Stellar address bound to a username.
    // Use when the backend generates a new keypair for a user.
    // The old address mapping is removed; the new one is created.
    // =========================================================================

    pub fn update_address(
        env:         Env,
        username:    String,
        new_address: Address,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;

        let u_key = DataKey::UsernameToAddr(username.clone());
        let old_address: Address = env.storage().persistent()
            .get(&u_key)
            .ok_or(Error::UsernameNotFound)?;

        // No-op if address is already the same
        if old_address == new_address {
            return Ok(());
        }

        // Ensure new address isn't bound to a different username
        let new_a_key = DataKey::AddrToUsername(new_address.clone());
        if env.storage().persistent().has(&new_a_key) {
            return Err(Error::UserAlreadyRegistered);
        }

        // Remove old reverse mapping, write updated mappings
        env.storage().persistent().remove(&DataKey::AddrToUsername(old_address.clone()));
        env.storage().persistent().set(&u_key,     &new_address);
        env.storage().persistent().set(&new_a_key, &username);
        Self::extend_user_keys(&env, &username);

        env.events().publish(
            (symbol_short!("upd_addr"), username),
            (old_address, new_address),
        );

        Ok(())
    }

    // =========================================================================
    // EXTEND USER TTL
    //
    // Admin utility to prevent storage expiry for inactive users.
    // Call periodically for users who haven't transacted in ~30+ days.
    // =========================================================================

    pub fn extend_user_ttl(
        env:      Env,
        username: String,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::require_user_exists(&env, &username)?;
        Self::extend_user_keys(&env, &username);
        Ok(())
    }

    // =========================================================================
    // BATCH EXTEND USER TTL
    //
    // Extends storage TTL for many usernames in one transaction.
    // Silently skips usernames that no longer exist (safe, idempotent).
    // =========================================================================

    pub fn batch_extend_user_ttl(
        env:       Env,
        usernames: Vec<String>,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;

        if usernames.len() > BATCH_MAX_SIZE {
            return Err(Error::BatchTooLarge);
        }

        let len = usernames.len();
        let mut i = 0u32;
        while i < len {
            let username = usernames.get(i).unwrap();
            if env.storage().persistent().has(&DataKey::UsernameToAddr(username.clone())) {
                Self::extend_user_keys(&env, &username);
            }
            i += 1;
        }

        Ok(())
    }

    // =========================================================================
    // DEPOSIT — BY USERNAME
    //
    // Called by the Cheese backend after it detects an inbound USDC payment
    // to the contract address via Horizon event streaming.
    //
    // `deposit_id` — unique identifier for this deposit event, e.g. the
    //   Stellar transaction hash or "txhash:op_index". Stored for ~30 days
    //   to prevent the same Horizon event being credited twice on retries.
    // =========================================================================

    pub fn deposit(
        env:        Env,
        username:   String,
        amount:     i128,
        deposit_id: String,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::require_not_paused(&env)?;

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        Self::check_and_record_deposit(&env, &deposit_id)?;
        Self::require_user_exists(&env, &username)?;

        let key     = DataKey::Balance(username.clone());
        let current = Self::read_balance(&env, &key);
        Self::write_balance(&env, &key, current + amount);
        Self::extend_user_keys(&env, &username);
        Self::adjust_total_balance(&env, amount);

        env.events().publish(
            (symbol_short!("deposit"), username.clone()),
            (amount, deposit_id),
        );

        Ok(())
    }

    // =========================================================================
    // DEPOSIT — BY STELLAR ADDRESS
    //
    // Same as deposit() but identifies the recipient by Stellar address.
    // Use when an external wallet sends USDC directly to a user's address
    // and the backend has the address but not the username.
    // =========================================================================

    pub fn deposit_by_address(
        env:        Env,
        address:    Address,
        amount:     i128,
        deposit_id: String,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::require_not_paused(&env)?;

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        Self::check_and_record_deposit(&env, &deposit_id)?;

        let username: String = env.storage().persistent()
            .get(&DataKey::AddrToUsername(address.clone()))
            .ok_or(Error::UserNotFound)?;

        let key     = DataKey::Balance(username.clone());
        let current = Self::read_balance(&env, &key);
        Self::write_balance(&env, &key, current + amount);
        Self::extend_user_keys(&env, &username);
        Self::adjust_total_balance(&env, amount);

        env.events().publish(
            (symbol_short!("deposit"), username.clone()),
            (amount, address, deposit_id),
        );

        Ok(())
    }

    // =========================================================================
    // WITHDRAW
    //
    // Backend verifies PIN off-chain, then calls withdraw() which atomically:
    //   1. Debits the internal balance on-chain
    //   2. Executes the outbound USDC transfer to `to_address` on-chain
    //
    // Both steps in the same call ensures they never diverge.
    // =========================================================================

    pub fn withdraw(
        env:        Env,
        username:   String,
        amount:     i128,
        to_address: Address,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::require_not_paused(&env)?;

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let min_withdrawal: i128 = env.storage().instance()
            .get(&DataKey::MinWithdrawal)
            .unwrap_or(0);
        if amount < min_withdrawal {
            return Err(Error::BelowMinWithdrawal);
        }

        let key     = DataKey::Balance(username.clone());
        let balance = Self::read_balance(&env, &key);
        if balance < amount {
            return Err(Error::InsufficientBalance);
        }

        // Debit first — before any external action
        Self::write_balance(&env, &key, balance - amount);
        Self::extend_user_keys(&env, &username);
        Self::adjust_total_balance(&env, -amount);

        // Execute outbound USDC transfer on-chain (atomic with the debit)
        let usdc = Self::usdc_client(&env);
        usdc.transfer(&env.current_contract_address(), &to_address, &amount);

        env.events().publish(
            (symbol_short!("withdraw"), username.clone()),
            (amount, to_address),
        );

        Ok(())
    }

    // =========================================================================
    // TRANSFER (username → username)
    //
    // Core Cheese P2P flow. All internal — no USDC moves on the Stellar ledger
    // except for the fee, which is sent directly to the treasury.
    // Backend verifies sender's PIN, then calls this as admin.
    //
    // Fee example at fee_rate_bps = 30 (0.30%):
    //   send 10_000_000 stroops ($1.00)
    //   fee =     30_000 stroops ($0.003)
    //   recipient gets 9_970_000 stroops ($0.997)
    // =========================================================================

    pub fn transfer(
        env:           Env,
        from_username: String,
        to_username:   String,
        amount:        i128,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::require_not_paused(&env)?;

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if from_username == to_username {
            return Err(Error::SelfTransfer);
        }

        Self::require_user_exists(&env, &from_username)?;
        Self::require_user_exists(&env, &to_username)?;

        let fee_bps: u32 = env.storage().instance()
            .get(&DataKey::FeeRateBps)
            .unwrap_or(0);
        let fee = (amount * fee_bps as i128) / 10_000;
        let net = amount - fee;

        let from_key = DataKey::Balance(from_username.clone());
        let from_bal = Self::read_balance(&env, &from_key);
        if from_bal < amount {
            return Err(Error::InsufficientBalance);
        }
        Self::write_balance(&env, &from_key, from_bal - amount);
        Self::extend_user_keys(&env, &from_username);

        let to_key = DataKey::Balance(to_username.clone());
        let to_bal = Self::read_balance(&env, &to_key);
        Self::write_balance(&env, &to_key, to_bal + net);
        Self::extend_user_keys(&env, &to_username);

        // Fee exits the contract — reduce tracked total accordingly
        if fee > 0 {
            Self::adjust_total_balance(&env, -fee);
            let treasury: Address = env.storage().instance()
                .get(&DataKey::FeeTreasury)
                .ok_or(Error::NotInitialized)?;
            let usdc = Self::usdc_client(&env);
            usdc.transfer(&env.current_contract_address(), &treasury, &fee);
        }

        env.events().publish(
            (symbol_short!("transfer"), from_username.clone(), to_username.clone()),
            (amount, fee),
        );

        Ok(())
    }

    // =========================================================================
    // PAYLINK — CREATE
    // =========================================================================

    pub fn create_paylink(
        env:               Env,
        creator_username:  String,
        token_id:          String,
        amount:            i128,
        note:              String,
        expiration_ledger: u32,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::require_not_paused(&env)?;
        Self::validate_token_id(&token_id)?;

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }
        if note.len() > NOTE_MAX_LEN {
            return Err(Error::NoteTooLong);
        }

        Self::require_user_exists(&env, &creator_username)?;

        let key = DataKey::PayLink(token_id.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::PayLinkAlreadyExists);
        }
        if expiration_ledger <= env.ledger().sequence() {
            return Err(Error::PayLinkExpired);
        }

        let link = PayLinkData {
            creator_username: creator_username.clone(),
            amount,
            note,
            paid:              false,
            cancelled:         false,
            expiration_ledger,
            created_at_ledger: env.ledger().sequence(),
            payer_username:    None,
            paid_at_ledger:    None,
        };

        env.storage().persistent().set(&key, &link);
        env.storage().persistent().extend_ttl(&key, PAYLINK_TTL_THRESHOLD, PAYLINK_TTL_EXTEND_TO);

        env.events().publish(
            (symbol_short!("pl_create"), creator_username),
            (token_id, amount),
        );

        Ok(())
    }

    // =========================================================================
    // PAYLINK — PAY
    // =========================================================================

    pub fn pay_paylink(
        env:            Env,
        payer_username: String,
        token_id:       String,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::require_not_paused(&env)?;
        Self::validate_token_id(&token_id)?;

        Self::require_user_exists(&env, &payer_username)?;

        let key  = DataKey::PayLink(token_id.clone());
        let mut link: PayLinkData = env.storage().persistent()
            .get(&key)
            .ok_or(Error::PayLinkNotFound)?;

        if link.paid      { return Err(Error::PayLinkAlreadyPaid); }
        if link.cancelled { return Err(Error::PayLinkCancelled);   }
        if env.ledger().sequence() > link.expiration_ledger {
            return Err(Error::PayLinkExpired);
        }

        let fee_bps: u32 = env.storage().instance()
            .get(&DataKey::FeeRateBps)
            .unwrap_or(0);
        let fee = (link.amount * fee_bps as i128) / 10_000;
        let net = link.amount - fee;

        let payer_key = DataKey::Balance(payer_username.clone());
        let payer_bal = Self::read_balance(&env, &payer_key);
        if payer_bal < link.amount {
            return Err(Error::InsufficientBalance);
        }
        Self::write_balance(&env, &payer_key, payer_bal - link.amount);
        Self::extend_user_keys(&env, &payer_username);

        let creator_key = DataKey::Balance(link.creator_username.clone());
        let creator_bal = Self::read_balance(&env, &creator_key);
        Self::write_balance(&env, &creator_key, creator_bal + net);
        Self::extend_user_keys(&env, &link.creator_username);

        // Fee exits the contract — reduce tracked total accordingly
        if fee > 0 {
            Self::adjust_total_balance(&env, -fee);
            let treasury: Address = env.storage().instance()
                .get(&DataKey::FeeTreasury)
                .ok_or(Error::NotInitialized)?;
            let usdc = Self::usdc_client(&env);
            usdc.transfer(&env.current_contract_address(), &treasury, &fee);
        }

        link.paid           = true;
        link.payer_username = Some(payer_username.clone());
        link.paid_at_ledger = Some(env.ledger().sequence());
        env.storage().persistent().set(&key, &link);

        env.events().publish(
            (symbol_short!("pl_paid"), payer_username, link.creator_username.clone()),
            (token_id, link.amount, fee),
        );

        Ok(())
    }

    // =========================================================================
    // PAYLINK — CANCEL (by creator)
    //
    // Intentionally skips require_not_paused: cancellation moves no funds,
    // so users should be able to cancel even during an emergency pause.
    // =========================================================================

    pub fn cancel_paylink(
        env:              Env,
        creator_username: String,
        token_id:         String,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::validate_token_id(&token_id)?;

        let key  = DataKey::PayLink(token_id.clone());
        let mut link: PayLinkData = env.storage().persistent()
            .get(&key)
            .ok_or(Error::PayLinkNotFound)?;

        if link.creator_username != creator_username {
            return Err(Error::NotPayLinkCreator);
        }
        if link.paid      { return Err(Error::PayLinkAlreadyPaid); }
        if link.cancelled { return Err(Error::PayLinkCancelled);   }

        link.cancelled = true;
        env.storage().persistent().set(&key, &link);

        env.events().publish(
            (symbol_short!("pl_cancel"), creator_username),
            token_id,
        );

        Ok(())
    }

    // =========================================================================
    // PAYLINK — ADMIN CANCEL
    //
    // Force-cancel any paylink regardless of who created it.
    // Intended for fraud/dispute response. Can run while paused.
    // =========================================================================

    pub fn admin_cancel_paylink(
        env:      Env,
        token_id: String,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::validate_token_id(&token_id)?;

        let key  = DataKey::PayLink(token_id.clone());
        let mut link: PayLinkData = env.storage().persistent()
            .get(&key)
            .ok_or(Error::PayLinkNotFound)?;

        if link.paid      { return Err(Error::PayLinkAlreadyPaid); }
        if link.cancelled { return Err(Error::PayLinkCancelled);   }

        link.cancelled = true;
        env.storage().persistent().set(&key, &link);
        // Extend TTL so the cancelled record survives for audit
        env.storage().persistent().extend_ttl(&key, PAYLINK_TTL_THRESHOLD, PAYLINK_TTL_EXTEND_TO);

        env.events().publish(
            (symbol_short!("adm_cncl"), link.creator_username),
            token_id,
        );

        Ok(())
    }

    // =========================================================================
    // PAYLINK — EXTEND STORAGE TTL
    //
    // Prevents a paylink's storage entry from being evicted before it naturally
    // expires. Needed if `expiration_ledger` is set further in the future than
    // the ~12-day storage TTL window.
    // =========================================================================

    pub fn extend_paylink_ttl(
        env:      Env,
        token_id: String,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::validate_token_id(&token_id)?;

        let key = DataKey::PayLink(token_id);
        if !env.storage().persistent().has(&key) {
            return Err(Error::PayLinkNotFound);
        }

        env.storage().persistent().extend_ttl(&key, PAYLINK_TTL_THRESHOLD, PAYLINK_TTL_EXTEND_TO);
        Ok(())
    }

    // =========================================================================
    // VIEW FUNCTIONS
    // =========================================================================

    /// Stellar-only USDC balance in stroops.
    /// Divide by 10_000_000 to get display value in USDC.
    pub fn balance(env: Env, username: String) -> i128 {
        Self::read_balance(&env, &DataKey::Balance(username))
    }

    /// Resolve a @username to its registered Stellar address.
    pub fn resolve_username(env: Env, username: String) -> Result<Address, Error> {
        env.storage().persistent()
            .get(&DataKey::UsernameToAddr(username))
            .ok_or(Error::UsernameNotFound)
    }

    /// Reverse lookup — get @username from a Stellar address.
    pub fn get_username(env: Env, address: Address) -> Result<String, Error> {
        env.storage().persistent()
            .get(&DataKey::AddrToUsername(address))
            .ok_or(Error::UserNotFound)
    }

    /// Returns full PayLink data by token_id.
    pub fn get_paylink(env: Env, token_id: String) -> Result<PayLinkData, Error> {
        env.storage().persistent()
            .get(&DataKey::PayLink(token_id))
            .ok_or(Error::PayLinkNotFound)
    }

    /// Returns true if the username is currently registered.
    pub fn is_registered(env: Env, username: String) -> bool {
        env.storage().persistent().has(&DataKey::UsernameToAddr(username))
    }

    /// Returns true if the address is currently registered.
    pub fn is_address_registered(env: Env, address: Address) -> bool {
        env.storage().persistent().has(&DataKey::AddrToUsername(address))
    }

    pub fn fee_rate(env: Env) -> u32 {
        env.storage().instance().get(&DataKey::FeeRateBps).unwrap_or(0)
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage().instance().get(&DataKey::Paused).unwrap_or(false)
    }

    /// Running sum of all internal user balances in stroops.
    /// Compare against usdc.balance(contract_address) to detect accounting drift.
    pub fn total_internal_balance(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::TotalBalance).unwrap_or(0)
    }

    pub fn get_admin(env: Env) -> Result<Address, Error> {
        env.storage().instance().get(&DataKey::Admin).ok_or(Error::NotInitialized)
    }

    pub fn get_fee_treasury(env: Env) -> Result<Address, Error> {
        env.storage().instance().get(&DataKey::FeeTreasury).ok_or(Error::NotInitialized)
    }

    pub fn get_usdc_token(env: Env) -> Result<Address, Error> {
        env.storage().instance().get(&DataKey::UsdcToken).ok_or(Error::NotInitialized)
    }

    pub fn get_min_withdrawal(env: Env) -> i128 {
        env.storage().instance().get(&DataKey::MinWithdrawal).unwrap_or(0)
    }

    /// Returns the pending admin candidate if one has been proposed, else None.
    pub fn get_pending_admin(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::PendingAdmin)
    }

    // =========================================================================
    // ADMIN FUNCTIONS
    // =========================================================================

    pub fn set_fee_rate(env: Env, new_fee_bps: u32) -> Result<(), Error> {
        Self::require_admin(&env)?;
        if new_fee_bps > 500 {
            return Err(Error::FeeTooHigh);
        }
        env.storage().instance().set(&DataKey::FeeRateBps, &new_fee_bps);
        env.events().publish((symbol_short!("fee_set"),), new_fee_bps);
        Ok(())
    }

    pub fn set_fee_treasury(env: Env, new_treasury: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::FeeTreasury, &new_treasury);
        env.events().publish((symbol_short!("trsry_set"),), new_treasury);
        Ok(())
    }

    /// Update the USDC token contract address.
    /// Use if the SAC address ever changes in a new network environment.
    pub fn set_usdc_token(env: Env, new_token: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::UsdcToken, &new_token);
        env.events().publish((symbol_short!("usdc_set"),), new_token);
        Ok(())
    }

    /// Set the minimum single withdrawal amount in stroops. 0 = no minimum.
    pub fn set_min_withdrawal(env: Env, min_amount: i128) -> Result<(), Error> {
        Self::require_admin(&env)?;
        if min_amount < 0 {
            return Err(Error::InvalidAmount);
        }
        env.storage().instance().set(&DataKey::MinWithdrawal, &min_amount);
        env.events().publish((symbol_short!("min_wdraw"),), min_amount);
        Ok(())
    }

    /// Step 1 of 2 — propose a new admin. Current admin signs.
    /// The candidate address must then call accept_admin() to complete the transfer.
    /// Replaces the old single-step transfer_admin to prevent accidents with dead addresses.
    pub fn propose_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::PendingAdmin, &new_admin);
        env.events().publish((symbol_short!("adm_prop"),), new_admin);
        Ok(())
    }

    /// Step 2 of 2 — accept a pending admin proposal. The proposed new admin signs.
    /// Clears the pending admin slot after promotion.
    pub fn accept_admin(env: Env) -> Result<(), Error> {
        Self::extend_instance_ttl(&env);

        let pending: Address = env.storage().instance()
            .get(&DataKey::PendingAdmin)
            .ok_or(Error::NoPendingAdmin)?;

        // New admin must sign — proves they control the key before promotion
        pending.require_auth();

        env.storage().instance().set(&DataKey::Admin, &pending);
        env.storage().instance().remove(&DataKey::PendingAdmin);
        env.events().publish((symbol_short!("adm_accpt"),), pending);
        Ok(())
    }

    pub fn pause(env: Env) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &true);
        env.events().publish((symbol_short!("paused"),), ());
        Ok(())
    }

    pub fn unpause(env: Env) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Paused, &false);
        env.events().publish((symbol_short!("unpaused"),), ());
        Ok(())
    }

    // =========================================================================
    // SWEEP EXCESS
    //
    // Transfers any USDC held by the contract above the tracked internal balance
    // sum to `recipient`. Handles drift from direct sends, airdrops, or backend
    // discrepancies. No-ops silently if contract USDC ≤ total_internal_balance.
    // =========================================================================

    pub fn sweep_excess(env: Env, recipient: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;

        let usdc          = Self::usdc_client(&env);
        let contract_usdc = usdc.balance(&env.current_contract_address());
        let tracked: i128 = env.storage().instance()
            .get(&DataKey::TotalBalance)
            .unwrap_or(0);

        let excess = contract_usdc - tracked;
        if excess <= 0 {
            return Ok(());
        }

        usdc.transfer(&env.current_contract_address(), &recipient, &excess);
        env.events().publish((symbol_short!("sweep"),), (excess, recipient));
        Ok(())
    }

    // =========================================================================
    // UPGRADE
    //
    // Replaces the contract's WASM bytecode while preserving all storage.
    // `new_wasm_hash` must be the hash of a WASM binary that has already been
    // uploaded to the network via `stellar contract upload`.
    //
    // Only the admin can call this. Use a multisig admin to require M-of-N
    // approval before any upgrade reaches mainnet.
    // =========================================================================

    pub fn upgrade(env: Env, new_wasm_hash: BytesN<32>) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.deployer().update_current_contract_wasm(new_wasm_hash);
        Ok(())
    }

    // =========================================================================
    // PRIVATE HELPERS
    // =========================================================================

    fn require_not_paused(env: &Env) -> Result<(), Error> {
        let paused: bool = env.storage().instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if paused { Err(Error::ContractPaused) } else { Ok(()) }
    }

    fn require_admin(env: &Env) -> Result<(), Error> {
        let admin: Address = env.storage().instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();
        // Extend instance TTL on every admin call so the contract never expires
        // from inactivity. All public functions call require_admin, so a single
        // extend here covers the entire contract surface.
        Self::extend_instance_ttl(env);
        Ok(())
    }

    fn require_user_exists(env: &Env, username: &String) -> Result<(), Error> {
        if !env.storage().persistent()
            .has(&DataKey::UsernameToAddr(username.clone()))
        {
            return Err(Error::UsernameNotFound);
        }
        Ok(())
    }

    fn usdc_client(env: &Env) -> token::TokenClient {
        let addr: Address = env.storage().instance()
            .get(&DataKey::UsdcToken)
            .unwrap();
        token::TokenClient::new(env, &addr)
    }

    fn read_balance(env: &Env, key: &DataKey) -> i128 {
        let balance: i128 = env.storage().persistent()
            .get(key)
            .unwrap_or(0);
        // Always extend TTL regardless of value — a zero-balance account must
        // not be silently evicted; its username/address entries would orphan.
        Self::extend_ttl(env, key);
        balance
    }

    fn write_balance(env: &Env, key: &DataKey, amount: i128) {
        env.storage().persistent().set(key, &amount);
        Self::extend_ttl(env, key);
    }

    fn extend_ttl(env: &Env, key: &DataKey) {
        env.storage().persistent().extend_ttl(key, LEDGER_TTL_THRESHOLD, LEDGER_TTL_EXTEND_TO);
    }

    /// Extend all three storage keys associated with a user in one call.
    /// Call this on every operation that touches a user to prevent their
    /// username/address mappings from expiring while their balance is active.
    fn extend_user_keys(env: &Env, username: &String) {
        let u_key = DataKey::UsernameToAddr(username.clone());
        Self::extend_ttl(env, &u_key);

        // Extend the AddrToUsername reverse mapping too
        let addr_opt: Option<Address> = env.storage().persistent().get(&u_key);
        if let Some(addr) = addr_opt {
            Self::extend_ttl(env, &DataKey::AddrToUsername(addr));
        }

        Self::extend_ttl(env, &DataKey::Balance(username.clone()));
    }

    fn adjust_total_balance(env: &Env, delta: i128) {
        let current: i128 = env.storage().instance()
            .get(&DataKey::TotalBalance)
            .unwrap_or(0);
        env.storage().instance().set(&DataKey::TotalBalance, &(current + delta));
    }

    /// Keep instance storage (admin, config, paused flag, etc.) alive.
    /// Called from require_admin so every admin operation renews it automatically,
    /// and explicitly from initialize and accept_admin.
    fn extend_instance_ttl(env: &Env) {
        env.storage().instance().extend_ttl(LEDGER_TTL_THRESHOLD, LEDGER_TTL_EXTEND_TO);
    }

    /// Verify a deposit_id has not been seen before, then record it.
    /// Stored for DEPOSIT_ID_TTL (~30 days) — sufficient window to catch retries.
    fn check_and_record_deposit(env: &Env, deposit_id: &String) -> Result<(), Error> {
        let key = DataKey::ProcessedDeposit(deposit_id.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::DepositAlreadyProcessed);
        }
        env.storage().persistent().set(&key, &true);
        // threshold = 0 so the TTL is always set to exactly DEPOSIT_ID_TTL
        // (we want a fixed expiry, not a sliding window)
        env.storage().persistent().extend_ttl(&key, 0, DEPOSIT_ID_TTL);
        Ok(())
    }

    fn validate_username(username: &String) -> Result<(), Error> {
        let len = username.len();
        if len < USERNAME_MIN_LEN || len > USERNAME_MAX_LEN {
            return Err(Error::InvalidUsername);
        }
        Ok(())
    }

    fn validate_token_id(token_id: &String) -> Result<(), Error> {
        let len = token_id.len();
        if len < TOKEN_ID_MIN_LEN || len > TOKEN_ID_MAX_LEN {
            return Err(Error::InvalidTokenId);
        }
        Ok(())
    }
}
