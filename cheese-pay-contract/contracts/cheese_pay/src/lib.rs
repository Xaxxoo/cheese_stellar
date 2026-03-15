#![no_std]
use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype,
    token, Address, Env, String, symbol_short,
};

// ─────────────────────────────────────────────────────────────────────────────
// TTL constants (Stellar produces ~1 ledger per 5 seconds)
// Threshold = when we start extending. Extend-to = target lifetime.
// ─────────────────────────────────────────────────────────────────────────────
const BALANCE_TTL_THRESHOLD: u32 = 518_400;   // ~30 days
const BALANCE_TTL_EXTEND_TO: u32 = 1_036_800; // ~60 days
const PAYLINK_TTL_THRESHOLD: u32 = 103_680;   // ~6 days
const PAYLINK_TTL_EXTEND_TO: u32 = 207_360;   // ~12 days

// ─────────────────────────────────────────────────────────────────────────────
// Error codes
// Using #[contracterror] means callers (NestJS, frontend) get typed errors
// instead of opaque panics. Critical for production debugging.
// ─────────────────────────────────────────────────────────────────────────────
#[contracterror]
#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    AlreadyInitialized   = 1,
    NotInitialized       = 2,
    ContractPaused       = 3,
    Unauthorized         = 4,
    InsufficientBalance  = 5,
    InvalidAmount        = 6,
    PayLinkNotFound      = 7,
    PayLinkAlreadyPaid   = 8,
    PayLinkCancelled     = 9,
    PayLinkAlreadyExists = 10,
    PayLinkExpired       = 11,
    NotPayLinkCreator    = 12,
    FeeTooHigh           = 13,
}

// ─────────────────────────────────────────────────────────────────────────────
// Storage keys
// ─────────────────────────────────────────────────────────────────────────────
#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    Admin,
    UsdcToken,
    FeeRateBps,
    FeeTreasury,
    Paused,
    Balance(Address),
    PayLink(String),
}

// ─────────────────────────────────────────────────────────────────────────────
// PayLink data
// expiration_ledger: after this ledger, the link cannot be paid.
// ─────────────────────────────────────────────────────────────────────────────
#[derive(Clone)]
#[contracttype]
pub struct PayLinkData {
    pub creator:           Address,
    pub amount:            i128,
    pub note:              String,
    pub paid:              bool,
    pub cancelled:         bool,
    pub expiration_ledger: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Contract
// ─────────────────────────────────────────────────────────────────────────────
#[contract]
pub struct CheesePay;

#[contractimpl]
impl CheesePay {

    // ── Init ──────────────────────────────────────────────────────────────────

    /// Deploy once. Panics if called again — idempotency guard.
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

    // ── Deposit ───────────────────────────────────────────────────────────────

    /// Pull USDC from user's Stellar wallet into their Cheese internal balance.
    ///
    /// The user does NOT need to call `approve` first. Soroban's auth framework
    /// lets the user sign a single transaction that authorises both:
    ///   1. calling this contract's deposit()
    ///   2. the token.transfer() sub-call from their address
    pub fn deposit(env: Env, from: Address, amount: i128) -> Result<(), Error> {
        Self::require_not_paused(&env)?;
        from.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        // External call — pull USDC from user into this contract.
        // State update happens AFTER to follow checks-effects-interactions.
        let usdc = Self::usdc_client(&env);
        usdc.transfer(&from, &env.current_contract_address(), &amount);

        // Credit internal balance
        let key     = DataKey::Balance(from.clone());
        let current = Self::read_balance(&env, &key);
        Self::write_balance(&env, &key, current + amount);

        // Emit event — backend indexes this for real-time balance updates
        env.events().publish(
            (symbol_short!("deposit"), from.clone()),
            amount,
        );

        Ok(())
    }

    // ── Withdraw ──────────────────────────────────────────────────────────────

    /// Return USDC from internal balance back to user's Stellar wallet.
    pub fn withdraw(env: Env, to: Address, amount: i128) -> Result<(), Error> {
        Self::require_not_paused(&env)?;
        to.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let key     = DataKey::Balance(to.clone());
        let balance = Self::read_balance(&env, &key);

        if balance < amount {
            return Err(Error::InsufficientBalance);
        }

        // Update state BEFORE external call
        Self::write_balance(&env, &key, balance - amount);

        // External call — send USDC from contract back to user
        let usdc = Self::usdc_client(&env);
        usdc.transfer(&env.current_contract_address(), &to, &amount);

        env.events().publish(
            (symbol_short!("withdraw"), to.clone()),
            amount,
        );

        Ok(())
    }

    // ── Internal Transfer ─────────────────────────────────────────────────────

    /// Move USDC between two Cheese users.
    ///
    /// Fee is deducted from the sender and transferred on-chain to the
    /// fee treasury. Net amount is credited to the recipient.
    ///
    /// Example: send $10, fee_rate = 30bps (0.30%)
    ///   fee = 30_000 stroops (~$0.003)
    ///   recipient receives = 9_970_000 stroops (~$9.997)
    pub fn transfer(
        env:    Env,
        from:   Address,
        to:     Address,
        amount: i128,
    ) -> Result<(), Error> {
        Self::require_not_paused(&env)?;
        from.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let fee_bps: i128 = env.storage().instance()
            .get(&DataKey::FeeRateBps).unwrap_or(0);
        let fee = (amount * fee_bps) / 10_000;
        let net = amount - fee;

        // Debit sender
        let from_key = DataKey::Balance(from.clone());
        let from_bal = Self::read_balance(&env, &from_key);
        if from_bal < amount {
            return Err(Error::InsufficientBalance);
        }
        Self::write_balance(&env, &from_key, from_bal - amount);

        // Credit recipient
        let to_key = DataKey::Balance(to.clone());
        let to_bal = Self::read_balance(&env, &to_key);
        Self::write_balance(&env, &to_key, to_bal + net);

        // Transfer fee to treasury via on-chain USDC transfer
        if fee > 0 {
            let treasury: Address = env.storage().instance()
                .get(&DataKey::FeeTreasury)
                .ok_or(Error::NotInitialized)?;
            let usdc = Self::usdc_client(&env);
            usdc.transfer(&env.current_contract_address(), &treasury, &fee);
        }

        env.events().publish(
            (symbol_short!("transfer"), from.clone(), to.clone()),
            (amount, fee),
        );

        Ok(())
    }

    // ── PayLink — Create ──────────────────────────────────────────────────────

    /// Register a payment request on-chain.
    ///
    /// `token_id` is a unique string generated by your backend (e.g. "CHZ-abc123").
    /// `expiration_ledger` is the last ledger on which the link can be paid —
    /// set this to current_ledger + ~103_680 for a ~6 day window.
    pub fn create_paylink(
        env:               Env,
        creator:           Address,
        token_id:          String,
        amount:            i128,
        note:              String,
        expiration_ledger: u32,
    ) -> Result<(), Error> {
        Self::require_not_paused(&env)?;
        creator.require_auth();

        if amount <= 0 {
            return Err(Error::InvalidAmount);
        }

        let key = DataKey::PayLink(token_id.clone());
        if env.storage().persistent().has(&key) {
            return Err(Error::PayLinkAlreadyExists);
        }

        // expiration_ledger must be in the future
        if expiration_ledger <= env.ledger().sequence() {
            return Err(Error::PayLinkExpired);
        }

        let link = PayLinkData {
            creator:    creator.clone(),
            amount,
            note,
            paid:       false,
            cancelled:  false,
            expiration_ledger,
        };

        env.storage().persistent().set(&key, &link);
        env.storage().persistent().extend_ttl(
            &key,
            PAYLINK_TTL_THRESHOLD,
            PAYLINK_TTL_EXTEND_TO,
        );

        env.events().publish(
            (symbol_short!("pl_create"), creator.clone()),
            (token_id, amount),
        );

        Ok(())
    }

    // ── PayLink — Pay ─────────────────────────────────────────────────────────

    /// Settle a PayLink from the payer's internal Cheese balance.
    pub fn pay_paylink(
        env:      Env,
        payer:    Address,
        token_id: String,
    ) -> Result<(), Error> {
        Self::require_not_paused(&env)?;
        payer.require_auth();

        let key  = DataKey::PayLink(token_id.clone());
        let mut link: PayLinkData = env.storage().persistent()
            .get(&key)
            .ok_or(Error::PayLinkNotFound)?;

        if link.paid {
            return Err(Error::PayLinkAlreadyPaid);
        }
        if link.cancelled {
            return Err(Error::PayLinkCancelled);
        }
        if env.ledger().sequence() > link.expiration_ledger {
            return Err(Error::PayLinkExpired);
        }

        let fee_bps: i128 = env.storage().instance()
            .get(&DataKey::FeeRateBps).unwrap_or(0);
        let fee = (link.amount * fee_bps) / 10_000;
        let net = link.amount - fee;

        // Debit payer
        let payer_key = DataKey::Balance(payer.clone());
        let payer_bal = Self::read_balance(&env, &payer_key);
        if payer_bal < link.amount {
            return Err(Error::InsufficientBalance);
        }
        Self::write_balance(&env, &payer_key, payer_bal - link.amount);

        // Credit creator
        let creator_key = DataKey::Balance(link.creator.clone());
        let creator_bal = Self::read_balance(&env, &creator_key);
        Self::write_balance(&env, &creator_key, creator_bal + net);

        // Transfer fee to treasury
        if fee > 0 {
            let treasury: Address = env.storage().instance()
                .get(&DataKey::FeeTreasury)
                .ok_or(Error::NotInitialized)?;
            let usdc = Self::usdc_client(&env);
            usdc.transfer(&env.current_contract_address(), &treasury, &fee);
        }

        // Mark paid and update storage
        link.paid = true;
        env.storage().persistent().set(&key, &link);

        env.events().publish(
            (symbol_short!("pl_paid"), payer.clone(), link.creator.clone()),
            (token_id, link.amount, fee),
        );

        Ok(())
    }

    // ── PayLink — Cancel ──────────────────────────────────────────────────────

    /// Creator cancels an unpaid PayLink.
    /// Cannot cancel an already paid link.
    pub fn cancel_paylink(
        env:      Env,
        creator:  Address,
        token_id: String,
    ) -> Result<(), Error> {
        creator.require_auth();

        let key  = DataKey::PayLink(token_id.clone());
        let mut link: PayLinkData = env.storage().persistent()
            .get(&key)
            .ok_or(Error::PayLinkNotFound)?;

        if link.creator != creator {
            return Err(Error::NotPayLinkCreator);
        }
        if link.paid {
            return Err(Error::PayLinkAlreadyPaid);
        }
        if link.cancelled {
            return Err(Error::PayLinkCancelled);
        }

        link.cancelled = true;
        env.storage().persistent().set(&key, &link);

        env.events().publish(
            (symbol_short!("pl_cancel"), creator.clone()),
            token_id,
        );

        Ok(())
    }

    // ── View functions ────────────────────────────────────────────────────────

    pub fn balance(env: Env, user: Address) -> i128 {
        Self::read_balance(&env, &DataKey::Balance(user))
    }

    pub fn get_paylink(env: Env, token_id: String) -> Result<PayLinkData, Error> {
        env.storage().persistent()
            .get(&DataKey::PayLink(token_id))
            .ok_or(Error::PayLinkNotFound)
    }

    pub fn fee_rate(env: Env) -> i128 {
        env.storage().instance()
            .get(&DataKey::FeeRateBps).unwrap_or(0)
    }

    pub fn is_paused(env: Env) -> bool {
        env.storage().instance()
            .get(&DataKey::Paused).unwrap_or(false)
    }

    // ── Admin ─────────────────────────────────────────────────────────────────

    pub fn set_fee_rate(env: Env, new_fee_bps: i128) -> Result<(), Error> {
        Self::require_admin(&env)?;
        if new_fee_bps < 0 || new_fee_bps > 500 {
            return Err(Error::FeeTooHigh);
        }
        env.storage().instance().set(&DataKey::FeeRateBps, &new_fee_bps);

        env.events().publish(
            (symbol_short!("fee_set"),),
            new_fee_bps,
        );
        Ok(())
    }

    pub fn set_fee_treasury(env: Env, new_treasury: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::FeeTreasury, &new_treasury);
        Ok(())
    }

    /// Transfer admin rights to a new address.
    /// The CURRENT admin must sign, then the new admin takes effect immediately.
    /// Use this to rotate to a multisig once deployed.
    pub fn transfer_admin(env: Env, new_admin: Address) -> Result<(), Error> {
        Self::require_admin(&env)?;
        env.storage().instance().set(&DataKey::Admin, &new_admin);

        env.events().publish(
            (symbol_short!("adm_xfer"),),
            new_admin,
        );
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

    // ── Private helpers ───────────────────────────────────────────────────────

    fn require_not_paused(env: &Env) -> Result<(), Error> {
        let paused: bool = env.storage().instance()
            .get(&DataKey::Paused).unwrap_or(false);
        if paused {
            Err(Error::ContractPaused)
        } else {
            Ok(())
        }
    }

    fn require_admin(env: &Env) -> Result<(), Error> {
        let admin: Address = env.storage().instance()
            .get(&DataKey::Admin)
            .ok_or(Error::NotInitialized)?;
        admin.require_auth();
        Ok(())
    }

    fn usdc_client(env: &Env) -> token::TokenClient {
        let addr: Address = env.storage().instance()
            .get(&DataKey::UsdcToken)
            .unwrap();
        token::TokenClient::new(env, &addr)
    }

    /// Read a persistent balance and extend its TTL in the same operation.
    /// Without TTL extension, balances get archived after ~1 month and
    /// become inaccessible until manually restored — unacceptable for a wallet.
    fn read_balance(env: &Env, key: &DataKey) -> i128 {
        let balance: i128 = env.storage().persistent()
            .get(key).unwrap_or(0);
        if balance > 0 {
            env.storage().persistent().extend_ttl(
                key,
                BALANCE_TTL_THRESHOLD,
                BALANCE_TTL_EXTEND_TO,
            );
        }
        balance
    }

    fn write_balance(env: &Env, key: &DataKey, amount: i128) {
        env.storage().persistent().set(key, &amount);
        env.storage().persistent().extend_ttl(
            key,
            BALANCE_TTL_THRESHOLD,
            BALANCE_TTL_EXTEND_TO,
        );
    }
}