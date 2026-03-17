# 🧀 Cheese Pay — Soroban Smart Contract

![Network](https://img.shields.io/badge/network-Stellar%20Mainnet%20%2F%20Testnet-brightgreen)
![Language](https://img.shields.io/badge/language-Rust-orange)
![SDK](https://img.shields.io/badge/SDK-soroban--sdk%2022.x-blue)
![License](https://img.shields.io/badge/license-MIT-lightgrey)

The on-chain settlement layer for **Cheese Wallet** — a Nigerian USDC wallet where merchants accept crypto and receive Naira. This contract handles all USDC custody, internal transfers, PayLink payment requests, yield staking, and platform fee collection directly on the Stellar network.

---

## Overview

Cheese Pay operates as a **custodial vault contract**. Users deposit USDC into the contract, and all Cheese-to-Cheese transactions happen as internal balance moves — fast, cheap, and fully on-chain. Only deposits and withdrawals touch the underlying Stellar USDC Stellar Asset Contract (SAC).

```
User Wallet (USDC)
      │
      │  deposit()
      ▼
┌─────────────────────────────────┐
│        CheesePay Contract       │
│                                 │
│  Internal Balances (Persistent) │
│  ├─ balance(address)            │
│  ├─ stake_balance(address)      │
│  └─ paylink(token_id)           │
│                                 │
│  Instance Storage               │
│  ├─ admin                       │
│  ├─ usdc_token (SAC address)    │
│  ├─ fee_rate_bps                │
│  ├─ fee_treasury                │
│  └─ paused                      │
└─────────────────────────────────┘
      │
      │  withdraw()
      ▼
User Wallet (USDC)
```

---

## Features

| Feature | Function | Description |
|---|---|---|
| Deposit | `deposit` | Pull USDC from user wallet into Cheese |
| Withdraw | `withdraw` | Return USDC to user's external wallet |
| Internal Transfer | `transfer` | Move USDC between Cheese users, fee deducted |
| Create PayLink | `create_paylink` | Register a payment request on-chain |
| Pay a PayLink | `pay_paylink` | Settle a PayLink from internal balance |
| Cancel PayLink | `cancel_paylink` | Creator cancels an unpaid request |
| Set Fee Rate | `set_fee_rate` | Admin updates platform fee (max 5%) |
| Pause / Unpause | `pause` / `unpause` | Admin emergency circuit breaker |

---

## Prerequisites

| Tool | Version | Install |
|---|---|---|
| Rust | ≥ 1.74 | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| WASM target | `wasm32v1-none` | `rustup target add wasm32v1-none` |
| Stellar CLI | ≥ 25.x | `brew install stellar-cli` or see [install docs](https://developers.stellar.org/docs/tools/cli/install-cli) |

---

## Project Structure

```
cheese-pay-contract/
├── Cargo.toml                  # Workspace root — sets release profile
├── Cargo.lock
└── contracts/
    └── cheese_pay/
        ├── Cargo.toml          # Contract crate — soroban-sdk dependency
        └── src/
            ├── lib.rs          # Full contract implementation
            └── test.rs         # Unit tests
```

---

## Setup

```bash
# 1. Clone the repo
git clone https://github.com/xaxxoo/cheese-stellar.git
cd cheese-pay-contract

# 2. Install the WASM target (if not already done)
rustup target add wasm32v1-none

# 3. Verify stellar CLI is installed
stellar --version
```

---

## Build

```bash
stellar contract build
```

Output WASM:
```
target/wasm32v1-none/release/cheese_pay.wasm
```

To build with optimisation:
```bash
stellar contract build --optimize
```

---

## Test

```bash
cargo test
```

Run a specific test:
```bash
cargo test test_deposit
```

Run tests with output:
```bash
cargo test -- --nocapture
```

---

## Deploy

### 1. Create and fund a testnet identity

```bash
# Generate deployer keypair
stellar keys generate deployer --network testnet

# Fund from Stellar Friendbot
stellar keys fund deployer --network testnet

# Confirm balance
stellar keys address deployer
```

### 2. Get the USDC Stellar Asset Contract (SAC) address

**Testnet USDC issuer:** `GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5`

```bash
stellar contract id asset \
  --network testnet \
  --source-account deployer \
  --asset "USDC:GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5"
```

Save this as `$USDC_CONTRACT_ID`.

**Mainnet USDC issuer (Circle):** `GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN`

```bash
stellar contract id asset \
  --network mainnet \
  --source-account deployer \
  --asset "USDC:GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN"
```

### 3. Deploy the contract

```bash
stellar contract deploy \
  --network testnet \
  --source-account deployer \
  --wasm target/wasm32v1-none/release/cheese_pay.wasm
```

Save the returned address as `$CONTRACT_ID`.

### 4. Initialise the contract

```bash
stellar contract invoke \
  --network testnet \
  --source-account deployer \
  --id $CONTRACT_ID \
  -- initialize \
  --admin $(stellar keys address deployer) \
  --usdc_token $USDC_CONTRACT_ID \
  --fee_rate_bps 30 \
  --fee_treasury $(stellar keys address deployer)
```

> `fee_rate_bps 30` = **0.30%** per transfer and PayLink payment. Acceptable range: 0–500 (max 5%).

---

## Contract Function Reference

All amounts are in **stroops** (1 USDC = `10_000_000` stroops — 7 decimal places).

### `initialize`
| Param | Type | Description |
|---|---|---|
| `admin` | `Address` | Contract admin (can pause, set fees, credit yield) |
| `usdc_token` | `Address` | USDC Stellar Asset Contract address |
| `fee_rate_bps` | `i128` | Platform fee in basis points (30 = 0.3%) |
| `fee_treasury` | `Address` | Address that receives platform fees |

---

### `deposit`
User deposits USDC into their Cheese internal balance.

> ⚠️ The user **must call `approve`** on the USDC SAC contract first, authorising the Cheese contract to pull the amount.

| Param | Type | Description |
|---|---|---|
| `from` | `Address` | User's address (must sign) |
| `amount` | `i128` | Amount in stroops |

```bash
# Step 1 — approve the contract on the USDC SAC
stellar contract invoke \
  --network testnet \
  --source-account alice \
  --id $USDC_CONTRACT_ID \
  -- approve \
  --from $(stellar keys address alice) \
  --spender $CONTRACT_ID \
  --amount 10000000 \
  --expiration_ledger 9999999

# Step 2 — deposit
stellar contract invoke \
  --network testnet \
  --source-account alice \
  --id $CONTRACT_ID \
  -- deposit \
  --from $(stellar keys address alice) \
  --amount 10000000
```

---

### `withdraw`
User withdraws USDC from their Cheese balance back to their wallet.

| Param | Type | Description |
|---|---|---|
| `to` | `Address` | User's address (must sign) |
| `amount` | `i128` | Amount in stroops |

---

### `transfer`
Internal Cheese-to-Cheese transfer. Platform fee is deducted from the sender and sent to the treasury on-chain.

| Param | Type | Description |
|---|---|---|
| `from` | `Address` | Sender (must sign) |
| `to` | `Address` | Recipient |
| `amount` | `i128` | Amount in stroops (fee deducted from this) |

**Fee calculation:**
```
fee = (amount × fee_rate_bps) / 10_000
net_received = amount - fee
```

---

### `create_paylink`
Creator registers a payment request on-chain.

| Param | Type | Description |
|---|---|---|
| `creator` | `Address` | PayLink owner (must sign) |
| `token` | `String` | Unique ID string generated by your backend |
| `amount` | `i128` | Requested amount in stroops |
| `note` | `String` | Payment description (e.g. "Rent split April") |

---

### `pay_paylink`
Payer settles a PayLink from their internal Cheese balance.

| Param | Type | Description |
|---|---|---|
| `payer` | `Address` | Must sign |
| `token` | `String` | PayLink token ID |

---

### `cancel_paylink`
Creator cancels an unpaid PayLink. Cannot cancel a paid link.

| Param | Type | Description |
|---|---|---|
| `creator` | `Address` | Must match original creator and sign |
| `token` | `String` | PayLink token ID |

---

### `stake`
Move internal balance into the yield pool.

| Param | Type | Description |
|---|---|---|
| `from` | `Address` | Must sign |
| `amount` | `i128` | Amount to stake |

---

### `unstake`
Return staked balance to internal balance.

| Param | Type | Description |
|---|---|---|
| `from` | `Address` | Must sign |
| `amount` | `i128` | Amount to unstake |

---

### `credit_yield` *(admin only)*
Admin credits yield earnings to a staker's stake balance (called after off-chain yield calculation).

| Param | Type | Description |
|---|---|---|
| `to` | `Address` | Recipient staker |
| `amount` | `i128` | Yield amount in stroops |

---

### `set_fee_rate` *(admin only)*
Update the platform fee rate.

| Param | Type | Description |
|---|---|---|
| `new_fee_bps` | `i128` | New rate in basis points. Max: 500 (5%) |

---

### `pause` / `unpause` *(admin only)*
Emergency circuit breaker. When paused, all `deposit`, `withdraw`, `transfer`, `create_paylink`, `pay_paylink`, `stake`, and `unstake` calls revert.

---

### View functions

```bash
# Check internal balance
stellar contract invoke --id $CONTRACT_ID --network testnet \
  -- balance --user <ADDRESS>

# Check stake balance
stellar contract invoke --id $CONTRACT_ID --network testnet \
  -- stake_balance --user <ADDRESS>

# Get PayLink details
stellar contract invoke --id $CONTRACT_ID --network testnet \
  -- get_paylink --token "CHZ-abc123"
```

---

## NestJS Integration

Your NestJS backend invokes the contract using `@stellar/stellar-sdk`. Your secret key **never leaves the server**.

```typescript
import * as StellarSdk from '@stellar/stellar-sdk'

const server   = new StellarSdk.rpc.Server('https://soroban-testnet.stellar.org')
const contract = new StellarSdk.Contract(process.env.CHEESE_PAY_CONTRACT_ID)
const keypair  = StellarSdk.Keypair.fromSecret(process.env.STELLAR_SECRET_KEY)

// Helper — build, simulate, sign, submit
async function invoke(operation: StellarSdk.xdr.Operation) {
  const account = await server.getAccount(keypair.publicKey())
  const tx = new StellarSdk.TransactionBuilder(account, {
    fee: StellarSdk.BASE_FEE,
    networkPassphrase: StellarSdk.Networks.TESTNET,
  })
    .addOperation(operation)
    .setTimeout(30)
    .build()

  const simResult = await server.simulateTransaction(tx)
  if (StellarSdk.rpc.Api.isSimulationError(simResult)) {
    throw new Error(simResult.error)
  }

  const preparedTx = StellarSdk.rpc.assembleTransaction(tx, simResult).build()
  preparedTx.sign(keypair)
  return server.sendTransaction(preparedTx)
}

// Deposit
await invoke(contract.call(
  'deposit',
  StellarSdk.nativeToScVal(userAddress, { type: 'address' }),
  StellarSdk.nativeToScVal(amountStroops, { type: 'i128' }),
))

// Internal transfer
await invoke(contract.call(
  'transfer',
  StellarSdk.nativeToScVal(fromAddress, { type: 'address' }),
  StellarSdk.nativeToScVal(toAddress,   { type: 'address' }),
  StellarSdk.nativeToScVal(amountStroops, { type: 'i128' }),
))

// Create PayLink
await invoke(contract.call(
  'create_paylink',
  StellarSdk.nativeToScVal(creatorAddress, { type: 'address' }),
  StellarSdk.nativeToScVal(tokenId,        { type: 'string' }),
  StellarSdk.nativeToScVal(amountStroops,  { type: 'i128' }),
  StellarSdk.nativeToScVal(note,           { type: 'string' }),
))
```

---

## Environment Variables

```env
# .env
CHEESE_PAY_CONTRACT_ID=C...           # deployed contract address
USDC_CONTRACT_ID=C...                 # USDC Stellar Asset Contract
STELLAR_SECRET_KEY=S...               # admin / service account secret key
STELLAR_NETWORK=testnet               # testnet | mainnet
STELLAR_RPC_URL=https://soroban-testnet.stellar.org
```

---

## Security Notes

- **Admin key** should be a multisig or hardware-backed account in production — it controls fees, pausing, and yield credits.
- **Never expose `STELLAR_SECRET_KEY`** in client-side code. All contract invocations must route through your NestJS backend.
- The `pause` function gives the team an emergency stop if a bug is discovered. Wire it to an internal ops dashboard.
- `fee_rate_bps` is capped at 500 (5%) in the contract itself — cannot be overridden even by the admin.
- All balance operations use `i128` overflow-checked arithmetic (`overflow-checks = true` in release profile).
- `create_paylink` enforces token uniqueness — duplicate token IDs will revert, preventing double-registration.

---

## Networks

| Network | RPC URL | USDC Issuer |
|---|---|---|
| Testnet | `https://soroban-testnet.stellar.org` | `GBBD47IF6LWK7P7MDEVSCWR7DPUWV3NY3DTQEVFL4NAT4AQH3ZLLFLA5` |
| Mainnet | `https://mainnet.stellar.validationcloud.io/v1/<API_KEY>` | `GA5ZSEJYB37JRC5AVCIA5MOP4RHTM335X2KGX3IHOJAPP5RE34K4KZVN` |

---

## License

MIT © Cheese Wallet
