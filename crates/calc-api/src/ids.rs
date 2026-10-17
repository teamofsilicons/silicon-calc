//! Ids for sheets, folders, copies and transfers: 12 characters of [a-z0-9], unguessable.

use rand::Rng as _;

const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyz23456789";

fn short(prefix: &str) -> String {
    let mut rng = rand::rng();
    let body: String = (0..12 - prefix.len()).map(|_| ALPHABET[rng.random_range(0..ALPHABET.len())] as char).collect();
    format!("{prefix}{body}")
}

pub fn sheet() -> String {
    short("")
}

pub fn folder() -> String {
    short("f")
}

pub fn copy() -> String {
    short("c")
}

pub fn transfer() -> String {
    short("t")
}

/// The id of a sheet's main line.
pub fn main_line(sheet: &str) -> String {
    format!("main:{sheet}")
}

/// Whether text could be an id of ours (so a bad id is a 404, never a query on junk).
pub fn looks_like_id(s: &str) -> bool {
    s.len() == 12 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}
