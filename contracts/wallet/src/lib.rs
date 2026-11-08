//! The wallet that lives on every Satchel card's account, `<token_id>.<collection>`.
//!
//! * It has no state and no access keys. Nobody can sign for this account.
//! * It derives its collection (parent account) and token id from its own name.
//! * It only acts when its parent collection asks, and the collection only asks on
//!   behalf of whoever holds card `<token_id>` at that moment.
//! * It can send NEAR and call other contracts (so a card can hold tokens, buy
//!   things, vote...). It cannot add keys, deploy code or delete itself: no method
//!   creates those actions.
//!
//! Written directly against the NEAR host functions (no SDK, no serde) so the wasm
//! is a few KB: it is deployed once as a global contract by hash, and global
//! deployments are charged per byte. Arguments are borsh-encoded by the collection.
//!
//! Methods:
//! * `send_near(receiver_id: AccountId, amount: u128)`            borsh, parent only
//! * `call_contract(receiver_id, method_name: String, args: Vec<u8>, deposit: u128, gas: u64)`
//!                                                                  borsh, parent only
//! * `wallet_info() -> {"collection": "...", "token_id": "..."}`   JSON, anyone

/// Pure helpers, unit-tested on the host.
pub mod logic {
    /// `"7.club.near"` -> `("7", "club.near")`.
    pub fn split_self(account: &[u8]) -> Option<(&[u8], &[u8])> {
        let dot = account.iter().position(|&b| b == b'.')?;
        let (token, rest) = account.split_at(dot);
        let parent = &rest[1..];
        if token.is_empty() || parent.is_empty() {
            return None;
        }
        Some((token, parent))
    }

    /// Minimal borsh reader.
    pub struct Reader<'a> {
        buf: &'a [u8],
        pos: usize,
    }

    impl<'a> Reader<'a> {
        pub fn new(buf: &'a [u8]) -> Self {
            Reader { buf, pos: 0 }
        }

        fn take(&mut self, n: usize) -> Option<&'a [u8]> {
            let end = self.pos.checked_add(n)?;
            let s = self.buf.get(self.pos..end)?;
            self.pos = end;
            Some(s)
        }

        pub fn u64(&mut self) -> Option<u64> {
            Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
        }

        pub fn u128(&mut self) -> Option<u128> {
            Some(u128::from_le_bytes(self.take(16)?.try_into().ok()?))
        }

        /// `String` / `Vec<u8>`: u32 length + bytes.
        pub fn bytes(&mut self) -> Option<&'a [u8]> {
            let len = u32::from_le_bytes(self.take(4)?.try_into().ok()?) as usize;
            self.take(len)
        }

        pub fn done(&self) -> bool {
            self.pos == self.buf.len()
        }
    }

    /// `{"collection":"<parent>","token_id":"<token>"}`. Account ids only contain
    /// `[a-z0-9._-]`, so no escaping is needed.
    pub fn info_json(token: &[u8], parent: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(40 + token.len() + parent.len());
        out.extend_from_slice(b"{\"collection\":\"");
        out.extend_from_slice(parent);
        out.extend_from_slice(b"\",\"token_id\":\"");
        out.extend_from_slice(token);
        out.extend_from_slice(b"\"}");
        out
    }

    pub const ERR_PARENT: &str = "only the card's collection contract can move this wallet's funds";
    pub const ERR_ARGS: &str = "bad arguments (borsh expected)";
    pub const ERR_SELF: &str = "a wallet cannot call itself";
    pub const ERR_NAME: &str = "wallet must live on a sub-account";
}

#[cfg(target_arch = "wasm32")]
mod wasm {
    use super::logic::*;

    #[link(wasm_import_module = "env")]
    unsafe extern "C" {
        fn read_register(register_id: u64, ptr: u64);
        fn register_len(register_id: u64) -> u64;
        fn current_account_id(register_id: u64);
        fn predecessor_account_id(register_id: u64);
        fn input(register_id: u64);
        fn value_return(value_len: u64, value_ptr: u64);
        fn panic_utf8(len: u64, ptr: u64) -> !;
        fn promise_batch_create(account_id_len: u64, account_id_ptr: u64) -> u64;
        fn promise_batch_action_transfer(promise_index: u64, amount_ptr: u64);
        fn promise_batch_action_function_call(
            promise_index: u64,
            method_name_len: u64,
            method_name_ptr: u64,
            arguments_len: u64,
            arguments_ptr: u64,
            amount_ptr: u64,
            gas: u64,
        );
        fn promise_return(promise_id: u64);
    }

    const REG: u64 = 0;

    fn fail(msg: &str) -> ! {
        unsafe { panic_utf8(msg.len() as u64, msg.as_ptr() as u64) }
    }

    fn register() -> Vec<u8> {
        unsafe {
            let len = register_len(REG);
            if len == u64::MAX {
                return Vec::new();
            }
            let mut buf = vec![0u8; len as usize];
            read_register(REG, buf.as_mut_ptr() as u64);
            buf
        }
    }

    fn me() -> Vec<u8> {
        unsafe { current_account_id(REG) };
        register()
    }

    fn args() -> Vec<u8> {
        unsafe { input(REG) };
        register()
    }

    /// Panics unless the caller is the parent collection.
    fn assert_parent(me: &[u8]) {
        unsafe { predecessor_account_id(REG) };
        let caller = register();
        let (_, parent) = split_self(me).unwrap_or_else(|| fail(ERR_NAME));
        if caller.as_slice() != parent {
            fail(ERR_PARENT);
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn wallet_info() {
        let me = me();
        let (token, parent) = split_self(&me).unwrap_or_else(|| fail(ERR_NAME));
        let json = info_json(token, parent);
        unsafe { value_return(json.len() as u64, json.as_ptr() as u64) };
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn send_near() {
        let me = me();
        assert_parent(&me);
        let input = args();
        let mut r = Reader::new(&input);
        let receiver = r.bytes().unwrap_or_else(|| fail(ERR_ARGS));
        let amount = r.u128().unwrap_or_else(|| fail(ERR_ARGS));
        if !r.done() {
            fail(ERR_ARGS);
        }
        let amount = amount.to_le_bytes();
        unsafe {
            let p = promise_batch_create(receiver.len() as u64, receiver.as_ptr() as u64);
            promise_batch_action_transfer(p, amount.as_ptr() as u64);
            promise_return(p);
        }
    }

    #[unsafe(no_mangle)]
    pub extern "C" fn call_contract() {
        let me = me();
        assert_parent(&me);
        let input = args();
        let mut r = Reader::new(&input);
        let receiver = r.bytes().unwrap_or_else(|| fail(ERR_ARGS));
        let method = r.bytes().unwrap_or_else(|| fail(ERR_ARGS));
        let call_args = r.bytes().unwrap_or_else(|| fail(ERR_ARGS));
        let deposit = r.u128().unwrap_or_else(|| fail(ERR_ARGS));
        let gas = r.u64().unwrap_or_else(|| fail(ERR_ARGS));
        if !r.done() {
            fail(ERR_ARGS);
        }
        if receiver == me.as_slice() {
            fail(ERR_SELF);
        }
        let deposit = deposit.to_le_bytes();
        unsafe {
            let p = promise_batch_create(receiver.len() as u64, receiver.as_ptr() as u64);
            promise_batch_action_function_call(
                p,
                method.len() as u64,
                method.as_ptr() as u64,
                call_args.len() as u64,
                call_args.as_ptr() as u64,
                deposit.as_ptr() as u64,
                gas,
            );
            promise_return(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::logic::*;

    fn borsh_str(s: &[u8]) -> Vec<u8> {
        let mut v = (s.len() as u32).to_le_bytes().to_vec();
        v.extend_from_slice(s);
        v
    }

    #[test]
    fn splits_own_name() {
        assert_eq!(split_self(b"7.club.near"), Some((&b"7"[..], &b"club.near"[..])));
        assert_eq!(split_self(b"12.satchel-club.testnet"), Some((&b"12"[..], &b"satchel-club.testnet"[..])));
        assert_eq!(split_self(b"toplevel"), None);
        assert_eq!(split_self(b".club.near"), None);
    }

    #[test]
    fn info_is_json() {
        assert_eq!(info_json(b"7", b"club.near"), br#"{"collection":"club.near","token_id":"7"}"#.to_vec());
    }

    #[test]
    fn reads_send_near_args() {
        let mut buf = borsh_str(b"bob.near");
        buf.extend_from_slice(&5u128.to_le_bytes());
        let mut r = Reader::new(&buf);
        assert_eq!(r.bytes(), Some(&b"bob.near"[..]));
        assert_eq!(r.u128(), Some(5));
        assert!(r.done());
    }

    #[test]
    fn reads_call_contract_args() {
        let mut buf = borsh_str(b"dex.near");
        buf.extend(borsh_str(b"swap"));
        buf.extend(borsh_str(br#"{"a":1}"#));
        buf.extend_from_slice(&7u128.to_le_bytes());
        buf.extend_from_slice(&30_000_000_000_000u64.to_le_bytes());
        let mut r = Reader::new(&buf);
        assert_eq!(r.bytes(), Some(&b"dex.near"[..]));
        assert_eq!(r.bytes(), Some(&b"swap"[..]));
        assert_eq!(r.bytes(), Some(&br#"{"a":1}"#[..]));
        assert_eq!(r.u128(), Some(7));
        assert_eq!(r.u64(), Some(30_000_000_000_000));
        assert!(r.done());
    }

    #[test]
    fn rejects_truncated_args() {
        let mut buf = borsh_str(b"bob.near");
        buf.extend_from_slice(&[1, 2, 3]);
        let mut r = Reader::new(&buf);
        assert!(r.bytes().is_some());
        assert_eq!(r.u128(), None);
        // a length prefix larger than the buffer
        let mut r = Reader::new(&[200, 0, 0, 0, 1]);
        assert_eq!(r.bytes(), None);
    }
}
