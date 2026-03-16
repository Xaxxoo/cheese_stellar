#![cfg(test)]
use super::*;
use soroban_sdk::{
    testutils::Address as _,
    token::{StellarAssetClient, TokenClient},
    Address, Env, String,
};

fn setup() -> (Env, Address, Address, Address, Address) {
    let env     = Env::default();
    env.mock_all_auths();

    let admin    = Address::generate(&env);
    let treasury = Address::generate(&env);

    // Deploy a mock USDC token
    let usdc_addr = env.register_stellar_asset_contract_v2(admin.clone()).address();

    // Deploy the CheesePay contract
    let contract_id = env.register(CheesePay, ());
    let client      = CheesePayClient::new(&env, &contract_id);

    client.initialize(&admin, &usdc_addr, &30_i128, &treasury);

    (env, contract_id, usdc_addr, admin, treasury)
}

#[test]
fn test_register_and_deposit() {
    let (env, contract_id, usdc_addr, admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let user_addr = Address::generate(&env);
    let username  = String::from_str(&env, "temi");

    // Register user
    client.register_user(&username, &user_addr);

    // Mint USDC to the contract (simulating an inbound deposit)
    let sac = StellarAssetClient::new(&env, &usdc_addr);
    sac.mint(&contract_id, &10_000_000_i128); // 1 USDC

    // Credit the deposit
    client.deposit(&username, &10_000_000_i128);

    // Check balance
    assert_eq!(client.balance(&username), 10_000_000_i128);
}

#[test]
fn test_transfer_with_fee() {
    let (env, contract_id, usdc_addr, admin, treasury) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let addr_a = Address::generate(&env);
    let addr_b = Address::generate(&env);
    let user_a = String::from_str(&env, "temi");
    let user_b = String::from_str(&env, "ade");

    client.register_user(&user_a, &addr_a);
    client.register_user(&user_b, &addr_b);

    // Fund contract and deposit for temi
    let sac = StellarAssetClient::new(&env, &usdc_addr);
    sac.mint(&contract_id, &10_000_000_i128);
    client.deposit(&user_a, &10_000_000_i128);

    // Transfer from temi → ade
    // fee = 10_000_000 * 30 / 10_000 = 30_000 stroops
    client.transfer(&user_a, &user_b, &10_000_000_i128);

    assert_eq!(client.balance(&user_a), 0_i128);
    assert_eq!(client.balance(&user_b), 9_970_000_i128); // net after fee
}

#[test]
fn test_paylink_flow() {
    let (env, contract_id, usdc_addr, admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let addr_creator = Address::generate(&env);
    let addr_payer   = Address::generate(&env);
    let creator      = String::from_str(&env, "seun");
    let payer        = String::from_str(&env, "kolade");
    let token_id     = String::from_str(&env, "CHZ-abc123");
    let note         = String::from_str(&env, "Dinner split");

    client.register_user(&creator, &addr_creator);
    client.register_user(&payer,   &addr_payer);

    // Fund payer
    let sac = StellarAssetClient::new(&env, &usdc_addr);
    sac.mint(&contract_id, &5_000_000_i128);
    client.deposit(&payer, &5_000_000_i128);

    // Creator makes a paylink for 0.50 USDC
    let expiry = env.ledger().sequence() + 103_680;
    client.create_paylink(&creator, &token_id, &5_000_000_i128, &note, &expiry);

    // Payer settles it
    client.pay_paylink(&payer, &token_id);

    assert_eq!(client.balance(&payer), 0_i128);
    assert!(client.balance(&creator) > 0_i128);
}

#[test]
#[should_panic]
fn test_double_payment_prevented() {
    let (env, contract_id, usdc_addr, admin, _) = setup();
    let client = CheesePayClient::new(&env, &contract_id);

    let addr_creator = Address::generate(&env);
    let addr_payer   = Address::generate(&env);
    let creator  = String::from_str(&env, "seun");
    let payer    = String::from_str(&env, "kolade");
    let token_id = String::from_str(&env, "CHZ-xyz");
    let note     = String::from_str(&env, "");

    client.register_user(&creator, &addr_creator);
    client.register_user(&payer,   &addr_payer);

    let sac = StellarAssetClient::new(&env, &usdc_addr);
    sac.mint(&contract_id, &20_000_000_i128);
    client.deposit(&payer, &20_000_000_i128);

    let expiry = env.ledger().sequence() + 103_680;
    client.create_paylink(&creator, &token_id, &5_000_000_i128, &note, &expiry);
    client.pay_paylink(&payer, &token_id);
    client.pay_paylink(&payer, &token_id); // should panic — already paid
}