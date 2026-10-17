//! Navigate semantic answers, retaining source anchors across reflow and history pagination.

use super::*;
use crate::history_cell::UserHistoryCell;

impl TranscriptView {
    pub(super) fn jump_answer(&mut self, cells: &[Arc<dyn HistoryCell>], newer: bool) {
        let answer_turn = self.answer_turn;
        self.cancel_search();
        self.cancel_beginning();
        self.answer_turn = answer_turn;
        self.selection = None;
        self.pending_previous_answer = false;

        // A turn can contain several streamed chunks and tool calls. Only its first answer
        // counts; a prompt with no answer contributes no destination.
        let mut awaiting_answer = None;
        let mut answers = Vec::new();
        for (index, cell) in cells.iter().enumerate() {
            if cell.as_any().is::<UserHistoryCell>() {
                awaiting_answer = Some(EntryKey::cell(cell));
            } else if let Some(prompt) = awaiting_answer
                && cell.is_assistant_response()
            {
                answers.push((prompt, index));
                awaiting_answer = None;
            }
        }
        if let Some(prompt) = awaiting_answer
            && self
                .live_key
                .is_some_and(|(_, key)| key.is_assistant_response)
            && self
                .live
                .as_ref()
                .is_some_and(|layout| layout.row_count() > 0)
        {
            answers.push((prompt, cells.len()));
        }
        // A live tail can commit or consolidate between key presses. Its prompt identity
        // still names the same turn even when the answer cell identity changes.
        let current = self
            .answer_turn
            .and_then(|turn| {
                answers
                    .iter()
                    .find_map(|(prompt, index)| (*prompt == turn).then_some(*index))
            })
            .or_else(|| match self.position {
                Position::Latest => None,
                Position::Reading(anchor) => Some(self.resolve(cells, anchor)),
            });
        let target = if newer {
            current.and_then(|current| answers.into_iter().find(|(_, index)| *index > current))
        } else {
            answers
                .into_iter()
                .rev()
                .find(|(_, index)| current.is_none_or(|current| *index < current))
        };
        if let Some((prompt, index)) = target {
            self.jump_to_entry(cells, index);
            self.answer_turn = Some(prompt);
            if index == cells.len() {
                self.scroll(cells, /*rows*/ 0);
            }
        } else if !newer && self.history.has_unloaded_history() {
            // Keep the current anchor while the app fetches pages, including pages with no
            // answers. Retrying from the same source anchor skips no turns at a page boundary.
            self.pending_previous_answer = true;
        }
    }

    pub(super) fn resume_answer_navigation(&mut self, cells: &[Arc<dyn HistoryCell>]) {
        if self.pending_previous_answer {
            self.jump_answer(cells, false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history_cell::AgentMessageCell;
    use crate::history_cell::new_user_prompt;
    use crate::transcript_view::tests::render;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyModifiers;

    fn prompt() -> Arc<dyn HistoryCell> {
        Arc::new(new_user_prompt("question".into(), vec![], vec![], vec![]))
    }

    fn answer(text: &str, first: bool) -> Arc<dyn HistoryCell> {
        Arc::new(AgentMessageCell::new(
            text.lines().map(|line| line.to_owned().into()).collect(),
            first,
        ))
    }

    fn press(view: &mut TranscriptView, cells: &[Arc<dyn HistoryCell>], code: KeyCode) {
        assert!(
            view.handle_key(KeyEvent::new(code, KeyModifiers::ALT), cells)
                .is_some()
        );
    }

    fn at(view: &TranscriptView, cells: &[Arc<dyn HistoryCell>], index: usize) {
        let Position::Reading(anchor) = view.position else {
            panic!("expected reading anchor")
        };
        assert_eq!(view.resolve(cells, anchor), index);
        assert_eq!(anchor.offset, 0);
    }

    #[test]
    fn answer_navigation_skips_chunks_tools_and_unanswered_prompts() {
        let cells = vec![
            prompt(),
            answer("first answer\nsecond line", true),
            answer("continued", false),
            prompt(),
            prompt(),
            Arc::new(crate::history_cell::PlainHistoryCell::new(vec![
                "tool".into(),
            ])) as Arc<dyn HistoryCell>,
            answer(&"long answer ".repeat(80), true),
            prompt(),
        ];
        let mut view = TranscriptView::default();
        render(&mut view, &cells, 40, 8);
        press(&mut view, &cells, KeyCode::Up);
        at(&view, &cells, 6);
        render(&mut view, &cells, 20, 5);
        at(&view, &cells, 6);
        press(&mut view, &cells, KeyCode::Up);
        at(&view, &cells, 1);
        press(&mut view, &cells, KeyCode::Up);
        at(&view, &cells, 1);
        press(&mut view, &cells, KeyCode::Down);
        at(&view, &cells, 6);
        press(&mut view, &cells, KeyCode::Down);
        at(&view, &cells, 6);
        view.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL), &cells);
        assert!(view.is_following());
        press(&mut view, &cells, KeyCode::Up);
        at(&view, &cells, 6);
    }

    #[test]
    fn previous_answer_waits_across_pages_and_latest_cancels_pending_jump() {
        let mut cells = vec![prompt(), answer("new answer", true)];
        let mut view = TranscriptView::default();
        view.history = TranscriptHistoryState::Partial;
        press(&mut view, &cells, KeyCode::Up);
        press(&mut view, &cells, KeyCode::Up);
        assert!(view.pending_previous_answer);
        cells.insert(0, prompt()); // A page without an answer must not consume the request.
        render(&mut view, &cells, 40, 8);
        assert!(view.pending_previous_answer);
        cells.splice(0..0, [prompt(), answer("older answer", true)]);
        render(&mut view, &cells, 40, 8);
        at(&view, &cells, 1);
        assert!(!view.pending_previous_answer);
        press(&mut view, &cells, KeyCode::Up);
        view.jump_to_latest();
        render(&mut view, &cells, 40, 8);
        assert!(view.is_following());
    }

    #[test]
    fn live_answer_stays_in_its_turn_when_committed_and_consolidated() {
        let mut cells = vec![prompt(), answer("old answer", true), prompt()];
        let mut view = TranscriptView::default();
        let key = ActiveCellTranscriptKey {
            cacheable: true,
            revision: 1,
            is_stream_continuation: false,
            is_assistant_response: true,
            animation_tick: None,
        };
        view.sync_live_tail(40, Some(key), |_| Some(vec!["streaming answer".into()]));
        render(&mut view, &cells, 40, 8);
        press(&mut view, &cells, KeyCode::Up);
        at(&view, &cells, 3);
        render(&mut view, &cells, 40, 8);
        view.sync_live_tail(
            40,
            Some(ActiveCellTranscriptKey { revision: 2, ..key }),
            |_| Some(vec!["streaming answer grows".into()]),
        );
        render(&mut view, &cells, 20, 5);
        at(&view, &cells, 3);
        cells.push(answer("committed answer", true));
        view.sync_live_tail(20, None, |_| None);
        render(&mut view, &cells, 20, 5);
        at(&view, &cells, 3);
        press(&mut view, &cells, KeyCode::Up);
        at(&view, &cells, 1);
        press(&mut view, &cells, KeyCode::Down);
        at(&view, &cells, 3);
        let replacement: Arc<dyn HistoryCell> =
            Arc::new(crate::history_cell::AgentMarkdownCell::new(
                "consolidated answer".into(),
                std::path::Path::new("/tmp"),
            ));
        view.replace_range(&cells, 3..4, &replacement);
        cells[3] = replacement;
        press(&mut view, &cells, KeyCode::Up);
        at(&view, &cells, 1);
    }

    #[test]
    fn tools_are_not_live_answers_and_releases_do_not_navigate() {
        let cells = vec![prompt()];
        let mut view = TranscriptView::default();
        let key = ActiveCellTranscriptKey {
            cacheable: true,
            revision: 1,
            is_stream_continuation: false,
            is_assistant_response: false,
            animation_tick: None,
        };
        view.sync_live_tail(40, Some(key), |_| Some(vec!["running tool".into()]));
        press(&mut view, &cells, KeyCode::Up);
        assert!(view.is_following());
        let release = KeyEvent::new_with_kind(
            KeyCode::Up,
            KeyModifiers::ALT,
            crossterm::event::KeyEventKind::Release,
        );
        assert!(view.handle_key(release, &cells).is_none());
        assert!(view.is_following());
    }

    #[test]
    fn page_boundary_requires_the_prompt_before_accepting_a_continuation() {
        let mut cells = vec![
            answer("partial older turn", false),
            prompt(),
            answer("new answer", true),
        ];
        let mut view = TranscriptView::default();
        view.history = TranscriptHistoryState::Partial;
        press(&mut view, &cells, KeyCode::Up);
        press(&mut view, &cells, KeyCode::Up);
        assert!(view.pending_previous_answer);
        cells.splice(0..0, [prompt(), answer("older turn begins", true)]);
        view.history_loaded(&cells, 0..2);
        at(&view, &cells, 1);
        assert!(!view.pending_previous_answer);
    }
}
