//! Unit tests for Satchel's bookkeeping. Promise execution (real accounts, real
//! transfers, races) is covered by the sandbox tests in integration-tests/.

use super::*;
use near_sdk::test_utils::{VMContextBuilder, get_logs};
use near_sdk::{PromiseResult, RuntimeFeesConfig, test_vm_config, testing_env};

const NEAR: u128 = 1_000_000_000_000_000_000_000_000;
const MILLI: u128 = NEAR / 1000;

fn club_id() -> AccountId {
    "club.near".parse().unwrap()
}
fn acc(n: &str) -> AccountId {
    format!("{n}.near").parse().unwrap()
}

fn cfg() -> ClubConfig {
    ClubConfig {
        name: "Satchel Club".into(),
        treasury: acc("treasury"),
        wallet_code_hash: [7u8; 32].into(),
        mint_price: U128(100 * MILLI),
        wallet_reserve: U128(10 * MILLI),
        max_cards: 5,
        ticket_min_price: U128(10 * MILLI),
        member_share_bps: 5000,
        royalty_bps: 500,
    }
}

fn ctx(who: &AccountId, deposit: u128) {
    testing_env!(
        VMContextBuilder::new()
            .current_account_id(club_id())
            .predecessor_account_id(who.clone())
            .signer_account_id(who.clone())
            .attached_deposit(NearToken::from_yoctonear(deposit))
            .block_timestamp(1_800_000_000_000_000_000)
            .build()
    );
}

/// Context for a callback, with the given promise result.
fn callback_ctx(ok: bool) {
    let result = if ok {
        PromiseResult::Successful(vec![])
    } else {
        PromiseResult::Failed
    };
    testing_env!(
        VMContextBuilder::new()
            .current_account_id(club_id())
            .predecessor_account_id(club_id())
            .build(),
        test_vm_config(),
        RuntimeFeesConfig::test(),
        Default::default(),
        vec![result]
    );
}

fn setup() -> Club {
    ctx(&club_id(), 0);
    Club::new(cfg())
}

fn mint(c: &mut Club, who: &AccountId) -> String {
    ctx(who, 100 * MILLI);
    c.mint(None).token_id
}

fn kinds(c: &Club, id: &str) -> Vec<String> {
    c.get_card(id.into()).unwrap().events.iter().map(|e| e.kind.clone()).collect()
}

// Claim: "minting card N creates the account N.<collection>"
#[test]
fn mint_assigns_wallet_account() {
    let mut c = setup();
    let id = mint(&mut c, &acc("alice"));
    assert_eq!(id, "1");
    let card = c.get_card("1".into()).unwrap();
    assert_eq!(card.wallet.as_str(), "1.club.near");
    assert_eq!(card.owner_id, acc("alice"));
    assert_eq!(kinds(&c, "1"), vec!["minted"]);
    let t = c.nft_token("1".into()).unwrap();
    assert!(t.metadata.unwrap().extra.unwrap().contains("1.club.near"));
    assert_eq!(c.get_club().cards, 1);
}

#[test]
fn mint_extra_deposit_seeds_wallet() {
    let mut c = setup();
    ctx(&acc("alice"), 600 * MILLI);
    c.mint(None);
    let ev = &c.get_card("1".into()).unwrap().events[0];
    assert_eq!(ev.amount.unwrap().0, 500 * MILLI);
}

#[test]
#[should_panic(expected = "attach at least")]
fn mint_needs_price() {
    let mut c = setup();
    ctx(&acc("alice"), 99 * MILLI);
    c.mint(None);
}

#[test]
#[should_panic(expected = "all cards are minted")]
fn mint_capped() {
    let mut c = setup();
    for _ in 0..6 {
        mint(&mut c, &acc("alice"));
    }
}

// Claim: "only the current holder can use the card's account"
#[test]
#[should_panic(expected = "only the current card holder")]
fn stranger_cannot_withdraw() {
    let mut c = setup();
    mint(&mut c, &acc("alice"));
    ctx(&acc("mallory"), 1);
    let _ = c.withdraw("1".into(), U128(1), None);
}

// Claim: "a card is busy while a wallet order is in flight: no second order, no
// transfer, no listing, no sale until it settles"
#[test]
fn busy_card_blocks_everything_until_callback() {
    let mut c = setup();
    let alice = acc("alice");
    mint(&mut c, &alice);
    ctx(&alice, 1);
    let _ = c.withdraw("1".into(), U128(5 * MILLI), None);
    assert!(c.get_card("1".into()).unwrap().busy);

    for attempt in 0..3 {
        ctx(&alice, 1);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match attempt {
            0 => {
                let _ = c.withdraw("1".into(), U128(1), None);
            }
            1 => c.nft_transfer(acc("bob"), "1".into(), None, None),
            _ => c.list_card("1".into(), U128(NEAR)),
        }));
        assert!(r.is_err(), "attempt {attempt} must fail while busy");
    }

    callback_ctx(true);
    assert!(c.on_wallet_done("1".into(), "withdrew".into(), alice.clone(), alice.clone(), U128(5 * MILLI), None));
    let card = c.get_card("1".into()).unwrap();
    assert!(!card.busy);
    assert_eq!(card.withdrawn.0, 5 * MILLI);
    assert_eq!(kinds(&c, "1"), vec!["minted", "withdrew"]);
}

#[test]
fn failed_wallet_order_unlocks_and_records_nothing() {
    let mut c = setup();
    let alice = acc("alice");
    mint(&mut c, &alice);
    ctx(&alice, 1);
    let _ = c.withdraw("1".into(), U128(50 * NEAR), None);
    callback_ctx(false);
    assert!(!c.on_wallet_done("1".into(), "withdrew".into(), alice.clone(), alice.clone(), U128(50 * NEAR), None));
    let card = c.get_card("1".into()).unwrap();
    assert!(!card.busy);
    assert_eq!(card.withdrawn.0, 0);
    assert_eq!(kinds(&c, "1"), vec!["minted", "failed"]);
}

// Claim: "any withdrawal cancels the card's sale listing"
#[test]
fn withdraw_cancels_listing() {
    let mut c = setup();
    let alice = acc("alice");
    mint(&mut c, &alice);
    ctx(&alice, 1);
    c.list_card("1".into(), U128(NEAR));
    assert!(c.get_card("1".into()).unwrap().listing.is_some());
    ctx(&alice, 1);
    let _ = c.withdraw("1".into(), U128(1), None);
    assert!(c.get_card("1".into()).unwrap().listing.is_none());
    ctx(&acc("bob"), NEAR);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.buy_card("1".into())));
    assert!(r.is_err(), "buying a cancelled listing must fail");
}

// Claim: "selling a card moves it at once; the seller gets price minus 5% royalty"
#[test]
fn buy_card_moves_ownership_and_pays_royalty() {
    let mut c = setup();
    let (alice, bob) = (acc("alice"), acc("bob"));
    mint(&mut c, &alice);
    ctx(&alice, 1);
    c.list_card("1".into(), U128(2 * NEAR));
    ctx(&bob, 2 * NEAR + 3);
    let t = c.buy_card("1".into());
    assert_eq!(t.owner_id, bob);
    let card = c.get_card("1".into()).unwrap();
    assert!(card.listing.is_none());
    assert_eq!(card.holders.last().unwrap().account_id, bob);
    assert_eq!(card.holders.last().unwrap().how, "sale");
    let club = c.get_club();
    assert_eq!(club.cards_sold, 1);
    assert_eq!(club.paid_to_treasury.0, 2 * NEAR * 5 / 100);
    assert!(get_logs().iter().any(|l| l.contains("nft_transfer") && l.contains("sold")));
    // the old holder has no rights left
    ctx(&alice, 1);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = c.withdraw("1".into(), U128(1), None);
    }));
    assert!(r.is_err());
    ctx(&alice, 0);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.post("1".into(), "still mine?".into())));
    assert!(r.is_err());
}

#[test]
#[should_panic(expected = "attached deposit is below the price")]
fn buy_card_needs_full_price() {
    let mut c = setup();
    mint(&mut c, &acc("alice"));
    ctx(&acc("alice"), 1);
    c.list_card("1".into(), U128(2 * NEAR));
    ctx(&acc("bob"), NEAR);
    c.buy_card("1".into());
}

// Claim: "transferring a card cancels its listing"
#[test]
fn transfer_clears_listing() {
    let mut c = setup();
    let alice = acc("alice");
    mint(&mut c, &alice);
    ctx(&alice, 1);
    c.list_card("1".into(), U128(NEAR));
    ctx(&alice, 1);
    c.nft_transfer(acc("bob"), "1".into(), None, None);
    let card = c.get_card("1".into()).unwrap();
    assert!(card.listing.is_none());
    assert_eq!(card.owner_id, acc("bob"));
    assert_eq!(kinds(&c, "1"), vec!["minted", "listed", "unlisted", "transferred"]);
}

#[test]
#[should_panic(expected = "only the card holder can transfer it")]
fn stranger_cannot_transfer() {
    let mut c = setup();
    mint(&mut c, &acc("alice"));
    ctx(&acc("mallory"), 1);
    c.nft_transfer(acc("mallory"), "1".into(), None, None);
}

// Claim: "50% of each ticket is split equally into every card's account; cards
// minted later do not share earlier sales"
#[test]
fn ticket_split() {
    let mut c = setup();
    for n in ["a", "b", "c"] {
        mint(&mut c, &acc(n));
    }
    ctx(&acc("fan"), NEAR);
    let r = c.buy_ticket("launch party".into());
    let per = NEAR / 2 / 3;
    assert_eq!(r.per_card.0, per);
    assert_eq!(r.cards, 3);
    assert_eq!(r.to_treasury.0, NEAR - 3 * per);
    mint(&mut c, &acc("d"));
    ctx(&acc("fan"), NEAR);
    let r2 = c.buy_ticket("second".into());
    assert_eq!(r2.per_card.0, NEAR / 2 / 4);
    assert_eq!(c.get_card("1".into()).unwrap().earned_from_tickets.0, per + NEAR / 2 / 4);
    assert_eq!(c.get_card("4".into()).unwrap().earned_from_tickets.0, NEAR / 2 / 4);
    let club = c.get_club();
    assert_eq!(club.tickets_sold, 2);
    assert_eq!(club.ticket_revenue.0, 2 * NEAR);
    assert_eq!(club.paid_to_cards.0 + club.paid_to_treasury.0, 2 * NEAR);
}

#[test]
#[should_panic(expected = "below the minimum")]
fn ticket_min_price() {
    let mut c = setup();
    ctx(&acc("fan"), MILLI);
    c.buy_ticket("x".into());
}

// Claim: "only current holders can post on the members' board"
#[test]
fn board_follows_the_card() {
    let mut c = setup();
    let (alice, bob) = (acc("alice"), acc("bob"));
    mint(&mut c, &alice);
    ctx(&alice, 0);
    c.post("1".into(), "hello club".into());
    ctx(&alice, 1);
    c.nft_transfer(bob.clone(), "1".into(), None, None);
    ctx(&bob, 0);
    c.post("1".into(), "new member here".into());
    ctx(&alice, 0);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.post("1".into(), "me again".into())));
    assert!(r.is_err());
    let board = c.get_board(None);
    assert_eq!(board.len(), 2);
    assert_eq!(board[0].author, bob);
    assert_eq!(board[1].author, alice);
}

#[test]
fn board_is_a_ring_of_100() {
    let mut c = setup();
    let alice = acc("alice");
    mint(&mut c, &alice);
    for i in 0..130 {
        ctx(&alice, 0);
        c.post("1".into(), format!("post {i}"));
    }
    assert_eq!(c.board.len(), 100);
    let newest = c.get_board(Some(3));
    assert_eq!(newest.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), vec!["post 129", "post 128", "post 127"]);
}

#[test]
fn card_event_log_is_capped() {
    let mut c = setup();
    let alice = acc("alice");
    mint(&mut c, &alice);
    for _ in 0..30 {
        ctx(&alice, 1);
        c.list_card("1".into(), U128(NEAR));
        ctx(&alice, 1);
        c.unlist_card("1".into());
    }
    let ev = c.get_card("1".into()).unwrap().events;
    assert_eq!(ev.len(), MAX_EVENTS_PER_CARD);
    assert_eq!(ev.last().unwrap().kind, "unlisted");
}

#[test]
#[should_panic(expected = "a card cannot call its own account")]
fn act_cannot_target_own_wallet() {
    let mut c = setup();
    let alice = acc("alice");
    mint(&mut c, &alice);
    ctx(&alice, 1);
    let _ = c.act("1".into(), "1.club.near".parse().unwrap(), "x".into(), Base64VecU8(vec![]), U128(0), 10);
}

#[test]
#[should_panic(expected = "gas_tgas must be 1..=150")]
fn act_gas_is_bounded() {
    let mut c = setup();
    let alice = acc("alice");
    mint(&mut c, &alice);
    ctx(&alice, 1);
    let _ = c.act("1".into(), acc("dex"), "swap".into(), Base64VecU8(vec![]), U128(0), 400);
}

#[test]
fn act_records_spending() {
    let mut c = setup();
    let alice = acc("alice");
    mint(&mut c, &alice);
    ctx(&alice, 1);
    let _ = c.act("1".into(), club_id(), "buy_ticket".into(), Base64VecU8(b"{}".to_vec()), U128(20 * MILLI), 30);
    callback_ctx(true);
    c.on_wallet_done("1".into(), "acted".into(), alice.clone(), club_id(), U128(20 * MILLI), Some("buy_ticket".into()));
    let card = c.get_card("1".into()).unwrap();
    assert_eq!(card.spent_by_calls.0, 20 * MILLI);
    assert_eq!(card.events.last().unwrap().note.as_deref(), Some("buy_ticket"));
}
