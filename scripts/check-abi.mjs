// Claim: "the club has no owner and no admin method; callbacks are private".
// Reads the ABI cargo-near generated for res/satchel_club.wasm.
import { readFileSync } from "node:fs";

const abi = JSON.parse(readFileSync(new URL("../res/satchel_club_abi.json", import.meta.url)));
const fns = abi.body.functions;
const calls = fns.filter((f) => f.kind === "call").map((f) => f.name).sort();
const expected = [
  "act", "buy_card", "buy_ticket", "list_card", "mint", "new", "nft_resolve_transfer", "nft_transfer",
  "nft_transfer_call", "on_wallet_created", "on_wallet_done", "post", "unlist_card", "withdraw",
].sort();
let ok = true;
const check = (cond, msg) => { console.log(`${cond ? "  ok  " : "  FAIL"}  ${msg}`); ok &&= cond; };
check(JSON.stringify(calls) === JSON.stringify(expected), `state-changing methods are exactly: ${expected.join(", ")}`);
if (JSON.stringify(calls) !== JSON.stringify(expected)) console.log("        found:", calls.join(", "));
const suspicious = fns.filter((f) => /owner_|admin|set_|update|upgrade|migrate|pause|approve/i.test(f.name));
check(suspicious.length === 0, "no admin-style method and no NEP-178 approvals");
for (const name of ["new", "nft_resolve_transfer", "on_wallet_created", "on_wallet_done"]) {
  const f = fns.find((x) => x.name === name);
  check(f?.modifiers?.includes("private"), `${name} is #[private]`);
}
// the wallet has no SDK and no ABI: check its exports straight from the wasm
const wasm = new WebAssembly.Module(readFileSync(new URL("../res/satchel_wallet.wasm", import.meta.url)));
const exportsFound = WebAssembly.Module.exports(wasm).filter((e) => e.kind === "function").map((e) => e.name).sort();
check(JSON.stringify(exportsFound) === JSON.stringify(["call_contract", "send_near", "wallet_info"]), `wallet exports only: ${exportsFound.join(", ")}`);
const imports = WebAssembly.Module.imports(wasm).map((i) => i.name);
check(!imports.some((n) => /add_key|deploy|delete_account|stake/.test(n)), "wallet imports no host function that adds keys, deploys code, deletes or stakes");
process.exit(ok ? 0 : 1);
