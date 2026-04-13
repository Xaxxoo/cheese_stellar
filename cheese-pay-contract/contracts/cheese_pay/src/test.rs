#![cfg(test)]
use super::*;
use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    vec, Address, Env, String,
};

fn setup() -> (Env, Address, Address, Address, Address) {
    let env     = Env::default();
    env.mock_all_auths();

    let admin    = Address::generate(&env);
    let treasury = Address::generate(&env);

    let usdc_addr   = env.register_stellar_asset_contract_v2(admin.clone()).address();
    let contract_id = env.register(CheesePay, ());
    let client      = CheesePayClient::new(&env, &contract_id);

    client.initialize(&admin, &usdc_addr, &30_u32, &treasury);

    (env, contract_id, usdc_addr, admin, treasury)
}

// ─── helpers ──────────────────────────────────────────────────────────────────

fn dep_id(env: &Env, s: &str) -> String { String::from_str(env, s) }
fn uname(env: &Env, s: &str)  -> String { String::from_str(env, s) }
fn tok(env: &Env, s: &str)    -> String { String::from_str(env, s) }

fn mint_and_deposit(
    env:         &Env,
    sac:         &StellarAssetClient,
    client:      &CheesePayClient,
    contract_id: &Address,
    username:    &String,
    amount:      i128,
    deposit_id:  &String,
) {
    sac.mint(contract_id, &amount);
    client.deposit(username, &amount, deposit_id);
}

// ─── registration ─────────────────────────────────────────────────────────────

#[test]
fn test_register_and_deposit() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let user = uname(&env, "temi");
    client.register_user(&user, &Address::generate(&env));

    mint_and_deposit(&env, &sac, &client, &contract_id, &user, 10_000_000, &dep_id(&env, "d1"));

    assert_eq!(client.balance(&user), 10_000_000);
    assert_eq!(client.total_internal_balance(), 10_000_000);
    assert!(client.is_registered(&user));
}

#[test]
fn test_invalid_username_rejected() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    // Empty username
    let result = client.try_register_user(
        &String::from_str(&env, ""),
        &Address::generate(&env),
    );
    assert_eq!(result, Err(Ok(Error::InvalidUsername)));

    // Username > 32 bytes
    let long = String::from_str(&env, "this_username_is_way_too_long_123");
    let result = client.try_register_user(&long, &Address::generate(&env));
    assert_eq!(result, Err(Ok(Error::InvalidUsername)));
}

#[test]
fn test_batch_register_users() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let usernames = vec![
        &env,
        uname(&env, "akin"),
        uname(&env, "bola"),
        uname(&env, "chidi"),
    ];
    let addresses = vec![
        &env,
        Address::generate(&env),
        Address::generate(&env),
        Address::generate(&env),
    ];

    client.batch_register_users(&usernames, &addresses);

    assert!(client.is_registered(&uname(&env, "akin")));
    assert!(client.is_registered(&uname(&env, "bola")));
    assert!(client.is_registered(&uname(&env, "chidi")));
}

#[test]
fn test_batch_length_mismatch() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let usernames = vec![&env, uname(&env, "akin"), uname(&env, "bola")];
    let addresses = vec![&env, Address::generate(&env)];

    let result = client.try_batch_register_users(&usernames, &addresses);
    assert_eq!(result, Err(Ok(Error::BatchLengthMismatch)));
}

#[test]
fn test_deregister_user() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let user = uname(&env, "temi");
    client.register_user(&user, &Address::generate(&env));

    assert!(client.is_registered(&user));

    client.deregister_user(&user);

    assert!(!client.is_registered(&user));
}

#[test]
fn test_deregister_user_with_balance_fails() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let user = uname(&env, "temi");
    client.register_user(&user, &Address::generate(&env));
    mint_and_deposit(&env, &sac, &client, &contract_id, &user, 1_000_000, &dep_id(&env, "d1"));

    let result = client.try_deregister_user(&user);
    assert_eq!(result, Err(Ok(Error::NonZeroBalance)));
}

#[test]
fn test_update_address() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let user    = uname(&env, "temi");
    let old_addr = Address::generate(&env);
    let new_addr = Address::generate(&env);

    client.register_user(&user, &old_addr);
    assert!(client.is_address_registered(&old_addr));
    assert!(!client.is_address_registered(&new_addr));

    client.update_address(&user, &new_addr);

    assert!(!client.is_address_registered(&old_addr));
    assert!(client.is_address_registered(&new_addr));
    assert_eq!(client.resolve_username(&user), new_addr);
    assert_eq!(client.get_username(&new_addr), user);
}

#[test]
fn test_update_address_noop_when_same() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let user = uname(&env, "temi");
    let addr = Address::generate(&env);
    client.register_user(&user, &addr);

    // Updating to the same address should succeed silently
    client.update_address(&user, &addr);
    assert_eq!(client.resolve_username(&user), addr);
}

#[test]
fn test_update_address_taken_fails() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let user_a = uname(&env, "temi");
    let user_b = uname(&env, "ade");
    let addr_a = Address::generate(&env);
    let addr_b = Address::generate(&env);

    client.register_user(&user_a, &addr_a);
    client.register_user(&user_b, &addr_b);

    // Trying to assign addr_b (already owned by user_b) to user_a should fail
    let result = client.try_update_address(&user_a, &addr_b);
    assert_eq!(result, Err(Ok(Error::UserAlreadyRegistered)));
}

// ─── deposit idempotency ──────────────────────────────────────────────────────

#[test]
fn test_duplicate_deposit_rejected() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let user = uname(&env, "temi");
    client.register_user(&user, &Address::generate(&env));
    sac.mint(&contract_id, &20_000_000);

    let id = dep_id(&env, "horizon-tx-abc123");
    client.deposit(&user, &10_000_000, &id);

    // Same deposit_id a second time must fail
    let result = client.try_deposit(&user, &10_000_000, &id);
    assert_eq!(result, Err(Ok(Error::DepositAlreadyProcessed)));

    // Balance must remain unchanged
    assert_eq!(client.balance(&user), 10_000_000);
}

#[test]
fn test_distinct_deposit_ids_accepted() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let user = uname(&env, "temi");
    client.register_user(&user, &Address::generate(&env));
    sac.mint(&contract_id, &20_000_000);

    client.deposit(&user, &10_000_000, &dep_id(&env, "dep-001"));
    client.deposit(&user, &10_000_000, &dep_id(&env, "dep-002"));

    assert_eq!(client.balance(&user), 20_000_000);
}

// ─── transfer ─────────────────────────────────────────────────────────────────

#[test]
fn test_transfer_with_fee() {
    let (env, contract_id, usdc_addr, _admin, treasury) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let user_a = uname(&env, "temi");
    let user_b = uname(&env, "ade");
    client.register_user(&user_a, &Address::generate(&env));
    client.register_user(&user_b, &Address::generate(&env));

    mint_and_deposit(&env, &sac, &client, &contract_id, &user_a, 10_000_000, &dep_id(&env, "d1"));

    // fee = 10_000_000 * 30 / 10_000 = 30_000 stroops
    client.transfer(&user_a, &user_b, &10_000_000);

    assert_eq!(client.balance(&user_a), 0);
    assert_eq!(client.balance(&user_b), 9_970_000);
    assert_eq!(client.total_internal_balance(), 9_970_000);

    let usdc = TokenClient::new(&env, &usdc_addr);
    assert_eq!(usdc.balance(&treasury), 30_000);
}

#[test]
fn test_self_transfer_rejected() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let user = uname(&env, "temi");
    client.register_user(&user, &Address::generate(&env));
    mint_and_deposit(&env, &sac, &client, &contract_id, &user, 5_000_000, &dep_id(&env, "d1"));

    let result = client.try_transfer(&user, &user, &1_000_000);
    assert_eq!(result, Err(Ok(Error::SelfTransfer)));
}

// ─── withdraw ─────────────────────────────────────────────────────────────────

#[test]
fn test_withdraw_on_chain() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let user = uname(&env, "temi");
    let dest = Address::generate(&env);
    client.register_user(&user, &Address::generate(&env));
    mint_and_deposit(&env, &sac, &client, &contract_id, &user, 10_000_000, &dep_id(&env, "d1"));

    client.withdraw(&user, &6_000_000, &dest);

    assert_eq!(client.balance(&user), 4_000_000);
    assert_eq!(client.total_internal_balance(), 4_000_000);

    let usdc = TokenClient::new(&env, &usdc_addr);
    assert_eq!(usdc.balance(&dest), 6_000_000);
}

#[test]
fn test_min_withdrawal_enforced() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let user = uname(&env, "temi");
    let dest = Address::generate(&env);
    client.register_user(&user, &Address::generate(&env));
    mint_and_deposit(&env, &sac, &client, &contract_id, &user, 10_000_000, &dep_id(&env, "d1"));

    // Set minimum to 1 USDC
    client.set_min_withdrawal(&1_000_000);

    // Withdrawal of 0.50 USDC should fail
    let result = client.try_withdraw(&user, &500_000, &dest);
    assert_eq!(result, Err(Ok(Error::BelowMinWithdrawal)));

    // Withdrawal at exactly the minimum should succeed
    client.withdraw(&user, &1_000_000, &dest);
    assert_eq!(client.balance(&user), 9_000_000);
}

// ─── paylink ──────────────────────────────────────────────────────────────────

#[test]
fn test_paylink_flow_with_metadata() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let creator  = uname(&env, "seun");
    let payer    = uname(&env, "kolade");
    let token_id = tok(&env, "CHZ-abc123");
    let note     = String::from_str(&env, "Dinner split");

    client.register_user(&creator, &Address::generate(&env));
    client.register_user(&payer,   &Address::generate(&env));

    sac.mint(&contract_id, &5_000_000);
    client.deposit(&payer, &5_000_000, &dep_id(&env, "d1"));

    let expiry = env.ledger().sequence() + 103_680;
    client.create_paylink(&creator, &token_id, &5_000_000, &note, &expiry);

    // Check created_at_ledger is set
    let link_before = client.get_paylink(&token_id);
    assert_eq!(link_before.created_at_ledger, env.ledger().sequence());
    assert_eq!(link_before.payer_username, None);
    assert_eq!(link_before.paid_at_ledger, None);

    client.pay_paylink(&payer, &token_id);

    // Check payer_username and paid_at_ledger are populated
    let link_after = client.get_paylink(&token_id);
    assert!(link_after.paid);
    assert_eq!(link_after.payer_username, Some(payer.clone()));
    assert_eq!(link_after.paid_at_ledger, Some(env.ledger().sequence()));

    assert_eq!(client.balance(&payer), 0);
    assert!(client.balance(&creator) > 0);
}

#[test]
fn test_admin_cancel_paylink() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let creator  = uname(&env, "seun");
    let payer    = uname(&env, "kolade");
    let token_id = tok(&env, "CHZ-fraud");
    let note     = String::from_str(&env, "");

    client.register_user(&creator, &Address::generate(&env));
    client.register_user(&payer,   &Address::generate(&env));
    sac.mint(&contract_id, &5_000_000);
    client.deposit(&payer, &5_000_000, &dep_id(&env, "d1"));

    let expiry = env.ledger().sequence() + 103_680;
    client.create_paylink(&creator, &token_id, &5_000_000, &note, &expiry);

    // Admin force-cancels regardless of creator
    client.admin_cancel_paylink(&token_id);

    let link = client.get_paylink(&token_id);
    assert!(link.cancelled);

    // Payer can no longer pay it
    let result = client.try_pay_paylink(&payer, &token_id);
    assert_eq!(result, Err(Ok(Error::PayLinkCancelled)));
}

#[test]
fn test_extend_paylink_ttl() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let creator  = uname(&env, "seun");
    let token_id = tok(&env, "CHZ-longrun");
    let note     = String::from_str(&env, "");

    client.register_user(&creator, &Address::generate(&env));

    let expiry = env.ledger().sequence() + 103_680;
    client.create_paylink(&creator, &token_id, &1_000_000, &note, &expiry);

    // Should succeed without panicking
    client.extend_paylink_ttl(&token_id);
}

#[test]
fn test_invalid_token_id_rejected() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let creator = uname(&env, "seun");
    client.register_user(&creator, &Address::generate(&env));

    let note   = String::from_str(&env, "");
    let expiry = env.ledger().sequence() + 103_680;

    // Empty token_id
    let result = client.try_create_paylink(
        &creator,
        &String::from_str(&env, ""),
        &1_000_000,
        &note,
        &expiry,
    );
    assert_eq!(result, Err(Ok(Error::InvalidTokenId)));
}

#[test]
#[should_panic]
fn test_double_payment_prevented() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);
    let sac    = StellarAssetClient::new(&env, &usdc_addr);

    let creator  = uname(&env, "seun");
    let payer    = uname(&env, "kolade");
    let token_id = tok(&env, "CHZ-xyz");
    let note     = String::from_str(&env, "");

    client.register_user(&creator, &Address::generate(&env));
    client.register_user(&payer,   &Address::generate(&env));
    sac.mint(&contract_id, &20_000_000);
    client.deposit(&payer, &20_000_000, &dep_id(&env, "d1"));

    let expiry = env.ledger().sequence() + 103_680;
    client.create_paylink(&creator, &token_id, &5_000_000, &note, &expiry);
    client.pay_paylink(&payer, &token_id);
    client.pay_paylink(&payer, &token_id); // should panic — already paid
}

// ─── admin management ─────────────────────────────────────────────────────────

#[test]
fn test_propose_accept_admin() {
    let (env, contract_id, _usdc_addr, _admin, _) = setup();
    let client    = CheesePayClient::new(&env, &contract_id);
    let new_admin = Address::generate(&env);

    // Step 1: current admin proposes
    client.propose_admin(&new_admin);
    assert_eq!(client.get_pending_admin(), Some(new_admin.clone()));

    // Step 2: proposed admin accepts (mock_all_auths covers their signature)
    client.accept_admin();
    assert_eq!(client.get_admin(), new_admin);
    assert_eq!(client.get_pending_admin(), None);
}

#[test]
fn test_accept_admin_without_proposal_fails() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let result = client.try_accept_admin();
    assert_eq!(result, Err(Ok(Error::NoPendingAdmin)));
}

// ─── view functions ───────────────────────────────────────────────────────────

#[test]
fn test_view_functions() {
    let (env, contract_id, usdc_addr, admin, treasury) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    assert_eq!(client.get_admin(),        admin);
    assert_eq!(client.get_fee_treasury(), treasury);
    assert_eq!(client.get_usdc_token(),   usdc_addr);
    assert_eq!(client.get_min_withdrawal(), 0);
    assert_eq!(client.fee_rate(), 30_u32);
    assert!(!client.is_paused());

    let user = uname(&env, "temi");
    assert!(!client.is_registered(&user));
    client.register_user(&user, &Address::generate(&env));
    assert!(client.is_registered(&user));
}

#[test]
fn test_is_address_registered() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let addr = Address::generate(&env);
    assert!(!client.is_address_registered(&addr));

    client.register_user(&uname(&env, "temi"), &addr);
    assert!(client.is_address_registered(&addr));
}

// ─── admin config functions ───────────────────────────────────────────────────

#[test]
fn test_set_usdc_token() {
    let (env, contract_id, _, _admin, _) = setup();
    let client    = CheesePayClient::new(&env, &contract_id);
    let new_token = Address::generate(&env);

    client.set_usdc_token(&new_token);
    assert_eq!(client.get_usdc_token(), new_token);
}

// ─── sweep excess ─────────────────────────────────────────────────────────────

#[test]
fn test_sweep_excess() {
    let (env, contract_id, usdc_addr, _admin, _) = setup();
    let client    = CheesePayClient::new(&env, &contract_id);
    let sac       = StellarAssetClient::new(&env, &usdc_addr);
    let recipient = Address::generate(&env);

    let user = uname(&env, "temi");
    client.register_user(&user, &Address::generate(&env));

    // Mint 15 USDC to the contract but only record 10 USDC as a deposit
    sac.mint(&contract_id, &15_000_000);
    client.deposit(&user, &10_000_000, &dep_id(&env, "d1"));

    client.sweep_excess(&recipient);

    let usdc = TokenClient::new(&env, &usdc_addr);
    assert_eq!(usdc.balance(&recipient), 5_000_000);
    assert_eq!(client.balance(&user), 10_000_000);
    assert_eq!(client.total_internal_balance(), 10_000_000);
}

// ─── note / batch limits ──────────────────────────────────────────────────────

#[test]
fn test_note_too_long_rejected() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let creator  = uname(&env, "seun");
    let token_id = tok(&env, "CHZ-note");
    // Build a 257-byte note (one over the 256-byte limit)
    let long_note = String::from_str(&env, &"x".repeat(257));
    let expiry = env.ledger().sequence() + 103_680;

    client.register_user(&creator, &Address::generate(&env));

    let result = client.try_create_paylink(&creator, &token_id, &1_000_000, &long_note, &expiry);
    assert_eq!(result, Err(Ok(Error::NoteTooLong)));
}

#[test]
fn test_batch_too_large_rejected() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    // Build 51 identical entries (one over the 50-entry limit).
    // We reuse the same username string — the batch size check fires before
    // any per-entry validation so the duplicate doesn't matter here.
    let mut usernames_vec = soroban_sdk::Vec::new(&env);
    let mut addresses_vec = soroban_sdk::Vec::new(&env);
    let placeholder = uname(&env, "x");
    for _ in 0u32..51 {
        usernames_vec.push_back(placeholder.clone());
        addresses_vec.push_back(Address::generate(&env));
    }

    let result = client.try_batch_register_users(&usernames_vec, &addresses_vec);
    assert_eq!(result, Err(Ok(Error::BatchTooLarge)));
}

// ─── TTL helpers ──────────────────────────────────────────────────────────────

#[test]
fn test_extend_user_ttl() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let user = uname(&env, "temi");
    client.register_user(&user, &Address::generate(&env));
    client.extend_user_ttl(&user);
}

#[test]
fn test_batch_extend_user_ttl() {
    let (env, contract_id, _, _admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let user_a = uname(&env, "temi");
    let user_b = uname(&env, "ade");
    client.register_user(&user_a, &Address::generate(&env));
    client.register_user(&user_b, &Address::generate(&env));

    // Should extend both without panicking; unknown names are skipped silently
    let batch = vec![&env, user_a.clone(), user_b.clone(), uname(&env, "ghost")];
    client.batch_extend_user_ttl(&batch);
}
