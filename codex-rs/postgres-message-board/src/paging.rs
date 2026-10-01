//! Bounded offset pagination. Concurrent posts may shift results between pages.
//! Cursors belong to the original query; restart without one to refresh the listing.

use crate::board::StoredPost;
use crate::board::invalid;
use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use codex_agent_message_board_extension::Page;
use codex_agent_message_board_extension::PageRequest;
use codex_agent_message_board_extension::PostPreview;
use codex_agent_message_board_extension::SortDirection;
use codex_protocol::error::Result;

pub(crate) struct Window {
    offset: u32,
    pub(crate) limit: usize,
}

impl Window {
    pub(crate) fn new(page: &PageRequest) -> Result<Self> {
        let offset = match &page.cursor {
            Some(cursor) => {
                let mut bytes = [0; 4];
                let len = BASE64_URL_SAFE_NO_PAD
                    .decode_slice(cursor, &mut bytes)
                    .map_err(|_| invalid("invalid cursor"))?;
                if len != bytes.len() {
                    return Err(invalid("invalid cursor"));
                }
                u32::from_be_bytes(bytes)
            }
            None => 0,
        };
        Ok(Self {
            offset,
            limit: (page.limit.get() as usize).min(50),
        })
    }

    pub(crate) fn offset(&self) -> i64 {
        i64::from(self.offset)
    }

    pub(crate) fn finish<T>(self, mut results: Vec<T>) -> Result<Page<T>> {
        let has_more = results.len() > self.limit;
        results.truncate(self.limit);
        let next_cursor = if has_more {
            let offset = self
                .offset
                .checked_add(results.len() as u32)
                .ok_or_else(|| invalid("cursor offset exceeds the board limit"))?;
            Some(BASE64_URL_SAFE_NO_PAD.encode(offset.to_be_bytes()))
        } else {
            None
        };
        Ok(Page {
            results,
            next_cursor,
        })
    }
}

pub(crate) fn direction(direction: SortDirection) -> &'static str {
    match direction {
        SortDirection::NewestFirst => "DESC",
        SortDirection::OldestFirst => "ASC",
    }
}

pub(crate) fn preview(post: StoredPost, max_chars: usize) -> PostPreview {
    let n_chars = post.text.chars().count();
    let text_preview = post.text.chars().take(max_chars).collect();
    PostPreview {
        metadata: post.metadata,
        text_preview,
        n_chars,
        truncated: n_chars > max_chars,
    }
}
