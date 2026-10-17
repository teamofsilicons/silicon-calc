//! Notes (Excel's legacy comments) and threaded comments with their replies, as [`Note`]s.

use ahash::HashMap;
use quick_xml::events::Event;

use crate::{
    addr::CellPos,
    formats::{
        Note,
        xml::{attr, name_of, read_text, reader},
    },
};

use super::{Package, decode_escapes, read_text_runs};

pub(super) fn read_legacy(pkg: &mut Package<'_>, path: &str, tab: usize) -> Vec<Note> {
    let Some(xml) = pkg.read(path) else { return Vec::new() };
    let mut out = Vec::new();
    let mut authors: Vec<String> = Vec::new();
    let mut r = reader(xml.as_slice());
    let mut buf = Vec::new();
    let mut b2 = Vec::new();
    let mut current: Option<(CellPos, String)> = None;
    loop {
        buf.clear();
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match name_of(&e) {
                b"author" => authors.push(read_text(&mut r, &mut b2, &[])),
                b"comment" => {
                    let pos = attr(&e, b"ref").and_then(|a| CellPos::parse(&a));
                    let author = attr(&e, b"authorId")
                        .and_then(|i| i.parse::<usize>().ok())
                        .and_then(|i| authors.get(i).cloned())
                        .unwrap_or_default();
                    current = pos.map(|p| (p, author));
                }
                b"text" => {
                    let text = decode_escapes(&read_text_runs(&mut r, &mut b2)).into_owned();
                    if let Some((pos, author)) = current.take() {
                        // Excel starts a note with its author's name in bold: "Ada:\n…".
                        let body = text
                            .strip_prefix(&format!("{author}:"))
                            .map(|t| t.trim_start_matches(['\r', '\n']).to_owned())
                            .unwrap_or(text);
                        out.push(Note { tab, row: pos.row, col: pos.col, author, text: body.trim_end().to_owned(), replies: Vec::new() });
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

pub(super) fn read_threaded(pkg: &mut Package<'_>, path: &str, persons: Option<&str>, tab: usize) -> Vec<Note> {
    let mut people: HashMap<String, String> = HashMap::default();
    if let Some(p) = persons.and_then(|p| pkg.read(p)) {
        let mut r = reader(p.as_slice());
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match r.read_event_into(&mut buf) {
                Ok(Event::Start(e)) | Ok(Event::Empty(e)) if name_of(&e) == b"person" => {
                    if let (Some(id), Some(name)) = (attr(&e, b"id"), attr(&e, b"displayName")) {
                        people.insert(id, name);
                    }
                }
                Ok(Event::Eof) | Err(_) => break,
                _ => {}
            }
        }
    }
    let Some(xml) = pkg.read(path) else { return Vec::new() };
    let mut r = reader(xml.as_slice());
    let mut buf = Vec::new();
    let mut b2 = Vec::new();
    let mut notes: Vec<Note> = Vec::new();
    let mut ids: HashMap<String, usize> = HashMap::default();
    // (cell, author, id, parent)
    let mut current: Option<(CellPos, String, Option<String>, Option<String>)> = None;
    loop {
        buf.clear();
        match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => match name_of(&e) {
                b"threadedComment" => {
                    let pos = attr(&e, b"ref").and_then(|a| CellPos::parse(&a));
                    let author = attr(&e, b"personId").and_then(|p| people.get(&p).cloned()).unwrap_or_default();
                    current = pos.map(|p| (p, author, attr(&e, b"id"), attr(&e, b"parentId")));
                }
                b"text" => {
                    let text = read_text(&mut r, &mut b2, &[]);
                    if let Some((pos, author, id, parent)) = current.take() {
                        match parent.and_then(|p| ids.get(&p).copied()) {
                            Some(i) => notes[i].replies.push((author, text)),
                            None => {
                                if let Some(id) = id {
                                    ids.insert(id, notes.len());
                                }
                                notes.push(Note { tab, row: pos.row, col: pos.col, author, text, replies: Vec::new() });
                            }
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    notes
}
