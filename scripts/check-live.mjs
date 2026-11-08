// Checks the shared testnet deployment against this repository:
//   npm run check:live              # code, parameters, card accounts
//   npm run check:live -- --locked  # also require that the club account has no keys
//
// Claim: "the deployed club is exactly res/satchel_club.wasm with deploy/testnet.json,
// every card account runs res/satchel_wallet.wasm by global hash and has no keys".
import { readFileSync } from "node:fs";
import { check, provider, sha256Base58, view } from "./lib.mjs";

const deploy = JSON.parse(readFileSync(new URL("../deploy/testnet.json", import.meta.url)));
const id = deploy.contract;
const club = readFileSync(new URL("../res/satchel_club.wasm", import.meta.url));
const wallet = readFileSync(new URL("../res/satchel_wallet.wasm", import.meta.url));

console.log(`Checking ${id} on testnet`);
const acct = await provider.viewAccount({ accountId: id });
check(acct.code_hash === sha256Base58(club), `club code hash ${acct.code_hash} = sha256(res/satchel_club.wasm)`);
const c = await view(id, "get_club");
check(JSON.stringify(c.config) === JSON.stringify(deploy.config), "get_club().config equals deploy/testnet.json");
check(c.config.wallet_code_hash === sha256Base58(wallet), "wallet_code_hash = sha256(res/satchel_wallet.wasm)");
await provider.viewGlobalContractCode({ identifier: { codeHash: c.config.wallet_code_hash } });
check(true, "that wallet code exists on testnet as a global contract");
console.log(`  info  ${c.cards} card(s) minted`);
for (let i = 1; i <= c.cards; i++) {
  const w = `${i}.${id}`;
  const a = await provider.viewAccount({ accountId: w });
  const keys = await provider.viewAccessKeyList({ accountId: w });
  check(a.global_contract_hash === c.config.wallet_code_hash && keys.keys.length === 0, `${w}: wallet code by hash, no access keys`);
}
const keys = await provider.viewAccessKeyList({ accountId: id });
console.log(`  info  ${id} has ${keys.keys.length} access key(s)`);
if (process.argv.includes("--locked")) check(keys.keys.length === 0, "club account is locked (no access keys)");
console.log(process.exitCode ? "Some checks FAILED" : "All checks passed");
