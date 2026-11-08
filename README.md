# Satchel: membership cards that are wallets

Satchel is a club membership NFT collection on NEAR (NEP-171) where **every card is
also a real NEAR account**. Minting card 7 creates the account `7.satchel-club.testnet`.
Anyone can send NEAR to that account from any wallet or exchange. Ticket sales pay
into every card's account automatically. Whoever holds the card controls that money
and the card's members-only rights. Give or sell the card and the account, its balance
and its rights move with it at once; the old holder keeps nothing.

Built at NEAR HACKATHON (November 2026) for the **NFTs That Do Something** track
(`nfts`). Testnet only.

- Live page (testnet): https://christu117.github.io/satchel/
- Club contract: [`satchel-club.testnet`](https://testnet.nearblocks.io/address/satchel-club.testnet)
- The same author's other, independent hackathon project: **STAG**, a memecoin whose
  supply follows a coordination game (`memecoins` track),
  https://github.com/ChrisTu117/staghunt. The two share no code or contracts.

## The problem

An NFT usually points at a picture. A membership often comes with money attached: a
share of what the club earns, a deposit, a prepaid balance. Today that money sits in a
separate wallet or in the club's own books, so selling the membership means trusting
the seller (or the club) to hand the money over as well. Satchel makes the money part
of the card: each card is an account with its own address, the club pays into it, and
control of the account is exactly ownership of the card.

## Try it in 2 minutes (no wallet needed)

On the live page (or `npm run serve` and open http://localhost:8081):

1. Click **Create a testnet account**. The page asks the public testnet faucet for a
   throwaway account (10 testnet NEAR). Its key stays in your browser.
2. Type `1` in "put NEAR in it" and click **Mint a card**. The page opens your card:
   its account (for example `3.satchel-club.testnet`) holds 1.010 NEAR, and the row
   "Access keys on that account" reads **none: nobody can sign for it** (read live
   from RPC; the explorer link shows the same account).
3. Click **Withdraw to me** (0.5 NEAR): the card's balance drops and your account's rises.
4. Post on the **members' board** with the card.
5. **List for sale** at some price. Open another browser profile (or a private
   window) with a second guest account and **Buy this card**. In that window you now
   "control" the card's account and can withdraw and post. Back in the first window,
   **Try to withdraw anyway** is refused by the contract: "only the current card holder
   can use its account".
6. From any account, **Send NEAR to** a card (a plain transfer) or **Buy ticket**: half of
   every ticket is split across all cards' accounts; each card's history shows it.

## Run the checks yourself

| Command | What it does | Time |
|---|---|---|
| `cargo test -p satchel-club -p satchel-wallet` | 25 unit tests (bookkeeping, permissions, busy lock, market, board, wallet argument parsing) | ~1 min |
| `cargo test -p satchel-integration-tests` | 7 tests running `res/*.wasm` in a local NEAR sandbox node: real sub-accounts, real transfers, a withdrawal racing a sale, a payout to 100 cards | ~1 min after a first build of several minutes |
| `npm install && npm run demo` | end-to-end on **testnet**: deploys a fresh club, mints, pays in from outside, sells the card, 19 checks | ~2 min |
| `npm run check:abi` | the club has no admin method and private callbacks; the wallet wasm exports 3 functions and imports nothing that could add keys, deploy or delete | 1 s |
| `npm run check:live` | the deployed `satchel-club.testnet` runs exactly `res/satchel_club.wasm` with `deploy/testnet.json`, and every card account runs `res/satchel_wallet.wasm` by global hash with no keys | 5 s |

Requirements: Rust 1.95+ with the `wasm32-unknown-unknown` target, Node 20+.
`scripts/build.sh` rebuilds both wasm files (needs [cargo-near](https://github.com/near/cargo-near));
prebuilt copies are committed in `res/`.

`npm run demo` takes one account from the public testnet faucet, which limits accounts
per IP address and day. If it answers 429, run it with your own testnet account:
`SPONSOR=you.testnet npm run demo` (it reads the key that `near` CLI stored in
`~/.near-credentials/testnet/` and deletes the demo accounts afterwards).

## How it works

```
                 holder of card 7
                        │  withdraw / act / list / post   (checked: caller holds card 7 now)
                        ▼
   satchel-club.testnet (NEP-171 collection, no owner)
                        │  send_near / call_contract      (borsh args)
                        ▼
   7.satchel-club.testnet  (no access keys; wallet code by global-contract hash;
                            obeys only its parent)
                        ▲
     anyone: plain NEAR transfer · ticket sales: 50% split over all cards
```

- **Real accounts as sub-accounts.** `mint` creates `<id>.<club>` with
  `CreateAccount + Transfer + UseGlobalContract` in one receipt. The account gets no
  access key, ever.
- **The wallet** (`contracts/wallet`, 19.7 KB) is written without an SDK directly
  against NEAR host functions. It is stateless: it reads its token id and its parent
  from its own account name, and only accepts orders whose predecessor is the parent.
  It can send NEAR (`send_near`) or call any contract with a deposit (`call_contract`),
  so a card can hold tokens, buy things or vote. No code path creates AddKey, Deploy,
  DeleteAccount or Stake actions.
- **Global contract by hash (NEP-591).** The wallet code is deployed once; each card
  account references it by its sha256. Code addressed by hash can never change. A card
  account costs 0.01 NEAR instead of carrying its own copy of the code.
- **The club** (`contracts/club`) is a standard NEP-171 contract (core, metadata,
  enumeration) from `near-contract-standards`, plus: `withdraw`, `act` (make the card's
  account call a contract), a built-in market (`list_card`, `buy_card` with a 5%
  royalty to the treasury), `buy_ticket` (50% split equally into every card account,
  50% to the treasury) and a members-only board (`post`). Every card keeps an on-chain
  history: holders, withdrawals, spending, sales, failures.
- **No admin.** `new` is `#[private]`; no method changes the configuration or the
  wallet code hash. The live club account `satchel-club.testnet` has no access keys
  at all (deleted on 2026-11-09), so nobody can redeploy or change it;
  `npm run check:live -- --locked` verifies this.

## Threat model: how a seller could try to keep the money

| Attempt | What stops it | Checked by |
|---|---|---|
| Withdraw after the sale | The club checks the caller holds the card in the same receipt that orders the wallet | sandbox `control_moves_with_the_card`; demo step 5 |
| Fire a withdrawal and sell before it lands | The card is **busy** while a wallet order is in flight: it cannot be transferred, listed, sold or ordered again until the callback clears it | unit `busy_card_blocks_everything_until_callback`; sandbox `withdrawal_cannot_race_a_sale` (both sent in the same moment: the seller never gets both) |
| List at a price, then drain the card before the buyer pays | Any withdrawal or `act` cancels the listing, so a buyer can only buy a listing made after the last withdrawal | unit `withdraw_cancels_listing` |
| Keep a key to the card's account | The account is created with no key, and the wallet has no method that adds one | sandbox `card_is_a_real_account` (key list empty); `npm run check:abi` (wallet imports) |
| Call the wallet directly | It only obeys its parent club; even the current holder must go through the club | sandbox `card_is_a_real_account` (a stranger and the holder are both refused) |
| Leave an approval so a third party can move the card later | NEP-178 approvals are not enabled at all | `npm run check:abi` |
| Use the card's rights after selling | Board posts and every wallet order check the current holder | unit `board_follows_the_card`; sandbox `control_moves_with_the_card` |
| A payout in flight during a sale | Payouts are plain transfers into the card's account, so they land in the account whoever holds the card; the new holder can withdraw them | sandbox `ticket_revenue_lands_in_card_accounts` |
| The club operator changes the rules or the wallet code | No owner, no setter, private init; wallet code addressed by immutable hash; club account locked | `npm run check:abi`; `npm run check:live -- --locked` |

Not covered: a holder who uses `nft_transfer_call` to send the card to a malicious
contract hands that contract full control during the call, as with any NFT. Buyers
see a card's current balance and history before buying, but the built-in market does
not let a buyer set a minimum balance (balances of other accounts are not readable
synchronously on NEAR).

## Claims and how each is checked

| Claim | Check |
|---|---|
| Minting card N creates the account `N.<club>`, with no access keys, running the wallet code by global hash | sandbox `card_is_a_real_account`; demo step 2; `npm run check:live` for every live card |
| Anyone can pay a card with a plain transfer | sandbox `card_is_a_real_account`; demo step 3 |
| 50% of a ticket is split equally into all card accounts; cards minted later don't share earlier sales | unit `ticket_split`; sandbox `ticket_revenue_lands_in_card_accounts`; demo step 3 |
| A ticket sale pays all 100 cards in one transaction | sandbox `ticket_split_with_100_cards_fits_in_gas` |
| Only the current holder can withdraw, act or post | unit `stranger_cannot_withdraw`, `board_follows_the_card`; sandbox tests; demo step 4 |
| Selling moves the card, its money and its rights; the seller gets the price minus 5% | unit `buy_card_moves_ownership_and_pays_royalty`; sandbox `control_moves_with_the_card`; demo step 5 |
| A card's account can call other contracts with its own money | sandbox `card_account_can_act` (the card buys a ticket) |
| A failed wallet order changes nothing and unlocks the card | unit `failed_wallet_order_unlocks_and_records_nothing`; sandbox `card_is_a_real_account` |
| Nobody else can initialize the club; there is no admin method | sandbox `nobody_else_can_initialize`; `npm run check:abi` |
| The live club is this code with these parameters | `npm run check:live` |

## Parameters of the live club (`deploy/testnet.json`)

| | |
|---|---|
| Mint price | 0.1 NEAR (0.01 of it funds the card account; anything above the price goes into the card) |
| Cards | at most 100 |
| Ticket split | 50% equally to all cards, 50% to `satchel-treasury.testnet`; minimum ticket 0.01 NEAR |
| Royalty on resale | 5% to the treasury |

## Known limits

- **Push payouts** to every card cap the collection at a size whose payout fits in one
  transaction; 100 cards are tested. A larger club would switch to pull payouts.
- **Gas rewards.** NEAR gives a contract account 30% of the gas its code burns, so a
  card's balance grows by a tiny amount each time its wallet code runs. The history
  shows tickets, withdrawals and spending exactly; the balance is the account's real
  balance.
- **History length.** Each card keeps its last 40 events and 20 holders on chain;
  the board keeps the last 100 posts. Older entries remain in the NEP-297 event logs.
- **Testnet faucet.** The page's "Create a testnet account" depends on the public
  testnet faucet, which limits accounts per IP address and day.

## Repository layout

```
contracts/club/             NEP-171 collection: mint, withdraw, act, market, tickets, board
contracts/wallet/           the card account's code (no SDK, host functions only)
integration-tests/          sandbox tests against res/*.wasm
res/                        built wasm + club ABI (scripts/build.sh)
web/src/                    page source (plain JS + near-api-js), bundled to docs/
docs/                       the static page served by GitHub Pages
scripts/                    testnet demo, ABI check, live check, build
deploy/testnet.json         parameters of the live deployment
```

## Tools and libraries

Written with **Claude Code** (Anthropic's AI coding agent): the contracts, tests,
scripts and page were AI-generated and checked by the tests above. Libraries:
`near-sdk` 5.30.1 and `near-contract-standards` 5.30.1 (club), no library in the
wallet, `cargo-near` 0.23.0, `near-sandbox` 0.4.0, `near-api` 0.8 (Rust tests),
`near-api-js` 7.4.0 (page and scripts), `esbuild` 0.25. All code was written during
the hackathon (November 2026); there is no earlier baseline.

## License

MIT, see [LICENSE](LICENSE).
