//! Glyph Protocol dispatch (spec §3–§7).
//!
//! The escape parser surfaces a glyph-protocol APC as
//! `Action::GlyphProtocol(body)`; here we parse the body and apply it to
//! this session's [`Glossary`](wezterm_glyph_protocol::Glossary), writing
//! any reply back to the PTY.

use super::TerminalState;
use std::io::Write;
use wezterm_glyph_protocol::parse::{
    self, GlyphCommand, ParseError, QueryStatus, SUPPORTED_FORMATS,
};

impl TerminalState {
    pub(crate) fn glyph_protocol(&mut self, data: Vec<u8>) {
        // Gate the entire protocol behind the config option. When off, every
        // glyph-protocol APC is ignored: no `s` reply, no register/clear,
        // and (since nothing is ever registered) the renderer has nothing to
        // consult. Clients detect "unsupported" via the absent `s` reply
        // (spec §3.3).
        if !self.config.enable_glyph_protocol() {
            return;
        }
        match parse::parse(&data) {
            Ok(GlyphCommand::Support) => {
                self.glyph_reply(&parse::format_support_response(SUPPORTED_FORMATS));
            }
            Ok(GlyphCommand::Query { cp }) => {
                // The terminal model owns the glossary but cannot see the
                // GUI's font fallback chain, so it reports glossary coverage
                // only; accurate `system` coverage would need the font stack.
                let in_glossary = self.glyph_glossary.lock().unwrap().contains(cp);
                let status = if in_glossary {
                    QueryStatus::Glossary
                } else {
                    QueryStatus::Free
                };
                self.glyph_reply(&parse::format_query_response(cp, status));
            }
            Ok(GlyphCommand::Register { cp, glyph, reply }) => match glyph.validate() {
                Ok(()) => {
                    self.glyph_glossary.lock().unwrap().insert(cp, glyph);
                    if reply.emit_success() {
                        self.glyph_reply(&parse::format_register_ok(cp));
                    }
                }
                Err(reason) => {
                    if reply.emit_error() {
                        self.glyph_reply(&parse::format_register_error(cp, reason));
                    }
                }
            },
            Ok(GlyphCommand::Clear { cp }) => {
                {
                    let mut g = self.glyph_glossary.lock().unwrap();
                    match cp {
                        Some(cp) => g.clear_one(cp),
                        None => g.clear_all(),
                    }
                }
                self.glyph_reply(&parse::format_clear_ok(cp));
            }
            // Register rejected at parse time (e.g. out-of-namespace,
            // malformed/oversized payload): reply honours the reply level.
            Err(ParseError::RegisterFailed { cp, reason, reply }) => {
                if reply.emit_error() {
                    self.glyph_reply(&parse::format_register_error(cp, reason));
                }
            }
            Err(ParseError::ClearOutOfNamespace) => {
                self.glyph_reply(&parse::format_clear_error_out_of_namespace());
            }
            // Not our protocol, or unparseable framing: ignore, per the APC
            // convention that terminals drop commands they don't understand.
            Err(ParseError::NotGlyphProtocol) | Err(ParseError::Malformed(_)) => {}
        }
    }

    fn glyph_reply(&mut self, resp: &str) {
        let _ = write!(self.writer, "{}", resp);
        let _ = self.writer.flush();
    }
}
