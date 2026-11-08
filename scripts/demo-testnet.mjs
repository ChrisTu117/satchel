// End-to-end demo on NEAR testnet, no setup needed besides `npm install`:
//
//   npm run demo                                  # one account from the testnet faucet
//   SPONSOR=you.testnet npm run demo              # or pay with your own testnet account
//
// 1. deploys res/satchel_club.wasm on a fresh account (the wallet code is already on
//    testnet as a global contract; it is deployed first if it is missing);
// 2. alice mints card #1 with 0.5 NEAR inside -> a real account 1.<club> appears,
//    with no access keys, running the wallet code by hash;
// 3. bob pays that account 0.2 NEAR with a plain transfer; carol buys a 0.4 NEAR
//    ticket and half of it lands in the card's account;
// 4. alice withdraws; bob cannot;
// 5. alice sells the card to dave for 0.5 NEAR: dave now controls the account and
//    the members' board, alice controls nothing;
// 6. prints explorer links and checks every number. Exit code 0 = all checks passed.

import { readFileSync } from "node:fs";
import {
  NEAR,
  TGAS,
  accountLink,
  balance,
  call,
  settledBalance,
  check,
  cleanup,
  demoRoot,
  near,
  provider,
  sha256Base58,
  subAccount,
  txLink,
  view,
} from "./lib.mjs";
import { actions } from "near-api-js";

const CLUB_WASM = readFileSync(new URL("../res/satchel_club.wasm", import.meta.url));
const WALLET_WASM = readFileSync(new URL("../res/satchel_wallet.wasm", import.meta.url));
const WALLET_HASH = sha256Base58(WALLET_WASM);
const MILLI = NEAR / 1000n;
// a wallet account earns 30% of the gas its code burns: allow 1 milliNEAR of slack
const about = (a, b) => a >= b && a <= b + MILLI;
const created = { root: null, children: [] }; // for cleanup, also after a failure

async function main() {
  console.log("1. Fresh club deployment");
  const club = await demoRoot("satchel-demo");
  created.root = club;
  const clubId = club.accountId;
  let hasWalletCode = true;
  try {
    await provider.viewGlobalContractCode({ identifier: { codeHash: WALLET_HASH } });
  } catch {
    hasWalletCode = false;
  }
  if (!hasWalletCode) {
    console.log(`  wallet code ${WALLET_HASH} is not on this network yet: deploying it (about 2 NEAR)`);
    await club.deployGlobalContract(WALLET_WASM, "codeHash");
  }
  const treasury = await subAccount(club, "treasury", MILLI * 10n);
  created.children.push(treasury);
  const config = {
    name: "Satchel Demo Club",
    treasury: treasury.accountId,
    wallet_code_hash: WALLET_HASH,
    mint_price: (100n * MILLI).toString(),
    wallet_reserve: (10n * MILLI).toString(),
    max_cards: 20,
    ticket_min_price: (10n * MILLI).toString(),
    member_share_bps: 5000,
    royalty_bps: 500,
  };
  const deploy = await club.signAndSendTransaction({
    receiverId: clubId,
    actions: [actions.deployContract(CLUB_WASM), actions.functionCall("new", { config }, 100n * TGAS, 0n)],
  });
  if (deploy?.status?.Failure) throw new Error(`deploy failed: ${JSON.stringify(deploy.status.Failure)}`);
  console.log(`  club ${clubId}  ${accountLink(clubId)}`);
  console.log(`  deploy+init tx ${txLink(deploy.transaction.hash)}`);

  const members = {};
  for (const [name, amt] of [["alice", 900n], ["bob", 500n], ["carol", 900n], ["dave", 900n]]) {
    members[name] = await subAccount(club, name, amt * MILLI);
    created.children.push(members[name]);
  }
  const { alice: A, bob: B, carol: C, dave: D } = members;

  console.log("2. alice mints card #1 with 0.5 NEAR inside");
  const mint = await call(A, clubId, "mint", {}, { deposit: 600n * MILLI });
  const wallet = `1.${clubId}`;
  console.log(`  minted card ${mint.value.token_id}  ${txLink(mint.tx)}`);
  // give the account-creation receipt a moment to land
  for (let i = 0; i < 20; i++) {
    try {
      await provider.viewAccount({ accountId: wallet });
      break;
    } catch {
      await new Promise((r) => setTimeout(r, 1000));
    }
  }
  const acct = await provider.viewAccount({ accountId: wallet });
  check(mint.value.token_id === "1", "card id is 1");
  check(BigInt(acct.amount) === 510n * MILLI, `${wallet} exists with 0.51 NEAR (0.01 reserve + 0.5 inside)`);
  check(acct.global_contract_hash === WALLET_HASH, `it runs the wallet code by global hash ${WALLET_HASH}`);
  const keys = await provider.viewAccessKeyList({ accountId: wallet });
  check(keys.keys.length === 0, "it has no access keys: nobody can sign for it");
  const info = await view(wallet, "wallet_info");
  check(info.collection === clubId && info.token_id === "1", "wallet_info() names its club and card");

  console.log("3. Money comes in from outside");
  const t = await B.transfer({ receiverId: wallet, amount: 200n * MILLI });
  console.log(`  bob sent 0.2 NEAR with a plain transfer  ${txLink(t.transaction.hash)}`);
  check((await settledBalance(wallet, (b) => b === 710n * MILLI)) === 710n * MILLI, "card account holds 0.71 NEAR");
  const tTreasury = await balance(treasury.accountId);
  const ticket = await call(C, clubId, "buy_ticket", { event: "demo night" }, { deposit: 400n * MILLI, gas: 300n * TGAS });
  console.log(`  carol bought a 0.4 NEAR ticket  ${txLink(ticket.tx)}`);
  check(ticket.value.per_card === (200n * MILLI).toString(), "the only card gets half the ticket: 0.2 NEAR");
  check((await settledBalance(wallet, (b) => b === 910n * MILLI)) === 910n * MILLI, "card account holds 0.91 NEAR");
  const t2 = tTreasury + 200n * MILLI;
  check((await settledBalance(treasury.accountId, (b) => b === t2)) === t2, "treasury got the other 0.2 NEAR");

  console.log("4. Only the holder can move it");
  let failed = false;
  try {
    await call(B, clubId, "withdraw", { token_id: "1", amount: (100n * MILLI).toString() }, { deposit: 1n });
  } catch (e) {
    failed = true;
    console.log(`  bob's withdrawal refused: ${e.message.slice(0, 80)}`);
  }
  check(failed, "bob (not the holder) cannot withdraw");
  const w = await call(A, clubId, "withdraw", { token_id: "1", amount: (300n * MILLI).toString() }, { deposit: 1n });
  console.log(`  alice withdrew 0.3 NEAR  ${txLink(w.tx)}`);
  check(about(await settledBalance(wallet, (b) => about(b, 610n * MILLI)), 610n * MILLI), "card account holds ~0.61 NEAR");
  await call(A, clubId, "post", { token_id: "1", text: "alice here, card #1" });
  check((await view(clubId, "get_board")).length === 1, "alice posted on the members' board as holder of #1");

  console.log("5. alice sells the card to dave for 0.5 NEAR");
  await call(A, clubId, "list_card", { token_id: "1", price: (500n * MILLI).toString() }, { deposit: 1n });
  const aliceBefore = await balance(A.accountId);
  const treasuryBefore = await balance(treasury.accountId);
  const sale = await call(D, clubId, "buy_card", { token_id: "1" }, { deposit: 500n * MILLI });
  console.log(`  dave bought card #1  ${txLink(sale.tx)}`);
  const token = await view(clubId, "nft_token", { token_id: "1" });
  check(token.owner_id === D.accountId, "dave holds card #1");
  const t3 = treasuryBefore + 25n * MILLI;
  check((await settledBalance(treasury.accountId, (b) => b === t3)) === t3, "treasury got the 5% royalty: 0.025 NEAR");
  // alice's balance also receives gas refunds from her earlier calls, so "at least"
  const a3 = aliceBefore + 475n * MILLI;
  check((await settledBalance(A.accountId, (b) => b >= a3)) >= a3, "alice received the other 0.475 NEAR");
  failed = false;
  try {
    await call(A, clubId, "withdraw", { token_id: "1", amount: "1" }, { deposit: 1n });
  } catch {
    failed = true;
  }
  check(failed, "alice can no longer withdraw from the card she sold");
  failed = false;
  try {
    await call(A, clubId, "post", { token_id: "1", text: "still mine?" });
  } catch {
    failed = true;
  }
  check(failed, "alice can no longer post with card #1");
  const before = await balance(wallet);
  const dw = await call(D, clubId, "withdraw", { token_id: "1", amount: (200n * MILLI).toString() }, { deposit: 1n });
  console.log(`  dave withdrew 0.2 NEAR  ${txLink(dw.tx)}`);
  const w3 = before - 200n * MILLI;
  check(about(await settledBalance(wallet, (b) => about(b, w3)), w3), "dave controls the money that came with the card");
  const card = await view(clubId, "get_card", { token_id: "1" });
  check(
    JSON.stringify(card.holders.map((h) => h.account_id)) === JSON.stringify([A.accountId, D.accountId]),
    "card history lists alice then dave",
  );
  console.log(`  card #1 history: ${card.events.map((e) => e.kind).join(" -> ")}`);

  console.log(process.exitCode ? "\nSome checks FAILED" : "\nAll checks passed");
  if (await cleanup(club, [A, B, C, D, treasury])) {
    console.log(`Deleted the demo accounts; their NEAR went back to ${club.sponsor}. Card account ${wallet} stays (it has no keys).`);
  } else {
    console.log(`Open the page against this club: docs/index.html?club=${clubId}`);
  }
}

main().catch(async (e) => {
  console.error(e);
  if (created.root) await cleanup(created.root, created.children).catch(() => {});
  process.exit(1);
});
