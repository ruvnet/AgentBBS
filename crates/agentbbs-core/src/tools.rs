//! Shared agent tool layer (ADR-0050).
//!
//! One implementation per capability an agent can invoke, instead of each
//! caller (the MCP bridge, the live @mention loop-in path, a pod runner)
//! reimplementing "list boards" / "post a message" independently. Each
//! function takes the dependencies it actually needs (`&Bbs` + `Caps`, plus an
//! `&Identity` for anything that signs) and returns a domain [`Result`] — no
//! JSON-RPC or HTTP shape baked in; callers translate to their own wire format.
//!
//! `agentbbs-mcp`'s `tools/call` handlers are thin wrappers over these
//! functions (Phase 2 of ADR-0050); the live loop-in/Battle-Mode reply path and
//! pod-result posting are follow-up migrations.

use crate::board::{Message, MessageBody};
use crate::caps::{require, Caps};
use crate::error::Result;
use crate::identity::Identity;
use crate::rvf::RvfStore;
use crate::service::Bbs;

/// List all message boards, rendered as `slug — title` lines (one per board).
pub fn list_boards(bbs: &Bbs, caps: Caps) -> Result<String> {
    let boards = bbs.list_boards(caps)?;
    let lines: Vec<String> = boards
        .iter()
        .map(|b| format!("{} — {}", b.slug, b.title))
        .collect();
    Ok(if lines.is_empty() {
        "(no boards)".to_string()
    } else {
        lines.join("\n")
    })
}

/// Read up to `limit` recent messages from `board`, rendered as text.
pub fn read_board(bbs: &Bbs, caps: Caps, board: &str, limit: usize) -> Result<String> {
    let msgs = bbs.read_board(caps, board, limit)?;
    Ok(render_messages(board, &msgs))
}

/// Sign and post a message to `board` under `identity`, with `handle` as the
/// cosmetic display name (empty string if the caller has none — e.g. the MCP
/// tool, which never set one). Returns a short confirmation string naming the
/// new message's content-addressed id.
pub fn post_message(
    bbs: &Bbs,
    caps: Caps,
    identity: &Identity,
    board: &str,
    subject: &str,
    text: &str,
    handle: &str,
) -> Result<String> {
    // Enforce POST capability up front so denial reports/errors cleanly.
    require(caps, Caps::POST, "POST")?;
    let body = MessageBody {
        board: board.to_string(),
        parent: None,
        subject: subject.to_string(),
        body: text.to_string(),
        author: identity.id(),
        handle: handle.to_string(),
        created_at: chrono::Utc::now(),
    };
    let message = Message::sign(identity, body)?;
    let id = bbs.post(caps, message)?;
    Ok(format!("posted to {board} as {}", id.0))
}

/// Semantic nearest-neighbour search over `store`, rendered as text.
pub fn search_memory(store: &RvfStore, query: &[f32], top_k: usize) -> Result<String> {
    let hits = store.search(query, top_k)?;
    if hits.is_empty() {
        return Ok("(no memory hits)".to_string());
    }
    let lines: Vec<String> = hits
        .iter()
        .map(|h| format!("{} (score {:.4}) {}", h.id, h.score, h.meta))
        .collect();
    Ok(lines.join("\n"))
}

/// Render a board's messages as a human-readable text block (shared by the
/// `read_board` tool and any resource/listing surface that wants the same
/// rendering, e.g. MCP `resources/read`).
pub fn render_messages(slug: &str, msgs: &[Message]) -> String {
    if msgs.is_empty() {
        return format!("Board '{slug}': (no messages)");
    }
    let mut out = format!("Board '{slug}' — {} message(s):\n", msgs.len());
    for m in msgs {
        let subject = if m.body.subject.is_empty() {
            "(no subject)"
        } else {
            &m.body.subject
        };
        out.push_str(&format!(
            "\n[{}] {} by {}\n{}\n",
            m.id.short(),
            subject,
            m.body.author.short(),
            m.body.body
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::MemoryStore;
    use std::sync::Arc;

    fn bbs() -> Bbs {
        Bbs::new(
            Arc::new(MemoryStore::new()),
            Arc::new(crate::report::NullReporter),
        )
    }

    #[test]
    fn list_boards_empty_then_populated() {
        let bbs = bbs();
        assert_eq!(list_boards(&bbs, Caps::READ).unwrap(), "(no boards)");
        let founder = Identity::generate();
        bbs.create_board(
            Caps::all(),
            crate::board::Board::new("general", "General", founder.id()),
        )
        .unwrap();
        let out = list_boards(&bbs, Caps::READ).unwrap();
        assert!(out.contains("general — General"));
    }

    #[test]
    fn post_then_read_round_trips() {
        let bbs = bbs();
        let founder = Identity::generate();
        bbs.create_board(
            Caps::all(),
            crate::board::Board::new("general", "General", founder.id()),
        )
        .unwrap();
        let poster = Identity::generate();
        let caps = Caps::READ | Caps::POST;
        let confirmation = post_message(
            &bbs,
            caps,
            &poster,
            "general",
            "hi",
            "hello world",
            "poster",
        )
        .unwrap();
        assert!(confirmation.contains("posted to general"));
        let rendered = read_board(&bbs, caps, "general", 10).unwrap();
        assert!(rendered.contains("hello world"));
        assert!(rendered.contains("hi"));
        // The cosmetic handle lands on the stored message (not just rendered text).
        let msgs = bbs.read_board(caps, "general", 10).unwrap();
        assert_eq!(msgs[0].body.handle, "poster");
    }

    #[test]
    fn post_message_with_empty_handle_matches_the_old_mcp_default() {
        let bbs = bbs();
        let founder = Identity::generate();
        bbs.create_board(
            Caps::all(),
            crate::board::Board::new("general", "General", founder.id()),
        )
        .unwrap();
        let poster = Identity::generate();
        let caps = Caps::READ | Caps::POST;
        post_message(&bbs, caps, &poster, "general", "", "via mcp", "").unwrap();
        let msgs = bbs.read_board(caps, "general", 10).unwrap();
        assert_eq!(msgs[0].body.handle, "");
    }

    #[test]
    fn post_message_without_post_cap_is_denied() {
        let bbs = bbs();
        let founder = Identity::generate();
        bbs.create_board(
            Caps::all(),
            crate::board::Board::new("general", "General", founder.id()),
        )
        .unwrap();
        let poster = Identity::generate();
        let err = post_message(&bbs, Caps::READ, &poster, "general", "", "nope", "").unwrap_err();
        assert!(matches!(err, crate::error::Error::PermissionDenied(_)));
    }

    #[test]
    fn search_memory_empty_store_reports_no_hits() {
        let store = RvfStore::new(4);
        assert_eq!(
            search_memory(&store, &[0.0, 0.0, 0.0, 0.0], 5).unwrap(),
            "(no memory hits)"
        );
    }
}
