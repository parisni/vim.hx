use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

use crate::commands::*;

use helix_core::command_line::Args;
use helix_core::graphemes::{
    nth_next_grapheme_boundary, nth_prev_grapheme_boundary, prev_grapheme_boundary,
};
use helix_core::line_ending::rope_is_line_ending;
use helix_core::{textobject, Range, RopeSlice, Selection, Transaction};
use helix_view::{document::Mode, editor::ConfigEvent, DocumentId};

#[derive(Default)]
pub struct AtomicState {
    visual_lines: AtomicBool,
    gv_visual_lines: AtomicBool,
    visual_block: AtomicBool,
    gv_visual_block: AtomicBool,
    highlight: AtomicBool,
    vim_paste: AtomicBool,
    cmd_hook_enabled: AtomicBool,
    vim_mode: AtomicBool,
    vim_pend_enable: AtomicBool,
    gv_selection: Mutex<Option<(Selection, DocumentId)>>,
}

pub static VIM_STATE: AtomicState = AtomicState::new();

impl AtomicState {
    pub const fn new() -> Self {
        Self {
            visual_lines: AtomicBool::new(false),
            gv_visual_lines: AtomicBool::new(false),
            visual_block: AtomicBool::new(false),
            gv_visual_block: AtomicBool::new(false),
            highlight: AtomicBool::new(false),
            vim_paste: AtomicBool::new(false),
            cmd_hook_enabled: AtomicBool::new(true),
            vim_mode: AtomicBool::new(true),
            vim_pend_enable: AtomicBool::new(false),
            gv_selection: Mutex::new(None),
        }
    }

    fn save_current_selection(&self, cx: &mut Context) {
        let (view, doc) = current!(cx.editor);
        let selection = doc.selection(view.id);

        self.set_gv_selection(selection.clone(), doc.id());
    }

    pub fn set_gv_selection(&self, sel: Selection, id: DocumentId) {
        let mut lock = self.gv_selection.lock().unwrap();
        *lock = Some((sel, id));

        self.gv_visual_lines
            .store(self.is_visual_line(), Ordering::Relaxed);
        self.gv_visual_block
            .store(self.is_visual_block(), Ordering::Relaxed);
    }

    pub fn get_gv_selection(&self) -> Option<(Selection, DocumentId)> {
        let is_gv_lines = self.gv_visual_lines.load(Ordering::Relaxed);
        let is_gv_block = self.gv_visual_block.load(Ordering::Relaxed);

        self.visual_lines.store(is_gv_lines, Ordering::Relaxed);
        self.visual_block.store(is_gv_block, Ordering::Relaxed);

        let lock = self.gv_selection.lock().unwrap();
        lock.clone()
    }

    pub fn visual_line(&self) {
        self.visual_lines.store(true, Ordering::Relaxed);
    }

    pub fn exit_visual_line(&self) {
        self.visual_lines.store(false, Ordering::Relaxed);
    }

    pub fn is_visual_line(&self) -> bool {
        self.visual_lines.load(Ordering::Relaxed)
    }

    pub fn visual_block(&self) {
        self.visual_block.store(true, Ordering::Relaxed);
    }

    pub fn exit_visual_block(&self) {
        self.visual_block.store(false, Ordering::Relaxed);
    }

    pub fn exit_visual_modes(&self) {
        self.exit_visual_line();
        self.exit_visual_block();
    }

    pub fn is_visual_block(&self) -> bool {
        self.visual_block.load(Ordering::Relaxed)
    }

    pub fn allow_highlight(&self) {
        self.highlight.store(true, Ordering::Relaxed);
    }

    pub fn reset_highlight(&self) {
        self.highlight.store(false, Ordering::Relaxed);
    }

    pub fn is_highlight_allowed(&self) -> bool {
        self.highlight.load(Ordering::Relaxed)
    }

    pub fn is_vim_paste(&self) -> bool {
        self.vim_paste.load(Ordering::Relaxed)
    }

    pub fn set_vim_paste(&self, val: bool) {
        self.vim_paste.store(val, Ordering::Relaxed);
    }

    pub fn is_cmd_hook_enabled(&self) -> bool {
        self.cmd_hook_enabled.load(Ordering::Relaxed)
    }

    pub fn set_cmd_hook_enabled(&self, val: bool) {
        self.cmd_hook_enabled.store(val, Ordering::Relaxed);
    }

    pub fn set_vim_mode(&self, val: bool) {
        self.vim_mode.store(val, Ordering::Relaxed);
        self.set_cmd_hook_enabled(val);
    }

    pub fn is_vim_mode(&self) -> bool {
        self.vim_mode.load(Ordering::Relaxed)
    }

    pub fn is_vim_pend_enable(&self) -> bool {
        self.vim_pend_enable.load(Ordering::Relaxed)
    }

    pub fn set_vim_pend_enable(&self, val: bool) {
        self.vim_pend_enable.store(val, Ordering::Relaxed);
    }
}

pub mod vim_hx_hooks {
    use super::*;

    pub fn hook_after_each_command(cx: &mut Context, cmd: &MappableCommand) {
        if !matches!(
            cmd,
            MappableCommand::Static { .. } | MappableCommand::Macro { .. }
        ) {
            return;
        }

        if !VIM_STATE.is_cmd_hook_enabled() {
            if VIM_STATE.is_vim_pend_enable() {
                VIM_STATE.set_cmd_hook_enabled(true);
                VIM_STATE.set_vim_pend_enable(false);
            }
            return;
        }
        match cx.editor.mode {
            Mode::Select => {
                // check if visual lines
                if VIM_STATE.is_visual_line() {
                    extend_to_line_bounds(cx);
                }

                VIM_STATE.save_current_selection(cx);
            }
            Mode::Normal => {
                if VIM_STATE.is_highlight_allowed() {
                    VIM_STATE.reset_highlight();
                } else {
                    collapse_selection(cx);
                }
            }
            _ => (),
        };
    }

    pub fn after_paste_start_pos(text: &Rope, range: &Range) -> usize {
        // A hacky method to switch between helix/vim paste
        if !VIM_STATE.is_vim_paste() {
            return range.to();
        }

        let slice = text.slice(..);
        let line = range.cursor_line(slice);
        if range.len() <= 1
            && prev_grapheme_boundary(slice, slice.line_to_char(line + 1)) == range.cursor(slice)
        {
            range.from()
        } else {
            range.to()
        }
    }
}

macro_rules! wrap_hooks {
    // with both before and after
    ($wrapper:ident, $func:path, before = $before:expr, after = $after:expr) => {
        pub fn $wrapper(cx: &mut Context) {
            $before(cx);
            $func(cx);
            $after(cx);
        }
    };

    // with only before
    ($wrapper:ident, $func:path, before = $before:expr) => {
        pub fn $wrapper(cx: &mut Context) {
            $before(cx);
            $func(cx);
        }
    };

    // with only after
    ($wrapper:ident, $func:path, after = $after:expr) => {
        pub fn $wrapper(cx: &mut Context) {
            $func(cx);
            $after(cx);
        }
    };
}

macro_rules! wrap_many_with_hooks {
    (
        [ $( ( $wrapper:ident, $func:path ) ),+ $(,)? ],
        before = $before:expr,
        after = $after:expr
    ) => {
        $(
            wrap_hooks!($wrapper, $func, before = $before, after = $after);
        )+
    };

    (
        [ $( ( $wrapper:ident, $func:path ) ),+ $(,)? ],
        before = $before:expr
    ) => {
        $(
            wrap_hooks!($wrapper, $func, before = $before);
        )+
    };

    (
        [ $( ( $wrapper:ident, $func:path ) ),+ $(,)? ],
        after = $after:expr
    ) => {
        $(
            wrap_hooks!($wrapper, $func, after = $after);
        )+
    };
}

#[macro_export]
macro_rules! static_commands_with_default {
    ($macro_to_call:ident! ( $($name:ident, $doc:literal,)* )) => {
        $macro_to_call! {
        vim_visual_lines, "Visual lines (vim)",
        vim_normal_mode, "Normal mode (vim)",
        vim_exit_select_mode, "Exit select mode (vim)",
        vim_move_next_word_start, "Move to start of next word (vim)",
        vim_move_next_long_word_start, "Move next long word (vim)",
        vim_extend_next_word_start, "Extend to start of next word (vim)",
        vim_extend_next_long_word_start, "Extend to start of next long word (vim)",
        vim_extend_visual_line_up, "Move up (vim)",
        vim_extend_visual_line_down, "Move down (vim)",
        vim_goto_last_line, "Go to line (vim)",
        vim_move_paragraph_forward, "Move by paragraph forward (vim)",
        vim_move_paragraph_backward, "Move by paragraph forward (vim)",
        vim_cursor_forward_search, "Forward search for word near cursor (vim)",
        vim_cursor_backward_search, "Backward search for word near cursor (vim)",
        vim_search, "Search for regex pattern (vim)",
        vim_rsearch, "Reverse search for regex pattern (vim)",
        vim_search_next, "Select next search match (vim)",
        vim_search_prev, "Select previous search match (vim)",
        vim_match_brackets, "Goto matching bracket (vim)",
        vim_delete, "Delete operator (vim)",
        vim_change, "Change operator (vim)",
        vim_yank, "Change operator (vim)",
        vim_yank_to_clipboard, "Change operator (vim)",
        vim_delete_till_line_end, "Delete till line end (vim)",
        vim_delete_any_selection, "Delete any Helix selection, `x` (vim)",
        vim_restore_last_selection, "Restore last visual-mode selection (vim)",
        vim_find_till_char, "Move till next occurrence of char (vim)",
        vim_find_next_char, "Move to next occurrence of char (vim)",
        vim_till_prev_char, "Move till previous occurrence of char (vim)",
        vim_find_prev_char, "Move to previous occurrence of char (vim)",
        vim_append, "Append text after the cursor (vim)",
        vim_select_mode, "Enter selection extend mode (vim)",
        vim_paste_after, "Paste after selection (vim)",
        vim_paste_before, "Paste before selection (vim)",
        vim_paste_clipboard_after, "Paste clipboard after selections (vim)",
        vim_paste_clipboard_before, "Paste clipboard before selections (vim)",
        vim_move_char_left, "Move left (vim)",
        vim_move_char_right, "Move right (vim)",
        vim_select_regex, "Select all regex matches inside selections (vim.hx)",
        vim_select_all, "Select all in both normal and select mode (vim.hx)",
        vim_cmd_off, "Allow Helix commands to run as intended (vim.hx)",
        vim_cmd_on, "End vim_cmd_off, only works in Vim mode (vim.hx)",
        vim_visual_block, "Enter visual block mode (vim)",
        vim_insert_at_line_start, "Insert at start of line except in visual block mode (vim)",
            $($name, $doc,)*
        }
    };
}

pub mod vim_typed_commands {
    use super::*;

    pub fn vim_toggle(
        cx: &mut compositor::Context,
        _args: Args,
        _event: PromptEvent,
    ) -> anyhow::Result<()> {
        VIM_STATE.set_vim_mode(!VIM_STATE.is_vim_mode());

        // The rest is copy/paste from typed::refresh_config
        cx.editor.config_events.0.send(ConfigEvent::Refresh)?;
        Ok(())
    }

    pub fn vim_enable(
        cx: &mut compositor::Context,
        _args: Args,
        _event: PromptEvent,
    ) -> anyhow::Result<()> {
        VIM_STATE.set_vim_mode(true);

        // The rest is copy/paste from typed::refresh_config
        cx.editor.config_events.0.send(ConfigEvent::Refresh)?;
        Ok(())
    }

    pub fn vim_disable(
        cx: &mut compositor::Context,
        _args: Args,
        _event: PromptEvent,
    ) -> anyhow::Result<()> {
        VIM_STATE.set_vim_mode(false);

        // The rest is copy/paste from typed::refresh_config
        cx.editor.config_events.0.send(ConfigEvent::Refresh)?;
        Ok(())
    }
}

pub use vim_commands::*;

mod vim_commands {
    use super::*;

    pub fn vim_visual_lines(cx: &mut Context) {
        select_mode(cx);
        VIM_STATE.visual_line();
        extend_to_line_bounds(cx);
    }

    pub fn vim_visual_block(cx: &mut Context) {
        vim_select_mode(cx);
        VIM_STATE.visual_block();
    }

    pub fn vim_insert_at_line_start(cx: &mut Context) {
        if VIM_STATE.is_visual_block() {
            // Helix multi-cursor outweigh exact Vim behaviour
            // to match Vim behavior mask the following line, maybe through configs
            collapse_selection(cx);

            normal_mode(cx);
            insert_mode(cx);
        } else {
            insert_at_line_start(cx);
        }
    }

    wrap_many_with_hooks!(
        [
            (vim_move_next_word_start, move_next_word_start),
            (vim_move_next_long_word_start, move_next_long_word_start),
        ],
        after = move_char_right
    );

    wrap_many_with_hooks!(
        [
            (vim_extend_next_word_start, extend_next_word_start),
            (vim_extend_next_long_word_start, extend_next_long_word_start),
        ],
        after = extend_char_right
    );

    pub fn vim_goto_last_line(cx: &mut Context) {
        if cx.count.is_none() {
            if cx.editor.mode == Mode::Select {
                extend_to_last_line(cx)
            } else {
                goto_last_line(cx);
            }
        } else {
            // Works the same as gg
            if cx.editor.mode == Mode::Select {
                extend_to_file_start(cx);
            } else {
                goto_file_start(cx);
            }
        }
    }

    pub fn vim_extend_visual_line_down(cx: &mut Context) {
        if VIM_STATE.is_visual_line() {
            extend_line_down(cx);
        } else if VIM_STATE.is_visual_block() {
            vim_utils::visual_block_impl(cx, Direction::Forward)
        } else {
            extend_visual_line_down(cx);
        }
    }

    pub fn vim_extend_visual_line_up(cx: &mut Context) {
        if VIM_STATE.is_visual_line() {
            extend_line_up(cx);
        } else if VIM_STATE.is_visual_block() {
            vim_utils::visual_block_impl(cx, Direction::Backward)
        } else {
            extend_visual_line_up(cx);
        }
    }

    pub fn vim_normal_mode(cx: &mut Context) {
        if cx.editor.mode == Mode::Select {
            VIM_STATE.save_current_selection(cx);
        }

        normal_mode(cx);

        if VIM_STATE.is_visual_block() {
            keep_primary_selection(cx);
        }

        VIM_STATE.exit_visual_modes();
    }

    pub fn vim_exit_select_mode(cx: &mut Context) {
        if cx.editor.mode == Mode::Select {
            VIM_STATE.save_current_selection(cx);
        }

        if VIM_STATE.is_visual_block() {
            keep_primary_selection(cx);
        }

        exit_select_mode(cx);

        VIM_STATE.exit_visual_modes();
    }

    pub fn vim_move_paragraph_forward(cx: &mut Context) {
        vim_utils::vim_save_to_jumplist(cx);
        goto_para_impl(cx, vim_utils::movement_paragraph_forward);
        if cx.editor.mode != Mode::Select {
            normal_mode(cx);
        }
    }

    pub fn vim_move_paragraph_backward(cx: &mut Context) {
        vim_utils::vim_save_to_jumplist(cx);
        goto_para_impl(cx, vim_utils::movement_paragraph_backward);
        if cx.editor.mode != Mode::Select {
            normal_mode(cx);
        }
    }

    pub fn vim_cursor_forward_search(cx: &mut Context) {
        vim_utils::vim_save_to_jumplist(cx);
        VIM_STATE.allow_highlight();
        vim_utils::cursor_search_impl(cx, Direction::Forward);
    }

    pub fn vim_cursor_backward_search(cx: &mut Context) {
        vim_utils::vim_save_to_jumplist(cx);
        VIM_STATE.allow_highlight();
        vim_utils::cursor_search_impl(cx, Direction::Backward);
    }

    pub fn vim_search(cx: &mut Context) {
        vim_utils::vim_save_to_jumplist(cx);
        VIM_STATE.allow_highlight();
        search(cx);
    }

    pub fn vim_rsearch(cx: &mut Context) {
        vim_utils::vim_save_to_jumplist(cx);
        VIM_STATE.allow_highlight();
        rsearch(cx);
    }

    pub fn vim_search_next(cx: &mut Context) {
        vim_utils::vim_save_to_jumplist(cx);
        VIM_STATE.allow_highlight();
        search_next(cx);
    }

    pub fn vim_search_prev(cx: &mut Context) {
        vim_utils::vim_save_to_jumplist(cx);
        VIM_STATE.allow_highlight();
        search_prev(cx);
    }

    pub fn vim_match_brackets(cx: &mut Context) {
        vim_utils::vim_save_to_jumplist(cx);
        match_brackets(cx);
    }

    pub fn vim_delete(cx: &mut Context) {
        VimOpCtx::operator_impl(cx, VimOp::Delete, cx.register);
    }

    pub fn vim_yank(cx: &mut Context) {
        VimOpCtx::operator_impl(cx, VimOp::Yank, cx.register);
    }

    pub fn vim_yank_to_clipboard(cx: &mut Context) {
        VimOpCtx::operator_impl(cx, VimOp::Yank, Some('+'));
    }

    pub fn vim_change(cx: &mut Context) {
        VimOpCtx::operator_impl(cx, VimOp::Change, cx.register);
    }

    pub fn vim_delete_till_line_end(cx: &mut Context) {
        match cx.editor.mode {
            Mode::Normal => {
                extend_to_line_end(cx);
                VimOpCtx::new(cx, VimOp::Delete).run_operator_for_current_selection(cx);
                normal_mode(cx);
            }
            Mode::Select => {
                VimOpCtx::new(cx, VimOp::Delete).run_operator_lines(cx);
            }
            _ => (),
        }
    }

    pub fn vim_delete_any_selection(cx: &mut Context) {
        VimOpCtx::new(cx, VimOp::Delete).run_operator_for_current_selection(cx);
        normal_mode(cx);
    }

    pub fn vim_restore_last_selection(cx: &mut Context) {
        if let Some((gv_selection, id)) = VIM_STATE.get_gv_selection() {
            let (view_id, doc_id, text_len) = {
                let (view, doc) = current!(cx.editor);
                (view.id, doc.id(), doc.text().len_chars())
            };

            if doc_id == id {
                let sel_len: usize = gv_selection.ranges().iter().map(|range| range.len()).sum();
                if sel_len > text_len {
                    return;
                }

                // TODO implement visual lines as well
                select_mode(cx);
                let (_, doc) = current!(cx.editor);
                doc.set_selection(view_id, gv_selection);
            }
        }
    }

    pub fn vim_find_till_char(cx: &mut Context) {
        VimOpCtx::vim_find_char(cx, None, Direction::Forward, false, false);
    }

    pub fn vim_find_next_char(cx: &mut Context) {
        VimOpCtx::vim_find_char(cx, None, Direction::Forward, true, false);
    }

    pub fn vim_till_prev_char(cx: &mut Context) {
        VimOpCtx::vim_find_char(cx, None, Direction::Backward, false, false);
    }

    pub fn vim_find_prev_char(cx: &mut Context) {
        VimOpCtx::vim_find_char(cx, None, Direction::Backward, true, false);
    }

    pub fn vim_append(cx: &mut Context) {
        append_mode(cx);
        collapse_selection(cx);
    }

    pub fn vim_select_mode(cx: &mut Context) {
        VIM_STATE.exit_visual_modes();
        select_mode(cx);
    }

    pub fn vim_paste_after(cx: &mut Context) {
        if cx.editor.mode == Mode::Select {
            replace_with_yanked(cx);
        } else {
            if !VIM_STATE.is_vim_paste() {
                VIM_STATE.set_vim_paste(true);
            }
            paste_after(cx);
        }
    }

    pub fn vim_paste_before(cx: &mut Context) {
        if cx.editor.mode == Mode::Select {
            replace_with_yanked(cx);
        } else {
            paste_before(cx);
        }
    }

    pub fn vim_paste_clipboard_after(cx: &mut Context) {
        if cx.editor.mode == Mode::Select {
            replace_selections_with_clipboard(cx);
        } else {
            if !VIM_STATE.is_vim_paste() {
                VIM_STATE.set_vim_paste(true);
            }
            paste_clipboard_after(cx);
        }
    }

    pub fn vim_paste_clipboard_before(cx: &mut Context) {
        if cx.editor.mode == Mode::Select {
            replace_selections_with_clipboard(cx);
        } else {
            paste_clipboard_before(cx);
        }
    }

    pub fn vim_move_char_left(cx: &mut Context) {
        move_impl(
            cx,
            vim_utils::vim_move_horizontally,
            Direction::Backward,
            Movement::Move,
        )
    }

    pub fn vim_move_char_right(cx: &mut Context) {
        move_impl(
            cx,
            vim_utils::vim_move_horizontally,
            Direction::Forward,
            Movement::Move,
        )
    }

    pub fn vim_select_regex(cx: &mut Context) {
        VIM_STATE.exit_visual_modes();
        select_regex(cx);
    }

    pub fn vim_select_all(cx: &mut Context) {
        VIM_STATE.exit_visual_line();
        VIM_STATE.allow_highlight();
        select_all(cx);
    }

    pub fn vim_cmd_off(_cx: &mut Context) {
        VIM_STATE.set_cmd_hook_enabled(false);
    }

    pub fn vim_cmd_on(_cx: &mut Context) {
        if VIM_STATE.is_vim_mode() {
            VIM_STATE.set_vim_pend_enable(true);
        }
    }
}

mod vim_utils {
    use super::*;

    pub fn movement_paragraph_backward(
        slice: RopeSlice,
        range: Range,
        count: usize,
        behavior: Movement,
    ) -> Range {
        //Mostly copy/past from Movements::move_prev_paragraph
        let mut line = range.cursor_line(slice);
        let first_char = slice.line_to_char(line) == range.cursor(slice);
        let prev_line_empty = rope_is_line_ending(slice.line(line.saturating_sub(1)));
        let curr_line_empty = rope_is_line_ending(slice.line(line));
        let prev_empty_to_line = prev_line_empty && !curr_line_empty;

        // skip character before paragraph boundary
        if prev_empty_to_line && !first_char {
            line += 1;
        }
        let mut lines = slice.lines_at(line);
        lines.reverse();
        let mut lines = lines.map(rope_is_line_ending).peekable();
        let mut last_line = line;
        for _ in 0..count {
            while lines.next_if(|&e| e).is_some() {
                line -= 1;
            }
            while lines.next_if(|&e| !e).is_some() {
                line -= 1;
            }
            if lines.next_if(|&e| e).is_some() {
                line -= 1;
            }
            if line == last_line {
                break;
            }
            last_line = line;
        }

        let head = slice.line_to_char(line);
        let anchor = if behavior == Movement::Move {
            // exclude first character after paragraph boundary
            if prev_empty_to_line && first_char {
                range.cursor(slice)
            } else {
                range.head
            }
        } else {
            range.put_cursor(slice, head, true).anchor
        };
        Range::new(anchor, head)
    }

    pub fn selection_line_range(cx: &mut Context) -> (usize, usize) {
        let (view, doc) = current!(cx.editor);
        let text = doc.text().slice(..);
        let selection = doc.selection(view.id);

        let lines: Vec<_> = selection
            .ranges()
            .iter()
            .map(|range| range.cursor_line(text))
            .collect();

        (
            lines.iter().min().cloned().unwrap_or(0),
            lines.iter().max().cloned().unwrap_or(0),
        )
    }

    pub fn visual_block_impl(cx: &mut Context, dir: Direction) {
        fn copy_selection(cx: &mut Context, dir: Direction) {
            // Favouring using user facing commands over hidden Helix implementation
            match dir {
                Direction::Forward => copy_selection_on_next_line(cx),
                Direction::Backward => copy_selection_on_prev_line(cx),
            }
        }
        let (view, doc) = current!(cx.editor);
        let selection = doc.selection(view.id);
        if selection.len() == 1 {
            copy_selection(cx, dir)
        } else {
            let (min_line, max_line) = vim_utils::selection_line_range(cx);

            if min_line == max_line {
                copy_selection(cx, dir);
                return;
            }

            let tgt_line = match dir {
                Direction::Backward => max_line,
                Direction::Forward => min_line,
            };

            let mut is_multi_line_cursor = false;
            loop {
                let (view, doc) = current!(cx.editor);
                let view_id = view.id;
                let text = doc.text().slice(..);
                let selection = doc.selection(view_id);

                if tgt_line == selection.primary().cursor_line(text) {
                    remove_primary_selection(cx);
                    is_multi_line_cursor = true
                } else if is_multi_line_cursor {
                    break;
                } else {
                    copy_selection(cx, dir);
                    break;
                }
            }
        }
    }

    pub fn is_line_end(slice: RopeSlice, range: Range, line: usize) -> bool {
        let new_line_char =
            prev_grapheme_boundary(slice, slice.line_to_char(line + 1)) == range.cursor(slice);

        let line_end =
            prev_grapheme_boundary(slice, line_end_char_index(&slice, line)) == range.cursor(slice);

        line_end || new_line_char
    }

    pub fn vim_move_horizontally(
        slice: RopeSlice,
        range: Range,
        dir: Direction,
        count: usize,
        behaviour: Movement,
        _: &TextFormat,
        _: &mut TextAnnotations,
    ) -> Range {
        let line = range.cursor_line(slice);

        let line_start = slice.line_to_char(line) == range.cursor(slice);
        let line_end = is_line_end(slice, range, line);

        // Check horizontall boundaries
        match dir {
            Direction::Forward => {
                if line_end {
                    return range;
                }
            }
            Direction::Backward => {
                if line_start {
                    return range;
                }
            }
        };

        // The following is copy/paste from movement::move_horizontally
        let pos = range.cursor(slice);

        // Compute the new position.
        let new_pos = match dir {
            Direction::Forward => nth_next_grapheme_boundary(slice, pos, count),
            Direction::Backward => nth_prev_grapheme_boundary(slice, pos, count),
        };

        // Compute the final new range.
        range.put_cursor(slice, new_pos, behaviour == Movement::Extend)
    }

    pub fn movement_paragraph_forward(
        slice: RopeSlice,
        range: Range,
        count: usize,
        behavior: Movement,
    ) -> Range {
        //Mostly copy/paste from Movements::move_next_paragraph
        let mut line = range.cursor_line(slice);
        let last_char =
            prev_grapheme_boundary(slice, slice.line_to_char(line + 1)) == range.cursor(slice);
        let curr_line_empty = rope_is_line_ending(slice.line(line));
        let next_line_empty =
            rope_is_line_ending(slice.line(slice.len_lines().saturating_sub(1).min(line + 1)));
        let curr_empty_to_line = curr_line_empty && !next_line_empty;

        // skip character after paragraph boundary
        if curr_empty_to_line && last_char {
            line += 1;
        }
        let mut lines = slice.lines_at(line).map(rope_is_line_ending).peekable();
        let mut last_line = line;
        for _ in 0..count {
            while lines.next_if(|&e| e).is_some() {
                line += 1;
            }
            while lines.next_if(|&e| !e).is_some() {
                line += 1;
            }
            if lines.next_if(|&e| e).is_some() {
                line += 1;
            }
            if line == last_line {
                break;
            }
            last_line = line;
        }
        let head = slice.line_to_char(line);
        let anchor = if behavior == Movement::Move {
            if curr_empty_to_line && last_char {
                range.head
            } else {
                range.cursor(slice)
            }
        } else {
            range.put_cursor(slice, head, true).anchor
        };
        Range::new(anchor, head)
    }

    pub fn cursor_search_impl(cx: &mut Context, direction: Direction) {
        fn find_keyword_char(slice: RopeSlice) -> Option<usize> {
            slice
                .chars()
                .position(|ch| ch.is_alphanumeric() || ch == '_')
        }
        fn goto_next_keyword_char_in_line(view: &mut View, doc: &mut Document) {
            let text = doc.text().slice(..);

            let selection = doc.selection(view.id).clone().transform(|range| {
                let line = range.cursor_line(text);
                let line_start = text.line_to_char(line);

                let pos_end =
                    graphemes::prev_grapheme_boundary(text, line_end_char_index(&text, line))
                        .max(line_start);

                let anchor = range.cursor(text);
                let search_limit = (pos_end + 1).min(text.len_chars());
                if let Some(pos) = find_keyword_char(text.slice(anchor..search_limit)) {
                    range.put_cursor(text, anchor + pos, false)
                } else {
                    range.put_cursor(text, anchor, false)
                }
            });
            doc.set_selection(view.id, selection);
        }

        exit_select_mode(cx);
        keep_primary_selection(cx);

        let count = cx.count();
        let (view, doc) = current!(cx.editor);
        goto_next_keyword_char_in_line(view, doc);

        let text = doc.text().slice(..);
        let selection = doc.selection(view.id);

        if selection.primary().fragment(text).trim().is_empty() {
            cx.editor.set_error("No string under cursor");
            return;
        }

        // Use Helix 'word' as a Vim 'keyword' equivalent
        let objtype = textobject::TextObject::Inside;
        let selection = selection
            .clone()
            .transform(|range| textobject::textobject_word(text, range, objtype, count, false));
        doc.set_selection(view.id, selection);
        search_selection_detect_word_boundaries(cx);

        let config = cx.editor.config();
        if config.search.smart_case {
            // Make the search case insensitive by prepending (?i) to the regex
            let register = cx.register.unwrap_or('/');
            let regex = match cx.editor.registers.first(register, cx.editor) {
                Some(regex) => format!("(?i){}", regex),
                None => return,
            };

            let msg = format!("register '{}' set to '{}'", register, &regex);
            match cx.editor.registers.push(register, regex) {
                Ok(_) => {
                    cx.editor.registers.last_search_register = register;
                    cx.editor.set_status(msg)
                }
                Err(err) => {
                    cx.editor
                        .set_error(format!("Failed to update register: {}", err));
                    return;
                }
            }
        }
        search_next_or_prev_impl(cx, Movement::Move, direction);
    }

    pub fn vim_save_to_jumplist(cx: &mut Context) {
        // TODO: Vim jumplist doesn't dublicate locations (e.g. multiple match_brackets)
        let (view, doc) = current!(cx.editor);
        push_jump(view, doc);
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum VimOp {
    Yank,
    Delete,
    Change,
}

#[derive(Clone, Copy)]
pub struct VimOpCtx {
    op: VimOp,
    count: Option<usize>,
    register: Option<char>,
}

impl VimOpCtx {
    fn new(cx: &mut Context, op: VimOp) -> Self {
        Self {
            op,
            count: Some(cx.count()),
            register: cx.register,
        }
    }

    fn with_custom_register(cx: &mut Context, op: VimOp, register: Option<char>) -> Self {
        Self {
            op,
            count: Some(cx.count()),
            register,
        }
    }

    fn get_full_line_selection(
        cx: &mut Context,
        count: usize,
        include_last_newline: bool,
    ) -> Selection {
        let (view, doc) = current!(cx.editor);

        return doc.selection(view.id).clone().transform(|range| {
            let text = doc.text().slice(..);

            let (start_line, end_line) = range.line_range(text);
            let start = text.line_to_char(start_line);

            let end = if include_last_newline {
                text.line_to_char((end_line + count).min(text.len_lines()))
            } else {
                line_end_char_index(&text, end_line + count - 1)
            };

            Range::new(start, end).with_direction(range.direction())
        });
    }

    fn yank_selection(editor: &mut Editor, selection: &Selection, register: Option<char>) {
        // Adapted from commands::yank_impl

        let register = register.unwrap_or(editor.config().default_yank_register);

        let (_, doc) = current!(editor);
        let text = doc.text().slice(..);

        let values: Vec<String> = selection.fragments(text).map(Cow::into_owned).collect();
        let selections = values.len();

        match editor.registers.write(register, values) {
            Ok(_) => editor.set_status(format!(
                "yanked {selections} selection{} to register {register}",
                if selections == 1 { "" } else { "s" }
            )),
            Err(err) => editor.set_error(err.to_string()),
        }
    }

    fn delete_selection_without_yank(cx: &mut Context, selection: &Selection) {
        let (view, doc) = current!(cx.editor);
        let transaction = Transaction::change_by_selection(doc.text(), selection, |range| {
            (range.from(), range.to(), None)
        });

        doc.apply(&transaction, view.id);
    }

    fn run_ops_after_command(
        &self,
        cx: &mut Context,
        fun: fn(cx: &mut Context),
        require_visual: bool,
    ) {
        if require_visual {
            select_mode(cx);
        }

        cx.count = std::num::NonZeroUsize::new(self.count.unwrap_or(1));

        fun(cx);
        self.run_operator_for_current_selection(cx);

        if require_visual {
            normal_mode(cx);
        }
    }

    fn run_operator(
        &self,
        cx: &mut Context,
        selection_to_yank: &Selection,
        selection_to_delete: &Selection,
    ) {
        Self::yank_selection(cx.editor, selection_to_yank, self.register);

        match self.op {
            VimOp::Delete | VimOp::Change => {
                Self::delete_selection_without_yank(cx, selection_to_delete);
            }
            _ => return,
        }

        if self.op == VimOp::Change {
            insert_mode(cx);
        }
    }

    fn run_operator_for_current_selection(&self, cx: &mut Context) {
        let (view, doc) = current!(cx.editor);
        let selection = doc.selection(view.id).clone();

        flip_selections(cx);
        collapse_selection(cx);
        self.run_operator(cx, &selection, &selection);
    }

    fn run_operator_lines(&self, cx: &mut Context) {
        let count = self.count.unwrap_or(1);
        let selection = Self::get_full_line_selection(cx, count, true);
        if self.op != VimOp::Change {
            self.run_operator(cx, &selection, &selection);
        } else {
            let delete_selection = Self::get_full_line_selection(cx, count, false);
            self.run_operator(cx, &selection, &delete_selection);
        }
    }

    fn char_to_instant_command(ch: char) -> Option<fn(&mut Context)> {
        match ch {
            'w' => Some(extend_next_word_start),
            'W' => Some(extend_next_long_word_start),
            'b' => Some(extend_prev_word_start),
            'B' => Some(extend_prev_long_word_start),
            'e' => Some(extend_next_word_end),
            'E' => Some(extend_next_long_word_end),
            '0' => Some(goto_line_start),
            '$' => Some(goto_line_end),
            '^' => Some(goto_first_nonwhitespace),
            _ => None,
        }
    }

    fn op_till_char(&self, cx: &mut Context) {
        Self::vim_find_char(cx, Some(*self), Direction::Forward, false, true);
    }

    fn op_next_char(&self, cx: &mut Context) {
        Self::vim_find_char(cx, Some(*self), Direction::Forward, true, true);
    }

    fn op_till_prev_char(&self, cx: &mut Context) {
        Self::vim_find_char(cx, Some(*self), Direction::Backward, false, true);
    }

    fn op_prev_char(&self, cx: &mut Context) {
        Self::vim_find_char(cx, Some(*self), Direction::Backward, true, true);
    }

    pub fn operator_impl(cx: &mut Context, op: VimOp, register: Option<char>) {
        let opcx = Self::with_custom_register(cx, op, register);
        if cx.editor.mode == Mode::Select {
            VIM_STATE.exit_visual_line();
            opcx.run_operator_for_current_selection(cx);
            exit_select_mode(cx);
            return;
        }

        cx.on_next_key(move |cx, event| {
            cx.editor.autoinfo = None;
            if let Some(ch) = event.char() {
                match ch {
                    'd' | 'y' | 'c' => opcx.run_operator_lines(cx),
                    'i' => {
                        Self::vim_modify_textobject(cx, Some(opcx), textobject::TextObject::Inside)
                    }
                    'a' => {
                        Self::vim_modify_textobject(cx, Some(opcx), textobject::TextObject::Around)
                    }
                    't' => opcx.op_till_char(cx),
                    'f' => opcx.op_next_char(cx),
                    'T' => opcx.op_till_prev_char(cx),
                    'F' => opcx.op_prev_char(cx),
                    _ => (),
                }

                if let Some(cmd_ch) = Self::char_to_instant_command(ch) {
                    opcx.run_ops_after_command(cx, cmd_ch, true);
                }
            }
        });

        let repeated_key = match op {
            VimOp::Delete => ("d", "Apply to lines"),
            VimOp::Yank => ("y", "Apply to lines"),
            VimOp::Change => ("c", "Apply to lines"),
        };
        let help_text = [
            ("i", "Apply inside"),
            ("a", "Apply around"),
            repeated_key,
            ("w, W, B, $, 0 ...", "Apply within line"),
        ];

        cx.editor.autoinfo = Some(Info::new("Apply Modifier", &help_text));
    }

    fn vim_find_char(
        cx: &mut Context,
        opcx: Option<VimOpCtx>,
        direction: Direction,
        inclusive: bool,
        extend: bool,
    ) {
        // Almost Copy/Paste from commands::find_char

        let count = if let Some(opcx) = opcx {
            opcx.count.unwrap_or(1)
        } else {
            cx.count()
        };

        // TODO: count is reset to 1 before next key so we move it into the closure here.
        // Would be nice to carry over.

        // need to wait for next key
        // TODO: should this be done by grapheme rather than char?  For example,
        // we can't properly handle the line-ending CRLF case here in terms of char.
        cx.on_next_key(move |cx, event| {
            let ch = match event {
                KeyEvent {
                    code: KeyCode::Enter,
                    ..
                } => {
                    find_char_line_ending(cx, count, direction, inclusive, extend);
                    return;
                }

                KeyEvent {
                    code: KeyCode::Tab, ..
                } => '\t',

                KeyEvent {
                    code: KeyCode::Char(ch),
                    ..
                } => ch,
                _ => return,
            };
            let motion = move |editor: &mut Editor| {
                match direction {
                    Direction::Forward => {
                        find_char_impl(editor, &find_next_char_impl, inclusive, extend, ch, count)
                    }
                    Direction::Backward => {
                        find_char_impl(editor, &find_prev_char_impl, inclusive, extend, ch, count)
                    }
                };
            };

            cx.editor.apply_motion(motion);

            if let Some(opcx) = opcx {
                opcx.run_operator_for_current_selection(cx);
            } else if cx.editor.mode == Mode::Normal {
                collapse_selection(cx)
            }
        })
    }

    fn vim_modify_textobject(
        cx: &mut Context,
        opcx: Option<VimOpCtx>,
        objtype: textobject::TextObject,
    ) {
        // Adapted from select_textobject

        let count = if let Some(opcx) = opcx {
            opcx.count.unwrap_or(1)
        } else {
            cx.count()
        };

        cx.on_next_key(move |cx, event| {
            cx.editor.autoinfo = None;
            if let Some(ch) = event.char() {
                let (view, doc) = current!(cx.editor);

                let loader = cx.editor.syn_loader.load();
                let text = doc.text().slice(..);

                let textobject_treesitter = |obj_name: &str, range: Range| -> Range {
                    let Some(syntax) = doc.syntax() else {
                        return range;
                    };
                    textobject::textobject_treesitter(
                        text, range, objtype, obj_name, syntax, &loader, count,
                    )
                };

                let textobject_change = |range: Range| -> Range {
                    let diff_handle = doc.diff_handle().unwrap();
                    let diff = diff_handle.load();
                    let line = range.cursor_line(text);
                    let hunk_idx = if let Some(hunk_idx) = diff.hunk_at(line as u32, false) {
                        hunk_idx
                    } else {
                        return range;
                    };
                    let hunk = diff.nth_hunk(hunk_idx).after;

                    let start = text.line_to_char(hunk.start as usize);
                    let end = text.line_to_char(hunk.end as usize);
                    Range::new(start, end).with_direction(range.direction())
                };
                let mut is_valid = true;
                let selection = doc.selection(view.id).clone().transform(|range| {
                    match ch {
                        'w' => textobject::textobject_word(text, range, objtype, count, false),
                        'W' => textobject::textobject_word(text, range, objtype, count, true),
                        't' => textobject_treesitter("class", range),
                        'f' => textobject_treesitter("function", range),
                        'a' => textobject_treesitter("parameter", range),
                        'c' => textobject_treesitter("comment", range),
                        'T' => textobject_treesitter("test", range),
                        'e' => textobject_treesitter("entry", range),
                        'p' => textobject::textobject_paragraph(text, range, objtype, count),
                        'm' => textobject::textobject_pair_surround_closest(
                            doc.syntax(),
                            text,
                            range,
                            objtype,
                            count,
                        ),
                        'g' => textobject_change(range),
                        // TODO: cancel new ranges if inconsistent surround matches across lines
                        ch if !ch.is_ascii_alphanumeric() => textobject::textobject_pair_surround(
                            doc.syntax(),
                            text,
                            range,
                            objtype,
                            ch,
                            count,
                        ),
                        _ => {
                            is_valid = false;
                            range
                        }
                    }
                });

                if let Some(opcx) = opcx {
                    if is_valid {
                        opcx.run_operator(cx, &selection, &selection);
                    }
                }
            }
        });

        let title = match objtype {
            textobject::TextObject::Inside => "Match inside",
            textobject::TextObject::Around => "Match around",
            _ => return,
        };
        let help_text = [
            ("w", "Word"),
            ("W", "WORD"),
            ("p", "Paragraph"),
            ("t", "Type definition (tree-sitter)"),
            ("f", "Function (tree-sitter)"),
            ("a", "Argument/parameter (tree-sitter)"),
            ("c", "Comment (tree-sitter)"),
            ("T", "Test (tree-sitter)"),
            ("e", "Data structure entry (tree-sitter)"),
            ("m", "Closest surrounding pair (tree-sitter)"),
            ("g", "Change"),
            (" ", "... or any character acting as a pair"),
        ];

        cx.editor.autoinfo = Some(Info::new(title, &help_text));
    }
}
