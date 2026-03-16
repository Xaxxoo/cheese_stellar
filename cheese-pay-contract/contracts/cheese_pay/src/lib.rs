#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    token, Address, Env, String, symbol_short,
};

// =============================================================================
// TTL CONSTANTS
// Stellar produces ~1 ledger per 5 seconds
// =============================================================================
const LEDGER_TTL_THRESHOLD: u32 = 518_400;   // ~30 days before we extend
const LEDGER_TTL_EXTEND_TO: u32 = 1_036_800; // extend to ~60 days
const PAYLINK_TTL_THRESHOLD: u32 = 103_680;  // ~6 days before we extend
const PAYLINK_TTL_EXTEND_TO: u32 = 207_360;  // extend to ~12 days

// =============================================================================
// ERRORS
// =============================================================================
#[contracterror]
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    AlreadyInitialized    = 1,
    NotInitialized        = 2,
    ContractPaused        = 3,
    Unauthorized          = 4,
    InsufficientBalance   = 5,
    InvalidAmount         = 6,
    PayLinkNotFound       = 7,
    PayLinkAlreadyPaid    = 8,
    PayLinkCancelled      = 9,
    PayLinkAlreadyExists  = 10,
    PayLinkExpired        = 11,
    NotPayLinkCreator     = 12,
    FeeTooHigh            = 13,
    UsernameTaken         = 14,
    UsernameNotFound      = 15,
    UserAlreadyRegistered = 16,
    UserNotFound          = 17,
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
    FeeRateBps,
    FeeTreasury,
    Paused,

    // ── Persistent (per-user) ─────────────────────────────────────────────────
    Balance(String),          // username        → i128 balance in stroops
    UsernameToAddr(String),   // username        → Stellar Address
    AddrToUsername(Address),  // Stellar Address → username

    // ── Persistent (per-paylink) ──────────────────────────────────────────────
    PayLink(String),          // token_id        → PayLinkData
}

// =============================================================================
// DATA STRUCTURES
// =============================================================================

/// On-chain payment request created by a Cheese user.
///
/// `expiration_ledger` — last ledger at which this link can be paid.
/// Backend default: current_ledger_sequence + 103_680  (~6 days)
#[derive(Clone)]
#[contracttype]
pub struct PayLinkData {
    pub creator_username:  String,
    pub amount:            i128,
    pub note:              String,
    pub paid:              bool,
    pub cancelled:         bool,
    pub expiration_ledger: u32,
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
        fee_rate_bps: i128,
        fee_treasury: Address,
    ) -> Result<(), Error> {
        if env.storage().instance().has(&DataKey::Admin) {
            return Err(Error::AlreadyInitialized);
        }
        if fee_rate_bps < 0 || fee_rate_bps > 500 {
            return Err(Error::FeeTooHigh);
        }
        env.storage().instance().set(&DataKey::Admin,       &admin);
        env.storage().instance().set(&DataKey::UsdcToken,   &usdc_token);
        env.storage().instance().set(&DataKey::FeeRateBps,  &fee_rate_bps);
        env.storage().instance().set(&DataKey::FeeTreasury, &fee_treasury);
        env.storage().instance().set(&DataKey::Paused,      &false);
        Ok(())
    }

    // =========================================================================
    // USER REGISTRATION
    //
    // Called by the Cheese backend the moment a user completes signup.
    // No KYC gate — registration is instant at account creation.
    //
    // KYC only controls tier limits and features in the NestJS backend.
    // It plays no role here in the contract.
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
        Self::extend_ttl(&env, &u_key);
        Self::extend_ttl(&env, &a_key);

        let b_key = DataKey::Balance(username.clone());
        env.storage().persistent().set(&b_key, &0_i128);
        Self::extend_ttl(&env, &b_key);

        env.events().publish(
            (symbol_short!("reg_user"), username.clone()),
            stellar_address,
        );

        Ok(())
    }

    // =========================================================================
    // DEPOSIT — BY USERNAME
    //
    // Called by the Cheese backend after it detects an inbound USDC payment
    // to the contract address via Horizon event streaming.
    //
    // Use this when the backend can map the deposit to a known username
    // directly (e.g. user deposited from inside the Cheese app).
    // =========================================================================

    pub fn deposit(
        env:      Env,
        username: String,
        amount:   i128,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::require_not_paused(&env)?;

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        Self::require_user_exists(&env, &username)?;

        let key     = DataKey::Balance(username.clone());
        let current = Self::read_balance(&env, &key);
        Self::write_balance(&env, &key, current + amount);

        env.events().publish(
            (symbol_short!("deposit"), username.clone()),
            amount,
        );

        Ok(())
    }

    // =========================================================================
    // DEPOSIT — BY STELLAR ADDRESS
    //
    // Same as deposit() but accepts a Stellar address instead of a username.
    //
    // Use this when:
    //   - An external wallet sends USDC directly to a user's Cheese address
    //   - The backend has the sender's Stellar address but not their username
    //   - A user shares their Stellar address instead of @username
    //
    // Resolves the address → username internally, then credits the balance.
    // Reverts with UserNotFound if the address is not registered.
    // =========================================================================

    pub fn deposit_by_address(
        env:     Env,
        address: Address,
        amount:  i128,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;
        Self::require_not_paused(&env)?;

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // Resolve Stellar address → username
        let username: String = env.storage().persistent()
            .get(&DataKey::AddrToUsername(address.clone()))
            .ok_or(Error::UserNotFound)?;

        let key     = DataKey::Balance(username.clone());
        let current = Self::read_balance(&env, &key);
        Self::write_balance(&env, &key, current + amount);

        env.events().publish(
            (symbol_short!("deposit"), username.clone()),
            (amount, address),
        );

        Ok(())
    }

    // =========================================================================
    // WITHDRAW
    //
    // Backend verifies PIN off-chain, then:
    //   1. Calls withdraw() to debit internal balance on-chain
    //   2. Constructs and sends the outbound USDC Stellar payment to
    //      `to_address` separately via the Stellar SDK
    //
    // `to_address` is emitted in the event so the backend can read it and
    // route the outbound payment without needing to store it separately.
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

        let key     = DataKey::Balance(username.clone());
        let balance = Self::read_balance(&env, &key);

        if balance < amount {
            return Err(Error::InsufficientBalance);
        }

        // Debit first — before any external action
        Self::write_balance(&env, &key, balance - amount);

        env.events().publish(
            (symbol_short!("withdraw"), username.clone()),
            (amount, to_address),
        );

        Ok(())
    }

    // =========================================================================
    // TRANSFER (username → username)
    //
    // Core Cheese P2P flow. All internal — no USDC moves on the Stellar ledger.
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

        Self::require_user_exists(&env, &from_username)?;
        Self::require_user_exists(&env, &to_username)?;

        let fee_bps: i128 = env.storage().instance()
            .get(&DataKey::FeeRateBps)
            .unwrap_or(0);
        let fee = (amount * fee_bps) / 10_000;
        let net = amount - fee;

        // Debit sender
        let from_key = DataKey::Balance(from_username.clone());
        let from_bal = Self::read_balance(&env, &from_key);
        if from_bal < amount {
            return Err(Error::InsufficientBalance);
        }
        Self::write_balance(&env, &from_key, from_bal - amount);

        // Credit recipient
        let to_key = DataKey::Balance(to_username.clone());
        let to_bal = Self::read_balance(&env, &to_key);
        Self::write_balance(&env, &to_key, to_bal + net);

        // Transfer fee to treasury via on-chain USDC payment
        if fee > 0 {
            let treasury: Address = env.storage().instance()
                .get(&DataKey::FeeTreasury)
                .ok_or(Error::NotInitialized)?;
            let usdc = Self::usdc_client(&env);
            usdc.transfer(
                &env.current_contract_address(),
                &treasury,
                &fee,
            );
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

        if amount <= 0 {
            return Err(Error::InvalidAmount);
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
            paid:             false,
            cancelled:        false,
            expiration_ledger,
        };

        env.storage().persistent().set(&key, &link);
        env.storage().persistent().extend_ttl(
            &key,
            PAYLINK_TTL_THRESHOLD,
            PAYLINK_TTL_EXTEND_TO,
        );

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

        let fee_bps: i128 = env.storage().instance()
            .get(&DataKey::FeeRateBps)
            .unwrap_or(0);
        let fee = (link.amount * fee_bps) / 10_000;
        let net = link.amount - fee;

        // Debit payer
        let payer_key = DataKey::Balance(payer_username.clone());
        let payer_bal = Self::read_balance(&env, &payer_key);
        if payer_bal < link.amount {
            return Err(Error::InsufficientBalance);
        }
        Self::write_balance(&env, &payer_key, payer_bal - link.amount);

        // Credit creator
        let creator_key = DataKey::Balance(link.creator_username.clone());
        let creator_bal = Self::read_balance(&env, &creator_key);
        Self::write_balance(&env, &creator_key, creator_bal + net);

        // Fee to treasury
        if fee > 0 {
            let treasury: Address = env.storage().instance()
                .get(&DataKey::FeeTreasury)
                .ok_or(Error::NotInitialized)?;
            let usdc = Self::usdc_client(&env);
            usdc.transfer(
                &env.current_contract_address(),
                &treasury,
                &fee,
            );
        }

        link.paid = true;
        env.storage().persistent().set(&key, &link);

        env.events().publish(
            (symbol_short!("pl_paid"), payer_username, link.creator_username.clone()),
            (token_id, link.amount, fee),
        );

        Ok(())
    }

    // =========================================================================
    // PAYLINK — CANCEL
    // =========================================================================

    pub fn cancel_paylink(
        env:              Env,
        creator_username: String,
        token_id:         String,
    ) -> Result<(), Error> {
        Self::require_admin(&env)?;

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
    // VIEW FUNCTIONS
    // =========================================================================

    /// Stellar-only USDC balance in stroops.
    /// Divide by 10_000_000 to get display value in USDC.
    /// The NestJS wallet service adds the EVM balance on top of this.
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

    pub fn fee_rate(env: Env) -> i128 {
        env.storage().instance()
            .get(&DataKey::FeeRateBps)
            .unwrap_or(0)
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage().instance()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    // =========================================================================
    // ADMIN FUNCTIONS
    // =========================================================================

    pub fn set_fee_rate(env: Env, new_fee_bps: i128) -> Result<(), Error> {
        Self::require_admin(&env)?;
        if new_fee_bps < 0 || new_fee_bps > 500 {
            return Err(Error::FeeTooHigh);
        }
        env.storage().instance().set(&DataKey::FeeRateBps, &new_fee_bps);
        env.events().publish((symbol_short!("fee_set"),), new_fee_bps);
        Ok(())
    }

    pub fn set_fee_treasury(env: Env, new_treasury: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::FeeTreasury, &new_treasury);
        Ok(())
    }

    /// Rotate the admin keypair. Current admin must sign this transaction.
    /// Use after deployment to move to a multisig or MPC wallet.
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);
        env.events().publish((symbol_short!("adm_xfer"),), new_admin);
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
        if balance > 0 {
            Self::extend_ttl(env, key);
        }
        balance
    }

    fn write_balance(env: &Env, key: &DataKey, amount: i128) {
        env.storage().persistent().set(key, &amount);
        Self::extend_ttl(env, key);
    }

    fn extend_ttl(env: &Env, key: &DataKey) {
        env.storage().persistent().extend_ttl(
            key,
            LEDGER_TTL_THRESHOLD,
            LEDGER_TTL_EXTEND_TO,
        );
    }
}