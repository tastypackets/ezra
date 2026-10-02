use std::borrow::Cow;
use std::num::NonZeroU64;

const FOOTER_START: &str = "\n\n<!-- ezra:status -->\n";
const FOOTER_END: &str = "\n<!-- /ezra:status -->";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentStatus {
    Received,
    Delivered,
    Unconfirmed,
    Failed,
}

pub struct StatusFooter<'context> {
    pub status: CommentStatus,
    pub chat_name: Option<&'context str>,
}

#[derive(Debug, Clone, Copy)]
pub struct CommentReference<'repository> {
    pub repository: &'repository str,
    pub comment_id: NonZeroU64,
    pub author_id: NonZeroU64,
}

impl StatusFooter<'_> {
    pub fn strip(body: &str) -> Cow<'_, str> {
        let mut request = Cow::Borrowed(body);
        let mut search_from = 0;
        while let Some(relative_start) = request[search_from..].find(FOOTER_START) {
            let start = search_from.saturating_add(relative_start);
            let content_start = start.saturating_add(FOOTER_START.len());
            let Some(relative_end) = request[content_start..].find(FOOTER_END) else {
                break;
            };
            let content_end = content_start.saturating_add(relative_end);
            let content = &request[content_start..content_end];
            if content.len() > 4096
                || !content.starts_with("<sub>")
                || !content.ends_with("</sub>")
                || content.contains('\n')
            {
                search_from = content_start;
                continue;
            }
            let end = content_end.saturating_add(FOOTER_END.len());
            request.to_mut().replace_range(start..end, "");
            search_from = start;
        }
        request
    }

    pub fn apply(&self, body: &str) -> String {
        let label = match self.status {
            CommentStatus::Received => "👀 Ezra: received",
            CommentStatus::Delivered => "✅ Ezra: delivered to chat",
            CommentStatus::Unconfirmed => "👀 Ezra: delivery unconfirmed",
            CommentStatus::Failed => "❌ Ezra: could not deliver",
        };
        let mut updated = Self::strip(body).into_owned();
        updated.push_str(FOOTER_START);
        updated.push_str("<sub>");
        updated.push_str(label);
        if let Some(chat_name) = self.chat_name.filter(|name| !name.trim().is_empty()) {
            updated.push_str(" · ");
            for character in chat_name[..chat_name.floor_char_boundary(512)].chars() {
                match character {
                    '&' => updated.push_str("&amp;"),
                    '<' => updated.push_str("&lt;"),
                    '>' => updated.push_str("&gt;"),
                    character if character.is_control() => updated.push(' '),
                    character => updated.push(character),
                }
            }
        }
        updated.push_str("</sub>");
        updated.push_str(FOOTER_END);
        updated
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_changes_preserve_the_request_and_do_not_accumulate_footers() {
        let request = "/ezra investigate\n\n```rust\nrun();\n```\n🦦\t\n";
        let mut body = request.to_owned();
        for status in [
            CommentStatus::Received,
            CommentStatus::Unconfirmed,
            CommentStatus::Delivered,
            CommentStatus::Failed,
        ] {
            let footer = StatusFooter {
                status,
                chat_name: Some("owner/repo#42: Fix the crash"),
            };
            body = footer.apply(&body);
            assert_eq!(StatusFooter::strip(&body), request);
            assert_eq!(body.matches(FOOTER_START).count(), 1);
            assert_eq!(footer.apply(&body), body);
            assert!(body.contains("owner/repo#42: Fix the crash"));
        }
    }

    #[test]
    fn text_added_after_a_footer_survives_when_the_footer_moves_to_the_bottom() {
        let footer = StatusFooter {
            status: CommentStatus::Received,
            chat_name: None,
        };
        let body = format!(
            "{}\n\nAlso check the other case.",
            footer.apply("/ezra fix it")
        );
        let updated = footer.apply(&body);
        assert_eq!(
            StatusFooter::strip(&updated),
            "/ezra fix it\n\nAlso check the other case."
        );
        assert!(updated.ends_with(FOOTER_END));
        assert_eq!(updated.matches(FOOTER_START).count(), 1);
    }

    #[test]
    fn malformed_marker_blocks_are_preserved() {
        for body in [
            format!("/ezra{FOOTER_START}user text{FOOTER_END}"),
            format!("/ezra{FOOTER_START}<sub>unfinished"),
            format!("/ezra{FOOTER_START}<sub>user\ntext</sub>{FOOTER_END}"),
        ] {
            assert_eq!(StatusFooter::strip(&body), body);
        }
    }

    #[test]
    fn chat_names_cannot_inject_html_or_unbounded_content() {
        let chat_name = format!("<img src=x> &\n{}", "🦦".repeat(1000));
        let body = StatusFooter {
            status: CommentStatus::Delivered,
            chat_name: Some(&chat_name),
        }
        .apply("/ezra");
        assert!(body.contains("&lt;img src=x&gt; &amp; "));
        assert!(!body.contains("<img"));
        assert!(body.len() < 4096);
        assert_eq!(StatusFooter::strip(&body), "/ezra");
    }
}
