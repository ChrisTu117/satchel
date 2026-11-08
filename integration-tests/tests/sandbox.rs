//! Runs the real wasm (res/*.wasm, built by `scripts/build.sh`) in a local NEAR
//! sandbox node: real accounts, real transfers, real receipt ordering.
//!
//! Each test names the README claim it backs.

use near_api::types::account::ContractState;
use near_api::{Account, AccountId, Contract, NearGas, NearToken, NetworkConfig, Signer, Tokens};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
const MILLI: u128 = NEAR / 1000;

/// Contract accounts earn 30% of the gas their code burns, so a wallet's balance can
/// end a little above the exact figure after its code runs. Allow up to 1 milliNEAR.
#[track_caller]
fn assert_about(actual: u128, expected: u128, what: &str) {
    assert!(
        actual >= expected && actual <= expected + MILLI,
        "{what}: got {actual}, expected {expected} (+ at most 1 milliNEAR of gas reward)"
    );
}

fn res(name: &str) -> Vec<u8> {
    let path = format!("{}/../res/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("{path} missing: run scripts/build.sh first"))
}

#[derive(Clone)]
struct Env {
    net: NetworkConfig,
    signer: Arc<Signer>,
    club: AccountId,
    treasury: AccountId,
    wallet_hash: [u8; 32],
}

struct Node {
    sandbox: near_sandbox::Sandbox,
    env: Env,
}

async fn new_account(sandbox: &near_sandbox::Sandbox, name: &str, near: u128) -> testresult::TestResult<AccountId> {
    let id: AccountId = name.parse()?;
    sandbox
        .create_account(id.as_str().parse()?)
        .initial_balance(NearToken::from_near(near))
        .send()
        .await?;
    Ok(id)
}

async fn setup(max_cards: u32) -> testresult::TestResult<Node> {
    let sandbox = near_sandbox::Sandbox::start_sandbox().await?;
    let net = NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse()?);
    let signer = Signer::from_secret_key(near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse()?)?;

    // 1. wallet code as a global contract, addressed by its hash (immutable)
    let wallet_wasm = res("satchel_wallet.wasm");
    let wallet_hash: [u8; 32] = Sha256::digest(&wallet_wasm).into();
    let deployer = new_account(&sandbox, "deployer.sandbox", 100).await?;
    Contract::deploy_global_contract_code(wallet_wasm)
        .as_hash()
        .with_signer(deployer, signer.clone())
        .send_to(&net)
        .await?
        .assert_success();

    // 2. the club
    let club = new_account(&sandbox, "club.sandbox", 50).await?;
    let treasury = new_account(&sandbox, "treasury.sandbox", 1).await?;
    let config = json!({
        "name": "Satchel Club",
        "treasury": treasury,
        "wallet_code_hash": bs58::encode(wallet_hash).into_string(),
        "mint_price": (100 * MILLI).to_string(),
        "wallet_reserve": (10 * MILLI).to_string(),
        "max_cards": max_cards,
        "ticket_min_price": (10 * MILLI).to_string(),
        "member_share_bps": 5000,
        "royalty_bps": 500,
    });
    Contract::deploy(club.clone())
        .use_code(res("satchel_club.wasm"))
        .with_init_call("new", json!({ "config": config }))?
        .with_signer(signer.clone())
        .send_to(&net)
        .await?
        .assert_success();
    Ok(Node {
        sandbox,
        env: Env { net, signer, club, treasury, wallet_hash },
    })
}

impl Env {
    /// Calls the club; returns the JSON result, or Err with the failure.
    async fn call(&self, who: &AccountId, method: &str, args: Value, deposit: u128) -> Result<Value, String> {
        self.call_on(&self.club, who, method, args, deposit, 300).await
    }

    async fn call_on(&self, contract: &AccountId, who: &AccountId, method: &str, args: Value, deposit: u128, tgas: u64) -> Result<Value, String> {
        let r = Contract(contract.clone())
            .call_function(method, args)
            .transaction()
            .deposit(NearToken::from_yoctonear(deposit))
            .gas(NearGas::from_tgas(tgas))
            .with_signer(who.clone(), self.signer.clone())
            .send_to(&self.net)
            .await
            .map_err(|e| e.to_string())?;
        match r.into_result() {
            Ok(ok) => Ok(ok.json::<Value>().unwrap_or(Value::Null)),
            Err(e) => Err(format!("{e:?}")),
        }
    }

    async fn view(&self, method: &str, args: Value) -> Value {
        self.view_on(&self.club, method, args).await
    }

    async fn view_on(&self, contract: &AccountId, method: &str, args: Value) -> Value {
        Contract(contract.clone())
            .call_function(method, args)
            .read_only::<Value>()
            .fetch_from(&self.net)
            .await
            .unwrap()
            .data
    }

    async fn balance(&self, id: &AccountId) -> u128 {
        Tokens::account(id.clone())
            .near_balance()
            .fetch_from(&self.net)
            .await
            .unwrap()
            .total
            .as_yoctonear()
    }

    async fn mint(&self, who: &AccountId, seed: u128) -> String {
        let v = self.call(who, "mint", json!({}), 100 * MILLI + seed).await.unwrap();
        v["token_id"].as_str().unwrap().to_string()
    }

    fn wallet(&self, token_id: &str) -> AccountId {
        format!("{token_id}.{}", self.club).parse().unwrap()
    }

    async fn pay(&self, from: &AccountId, to: &AccountId, yocto: u128) {
        Tokens::account(from.clone())
            .send_to(to.clone())
            .near(NearToken::from_yoctonear(yocto))
            .with_signer(self.signer.clone())
            .send_to(&self.net)
            .await
            .unwrap()
            .assert_success();
    }

    async fn withdraw(&self, who: &AccountId, token: &str, yocto: u128) -> Result<Value, String> {
        self.call(who, "withdraw", json!({ "token_id": token, "amount": yocto.to_string() }), 1).await
    }

    async fn owner(&self, token: &str) -> String {
        self.view("nft_token", json!({ "token_id": token })).await["owner_id"].as_str().unwrap().to_string()
    }
}

// Claims: "minting card N creates the real account N.<collection>, with no access
// keys, running the wallet code by global-contract hash"; "anyone can pay a card
// from any wallet with a plain transfer"; "only the holder can move its money".
#[tokio::test]
async fn card_is_a_real_account() -> testresult::TestResult<()> {
    let node = setup(10).await?;
    let e = &node.env;
    let alice = new_account(&node.sandbox, "alice.sandbox", 20).await?;
    let bob = new_account(&node.sandbox, "bob.sandbox", 20).await?;
    let mallory = new_account(&node.sandbox, "mallory.sandbox", 20).await?;

    let id = e.mint(&alice, NEAR).await; // 1 NEAR seed on top of the price
    assert_eq!(id, "1");
    let wallet = e.wallet("1");
    let view = Account(wallet.clone()).view().fetch_from(&e.net).await?.data;
    assert_eq!(view.contract_state, ContractState::GlobalHash(near_api::types::CryptoHash::try_from(&e.wallet_hash[..]).unwrap()));
    assert_eq!(view.amount.as_yoctonear(), 10 * MILLI + NEAR, "reserve + seed");
    let keys = Account(wallet.clone()).list_keys().fetch_from(&e.net).await?.data;
    assert!(keys.is_empty(), "the card account has no access keys");
    let info = e.view_on(&wallet, "wallet_info", json!({})).await;
    assert_eq!(info["token_id"], "1");
    assert_eq!(info["collection"], e.club.as_str());

    // plain transfer from any account
    e.pay(&bob, &wallet, 2 * NEAR).await;
    assert_eq!(e.balance(&wallet).await, 10 * MILLI + 3 * NEAR);

    // strangers and even the holder cannot call the wallet directly
    assert!(e.call_on(&wallet, &mallory, "send_near", json!({ "receiver_id": mallory, "amount": NEAR.to_string() }), 0, 30).await.is_err());
    assert!(e.call_on(&wallet, &alice, "send_near", json!({ "receiver_id": alice, "amount": NEAR.to_string() }), 0, 30).await.is_err());
    // strangers cannot withdraw through the club
    assert!(e.withdraw(&mallory, "1", NEAR).await.is_err());
    assert_about(e.balance(&wallet).await, 10 * MILLI + 3 * NEAR, "nothing left the wallet");

    // the holder can
    let before = e.balance(&alice).await;
    let w = e.balance(&wallet).await;
    e.withdraw(&alice, "1", NEAR).await.unwrap();
    assert_about(e.balance(&wallet).await, w - NEAR, "wallet paid 1 NEAR");
    assert!(e.balance(&alice).await > before + NEAR - 5 * MILLI, "alice received it (minus her gas)");
    let card = e.view("get_card", json!({ "token_id": "1" })).await;
    assert_eq!(card["withdrawn"], NEAR.to_string());
    assert_eq!(card["busy"], false);

    // a withdrawal larger than the balance fails and unlocks the card
    assert!(e.withdraw(&alice, "1", 50 * NEAR).await.is_ok(), "the club call itself succeeds");
    let card = e.view("get_card", json!({ "token_id": "1" })).await;
    assert_eq!(card["busy"], false);
    assert_eq!(card["events"].as_array().unwrap().last().unwrap()["kind"], "failed");
    let w2 = e.balance(&wallet).await;
    assert_about(w2, w - NEAR, "a failed withdrawal moves nothing");
    Ok(())
}

// Claim: "transferring or selling the card moves the account, its balance and its
// rights; the seller keeps nothing"
#[tokio::test]
async fn control_moves_with_the_card() -> testresult::TestResult<()> {
    let node = setup(10).await?;
    let e = &node.env;
    let alice = new_account(&node.sandbox, "alice.sandbox", 20).await?;
    let bob = new_account(&node.sandbox, "bob.sandbox", 20).await?;
    let dave = new_account(&node.sandbox, "dave.sandbox", 20).await?;
    e.mint(&alice, 3 * NEAR).await;
    let wallet = e.wallet("1");

    e.call(&alice, "post", json!({ "token_id": "1", "text": "hi, alice here" }), 0).await.unwrap();

    // gift to bob
    e.call(&alice, "nft_transfer", json!({ "receiver_id": bob, "token_id": "1" }), 1).await.unwrap();
    assert!(e.withdraw(&alice, "1", NEAR).await.is_err(), "old holder cannot withdraw");
    assert!(e.call(&alice, "post", json!({ "token_id": "1", "text": "still me" }), 0).await.is_err(), "old holder cannot post");
    e.withdraw(&bob, "1", NEAR).await.unwrap();
    e.call(&bob, "post", json!({ "token_id": "1", "text": "bob now" }), 0).await.unwrap();

    // sale to dave through the built-in market: 4 NEAR, 5% royalty
    e.call(&bob, "list_card", json!({ "token_id": "1", "price": (4 * NEAR).to_string() }), 1).await.unwrap();
    let bob_before = e.balance(&bob).await;
    let treasury_before = e.balance(&e.treasury).await;
    e.call(&dave, "buy_card", json!({ "token_id": "1" }), 4 * NEAR).await.unwrap();
    assert_eq!(e.owner("1").await, dave.as_str());
    assert_eq!(e.balance(&bob).await, bob_before + 4 * NEAR * 95 / 100);
    assert_eq!(e.balance(&e.treasury).await, treasury_before + 4 * NEAR * 5 / 100);
    assert!(e.withdraw(&bob, "1", NEAR).await.is_err(), "seller cannot withdraw after the sale");
    let w = e.balance(&wallet).await;
    e.withdraw(&dave, "1", NEAR).await.unwrap();
    assert_about(e.balance(&wallet).await, w - NEAR, "new holder withdrew 1 NEAR");

    let card = e.view("get_card", json!({ "token_id": "1" })).await;
    let holders: Vec<&str> = card["holders"].as_array().unwrap().iter().map(|h| h["account_id"].as_str().unwrap()).collect();
    assert_eq!(holders, vec![alice.as_str(), bob.as_str(), dave.as_str()]);
    let board = e.view("get_board", json!({})).await;
    assert_eq!(board.as_array().unwrap().len(), 2);
    Ok(())
}

// Claim: "a withdrawal cannot race a sale: whichever lands first wins, the seller
// never gets both the sale price and the card's money"
#[tokio::test]
async fn withdrawal_cannot_race_a_sale() -> testresult::TestResult<()> {
    let node = setup(10).await?;
    let e = &node.env;
    let seller = new_account(&node.sandbox, "seller.sandbox", 20).await?;
    let buyer = new_account(&node.sandbox, "buyer.sandbox", 20).await?;
    e.mint(&seller, 5 * NEAR).await;
    let wallet = e.wallet("1");
    e.call(&seller, "list_card", json!({ "token_id": "1", "price": NEAR.to_string() }), 1).await.unwrap();
    let wallet_before = e.balance(&wallet).await;

    // send both at once
    let (w, b) = tokio::join!(
        e.withdraw(&seller, "1", 5 * NEAR),
        e.call(&buyer, "buy_card", json!({ "token_id": "1" }), NEAR)
    );
    let withdrew = w.is_ok() && e.balance(&wallet).await < wallet_before;
    let bought = b.is_ok();
    println!("withdraw ok: {withdrew}, buy ok: {bought}");
    assert!(!(withdrew && bought), "the seller must not get both");
    assert!(withdrew || bought, "one of them goes through");
    if bought {
        assert_eq!(e.owner("1").await, buyer.as_str());
        assert_eq!(e.balance(&wallet).await, wallet_before, "buyer got the full wallet");
    } else {
        assert_eq!(e.owner("1").await, seller.as_str());
    }
    Ok(())
}

// Claim: "50% of each ticket is split equally into every card's account, the rest
// goes to the treasury"; "a payout in flight during a sale lands in the account and
// belongs to the new holder"
#[tokio::test]
async fn ticket_revenue_lands_in_card_accounts() -> testresult::TestResult<()> {
    let node = setup(10).await?;
    let e = &node.env;
    let alice = new_account(&node.sandbox, "alice.sandbox", 20).await?;
    let bob = new_account(&node.sandbox, "bob.sandbox", 20).await?;
    let fan = new_account(&node.sandbox, "fan.sandbox", 50).await?;
    for who in [&alice, &alice, &bob, &bob] {
        e.mint(who, 0).await;
    }
    let before: Vec<u128> = futures_balances(e, 4).await;
    let t_before = e.balance(&e.treasury).await;
    let r = e.call(&fan, "buy_ticket", json!({ "event": "launch" }), 4 * NEAR).await.unwrap();
    assert_eq!(r["per_card"], (NEAR / 2).to_string());
    let after = futures_balances(e, 4).await;
    for i in 0..4 {
        assert_eq!(after[i], before[i] + NEAR / 2, "card {} got 0.5 NEAR", i + 1);
    }
    assert_eq!(e.balance(&e.treasury).await, t_before + 2 * NEAR);

    // sale in the middle of a payout: ticket and transfer sent together
    let (t, x) = tokio::join!(
        e.call(&fan, "buy_ticket", json!({ "event": "second" }), 4 * NEAR),
        e.call(&alice, "nft_transfer", json!({ "receiver_id": bob, "token_id": "1" }), 1)
    );
    t.unwrap();
    x.unwrap();
    assert_eq!(e.owner("1").await, bob.as_str());
    assert_eq!(e.balance(&e.wallet("1")).await, before[0] + NEAR, "both payouts are in card 1's account");
    e.withdraw(&bob, "1", NEAR).await.unwrap();
    assert!(e.withdraw(&alice, "1", 1).await.is_err());
    let card = e.view("get_card", json!({ "token_id": "1" })).await;
    assert_eq!(card["earned_from_tickets"], NEAR.to_string());
    Ok(())
}

async fn futures_balances(e: &Env, n: u32) -> Vec<u128> {
    let mut v = vec![];
    for i in 1..=n {
        v.push(e.balance(&e.wallet(&i.to_string())).await);
    }
    v
}

// Claim: "the card's account can call other contracts for its holder" (here: the
// card buys a ticket with its own money)
#[tokio::test]
async fn card_account_can_act() -> testresult::TestResult<()> {
    let node = setup(10).await?;
    let e = &node.env;
    let alice = new_account(&node.sandbox, "alice.sandbox", 20).await?;
    e.mint(&alice, 2 * NEAR).await;
    let wallet = e.wallet("1");
    let before = e.balance(&wallet).await;
    use base64::Engine;
    let args = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&json!({ "event": "paid by card 1" }))?);
    e.call(
        &alice,
        "act",
        json!({ "token_id": "1", "receiver_id": e.club, "method_name": "buy_ticket", "args": args, "deposit": NEAR.to_string(), "gas_tgas": 50 }),
        1,
    )
    .await
    .unwrap();
    let club = e.view("get_club", json!({})).await;
    assert_eq!(club["tickets_sold"], 1);
    // the card paid 1 NEAR and, as the only card, got 0.5 back as its member share
    assert_about(e.balance(&wallet).await, before - NEAR + NEAR / 2, "card paid 1, got 0.5 back");
    let card = e.view("get_card", json!({ "token_id": "1" })).await;
    assert_eq!(card["spent_by_calls"], NEAR.to_string());
    // a stranger cannot make the card act
    let mallory = new_account(&node.sandbox, "mallory.sandbox", 5).await?;
    assert!(e.call(&mallory, "act", json!({ "token_id": "1", "receiver_id": mallory, "method_name": "x", "args": "", "deposit": "1", "gas_tgas": 5 }), 1).await.is_err());
    Ok(())
}

// Claim: "a ticket sale pays all 100 cards in one transaction"
#[tokio::test]
async fn ticket_split_with_100_cards_fits_in_gas() -> testresult::TestResult<()> {
    let node = setup(100).await?;
    let e = node.env.clone();
    let mut tasks = vec![];
    for m in 0..10 {
        let minter = new_account(&node.sandbox, &format!("m{m}.sandbox"), 20).await?;
        let e = e.clone();
        tasks.push(tokio::spawn(async move {
            for _ in 0..10 {
                e.mint(&minter, 0).await;
            }
        }));
    }
    for t in tasks {
        t.await?;
    }
    assert_eq!(e.view("get_club", json!({})).await["cards"], 100);
    let fan = new_account(&node.sandbox, "fan.sandbox", 50).await?;
    let first = e.balance(&e.wallet("1")).await;
    let last = e.balance(&e.wallet("100")).await;
    let r = e.call(&fan, "buy_ticket", json!({ "event": "big night" }), 10 * NEAR).await.unwrap();
    assert_eq!(r["cards"], 100);
    assert_eq!(e.balance(&e.wallet("1")).await, first + 5 * NEAR / 100);
    assert_eq!(e.balance(&e.wallet("100")).await, last + 5 * NEAR / 100);
    assert!(e.call(&fan, "mint", json!({}), 100 * MILLI).await.is_err(), "max_cards reached");
    Ok(())
}

#[tokio::test]
async fn nobody_else_can_initialize() -> testresult::TestResult<()> {
    let sandbox = near_sandbox::Sandbox::start_sandbox().await?;
    let net = NetworkConfig::from_rpc_url("sandbox", sandbox.rpc_addr.parse()?);
    let signer = Signer::from_secret_key(near_sandbox::config::DEFAULT_GENESIS_ACCOUNT_PRIVATE_KEY.parse()?)?;
    let club = new_account(&sandbox, "club.sandbox", 20).await?;
    let mallory = new_account(&sandbox, "mallory.sandbox", 5).await?;
    Contract::deploy(club.clone())
        .use_code(res("satchel_club.wasm"))
        .without_init_call()
        .with_signer(signer.clone())
        .send_to(&net)
        .await?
        .assert_success();
    let r = Contract(club)
        .call_function("new", json!({ "config": {
            "name": "x", "treasury": mallory, "wallet_code_hash": "11111111111111111111111111111111",
            "mint_price": "1", "wallet_reserve": "1", "max_cards": 1, "ticket_min_price": "1",
            "member_share_bps": 0, "royalty_bps": 0 } }))
        .transaction()
        .with_signer(mallory, signer)
        .send_to(&net)
        .await?;
    assert!(r.into_result().is_err());
    Ok(())
}
