use std::collections::HashMap;
use std::collections::hash_map::Entry;

use super::control::{ThreadId, ThreadStatusWire, ThreadWire, UnsubscribeStatus};

/// The chats a Codex server has loaded, and whether ezra's connection still follows each.
#[derive(Debug, Default)]
pub struct Chats {
    loaded: HashMap<ThreadId, Chat>,
    /// Codex answered the list of loaded chats.
    listed: bool,
}

#[derive(Debug, Default)]
struct Chat {
    /// Absent until Codex says.
    status: Option<ThreadStatusWire>,
    /// Codex started it while ezra was connected or asked ezra about it, so ezra follows it or
    /// soon will.
    followed: bool,
    leaving: Leaving,
}

/// Where ezra is in unsubscribing from a chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Leaving {
    /// On the chat's next status change.
    #[default]
    Due,
    Asked,
    Done,
}

impl Chat {
    /// Asks to unsubscribe unless that is on its way.
    fn leave(&mut self, id: ThreadId) -> Option<ThreadId> {
        (self.leaving != Leaving::Asked).then(|| {
            self.leaving = Leaving::Asked;
            id
        })
    }
}

impl Chats {
    /// Adds the chats Codex listed that are not known yet, and returns them to read.
    pub fn listed(&mut self, ids: Vec<ThreadId>) -> Vec<ThreadId> {
        self.listed = true;
        let mut unread = Vec::new();
        for id in ids {
            if let Entry::Vacant(entry) = self.loaded.entry(id) {
                unread.push(entry.key().clone());
                entry.insert(Chat::default());
            }
        }
        unread
    }

    /// A chat started. Returns the chat to unsubscribe from.
    pub fn started(&mut self, thread: ThreadWire) -> Option<ThreadId> {
        let chat = self.loaded.entry(thread.id.clone()).or_default();
        chat.status = Some(thread.status);
        chat.followed = true;
        chat.leave(thread.id)
    }

    /// Returns the chat to unsubscribe from when a try is due: on its first change, and after a
    /// try that did not settle it.
    pub fn changed(&mut self, id: ThreadId, status: ThreadStatusWire) -> Option<ThreadId> {
        if status == ThreadStatusWire::NotLoaded {
            self.loaded.remove(&id);
            return None;
        }
        let chat = self.loaded.entry(id.clone()).or_default();
        chat.status = Some(status);
        if chat.leaving == Leaving::Due {
            chat.leave(id)
        } else {
            None
        }
    }

    /// Takes a status read while none arrived since.
    pub fn read(&mut self, thread: ThreadWire) {
        let Some(chat) = self.loaded.get_mut(&thread.id) else {
            return;
        };
        if chat.status.is_some() {
            return;
        }
        if thread.status == ThreadStatusWire::NotLoaded {
            self.loaded.remove(&thread.id);
        } else {
            chat.status = Some(thread.status);
        }
    }

    pub fn closed(&mut self, id: &ThreadId) {
        self.loaded.remove(id);
    }

    /// Codex asked ezra something about a chat, so ezra follows it. Returns the chat to
    /// unsubscribe from.
    pub fn asked_about(&mut self, id: ThreadId) -> Option<ThreadId> {
        let chat = self.loaded.entry(id.clone()).or_default();
        chat.followed = true;
        chat.leave(id)
    }

    /// Codex answered an unsubscribe with `status`, absent when it failed.
    pub fn left(&mut self, id: &ThreadId, status: Option<UnsubscribeStatus>) {
        let Some(chat) = self.loaded.get_mut(id) else {
            return;
        };
        chat.leaving = match status {
            Some(UnsubscribeStatus::Unsubscribed | UnsubscribeStatus::NotLoaded) => Leaving::Done,
            Some(UnsubscribeStatus::NotSubscribed) if !chat.followed => Leaving::Done,
            Some(UnsubscribeStatus::NotSubscribed) | None => Leaving::Due,
        };
    }

    pub fn count(&self) -> u32 {
        u32::try_from(self.loaded.len()).unwrap_or(u32::MAX)
    }

    /// Chats running a turn or waiting for an answer, and chats whose state is not known yet.
    pub fn running(&self) -> u32 {
        let running = self
            .loaded
            .values()
            .filter(|chat| {
                chat.status.is_none_or(|status| {
                    matches!(status, ThreadStatusWire::Active | ThreadStatusWire::Unknown)
                })
            })
            .count();
        u32::try_from(running).unwrap_or(u32::MAX)
    }

    /// Whether a chat runs or may run, which is so until Codex listed its chats.
    pub fn are_busy(&self) -> bool {
        !self.listed || self.running() > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str) -> ThreadId {
        ThreadId(name.to_owned())
    }

    fn thread(name: &str, status: ThreadStatusWire) -> ThreadWire {
        ThreadWire {
            id: id(name),
            status,
        }
    }

    fn counts(chats: &Chats) -> (u32, u32) {
        (chats.count(), chats.running())
    }

    #[test]
    fn listed_chats_count_as_running_until_read() {
        let mut chats = Chats::default();
        assert_eq!(chats.listed(vec![id("a"), id("b")]), [id("a"), id("b")]);
        assert_eq!(counts(&chats), (2, 2));

        chats.read(thread("a", ThreadStatusWire::Active));
        chats.read(thread("b", ThreadStatusWire::Idle));
        assert_eq!(counts(&chats), (2, 1));

        assert_eq!(chats.listed(vec![id("a"), id("c")]), [id("c")]);
        assert_eq!(counts(&chats), (3, 2));
    }

    #[test]
    fn chats_are_busy_until_listed_and_while_one_runs() {
        let mut chats = Chats::default();
        chats.started(thread("a", ThreadStatusWire::Idle));
        assert!(chats.are_busy());

        chats.listed(Vec::new());
        assert!(!chats.are_busy());

        chats.changed(id("a"), ThreadStatusWire::Active);
        assert!(chats.are_busy());
        chats.changed(id("a"), ThreadStatusWire::Idle);
        assert!(!chats.are_busy());
        chats.listed(vec![id("b")]);
        assert!(chats.are_busy());
    }

    #[test]
    fn a_status_codex_sent_wins_over_a_read() {
        let mut chats = Chats::default();
        chats.listed(vec![id("a")]);
        chats.changed(id("a"), ThreadStatusWire::Idle);

        chats.read(thread("a", ThreadStatusWire::Active));

        assert_eq!(counts(&chats), (1, 0));
    }

    #[test]
    fn unloaded_and_closed_chats_are_forgotten() {
        let mut chats = Chats::default();
        chats.listed(vec![id("a"), id("b"), id("c")]);
        chats.read(thread("a", ThreadStatusWire::NotLoaded));
        chats.changed(id("b"), ThreadStatusWire::NotLoaded);
        assert_eq!(counts(&chats), (1, 1));

        chats.closed(&id("c"));
        assert_eq!(counts(&chats), (0, 0));
        chats.read(thread("c", ThreadStatusWire::Active));
        assert_eq!(counts(&chats), (0, 0));
    }

    #[test]
    fn errored_and_idle_chats_are_not_running() {
        let mut chats = Chats::default();
        chats.started(thread("a", ThreadStatusWire::SystemError));
        chats.started(thread("b", ThreadStatusWire::Idle));
        chats.started(thread("c", ThreadStatusWire::Active));

        assert_eq!(counts(&chats), (3, 1));
    }

    #[test]
    fn a_started_chat_is_left_once() {
        for answer in [
            UnsubscribeStatus::Unsubscribed,
            UnsubscribeStatus::NotLoaded,
        ] {
            let mut chats = Chats::default();
            assert_eq!(
                chats.started(thread("a", ThreadStatusWire::Idle)),
                Some(id("a"))
            );
            assert_eq!(chats.changed(id("a"), ThreadStatusWire::Active), None);

            chats.left(&id("a"), Some(answer));
            assert_eq!(
                chats.changed(id("a"), ThreadStatusWire::Idle),
                None,
                "{answer:?}"
            );
            assert_eq!(chats.asked_about(id("a")), Some(id("a")), "{answer:?}");
            assert_eq!(counts(&chats), (1, 0));
        }
    }

    #[test]
    fn a_started_chat_is_left_again_on_its_next_change_until_codex_settles_it() {
        let mut chats = Chats::default();
        assert_eq!(
            chats.started(thread("a", ThreadStatusWire::Idle)),
            Some(id("a"))
        );

        chats.left(&id("a"), Some(UnsubscribeStatus::NotSubscribed));
        assert_eq!(
            chats.changed(id("a"), ThreadStatusWire::Active),
            Some(id("a"))
        );
        chats.left(&id("a"), Some(UnsubscribeStatus::NotSubscribed));
        assert_eq!(
            chats.changed(id("a"), ThreadStatusWire::Idle),
            Some(id("a"))
        );

        chats.left(&id("a"), Some(UnsubscribeStatus::Unsubscribed));
        assert_eq!(chats.changed(id("a"), ThreadStatusWire::Active), None);
    }

    #[test]
    fn a_listed_chat_is_left_once_on_its_first_change() {
        let mut chats = Chats::default();
        chats.listed(vec![id("a")]);

        assert_eq!(
            chats.changed(id("a"), ThreadStatusWire::Active),
            Some(id("a"))
        );
        chats.left(&id("a"), Some(UnsubscribeStatus::NotSubscribed));

        assert_eq!(chats.changed(id("a"), ThreadStatusWire::Idle), None);
    }

    #[test]
    fn a_failed_or_unloaded_unsubscribe_is_settled_on_the_next_change() {
        let mut chats = Chats::default();
        chats.started(thread("a", ThreadStatusWire::Idle));
        chats.left(&id("a"), None);
        assert_eq!(
            chats.changed(id("a"), ThreadStatusWire::Active),
            Some(id("a"))
        );

        chats.left(&id("a"), Some(UnsubscribeStatus::NotLoaded));
        assert_eq!(chats.changed(id("a"), ThreadStatusWire::Idle), None);
    }

    #[test]
    fn codex_asking_about_a_chat_leaves_it_unless_already_asked() {
        let mut chats = Chats::default();
        chats.listed(vec![id("a")]);
        chats.changed(id("a"), ThreadStatusWire::Active);
        chats.left(&id("a"), Some(UnsubscribeStatus::NotSubscribed));

        assert_eq!(chats.asked_about(id("a")), Some(id("a")));
        assert_eq!(chats.asked_about(id("a")), None);
        chats.left(&id("a"), Some(UnsubscribeStatus::NotSubscribed));
        assert_eq!(
            chats.changed(id("a"), ThreadStatusWire::Idle),
            Some(id("a"))
        );

        assert_eq!(chats.asked_about(id("b")), Some(id("b")));
        assert_eq!(counts(&chats), (2, 1));
    }
}
