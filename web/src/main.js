import {
  accountUrl,
  call,
  createGuest,
  forgetGuest,
  loadGuest,
  nearBalance,
  sendNear,
  txUrl,
  view,
  viewAccount,
} from "./near.js";

const params = new URLSearchParams(location.search);
const CLUB = params.get("club") || "satchel-club.testnet";
const YOCTO = 10n ** 24n;
const STORAGE_PRICE = 10n ** 19n; // yoctoNEAR per byte

const $ = (id) => document.getElementById(id);
let guest = loadGuest();
let club = null;
let cards = [];
let balances = {};
let selected = null;
let loading = false;

// ---------- formatting ----------
function near(yocto, digits = 3) {
  const v = BigInt(yocto);
  const whole = v / YOCTO;
  const frac = ((v % YOCTO) * 10n ** BigInt(digits)) / YOCTO;
  return `${whole}.${frac.toString().padStart(digits, "0")}`;
}
function parseNear(text) {
  const m = String(text).trim().match(/^(\d+)(?:\.(\d{0,24}))?$/);
  if (!m) return null;
  return BigInt(m[1]) * YOCTO + BigInt((m[2] || "").padEnd(24, "0") || "0");
}
const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
const short = (a) => (a && a.length > 26 ? a.slice(0, 12) + "…" + a.slice(-12) : a);
const when = (ms) => new Date(ms).toLocaleString();
const isMine = (c) => guest && c.owner_id === guest.accountId;

function toast(html, kind = "info") {
  const t = $("toast");
  t.innerHTML = html;
  t.className = `toast ${kind}`;
  t.hidden = false;
}

// ---------- data ----------
async function load() {
  if (loading) return;
  loading = true;
  try {
    club = await view(CLUB, "get_club");
    cards = [];
    for (let from = 1; from <= club.cards; from += 50) {
      cards.push(...(await view(CLUB, "get_cards", { from, limit: 50 })));
    }
    const accts = await Promise.all(cards.map((c) => viewAccount(c.wallet).catch(() => null)));
    balances = {};
    cards.forEach((c, i) => {
      const a = accts[i];
      if (a) {
        const total = BigInt(a.amount);
        const locked = BigInt(a.storage_usage) * STORAGE_PRICE;
        balances[c.token_id] = { total, available: total > locked ? total - locked : 0n };
      }
    });
    if (selected) selected = await view(CLUB, "get_card", { token_id: selected.token_id });
    $("error").hidden = true;
    render();
  } catch (e) {
    $("error").hidden = false;
    $("error").textContent = `Could not read ${CLUB} on testnet: ${e.message}`;
  } finally {
    loading = false;
  }
}

// ---------- rendering ----------
function render() {
  renderClub();
  renderMe();
  renderCards();
  renderCard();
  renderBoard();
}

function renderClub() {
  const cfg = club.config;
  $("club-link").href = accountUrl(CLUB);
  $("club-link").textContent = CLUB;
  $("stat-cards").textContent = `${club.cards} / ${cfg.max_cards}`;
  $("stat-tickets").textContent = club.tickets_sold;
  $("stat-to-cards").textContent = `${near(club.paid_to_cards)} NEAR`;
  $("stat-per-card").textContent = `${near(club.earned_per_card)} NEAR`;
  $("stat-sales").textContent = `${club.cards_sold} (${near(club.sales_volume)} NEAR)`;
  $("rule-mint").textContent = near(cfg.mint_price, 2);
  $("rule-share").textContent = `${cfg.member_share_bps / 100}%`;
  $("rule-royalty").textContent = `${cfg.royalty_bps / 100}%`;
  $("rule-ticket-min").textContent = near(cfg.ticket_min_price, 2);
  $("rule-example").textContent = `${CLUB.length < 30 ? "7." + CLUB : "7.<club>"}`;
}

function renderMe() {
  const box = $("me");
  if (!guest) {
    box.innerHTML = `
      <p>Try it with a throwaway <b>testnet</b> account. The page asks the public testnet faucet for one (free, about 5 seconds, 10 testnet NEAR); its key stays in this browser.</p>
      <button id="btn-guest" class="primary">Create a testnet account</button>`;
    $("btn-guest").onclick = onCreateGuest;
    return;
  }
  const mine = cards.filter(isMine);
  box.innerHTML = `
    <div class="row"><span class="muted">Account</span><a href="${accountUrl(guest.accountId)}" target="_blank" rel="noopener">${esc(short(guest.accountId))}</a></div>
    <div class="row"><span class="muted">Testnet NEAR</span><b id="my-near">…</b></div>
    <div class="row"><span class="muted">Your cards</span><span>${mine.length ? mine.map((c) => `<a href="#card=${c.token_id}">#${c.token_id}</a>`).join(" ") : "none yet"}</span></div>
    <div class="actions">
      <div class="inline"><input id="seed" inputmode="decimal" placeholder="put NEAR in it (optional)" />
      <button id="btn-mint" class="primary">Mint a card (${near(club.config.mint_price, 2)} NEAR)</button></div>
      <div class="inline"><input id="ticket-event" placeholder="event name" value="Friday meetup" />
      <input id="ticket-price" inputmode="decimal" value="1" class="narrow" />
      <button id="btn-ticket">Buy ticket (NEAR)</button></div>
    </div>
    <p class="tiny"><a href="#" id="btn-forget">Forget this guest account</a></p>`;
  nearBalance(guest.accountId).then((b) => ($("my-near").textContent = near(b, 2))).catch(() => {});
  $("btn-mint").onclick = onMint;
  $("btn-ticket").onclick = onTicket;
  $("btn-forget").onclick = (e) => {
    e.preventDefault();
    if (confirm("Forget this guest account? Its key is only stored in this browser.")) {
      forgetGuest();
      guest = null;
      render();
    }
  };
}

function renderCards() {
  const grid = $("cards");
  if (!cards.length) {
    grid.innerHTML = `<p class="muted">No card minted yet.</p>`;
    return;
  }
  grid.innerHTML = cards
    .map((c) => {
      const b = balances[c.token_id];
      return `<a class="tile ${isMine(c) ? "mine" : ""} ${selected && selected.token_id === c.token_id ? "sel" : ""}" href="#card=${c.token_id}">
        <span class="num">#${c.token_id}</span>
        <span class="bal">${b ? near(b.total, 2) : "?"} Ⓝ</span>
        <span class="who">${isMine(c) ? "yours" : esc(short(c.owner_id))}</span>
        ${c.listing ? `<span class="tag">for sale ${near(c.listing.price, 2)}</span>` : ""}
      </a>`;
    })
    .join("");
}

function renderCard() {
  const box = $("card");
  if (!selected) {
    box.innerHTML = `<p class="muted">Pick a card above to see its account, history and what you can do with it.</p>`;
    return;
  }
  const c = selected;
  const b = balances[c.token_id];
  const mine = isMine(c);
  const holdersHtml = c.holders
    .map((h) => `<li><a href="${accountUrl(h.account_id)}" target="_blank" rel="noopener">${esc(short(h.account_id))}</a> <span class="muted">since ${when(h.since_ms)} (${h.how})</span></li>`)
    .join("");
  const eventsHtml = [...c.events]
    .reverse()
    .map(
      (e) => `<tr><td>${when(e.at_ms)}</td><td><b>${esc(e.kind)}</b></td><td>${esc(short(e.actor))}</td><td>${e.other ? esc(short(e.other)) : ""}</td><td>${e.amount ? near(e.amount) + " Ⓝ" : ""}</td><td class="muted">${esc(e.note ?? "")}</td></tr>`,
    )
    .join("");
  let actions = "";
  if (!guest) {
    actions = `<p class="muted">Create a testnet account (top right) to act on this card.</p>`;
  } else if (mine) {
    actions = `
      <p class="ok-note">You hold this card, so you control <b>${esc(c.wallet)}</b>.</p>
      <div class="inline"><input id="wd-amount" inputmode="decimal" placeholder="NEAR" value="${b ? near(b.available / 2n, 2) : ""}" /><button id="btn-withdraw" class="primary" ${c.busy ? "disabled" : ""}>Withdraw to me</button></div>
      <div class="inline"><input id="give-to" placeholder="account.testnet" /><button id="btn-give" ${c.busy ? "disabled" : ""}>Give card</button></div>
      <div class="inline"><input id="list-price" inputmode="decimal" placeholder="price in NEAR" /><button id="btn-list" ${c.busy ? "disabled" : ""}>${c.listing ? "Change price" : "List for sale"}</button>${c.listing ? `<button id="btn-unlist">Unlist</button>` : ""}</div>
      <div class="inline"><input id="post-text" maxlength="280" placeholder="post on the members' board" /><button id="btn-post">Post</button></div>`;
  } else {
    actions = `
      <div class="inline"><input id="pay-amount" inputmode="decimal" value="0.5" class="narrow" /><button id="btn-pay">Send NEAR to ${esc(short(c.wallet))}</button></div>
      <p class="tiny muted">A plain transfer, exactly what any wallet or exchange would do.</p>
      ${c.listing ? `<button id="btn-buy" class="primary" ${c.busy ? "disabled" : ""}>Buy this card for ${near(c.listing.price, 2)} NEAR</button><p class="tiny muted">You get the card and everything in its account. The seller gets the price minus ${club.config.royalty_bps / 100}% royalty to the club.</p>` : ""}
      <p class="muted">Only the holder can withdraw or post with this card. Try it: the contract will refuse.</p>
      <button id="btn-try-withdraw">Try to withdraw anyway</button>`;
  }
  box.innerHTML = `
    <div class="row head"><h2>Card #${esc(c.token_id)}</h2>${c.busy ? `<span class="tag">wallet order in flight</span>` : ""}</div>
    <div class="row"><span class="muted">Its account</span><a href="${accountUrl(c.wallet)}" target="_blank" rel="noopener">${esc(c.wallet)}</a></div>
    <div class="row"><span class="muted">Balance</span><b class="big">${b ? near(b.total) : "?"} NEAR</b></div>
    <div class="row"><span class="muted">Earned from tickets</span><span>${near(c.earned_from_tickets)} NEAR</span></div>
    <div class="row"><span class="muted">Withdrawn / spent</span><span>${near(c.withdrawn)} / ${near(c.spent_by_calls)} NEAR</span></div>
    <div class="row"><span class="muted">Holder</span><a href="${accountUrl(c.owner_id)}" target="_blank" rel="noopener">${esc(short(c.owner_id))}</a></div>
    ${c.listing ? `<div class="row"><span class="muted">For sale</span><b>${near(c.listing.price, 2)} NEAR</b></div>` : ""}
    <div class="actions">${actions}</div>
    <h3>Holders</h3><ol class="holders">${holdersHtml}</ol>
    <h3>History</h3><div class="scroll"><table><thead><tr><th>When</th><th>What</th><th>By</th><th>Other</th><th>Amount</th><th></th></tr></thead><tbody>${eventsHtml}</tbody></table></div>`;
  const on = (id, fn) => $(id) && ($(id).onclick = fn);
  on("btn-withdraw", () => {
    const amt = parseNear($("wd-amount").value);
    if (!amt) return toast("Enter an amount in NEAR", "err");
    act(`Withdraw ${near(amt)} NEAR from card #${c.token_id}`, () =>
      call(guest, CLUB, "withdraw", { token_id: c.token_id, amount: amt.toString() }, { deposit: "1", gasTgas: 60 }),
    );
  });
  on("btn-give", () => {
    const to = $("give-to").value.trim();
    if (!to) return toast("Enter the account to give the card to", "err");
    act(`Give card #${c.token_id} to ${to}`, () =>
      call(guest, CLUB, "nft_transfer", { receiver_id: to, token_id: c.token_id }, { deposit: "1", gasTgas: 50 }),
    );
  });
  on("btn-list", () => {
    const p = parseNear($("list-price").value);
    if (!p) return toast("Enter a price in NEAR", "err");
    act(`List card #${c.token_id} for ${near(p, 2)} NEAR`, () =>
      call(guest, CLUB, "list_card", { token_id: c.token_id, price: p.toString() }, { deposit: "1", gasTgas: 30 }),
    );
  });
  on("btn-unlist", () =>
    act(`Unlist card #${c.token_id}`, () => call(guest, CLUB, "unlist_card", { token_id: c.token_id }, { deposit: "1", gasTgas: 30 })),
  );
  on("btn-post", () => {
    const text = $("post-text").value.trim();
    if (!text) return;
    act("Post to the members' board", () => call(guest, CLUB, "post", { token_id: c.token_id, text }, { gasTgas: 30 }));
  });
  on("btn-pay", () => {
    const amt = parseNear($("pay-amount").value);
    if (!amt) return toast("Enter an amount in NEAR", "err");
    act(`Send ${near(amt)} NEAR to ${c.wallet}`, () => sendNear(guest, c.wallet, amt));
  });
  on("btn-buy", () =>
    act(`Buy card #${c.token_id}`, () => call(guest, CLUB, "buy_card", { token_id: c.token_id }, { deposit: c.listing.price, gasTgas: 60 })),
  );
  on("btn-try-withdraw", () =>
    act(`Withdraw from card #${c.token_id} without holding it`, () =>
      call(guest, CLUB, "withdraw", { token_id: c.token_id, amount: "1" }, { deposit: "1", gasTgas: 60 }),
    ),
  );
}

async function renderBoard() {
  try {
    const posts = await view(CLUB, "get_board", { limit: 20 });
    $("board").innerHTML = posts.length
      ? posts.map((p) => `<li><b>#${esc(p.token_id)}</b> <span class="muted">${esc(short(p.author))} · ${when(p.at_ms)}</span><br>${esc(p.text)}</li>`).join("")
      : `<li class="muted">No posts yet. Only current card holders can post.</li>`;
  } catch {
    /* ignore */
  }
}

// ---------- actions ----------
async function act(label, fn) {
  toast(`${esc(label)}…`);
  try {
    const { txHash } = await fn();
    toast(`${esc(label)}: done. <a href="${txUrl(txHash)}" target="_blank" rel="noopener">View transaction</a>`, "ok");
  } catch (e) {
    const link = e.txHash ? ` <a href="${txUrl(e.txHash)}" target="_blank" rel="noopener">transaction</a>` : "";
    toast(`${esc(label)} failed: ${esc(e.message)}${link}`, "err");
  }
  await load();
}

async function onCreateGuest() {
  toast("Asking the testnet faucet for an account…");
  try {
    guest = await createGuest("member");
    toast(`Created ${esc(guest.accountId)} with 10 testnet NEAR.`, "ok");
  } catch (e) {
    toast(`Faucet failed: ${esc(e.message)}. Try again in a minute.`, "err");
  }
  await load();
}

function onMint() {
  const seedText = $("seed").value.trim();
  const seed = seedText ? parseNear(seedText) : 0n;
  if (seed === null) return toast("Enter the extra NEAR as a number, e.g. 1.5", "err");
  const deposit = BigInt(club.config.mint_price) + seed;
  act(`Mint a card${seed ? ` with ${near(seed)} NEAR inside` : ""}`, async () => {
    const r = await call(guest, CLUB, "mint", {}, { deposit: deposit.toString(), gasTgas: 100 });
    if (r.result && r.result.token_id) location.hash = `card=${r.result.token_id}`;
    return r;
  });
}

function onTicket() {
  const price = parseNear($("ticket-price").value);
  const event = $("ticket-event").value.trim() || "event";
  if (!price) return toast("Enter a ticket price in NEAR", "err");
  act(`Buy a ticket for "${event}" (${near(price, 2)} NEAR)`, () =>
    call(guest, CLUB, "buy_ticket", { event }, { deposit: price.toString(), gasTgas: 300 }),
  );
}

async function route() {
  const m = location.hash.match(/card=(\d+)/);
  if (m) {
    try {
      selected = await view(CLUB, "get_card", { token_id: m[1] });
    } catch {
      selected = null;
    }
  } else {
    selected = null;
  }
  if (club) {
    renderCards();
    renderCard();
    if (selected) $("card").scrollIntoView({ behavior: "smooth", block: "start" });
  }
}

window.addEventListener("hashchange", route);
(async () => {
  await route();
  await load();
  setInterval(load, 15000);
})();
