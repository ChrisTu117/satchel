//! Satchel: club membership cards (NEP-171) that are also real NEAR accounts.
//!
//! Minting card `N` creates the account `N.<this contract>`, with no access keys,
//! running the stateless `satchel-wallet` code by global-contract hash. That wallet
//! only obeys this contract, and this contract only relays orders from whoever holds
//! card `N` at that moment. So:
//!
//! * anyone can pay a card from any wallet or exchange: it is a normal account;
//! * the holder withdraws or makes the card's account call other contracts;
//! * selling or transferring the card moves the account, its balance and its
//!   rights (members-only board) to the new holder at once.
//!
//! Club revenue: `buy_ticket` splits `member_share_bps` of every ticket equally into
//! all card accounts; the rest goes to the treasury. Selling a card through the
//! built-in market pays `royalty_bps` to the treasury.
//!
//! What stops a seller from keeping the money (README "Threat model"):
//! * ownership is checked in the same receipt that orders the wallet;
//! * a card is `busy` while a wallet order is in flight, and a busy card cannot be
//!   transferred, listed or sold, so a withdrawal cannot race a sale;
//! * any withdrawal or contract call cancels the card's sale listing;
//! * the wallet account has no keys and no method that adds one;
//! * approvals (NEP-178) are not enabled, so no third party can move a card.

use near_contract_standards::non_fungible_token::core::NonFungibleTokenCore;
use near_contract_standards::non_fungible_token::events::{NftMint, NftTransfer};
use near_contract_standards::non_fungible_token::metadata::{
    NFT_METADATA_SPEC, NFTContractMetadata, NonFungibleTokenMetadataProvider, TokenMetadata,
};
use near_contract_standards::non_fungible_token::{
    NonFungibleToken, NonFungibleTokenEnumeration, NonFungibleTokenResolver, Token, TokenId,
};
use near_sdk::json_types::{Base58CryptoHash, Base64VecU8, U128};
use near_sdk::serde_json::json;
use near_sdk::store::{LookupMap, LookupSet, Vector};
use near_sdk::{
    AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault, Promise, PromiseOrValue,
    assert_one_yocto, env, near, require,
};
use std::collections::HashMap;

pub const BPS: u128 = 10_000;
const MAX_EVENTS_PER_CARD: usize = 40;
const MAX_HOLDERS_PER_CARD: usize = 20;
const MAX_POSTS: u32 = 100;
const MAX_POST_LEN: usize = 280;
const MAX_PAGE: u32 = 50;
const GAS_WALLET_SEND: Gas = Gas::from_tgas(15);
const GAS_CALLBACK: Gas = Gas::from_tgas(15);
const MAX_ACT_GAS_TGAS: u64 = 150;

/// Orders for the card's wallet (contracts/wallet). Arguments are borsh-encoded;
/// the wallet has no SDK and parses them by hand.
fn wallet_send_near(wallet: AccountId, receiver: &AccountId, amount: u128) -> Promise {
    let args = near_sdk::borsh::to_vec(&(receiver.clone(), amount)).unwrap();
    Promise::new(wallet).function_call("send_near", args, NearToken::from_yoctonear(0), GAS_WALLET_SEND)
}

fn wallet_call_contract(
    wallet: AccountId,
    receiver: &AccountId,
    method: &str,
    call_args: Vec<u8>,
    deposit: u128,
    gas_tgas: u64,
) -> Promise {
    let gas = Gas::from_tgas(gas_tgas).as_gas();
    let args = near_sdk::borsh::to_vec(&(receiver.clone(), method.to_string(), call_args, deposit, gas)).unwrap();
    Promise::new(wallet).function_call(
        "call_contract",
        args,
        NearToken::from_yoctonear(0),
        Gas::from_tgas(gas_tgas + 10),
    )
}

#[derive(BorshStorageKey)]
#[near(serializers = [borsh])]
enum StorageKey {
    OwnerById,
    TokenMetadata,
    Enumeration,
    Cards,
    Listings,
    Busy,
    Board,
}

/// Fixed at `new`. No method changes it.
#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct ClubConfig {
    pub name: String,
    /// Receives the club's part of ticket sales and the royalty on card sales.
    pub treasury: AccountId,
    /// sha256 of `satchel-wallet` wasm, already deployed as a global contract by hash.
    pub wallet_code_hash: Base58CryptoHash,
    /// Price of a new card. Pays for storage and the wallet account.
    pub mint_price: U128,
    /// Part of `mint_price` sent to the new wallet account (covers its own storage).
    pub wallet_reserve: U128,
    pub max_cards: u32,
    pub ticket_min_price: U128,
    /// Share of each ticket split equally into all card wallets.
    pub member_share_bps: u16,
    /// Share of a card's sale price paid to the treasury.
    pub royalty_bps: u16,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct CardEvent {
    /// minted | transferred | listed | unlisted | sold | withdrew | acted | failed | posted
    pub kind: String,
    pub at_ms: u64,
    pub actor: AccountId,
    pub other: Option<AccountId>,
    pub amount: Option<U128>,
    pub note: Option<String>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Holder {
    pub account_id: AccountId,
    pub since_ms: u64,
    /// "mint", "transfer" or "sale"
    pub how: String,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug)]
pub struct Card {
    pub wallet: AccountId,
    pub minted_at_ms: u64,
    /// Value of `earned_per_card` when this card was minted.
    pub earned_offset: u128,
    pub withdrawn: u128,
    pub spent_by_calls: u128,
    pub holders: Vec<Holder>,
    pub events: Vec<CardEvent>,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Listing {
    pub seller: AccountId,
    pub price: U128,
    pub listed_at_ms: u64,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Debug)]
pub struct Post {
    pub index: u64,
    pub token_id: TokenId,
    pub author: AccountId,
    pub text: String,
    pub at_ms: u64,
}

#[near(serializers = [borsh])]
#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub tickets_sold: u64,
    pub ticket_revenue: u128,
    pub paid_to_cards: u128,
    pub paid_to_treasury: u128,
    pub cards_sold: u64,
    pub sales_volume: u128,
    pub posts: u64,
}

#[near(serializers = [json])]
pub struct ClubView {
    pub config: ClubConfig,
    pub cards: u32,
    /// Total each card has received from ticket sales since the first card was minted.
    pub earned_per_card: U128,
    pub tickets_sold: u64,
    pub ticket_revenue: U128,
    pub paid_to_cards: U128,
    pub paid_to_treasury: U128,
    pub cards_sold: u64,
    pub sales_volume: U128,
    pub posts: u64,
}

#[near(serializers = [json])]
pub struct CardView {
    pub token_id: TokenId,
    pub owner_id: AccountId,
    pub wallet: AccountId,
    pub minted_at_ms: u64,
    /// Received from ticket sales (other deposits are visible only as account balance).
    pub earned_from_tickets: U128,
    pub withdrawn: U128,
    pub spent_by_calls: U128,
    pub busy: bool,
    pub listing: Option<Listing>,
    pub holders: Vec<Holder>,
    pub events: Vec<CardEvent>,
}

#[near(serializers = [json])]
pub struct TicketReceipt {
    pub buyer: AccountId,
    pub event: String,
    pub price: U128,
    pub per_card: U128,
    pub cards: u32,
    pub to_treasury: U128,
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Club {
    tokens: NonFungibleToken,
    metadata: NFTContractMetadata,
    config: ClubConfig,
    next_id: u32,
    cards: LookupMap<TokenId, Card>,
    listings: LookupMap<TokenId, Listing>,
    busy: LookupSet<TokenId>,
    board: Vector<Post>,
    earned_per_card: u128,
    stats: Stats,
}

fn now_ms() -> u64 {
    env::block_timestamp_ms()
}

fn emit(event: &str, data: near_sdk::serde_json::Value) {
    env::log_str(&format!(
        "EVENT_JSON:{}",
        json!({"standard": "satchel", "version": "1.0.0", "event": event, "data": [data]})
    ));
}

#[near]
impl Club {
    /// Only the contract account can initialize it (no front-running between deploy
    /// and init). There is no owner afterwards.
    #[init]
    #[private]
    pub fn new(config: ClubConfig) -> Self {
        require!((config.member_share_bps as u128) <= BPS, "member_share_bps > 10000");
        require!((config.royalty_bps as u128) <= BPS, "royalty_bps > 10000");
        require!(config.wallet_reserve.0 <= config.mint_price.0, "reserve > mint price");
        require!(config.max_cards >= 1 && config.max_cards <= 200, "max_cards must be 1..=200");
        let metadata = NFTContractMetadata {
            spec: NFT_METADATA_SPEC.to_string(),
            name: config.name.clone(),
            symbol: "SATCHEL".to_string(),
            icon: None,
            base_uri: None,
            reference: None,
            reference_hash: None,
        };
        metadata.assert_valid();
        Self {
            tokens: NonFungibleToken::new(
                StorageKey::OwnerById,
                config.treasury.clone(),
                Some(StorageKey::TokenMetadata),
                Some(StorageKey::Enumeration),
                None::<StorageKey>,
            ),
            metadata,
            config,
            next_id: 1,
            cards: LookupMap::new(StorageKey::Cards),
            listings: LookupMap::new(StorageKey::Listings),
            busy: LookupSet::new(StorageKey::Busy),
            board: Vector::new(StorageKey::Board),
            earned_per_card: 0,
            stats: Stats::default(),
        }
    }

    // ---------- cards ----------

    /// Mint the next card to `receiver_id` (default: caller). Attach at least
    /// `mint_price`; anything above it goes into the new card's account.
    #[payable]
    pub fn mint(&mut self, receiver_id: Option<AccountId>) -> Token {
        let deposit = env::attached_deposit().as_yoctonear();
        require!(
            deposit >= self.config.mint_price.0,
            format!("attach at least {} yoctoNEAR", self.config.mint_price.0)
        );
        require!(self.next_id <= self.config.max_cards, "all cards are minted");
        let owner = receiver_id.unwrap_or_else(env::predecessor_account_id);
        let token_id = self.next_id.to_string();
        self.next_id += 1;
        let wallet: AccountId = format!("{}.{}", token_id, env::current_account_id())
            .parse()
            .unwrap();
        let metadata = TokenMetadata {
            title: Some(format!("{} #{}", self.config.name, token_id)),
            description: Some(format!(
                "Membership card. It is also the NEAR account {wallet}: send NEAR there from any wallet; whoever holds this card controls it."
            )),
            media: None,
            media_hash: None,
            copies: None,
            issued_at: Some(now_ms().to_string()),
            expires_at: None,
            starts_at: None,
            updated_at: None,
            extra: Some(json!({ "wallet": wallet }).to_string()),
            reference: None,
            reference_hash: None,
        };
        let token =
            self.tokens
                .internal_mint_with_refund(token_id.clone(), owner.clone(), Some(metadata), None);
        NftMint {
            owner_id: &owner,
            token_ids: &[&token_id],
            memo: None,
        }
        .emit();
        let seed = deposit - self.config.mint_price.0;
        self.cards.insert(
            token_id.clone(),
            Card {
                wallet: wallet.clone(),
                minted_at_ms: now_ms(),
                earned_offset: self.earned_per_card,
                withdrawn: 0,
                spent_by_calls: 0,
                holders: vec![Holder {
                    account_id: owner.clone(),
                    since_ms: now_ms(),
                    how: "mint".into(),
                }],
                events: vec![],
            },
        );
        self.log_event(
            &token_id,
            "minted",
            owner.clone(),
            Some(wallet.clone()),
            Some(seed),
            None,
        );
        let code_hash: near_sdk::CryptoHash = self.config.wallet_code_hash.into();
        Promise::new(wallet)
            .create_account()
            .transfer(NearToken::from_yoctonear(self.config.wallet_reserve.0 + seed))
            .use_global_contract(code_hash)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CALLBACK)
                    .on_wallet_created(token_id),
            )
            .detach();
        token
    }

    #[private]
    pub fn on_wallet_created(&mut self, token_id: TokenId) -> bool {
        let ok = near_sdk::is_promise_success();
        if !ok {
            let owner = self.owner_of(&token_id);
            self.log_event(&token_id, "failed", owner, None, None, Some("wallet account creation failed".into()));
        }
        ok
    }

    /// Send `amount` yoctoNEAR from the card's account to `receiver_id` (default:
    /// caller). Card holder only; needs 1 yoctoNEAR attached. Cancels any listing.
    #[payable]
    pub fn withdraw(
        &mut self,
        token_id: TokenId,
        amount: U128,
        receiver_id: Option<AccountId>,
    ) -> Promise {
        assert_one_yocto();
        let holder = self.assert_holder_and_lock(&token_id);
        let receiver = receiver_id.unwrap_or_else(|| holder.clone());
        require!(amount.0 > 0, "amount must be positive");
        let wallet = self.wallet_of(&token_id);
        wallet_send_near(wallet, &receiver, amount.0)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CALLBACK)
                    .on_wallet_done(token_id, "withdrew".into(), holder, receiver, amount, None),
            )
    }

    /// Make the card's account call `receiver_id.method_name(args)` with `deposit`
    /// yoctoNEAR from the card's balance. Card holder only; 1 yoctoNEAR attached.
    /// Cancels any listing.
    #[payable]
    pub fn act(
        &mut self,
        token_id: TokenId,
        receiver_id: AccountId,
        method_name: String,
        args: Base64VecU8,
        deposit: U128,
        gas_tgas: u64,
    ) -> Promise {
        assert_one_yocto();
        require!(gas_tgas >= 1 && gas_tgas <= MAX_ACT_GAS_TGAS, "gas_tgas must be 1..=150");
        let holder = self.assert_holder_and_lock(&token_id);
        let wallet = self.wallet_of(&token_id);
        require!(receiver_id != wallet, "a card cannot call its own account");
        wallet_call_contract(wallet, &receiver_id, &method_name, args.0, deposit.0, gas_tgas)
            .then(
                Self::ext(env::current_account_id())
                    .with_static_gas(GAS_CALLBACK)
                    .on_wallet_done(
                        token_id,
                        "acted".into(),
                        holder,
                        receiver_id,
                        deposit,
                        Some(method_name),
                    ),
            )
    }

    #[private]
    pub fn on_wallet_done(
        &mut self,
        token_id: TokenId,
        kind: String,
        holder: AccountId,
        other: AccountId,
        amount: U128,
        method: Option<String>,
    ) -> bool {
        self.busy.remove(&token_id);
        let ok = env::promise_results_count() == 1 && near_sdk::is_promise_success();
        if ok {
            if let Some(card) = self.cards.get_mut(&token_id) {
                if kind == "withdrew" {
                    card.withdrawn += amount.0;
                } else {
                    card.spent_by_calls += amount.0;
                }
            }
            self.log_event(&token_id, &kind, holder, Some(other), Some(amount.0), method);
        } else {
            let note = format!("{} failed{}", kind, method.map(|m| format!(": {m}")).unwrap_or_default());
            self.log_event(&token_id, "failed", holder, Some(other), Some(amount.0), Some(note));
        }
        ok
    }

    // ---------- built-in market ----------

    /// Offer the card for sale at `price` yoctoNEAR. Holder only; 1 yoctoNEAR.
    #[payable]
    pub fn list_card(&mut self, token_id: TokenId, price: U128) {
        assert_one_yocto();
        let holder = self.owner_of(&token_id);
        require!(env::predecessor_account_id() == holder, "only the card holder can list it");
        require!(!self.busy.contains(&token_id), "card is busy: a wallet order is in flight");
        require!(price.0 > 0, "price must be positive");
        self.listings.insert(
            token_id.clone(),
            Listing {
                seller: holder.clone(),
                price,
                listed_at_ms: now_ms(),
            },
        );
        self.log_event(&token_id, "listed", holder, None, Some(price.0), None);
    }

    #[payable]
    pub fn unlist_card(&mut self, token_id: TokenId) {
        assert_one_yocto();
        let holder = self.owner_of(&token_id);
        require!(env::predecessor_account_id() == holder, "only the card holder can unlist it");
        if self.listings.remove(&token_id).is_some() {
            self.log_event(&token_id, "unlisted", holder, None, None, None);
        }
    }

    /// Buy a listed card. Attach at least the price; the excess is refunded. The
    /// seller gets price minus `royalty_bps`, the treasury gets the royalty.
    #[payable]
    pub fn buy_card(&mut self, token_id: TokenId) -> Token {
        let buyer = env::predecessor_account_id();
        let listing = self
            .listings
            .get(&token_id)
            .cloned()
            .unwrap_or_else(|| env::panic_str("card is not for sale"));
        let owner = self.owner_of(&token_id);
        require!(listing.seller == owner, "stale listing");
        require!(buyer != owner, "you already hold this card");
        require!(!self.busy.contains(&token_id), "card is busy: a wallet order is in flight");
        let paid = env::attached_deposit().as_yoctonear();
        require!(paid >= listing.price.0, "attached deposit is below the price");
        let royalty = listing.price.0 * self.config.royalty_bps as u128 / BPS;
        let to_seller = listing.price.0 - royalty;
        self.listings.remove(&token_id);
        self.tokens.internal_transfer_unguarded(&token_id, &owner, &buyer);
        NftTransfer {
            old_owner_id: &owner,
            new_owner_id: &buyer,
            token_ids: &[&token_id],
            authorized_id: None,
            memo: Some("sold"),
        }
        .emit();
        self.record_holder(&token_id, &buyer, "sale");
        self.log_event(&token_id, "sold", owner.clone(), Some(buyer.clone()), Some(listing.price.0), None);
        self.stats.cards_sold += 1;
        self.stats.sales_volume += listing.price.0;
        self.stats.paid_to_treasury += royalty;
        if to_seller > 0 {
            Promise::new(owner).transfer(NearToken::from_yoctonear(to_seller)).detach();
        }
        if royalty > 0 {
            Promise::new(self.config.treasury.clone())
                .transfer(NearToken::from_yoctonear(royalty))
                .detach();
        }
        if paid > listing.price.0 {
            Promise::new(buyer.clone())
                .transfer(NearToken::from_yoctonear(paid - listing.price.0))
                .detach();
        }
        self.tokens.nft_token(token_id).unwrap()
    }

    // ---------- club revenue ----------

    /// Buy a ticket for `event`. `member_share_bps` of the price is split equally
    /// into every card's account, the rest goes to the treasury.
    #[payable]
    pub fn buy_ticket(&mut self, event: String) -> TicketReceipt {
        let price = env::attached_deposit().as_yoctonear();
        require!(price >= self.config.ticket_min_price.0, "ticket price is below the minimum");
        require!(!event.is_empty() && event.len() <= 64, "event name must be 1..=64 bytes");
        let cards = self.next_id - 1;
        let member_pot = price * self.config.member_share_bps as u128 / BPS;
        let per_card = if cards == 0 { 0 } else { member_pot / cards as u128 };
        let to_treasury = price - per_card * cards as u128;
        if per_card > 0 {
            let me = env::current_account_id();
            for id in 1..=cards {
                let wallet: AccountId = format!("{id}.{me}").parse().unwrap();
                Promise::new(wallet).transfer(NearToken::from_yoctonear(per_card)).detach();
            }
        }
        if to_treasury > 0 {
            Promise::new(self.config.treasury.clone())
                .transfer(NearToken::from_yoctonear(to_treasury))
                .detach();
        }
        self.earned_per_card += per_card;
        self.stats.tickets_sold += 1;
        self.stats.ticket_revenue += price;
        self.stats.paid_to_cards += per_card * cards as u128;
        self.stats.paid_to_treasury += to_treasury;
        let buyer = env::predecessor_account_id();
        emit(
            "ticket_sold",
            json!({"buyer": buyer, "event": event, "price": U128(price), "per_card": U128(per_card), "cards": cards}),
        );
        TicketReceipt {
            buyer,
            event,
            price: U128(price),
            per_card: U128(per_card),
            cards,
            to_treasury: U128(to_treasury),
        }
    }

    // ---------- members-only board ----------

    /// Post on the members' board as the holder of `token_id`. Anyone can read the
    /// board; only current card holders can write to it.
    pub fn post(&mut self, token_id: TokenId, text: String) -> Post {
        let author = env::predecessor_account_id();
        require!(self.owner_of(&token_id) == author, "only the holder of this card can post with it");
        let text = text.trim().to_string();
        require!(!text.is_empty() && text.len() <= MAX_POST_LEN, "post must be 1..=280 bytes");
        let post = Post {
            index: self.stats.posts,
            token_id: token_id.clone(),
            author: author.clone(),
            text,
            at_ms: now_ms(),
        };
        if self.board.len() < MAX_POSTS {
            self.board.push(post.clone());
        } else {
            let slot = (self.stats.posts % MAX_POSTS as u64) as u32;
            self.board.set(slot, post.clone());
        }
        self.stats.posts += 1;
        emit("posted", json!({"token_id": token_id, "author": author, "index": post.index}));
        post
    }

    // ---------- views ----------

    pub fn get_club(&self) -> ClubView {
        ClubView {
            config: self.config.clone(),
            cards: self.next_id - 1,
            earned_per_card: U128(self.earned_per_card),
            tickets_sold: self.stats.tickets_sold,
            ticket_revenue: U128(self.stats.ticket_revenue),
            paid_to_cards: U128(self.stats.paid_to_cards),
            paid_to_treasury: U128(self.stats.paid_to_treasury),
            cards_sold: self.stats.cards_sold,
            sales_volume: U128(self.stats.sales_volume),
            posts: self.stats.posts,
        }
    }

    pub fn get_card(&self, token_id: TokenId) -> Option<CardView> {
        let card = self.cards.get(&token_id)?;
        Some(CardView {
            token_id: token_id.clone(),
            owner_id: self.owner_of(&token_id),
            wallet: card.wallet.clone(),
            minted_at_ms: card.minted_at_ms,
            earned_from_tickets: U128(self.earned_per_card - card.earned_offset),
            withdrawn: U128(card.withdrawn),
            spent_by_calls: U128(card.spent_by_calls),
            busy: self.busy.contains(&token_id),
            listing: self.listings.get(&token_id).cloned(),
            holders: card.holders.clone(),
            events: card.events.clone(),
        })
    }

    /// Cards `from..from+limit` by id (ids start at 1), without their event logs.
    pub fn get_cards(&self, from: Option<u32>, limit: Option<u32>) -> Vec<CardView> {
        let from = from.unwrap_or(1).max(1);
        let limit = limit.unwrap_or(MAX_PAGE).min(MAX_PAGE);
        (from..self.next_id)
            .take(limit as usize)
            .filter_map(|id| {
                self.get_card(id.to_string()).map(|mut v| {
                    v.events.clear();
                    v
                })
            })
            .collect()
    }

    /// Board posts, newest first.
    pub fn get_board(&self, limit: Option<u32>) -> Vec<Post> {
        let limit = limit.unwrap_or(20).min(MAX_PAGE) as usize;
        let mut posts: Vec<Post> = self.board.iter().cloned().collect();
        posts.sort_by(|a, b| b.index.cmp(&a.index));
        posts.truncate(limit);
        posts
    }

    pub fn is_holder(&self, token_id: TokenId, account_id: AccountId) -> bool {
        self.tokens.owner_by_id.get(&token_id) == Some(account_id)
    }
}

// ---------- internals ----------
impl Club {
    fn owner_of(&self, token_id: &TokenId) -> AccountId {
        self.tokens
            .owner_by_id
            .get(token_id)
            .unwrap_or_else(|| env::panic_str("card not found"))
    }

    fn wallet_of(&self, token_id: &TokenId) -> AccountId {
        self.cards
            .get(token_id)
            .unwrap_or_else(|| env::panic_str("card not found"))
            .wallet
            .clone()
    }

    /// Checks the caller holds the card right now, marks it busy and cancels its listing.
    fn assert_holder_and_lock(&mut self, token_id: &TokenId) -> AccountId {
        let holder = self.owner_of(token_id);
        require!(
            env::predecessor_account_id() == holder,
            "only the current card holder can use its account"
        );
        require!(!self.busy.contains(token_id), "card is busy: a wallet order is in flight");
        self.busy.insert(token_id.clone());
        if self.listings.remove(token_id).is_some() {
            self.log_event(token_id, "unlisted", holder.clone(), None, None, Some("cancelled by a wallet order".into()));
        }
        holder
    }

    fn record_holder(&mut self, token_id: &TokenId, account: &AccountId, how: &str) {
        if let Some(card) = self.cards.get_mut(token_id) {
            if card.holders.len() >= MAX_HOLDERS_PER_CARD {
                card.holders.remove(1); // keep the minter
            }
            card.holders.push(Holder {
                account_id: account.clone(),
                since_ms: now_ms(),
                how: how.into(),
            });
        }
    }

    fn log_event(
        &mut self,
        token_id: &TokenId,
        kind: &str,
        actor: AccountId,
        other: Option<AccountId>,
        amount: Option<u128>,
        note: Option<String>,
    ) {
        emit(
            kind,
            json!({"token_id": token_id, "actor": actor, "other": other, "amount": amount.map(U128), "note": note}),
        );
        if let Some(card) = self.cards.get_mut(token_id) {
            if card.events.len() >= MAX_EVENTS_PER_CARD {
                card.events.remove(0);
            }
            card.events.push(CardEvent {
                kind: kind.into(),
                at_ms: now_ms(),
                actor,
                other,
                amount: amount.map(U128),
                note,
            });
        }
    }

    fn before_transfer(&mut self, token_id: &TokenId) -> AccountId {
        let owner = self.owner_of(token_id);
        require!(env::predecessor_account_id() == owner, "only the card holder can transfer it");
        require!(!self.busy.contains(token_id), "card is busy: a wallet order is in flight");
        if self.listings.remove(token_id).is_some() {
            self.log_event(token_id, "unlisted", owner.clone(), None, None, Some("card transferred".into()));
        }
        owner
    }
}

// ---------- NEP-171 ----------

#[near]
impl NonFungibleTokenCore for Club {
    #[payable]
    fn nft_transfer(
        &mut self,
        receiver_id: AccountId,
        token_id: TokenId,
        approval_id: Option<u64>,
        memo: Option<String>,
    ) {
        let owner = self.before_transfer(&token_id);
        self.tokens.nft_transfer(receiver_id.clone(), token_id.clone(), approval_id, memo);
        self.record_holder(&token_id, &receiver_id, "transfer");
        self.log_event(&token_id, "transferred", owner, Some(receiver_id), None, None);
    }

    #[payable]
    fn nft_transfer_call(
        &mut self,
        receiver_id: AccountId,
        token_id: TokenId,
        approval_id: Option<u64>,
        memo: Option<String>,
        msg: String,
    ) -> PromiseOrValue<bool> {
        let owner = self.before_transfer(&token_id);
        let r = self.tokens.nft_transfer_call(
            receiver_id.clone(),
            token_id.clone(),
            approval_id,
            memo,
            msg,
        );
        self.record_holder(&token_id, &receiver_id, "transfer");
        self.log_event(&token_id, "transferred", owner, Some(receiver_id), None, None);
        r
    }

    fn nft_token(&self, token_id: TokenId) -> Option<Token> {
        self.tokens.nft_token(token_id)
    }
}

#[near]
impl NonFungibleTokenResolver for Club {
    #[private]
    fn nft_resolve_transfer(
        &mut self,
        previous_owner_id: AccountId,
        receiver_id: AccountId,
        token_id: TokenId,
        approved_account_ids: Option<HashMap<AccountId, u64>>,
    ) -> bool {
        let kept = self.tokens.nft_resolve_transfer(
            previous_owner_id.clone(),
            receiver_id.clone(),
            token_id.clone(),
            approved_account_ids,
        );
        if !kept {
            self.record_holder(&token_id, &previous_owner_id, "transfer");
            self.log_event(&token_id, "transferred", receiver_id, Some(previous_owner_id), None, Some("returned by receiver".into()));
        }
        kept
    }
}

#[near]
impl NonFungibleTokenEnumeration for Club {
    fn nft_total_supply(&self) -> U128 {
        self.tokens.nft_total_supply()
    }

    fn nft_tokens(&self, from_index: Option<U128>, limit: Option<u64>) -> Vec<Token> {
        self.tokens.nft_tokens(from_index, limit)
    }

    fn nft_supply_for_owner(&self, account_id: AccountId) -> U128 {
        self.tokens.nft_supply_for_owner(account_id)
    }

    fn nft_tokens_for_owner(
        &self,
        account_id: AccountId,
        from_index: Option<U128>,
        limit: Option<u64>,
    ) -> Vec<Token> {
        self.tokens.nft_tokens_for_owner(account_id, from_index, limit)
    }
}

#[near]
impl NonFungibleTokenMetadataProvider for Club {
    fn nft_metadata(&self) -> NFTContractMetadata {
        self.metadata.clone()
    }
}

#[cfg(test)]
mod tests;
