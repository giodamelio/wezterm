//! Various tests of the terminal model and escape sequence
//! processing routines.

use super::*;
mod c0;
use bitflags::bitflags;
mod c1;
mod csi;
// mod selection; FIXME: port to render layer
use crate::color::ColorPalette;
use k9::assert_equal as assert_eq;
use std::sync::{Arc, Mutex};
use wezterm_escape_parser::csi::{Edit, EraseInDisplay, EraseInLine};
use wezterm_escape_parser::{OneBased, OperatingSystemCommand, CSI};
use wezterm_surface::{CursorShape, CursorVisibility, SequenceNo, SEQ_ZERO};

#[derive(Debug)]
struct LocalClip {
    clip: Mutex<Option<String>>,
}

impl LocalClip {
    fn new() -> Self {
        Self {
            clip: Mutex::new(None),
        }
    }
}

impl Clipboard for LocalClip {
    fn set_contents(
        &self,
        _selection: ClipboardSelection,
        clip: Option<String>,
    ) -> anyhow::Result<()> {
        *self.clip.lock().unwrap() = clip;
        Ok(())
    }
}

struct TestTerm {
    term: Terminal,
    /// Shared buffer capturing everything the terminal writes back to the
    /// PTY (answerback / protocol replies), so tests can assert on replies.
    writer: Arc<Mutex<Vec<u8>>>,
}

/// A `Write` that appends to a shared buffer, letting a test inspect the
/// bytes the terminal writes back to the PTY.
#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[derive(Debug)]
struct TestTermConfig {
    scrollback: usize,
    enable_glyph_protocol: bool,
}
impl TerminalConfiguration for TestTermConfig {
    fn scrollback_size(&self) -> usize {
        self.scrollback
    }

    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }

    fn enable_glyph_protocol(&self) -> bool {
        self.enable_glyph_protocol
    }
}

impl TestTerm {
    fn new(height: usize, width: usize, scrollback: usize) -> Self {
        // The glyph protocol is on by default for tests so the existing
        // glyph-protocol suite exercises it; the gate itself is covered by a
        // dedicated test that builds a disabled term.
        Self::new_with_glyph_protocol(height, width, scrollback, true)
    }

    fn new_with_glyph_protocol(
        height: usize,
        width: usize,
        scrollback: usize,
        enable_glyph_protocol: bool,
    ) -> Self {
        let _ = env_logger::Builder::new()
            .is_test(true)
            .filter_level(log::LevelFilter::Trace)
            .try_init();

        let writer = Arc::new(Mutex::new(Vec::new()));
        let mut term = Terminal::new(
            TerminalSize {
                rows: height,
                cols: width,
                pixel_width: width * 8,
                pixel_height: height * 16,
                dpi: 0,
            },
            Arc::new(TestTermConfig {
                scrollback,
                enable_glyph_protocol,
            }),
            "WezTerm",
            "O_o",
            Box::new(SharedWriter(Arc::clone(&writer))),
        );
        let clip: Arc<dyn Clipboard> = Arc::new(LocalClip::new());
        term.set_clipboard(&clip);

        let mut term = Self { term, writer };

        term.set_auto_wrap(true);

        term
    }

    fn print<B: AsRef<[u8]>>(&mut self, bytes: B) {
        self.term.advance_bytes(bytes);
    }

    /// Drain and return everything the terminal has written back to the PTY
    /// so far. Replies are delivered by a background `ThreadedWriter`, so
    /// this is racy on its own — use [`Self::wait_writer_output`] when a
    /// reply is expected and [`Self::expect_no_writer_output`] when none is.
    fn drain_writer(&self) -> Vec<u8> {
        std::mem::take(&mut *self.writer.lock().unwrap())
    }

    /// Wait (bounded) for the async PTY writer to deliver a reply, then
    /// drain and return it. Polls until the buffer is non-empty and stable.
    fn wait_writer_output(&self) -> Vec<u8> {
        let mut last = 0usize;
        for _ in 0..400 {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let len = self.writer.lock().unwrap().len();
            if len > 0 && len == last {
                break;
            }
            last = len;
        }
        self.drain_writer()
    }

    /// Assert that no reply is produced within a short settling window.
    fn expect_no_writer_output(&self) {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let out = self.drain_writer();
        assert!(
            out.is_empty(),
            "expected no PTY reply, got {:?}",
            String::from_utf8_lossy(&out)
        );
    }

    fn set_mode(&mut self, mode: &str, enable: bool) {
        self.print(CSI);
        self.print(mode);
        self.print(if enable { b"h" } else { b"l" });
    }

    fn set_auto_wrap(&mut self, enable: bool) {
        self.set_mode("?7", enable);
    }

    fn set_left_and_right_margins(&mut self, left: usize, right: usize) {
        self.print(CSI);
        self.print(format!("{};{}s", left + 1, right + 1));
    }

    fn set_scroll_region(&mut self, top: usize, bottom: usize) {
        self.print(CSI);
        self.print(format!("{};{}r", top + 1, bottom + 1));
    }

    fn delete_lines(&mut self, n: isize) {
        self.print(CSI);
        self.print(format!("{}M", n));
    }

    fn cup(&mut self, col: isize, row: isize) {
        self.print(CSI);
        self.print(format!("{};{}H", row + 1, col + 1));
    }

    fn hvp(&mut self, col: isize, row: isize) {
        self.print(CSI);
        self.print(format!("{};{}f", row + 1, col + 1));
    }

    fn erase_in_display(&mut self, erase: EraseInDisplay) {
        let csi = CSI::Edit(Edit::EraseInDisplay(erase));
        self.print(format!("{}", csi));
    }

    fn erase_in_line(&mut self, erase: EraseInLine) {
        let csi = CSI::Edit(Edit::EraseInLine(erase));
        self.print(format!("{}", csi));
    }

    fn hyperlink(&mut self, link: &Arc<Hyperlink>) {
        let osc = OperatingSystemCommand::SetHyperlink(Some(link.as_ref().clone()));
        self.print(format!("{}", osc));
    }

    fn hyperlink_off(&mut self) {
        self.print("\x1b]8;;\x1b\\");
    }

    fn soft_reset(&mut self) {
        self.print(CSI);
        self.print("!p");
    }

    fn assert_cursor_pos(&self, x: usize, y: i64, reason: Option<&str>, seqno: Option<SequenceNo>) {
        let cursor = self.cursor_pos();
        let expect = CursorPosition {
            x,
            y,
            shape: CursorShape::Default,
            visibility: CursorVisibility::Visible,
            seqno: seqno.unwrap_or_else(|| self.current_seqno()),
        };
        assert_eq!(
            cursor, expect,
            "actual cursor (left) didn't match expected cursor (right) reason={:?}",
            reason
        );
    }

    fn assert_dirty_lines(&self, seqno: SequenceNo, expected: &[usize], reason: Option<&str>) {
        let mut seqs = vec![];
        let mut dirty_indices = vec![];

        self.screen().for_each_phys_line(|i, line| {
            seqs.push(line.current_seqno());
            if line.changed_since(seqno) {
                dirty_indices.push(i);
            }
        });
        assert_eq!(
            &dirty_indices, &expected,
            "actual dirty lines (left) didn't match expected dirty \
             lines (right) reason={:?}. threshold seq: {} seqs: {:?}",
            reason, seqno, seqs
        );
    }
}

impl Deref for TestTerm {
    type Target = Terminal;

    fn deref(&self) -> &Terminal {
        &self.term
    }
}

impl DerefMut for TestTerm {
    fn deref_mut(&mut self) -> &mut Terminal {
        &mut self.term
    }
}

/// Asserts that both line slices match according to the
/// selected flags.
fn assert_lines_equal(
    file: &str,
    line_no: u32,
    lines: &[Line],
    expect_lines: &[Line],
    compare: Compare,
) {
    let mut expect_iter = expect_lines.iter();

    println!("actual_lines:");
    for line in lines {
        println!("[{}]", line.as_str());
    }
    println!("expect_lines");
    for line in expect_lines {
        println!("[{}]", line.as_str());
    }

    for (idx, line) in lines.iter().enumerate() {
        let expect = match expect_iter.next() {
            Some(e) => e,
            None => break,
        };

        if compare.contains(Compare::ATTRS) {
            let line_attrs: Vec<_> = line.visible_cells().map(|c| c.attrs().clone()).collect();
            let expect_attrs: Vec<_> = expect.visible_cells().map(|c| c.attrs().clone()).collect();
            assert_eq!(
                expect_attrs,
                line_attrs,
                "{}:{}: line {} `{}` attrs didn't match (left=expected, right=actual)",
                file,
                line_no,
                idx,
                line.as_str()
            );
        }
        if compare.contains(Compare::TEXT) {
            let line_str = line.as_str();
            let expect_str = expect.as_str();
            assert_eq!(
                line_str,
                expect_str,
                "{}:{}: line {} text didn't match '{}' vs '{}'",
                file,
                line_no,
                idx,
                line_str.escape_default(),
                expect_str.escape_default()
            );
        }
    }

    assert_eq!(
        lines.len(),
        expect_lines.len(),
        "{}:{}: expectation has wrong number of lines",
        file,
        line_no
    );
}

bitflags! {
    struct Compare : u8{
        const TEXT = 1;
        const ATTRS = 2;
        const DIRTY = 4;
    }
}

fn print_all_lines(term: &Terminal) {
    let screen = term.screen();

    println!("whole screen contents are:");
    screen.for_each_phys_line(|_, line| {
        println!("[{}]", line.as_str());
    });
}

fn print_visible_lines(term: &Terminal) {
    let screen = term.screen();

    println!("screen contents are:");
    for line in screen.visible_lines().iter() {
        println!("[{}]", line.as_str());
    }
}

/// Asserts that the visible lines of the terminal have the
/// same character contents as the expected lines.
/// The other cell attributes are not compared; this is
/// a convenience for writing visually understandable tests.
fn assert_visible_contents(term: &Terminal, file: &str, line: u32, expect_lines: &[&str]) {
    print_visible_lines(&term);
    let screen = term.screen();

    let expect: Vec<Line> = expect_lines.iter().map(|s| (*s).into()).collect();

    assert_lines_equal(file, line, &screen.visible_lines(), &expect, Compare::TEXT);
}

fn assert_all_contents(term: &Terminal, file: &str, line: u32, expect_lines: &[&str]) {
    print_all_lines(&term);
    let screen = term.screen();

    let expect: Vec<Line> = expect_lines.iter().map(|s| (*s).into()).collect();

    assert_lines_equal(file, line, &screen.all_lines(), &expect, Compare::TEXT);
}

#[test]
fn test_semantic_1539() {
    use wezterm_escape_parser::osc::FinalTermSemanticPrompt;
    let mut term = TestTerm::new(5, 10, 0);
    term.print(format!(
        "{}prompt\r\nwoot",
        OperatingSystemCommand::FinalTermSemanticPrompt(
            FinalTermSemanticPrompt::MarkEndOfPromptAndStartOfInputUntilEndOfLine
        )
    ));

    assert_visible_contents(&term, file!(), line!(), &["prompt", "woot", "", "", ""]);

    k9::snapshot!(
        term.get_semantic_zones().unwrap(),
        "
[
    SemanticZone {
        start_y: 0,
        start_x: 0,
        end_y: 0,
        end_x: 5,
        semantic_type: Input,
    },
    SemanticZone {
        start_y: 1,
        start_x: 0,
        end_y: 1,
        end_x: 3,
        semantic_type: Output,
    },
]
"
    );
}

#[test]
fn test_semantic() {
    use wezterm_escape_parser::osc::FinalTermSemanticPrompt;
    let mut term = TestTerm::new(5, 10, 0);
    term.print("hello");
    term.print(format!(
        "{}",
        OperatingSystemCommand::FinalTermSemanticPrompt(FinalTermSemanticPrompt::FreshLine)
    ));
    term.print("there");

    assert_visible_contents(&term, file!(), line!(), &["hello", "there", "", "", ""]);

    term.cup(0, 2);
    term.print(format!(
        "{}",
        OperatingSystemCommand::FinalTermSemanticPrompt(FinalTermSemanticPrompt::FreshLine)
    ));
    term.print("three");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["hello", "there", "three", "", ""],
    );

    k9::snapshot!(
        term.get_semantic_zones().unwrap(),
        "
[
    SemanticZone {
        start_y: 0,
        start_x: 0,
        end_y: 2,
        end_x: 4,
        semantic_type: Output,
    },
]
"
    );

    term.print(format!(
        "{}",
        OperatingSystemCommand::FinalTermSemanticPrompt(
            FinalTermSemanticPrompt::FreshLineAndStartPrompt {
                aid: None,
                cl: None
            }
        )
    ));
    term.print("> ");
    term.print(format!(
        "{}",
        OperatingSystemCommand::FinalTermSemanticPrompt(
            FinalTermSemanticPrompt::MarkEndOfPromptAndStartOfInputUntilNextMarker
        )
    ));
    term.print("ls -l\r\n");
    term.print(format!(
        "{}",
        OperatingSystemCommand::FinalTermSemanticPrompt(
            FinalTermSemanticPrompt::MarkEndOfInputAndStartOfOutput { aid: None }
        )
    ));
    term.print("some file");

    let output = CellAttributes::default();
    let mut input = CellAttributes::default();
    input.set_semantic_type(SemanticType::Input);

    let mut prompt_line = Line::from_text("> ls -l", &output, SEQ_ZERO, None);
    for i in 0..2 {
        prompt_line.cells_mut()[i]
            .attrs_mut()
            .set_semantic_type(SemanticType::Prompt);
    }
    for i in 2..7 {
        prompt_line.cells_mut()[i]
            .attrs_mut()
            .set_semantic_type(SemanticType::Input);
    }

    k9::snapshot!(
        term.get_semantic_zones().unwrap(),
        "
[
    SemanticZone {
        start_y: 0,
        start_x: 0,
        end_y: 2,
        end_x: 4,
        semantic_type: Output,
    },
    SemanticZone {
        start_y: 3,
        start_x: 0,
        end_y: 3,
        end_x: 1,
        semantic_type: Prompt,
    },
    SemanticZone {
        start_y: 3,
        start_x: 2,
        end_y: 3,
        end_x: 6,
        semantic_type: Input,
    },
    SemanticZone {
        start_y: 4,
        start_x: 0,
        end_y: 4,
        end_x: 8,
        semantic_type: Output,
    },
]
"
    );

    assert_lines_equal(
        file!(),
        line!(),
        &term.screen().visible_lines(),
        &[
            Line::from_text("hello", &output, SEQ_ZERO, None),
            Line::from_text("there", &output, SEQ_ZERO, None),
            Line::from_text("three", &output, SEQ_ZERO, None),
            prompt_line,
            Line::from_text("some file", &output, SEQ_ZERO, None),
        ],
        Compare::TEXT | Compare::ATTRS,
    );
}

#[test]
fn issue_1161() {
    let mut term = TestTerm::new(1, 5, 0);
    term.print("x\u{3000}x");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &[
            // U+3000 is ideographic space, a double-width space
            "x\u{3000}x",
        ],
    );
}

#[test]
fn basic_output() {
    let mut term = TestTerm::new(5, 10, 0);

    term.cup(1, 1);

    term.set_auto_wrap(false);
    term.print("hello, world!");
    assert_visible_contents(&term, file!(), line!(), &["", " hello, w!", "", "", ""]);

    term.set_auto_wrap(true);
    term.erase_in_display(EraseInDisplay::EraseToStartOfDisplay);
    term.cup(1, 1);
    term.print("hello, world!");
    assert_visible_contents(&term, file!(), line!(), &["", " hello, wo", "rld!", "", ""]);

    term.erase_in_display(EraseInDisplay::EraseToStartOfDisplay);
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["", "          ", "     ", "", ""],
    );

    term.cup(0, 2);
    term.print("woot");
    term.cup(2, 2);
    term.erase_in_line(EraseInLine::EraseToEndOfLine);
    assert_visible_contents(&term, file!(), line!(), &["", "          ", "wo", "", ""]);

    term.erase_in_line(EraseInLine::EraseToStartOfLine);
    assert_visible_contents(&term, file!(), line!(), &["", "          ", "   ", "", ""]);
}

/// Ensure that we dirty lines as the cursor is moved around, otherwise
/// the renderer won't draw the cursor in the right place
#[test]
fn cursor_movement_damage() {
    let mut term = TestTerm::new(2, 3, 0);

    let seqno = term.current_seqno();
    term.print("fooo.");
    assert_visible_contents(&term, file!(), line!(), &["foo", "o."]);
    term.assert_cursor_pos(2, 1, None, None);
    term.assert_dirty_lines(seqno, &[0, 1], None);

    term.cup(0, 1);

    let seqno = term.current_seqno();
    term.print("\x08");
    term.assert_cursor_pos(0, 1, Some("BS doesn't change the line"), Some(seqno));
    // Since we didn't move, the line isn't dirty
    term.assert_dirty_lines(seqno, &[], None);

    let seqno = term.current_seqno();
    term.cup(0, 0);
    term.assert_dirty_lines(
        seqno,
        &[],
        Some("cursor movement no longer dirties old and new lines"),
    );
    term.assert_cursor_pos(0, 0, None, None);
}
const NUM_COLS: usize = 3;

#[test]
fn scroll_up_within_left_and_right_margins() {
    let ones = "1".repeat(NUM_COLS);
    let twos = "2".repeat(NUM_COLS);
    let threes = "3".repeat(NUM_COLS);
    let fours = "4".repeat(NUM_COLS + 2);
    let fives = "5".repeat(NUM_COLS);

    let mut term = TestTerm::new(5, NUM_COLS + 2, 0);

    term.print(&ones);
    term.print("\r\n");
    term.print(&twos);
    term.print("\r\n");
    term.print(&threes);
    term.print("\r\n");
    term.print(&fours);
    term.print("\r\n");
    term.print(&fives);

    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "222", "333", "44444", "555"],
    );

    term.set_mode("?69", true); // allow left/right margins to be set
    term.set_left_and_right_margins(1, NUM_COLS + 1);
    term.set_scroll_region(2, 4);
    term.cup(1, 4);
    term.print("\n");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &[
            "111",
            "222",
            &format!("3{}", "4".repeat(NUM_COLS + 1)),
            &format!("4{}", "5".repeat(NUM_COLS - 1)),
            &format!("5{}", " ".repeat(NUM_COLS - 1)),
        ],
    );
}

#[test]
fn scroll_down_within_left_and_right_margins() {
    let ones = "1".repeat(NUM_COLS);
    let twos = "2".repeat(NUM_COLS);
    let threes = "3".repeat(NUM_COLS);
    let fours = "4".repeat(NUM_COLS + 2);
    let fives = "5".repeat(NUM_COLS);

    let mut term = TestTerm::new(5, NUM_COLS + 2, 0);

    term.print(&ones);
    term.print("\r\n");
    term.print(&twos);
    term.print("\r\n");
    term.print(&threes);
    term.print("\r\n");
    term.print(&fours);
    term.print("\r\n");
    term.print(&fives);

    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "222", "333", "44444", "555"],
    );

    term.set_mode("?69", true); // allow left/right margins to be set
    term.set_left_and_right_margins(1, NUM_COLS + 1);
    term.set_scroll_region(2, 5);
    term.cup(1, 2);

    // IL: Insert Line
    term.print(CSI);
    term.print("L");

    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &[
            "111",
            "222",
            &format!("3{}", " ".repeat(NUM_COLS - 1)),
            &format!("4{}", "3".repeat(NUM_COLS - 1)),
            &format!("5{}", "4".repeat(NUM_COLS + 1)),
        ],
    );
}

/// Replicates a bug I initially found via:
/// $ vim
/// :help
/// PageDown
#[test]
fn test_delete_lines() {
    let mut term = TestTerm::new(5, 3, 0);

    let seqno = term.current_seqno();
    term.print("111\r\n222\r\n333\r\n444\r\n555");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "222", "333", "444", "555"],
    );
    term.assert_dirty_lines(seqno, &[0, 1, 2, 3, 4], None);
    term.cup(0, 1);

    let seqno = term.current_seqno();
    term.assert_dirty_lines(seqno, &[], None);
    term.delete_lines(2);
    assert_visible_contents(&term, file!(), line!(), &["111", "444", "555", "", ""]);
    term.assert_dirty_lines(seqno, &[1, 2, 3, 4], None);

    term.cup(0, 3);
    term.print("aaa\r\nbbb");
    term.cup(0, 1);

    let seqno = term.current_seqno();
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "444", "555", "aaa", "bbb"],
    );

    // test with a scroll region smaller than the screen
    term.set_scroll_region(1, 3);
    term.cup(0, 1);
    print_all_lines(&term);
    term.delete_lines(2);

    assert_visible_contents(&term, file!(), line!(), &["111", "aaa", "", "", "bbb"]);
    term.assert_dirty_lines(seqno, &[1, 2, 3], None);

    // expand the scroll region to fill the screen
    term.set_scroll_region(0, 4);

    let seqno = term.current_seqno();
    print_all_lines(&term);
    term.delete_lines(1);

    assert_visible_contents(&term, file!(), line!(), &["aaa", "", "", "bbb", ""]);
    term.assert_dirty_lines(seqno, &[4], None);
}

/// Test DEC Special Graphics character set.
#[test]
fn test_dec_special_graphics() {
    let mut term = TestTerm::new(2, 50, 0);

    term.print("\u{1b}(0ABCabcdefghijklmnopqrstuvwxyzDEF\r\n\u{1b}(Bhello");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["ABC▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥DEF", "hello"],
    );

    term = TestTerm::new(2, 50, 0);
    term.print("\u{1b})0\u{0e}SO-ABCabcdefghijklmnopqrstuvwxyzDEF\r\n\u{0f}SI-hello");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["SO-ABC▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥DEF", "SI-hello"],
    );
}

/// Test double-width / double-height sequences.
#[test]
fn test_dec_double_width() {
    let mut term = TestTerm::new(4, 50, 0);

    term.print("\u{1b}#3line1\r\nline2\u{1b}#4\r\nli\u{1b}#6ne3\r\n\u{1b}#5line4");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["line1", "line2", "line3", "line4"],
    );

    let lines = term.screen().visible_lines();
    assert!(lines[0].is_double_height_top());
    assert!(lines[1].is_double_height_bottom());
    assert!(lines[2].is_double_width());
    assert!(lines[3].is_single_width());
}

/// This test skips over an edge case with cursor positioning,
/// while sizing down, but tries to trip over the same edge
/// case while sizing back up again
#[test]
fn test_resize_2162_by_2_then_up_1() {
    let num_lines = 4;
    let num_cols = 20;

    let mut term = TestTerm::new(num_lines, num_cols, 0);
    term.print("some long long text");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["some long long text", "", "", ""],
    );
    term.assert_cursor_pos(19, 0, None, Some(0));
    term.resize(TerminalSize {
        rows: num_lines,
        cols: num_cols - 2,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["some long long tex", "t", "", ""],
    );
    eprintln!("check cursor pos 2");
    term.assert_cursor_pos(1, 1, None, Some(6));
    term.resize(TerminalSize {
        rows: num_lines - 1,
        cols: num_cols,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(&term, file!(), line!(), &["some long long text", "", ""]);
    eprintln!("check cursor pos 3");
    term.assert_cursor_pos(19, 0, None, Some(7));
    term.resize(TerminalSize {
        rows: num_lines,
        cols: num_cols,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["some long long text", "", "", ""],
    );
    eprintln!("check cursor pos 3");
    term.assert_cursor_pos(19, 0, None, Some(8));
}

/// This test skips over an edge case with cursor positioning,
/// so it passes even ahead of a fix for issue 2162.
#[test]
fn test_resize_2162_by_2() {
    let num_lines = 4;
    let num_cols = 20;

    let mut term = TestTerm::new(num_lines, num_cols, 0);
    term.print("some long long text");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["some long long text", "", "", ""],
    );
    term.assert_cursor_pos(19, 0, None, Some(0));
    term.resize(TerminalSize {
        rows: num_lines,
        cols: num_cols - 2,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["some long long tex", "t", "", ""],
    );
    eprintln!("check cursor pos 2");
    term.assert_cursor_pos(1, 1, None, Some(6));
    term.resize(TerminalSize {
        rows: num_lines,
        cols: num_cols,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["some long long text", "", "", ""],
    );
    eprintln!("check cursor pos 3");
    term.assert_cursor_pos(19, 0, None, Some(7));
}

/// This case tickles an edge case where the cursor ends
/// up drifting away from where the line wraps and ends up
/// in the wrong place
#[test]
fn test_resize_2162() {
    let num_lines = 4;
    let num_cols = 20;

    let mut term = TestTerm::new(num_lines, num_cols, 0);
    term.print("some long long text");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["some long long text", "", "", ""],
    );
    term.assert_cursor_pos(19, 0, None, Some(0));
    term.resize(TerminalSize {
        rows: num_lines,
        cols: num_cols - 1,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["some long long text", "", "", ""],
    );
    eprintln!("check cursor pos 2");
    term.assert_cursor_pos(19, 0, None, Some(6));
    term.resize(TerminalSize {
        rows: num_lines,
        cols: num_cols,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["some long long text", "", "", ""],
    );
    eprintln!("check cursor pos 3");
    term.assert_cursor_pos(19, 0, None, Some(7));
}

/// Test the behavior of wrapped lines when we resize the terminal
/// wider and then narrower.
#[test]
fn test_resize_wrap() {
    const LINES: usize = 8;
    let mut term = TestTerm::new(LINES, 4, 0);
    term.print("111\r\n2222aa\r\n333\r\n");
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "2222", "aa", "333", "", "", "", ""],
    );
    term.resize(TerminalSize {
        rows: LINES,
        cols: 5,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "2222a", "a", "333", "", "", "", ""],
    );
    term.resize(TerminalSize {
        rows: LINES,
        cols: 6,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "2222aa", "333", "", "", "", "", ""],
    );
    term.resize(TerminalSize {
        rows: LINES,
        cols: 7,
        pixel_width: 0,
        pixel_height: 0,
        dpi: 0,
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "2222aa", "333", "", "", "", "", ""],
    );
    term.resize(TerminalSize {
        rows: LINES,
        cols: 8,
        ..Default::default()
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "2222aa", "333", "", "", "", "", ""],
    );

    // Resize smaller again
    term.resize(TerminalSize {
        rows: LINES,
        cols: 7,
        ..Default::default()
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "2222aa", "333", "", "", "", "", ""],
    );
    term.resize(TerminalSize {
        rows: LINES,
        cols: 6,
        ..Default::default()
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "2222aa", "333", "", "", "", "", ""],
    );
    term.resize(TerminalSize {
        rows: LINES,
        cols: 5,
        ..Default::default()
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "2222a", "a", "333", "", "", "", ""],
    );
    term.resize(TerminalSize {
        rows: LINES,
        cols: 4,
        ..Default::default()
    });
    assert_visible_contents(
        &term,
        file!(),
        line!(),
        &["111", "2222", "aa", "333", "", "", "", ""],
    );
}

#[test]
fn test_resize_wrap_issue_971() {
    const LINES: usize = 4;
    let mut term = TestTerm::new(LINES, 4, 0);
    term.print("====\r\nSS\r\n");
    assert_visible_contents(&term, file!(), line!(), &["====", "SS", "", ""]);
    term.resize(TerminalSize {
        rows: LINES,
        cols: 6,
        ..Default::default()
    });
    assert_visible_contents(&term, file!(), line!(), &["====", "SS", "", ""]);
}

#[test]
fn test_resize_wrap_sgc_issue_978() {
    const LINES: usize = 4;
    let mut term = TestTerm::new(LINES, 4, 0);
    term.print("\u{1b}(0qqqq\u{1b}(B\r\nSS\r\n");
    assert_visible_contents(&term, file!(), line!(), &["────", "SS", "", ""]);
    term.resize(TerminalSize {
        rows: LINES,
        cols: 6,
        ..Default::default()
    });
    assert_visible_contents(&term, file!(), line!(), &["────", "SS", "", ""]);
}

#[test]
fn test_resize_wrap_dectcm_issue_978() {
    const LINES: usize = 4;
    let mut term = TestTerm::new(LINES, 4, 0);
    term.print("\u{1b}[?25l====\u{1b}[?25h\r\nSS\r\n");
    assert_visible_contents(&term, file!(), line!(), &["====", "SS", "", ""]);
    term.resize(TerminalSize {
        rows: LINES,
        cols: 6,
        ..Default::default()
    });
    assert_visible_contents(&term, file!(), line!(), &["====", "SS", "", ""]);
}

#[test]
fn test_resize_wrap_escape_code_issue_978() {
    const LINES: usize = 4;
    let mut term = TestTerm::new(LINES, 4, 0);
    term.print("====\u{1b}[0m\r\nSS\r\n");
    assert_visible_contents(&term, file!(), line!(), &["====", "SS", "", ""]);
    term.resize(TerminalSize {
        rows: LINES,
        cols: 6,
        ..Default::default()
    });
    assert_visible_contents(&term, file!(), line!(), &["====", "SS", "", ""]);
}

#[test]
fn test_scrollup() {
    let mut term = TestTerm::new(2, 1, 4);
    term.print("1\n");
    assert_all_contents(&term, file!(), line!(), &["1", ""]);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 0);

    term.print("2\n");
    assert_all_contents(&term, file!(), line!(), &["1", "2", ""]);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 1);

    term.print("3\n");
    assert_all_contents(&term, file!(), line!(), &["1", "2", "3", ""]);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 2);

    term.print("4\n");
    assert_all_contents(&term, file!(), line!(), &["1", "2", "3", "4", ""]);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 3);

    term.print("5\n");
    assert_all_contents(&term, file!(), line!(), &["1", "2", "3", "4", "5", ""]);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 4);

    term.print("6\n");
    assert_all_contents(&term, file!(), line!(), &["2", "3", "4", "5", "6", ""]);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 5);

    term.print("7\n");
    assert_all_contents(&term, file!(), line!(), &["3", "4", "5", "6", "7", ""]);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 6);

    term.print("8\n");
    assert_all_contents(&term, file!(), line!(), &["4", "5", "6", "7", "8", ""]);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 7);
}

#[test]
fn test_ri() {
    let mut term = TestTerm::new(3, 1, 10);
    term.print("1\n\u{8d}\n");
    assert_all_contents(&term, file!(), line!(), &["1", "", ""]);
}

#[test]
fn test_scroll_margins() {
    let mut term = TestTerm::new(3, 1, 10);
    term.print("1\n2\n3\n4\n");
    assert_all_contents(&term, file!(), line!(), &["1", "2", "3", "4", ""]);

    let margins = CSI::Cursor(wezterm_escape_parser::csi::Cursor::SetTopAndBottomMargins {
        top: OneBased::new(1),
        bottom: OneBased::new(2),
    });
    term.print(format!("{}", margins));

    term.print("z\n");
    assert_all_contents(&term, file!(), line!(), &["1", "2", "z", "4", ""]);

    term.print("a\n");
    assert_all_contents(&term, file!(), line!(), &["1", "2", "z", "a", "", ""]);

    term.cup(0, 1);
    term.print("W\n");
    assert_all_contents(&term, file!(), line!(), &["1", "2", "z", "a", "W", "", ""]);
}

#[test]
fn test_emoji_with_modifier() {
    let waving_hand = "\u{1f44b}";
    let waving_hand_dark_tone = "\u{1f44b}\u{1f3ff}";

    let mut term = TestTerm::new(3, 5, 0);
    term.print(waving_hand);
    term.print("\r\n");
    term.print(waving_hand_dark_tone);

    assert_all_contents(
        &term,
        file!(),
        line!(),
        &[waving_hand, waving_hand_dark_tone, ""],
    );
}

#[test]
fn test_1573() {
    let sequence = "\u{1112}\u{1161}\u{11ab}";

    let mut term = TestTerm::new(2, 5, 0);
    term.print(sequence);
    term.print("\r\n");

    assert_all_contents(&term, file!(), line!(), &[sequence, ""]);

    use unicode_normalization::UnicodeNormalization;
    let recomposed: String = sequence.nfc().collect();
    assert_eq!(recomposed, "\u{d55c}");

    use finl_unicode::grapheme_clusters::Graphemes;
    let graphemes: Vec<_> = Graphemes::new(sequence).collect();
    assert_eq!(graphemes, vec![sequence]);
}

#[test]
fn test_region_scroll() {
    let mut term = TestTerm::new(5, 1, 10);
    term.print("1\n2\n3\n4\n5");

    // Test scroll region that doesn't start on first row, scrollback not used
    term.set_scroll_region(1, 2);
    term.cup(0, 2);
    let seqno = term.current_seqno();
    term.print("\na");
    assert_all_contents(&term, file!(), line!(), &["1", "3", "a", "4", "5"]);
    term.assert_dirty_lines(seqno, &[1, 2], None);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 0);
    assert_eq!(term.screen().visible_row_to_stable_row(4), 4);

    // Scroll region starting on first row, but is smaller than screen (see #6099)
    //  Scrollback will be used, which means lines below the scroll region
    //  have their stable index invalidated, and so need to be marked dirty
    term.set_scroll_region(0, 1);
    term.cup(0, 1);
    let seqno = term.current_seqno();
    term.print("\nb");
    assert_all_contents(&term, file!(), line!(), &["1", "3", "b", "a", "4", "5"]);
    term.assert_dirty_lines(seqno, &[2, 3, 4, 5], None);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 1);
    assert_eq!(term.screen().visible_row_to_stable_row(4), 5);

    // Test deletion of more lines than exist in scroll region
    term.cup(0, 1);
    let seqno = term.current_seqno();
    term.delete_lines(3);
    assert_all_contents(&term, file!(), line!(), &["1", "3", "", "a", "4", "5"]);
    term.assert_dirty_lines(seqno, &[2], None);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 1);
    assert_eq!(term.screen().visible_row_to_stable_row(4), 5);

    // Return to normal, entire-screen scrolling, optimal number of lines marked dirty
    term.set_scroll_region(0, 4);
    term.cup(0, 4);
    let seqno = term.current_seqno();
    term.print("\nX");
    assert_all_contents(&term, file!(), line!(), &["1", "3", "", "a", "4", "5", "X"]);
    term.assert_dirty_lines(seqno, &[6], None);
    assert_eq!(term.screen().visible_row_to_stable_row(4), 6);
}

#[test]
fn test_alt_screen_region_scroll() {
    // Test that scrollback is never used, and lines below the scroll region
    //  aren't made dirty or invalid. Only the scroll region is marked dirty.
    let mut term = TestTerm::new(5, 1, 10);
    term.print("M\no\nn\nk\ne\ny");

    // Enter alternate-screen mode, saving current state
    term.set_mode("?1049", true);
    term.print("1\n2\n3\n4\n5");

    // Test scroll region that doesn't start on first row
    term.set_scroll_region(1, 2);
    term.cup(0, 2);
    let seqno = term.current_seqno();
    term.print("\na");
    assert_all_contents(&term, file!(), line!(), &["1", "3", "a", "4", "5"]);
    term.assert_dirty_lines(seqno, &[1, 2], None);
    assert_eq!(term.screen().visible_row_to_stable_row(4), 4);

    // Test scroll region that starts on first row, still no scrollback
    term.set_scroll_region(0, 1);
    term.cup(0, 1);
    let seqno = term.current_seqno();
    term.print("\nb");
    assert_all_contents(&term, file!(), line!(), &["3", "b", "a", "4", "5"]);
    term.assert_dirty_lines(seqno, &[0, 1], None);
    assert_eq!(term.screen().visible_row_to_stable_row(4), 4);

    // Return to normal, entire-screen scrolling
    //  Not optimal, the entire screen is marked dirty for every line scrolled
    term.set_scroll_region(0, 4);
    term.cup(0, 4);
    let seqno = term.current_seqno();
    term.print("\nX");
    assert_all_contents(&term, file!(), line!(), &["b", "a", "4", "5", "X"]);
    term.assert_dirty_lines(seqno, &[0, 1, 2, 3, 4], None);
    assert_eq!(term.screen().visible_row_to_stable_row(4), 4);

    // Leave alternate-mode and ensure screen is restored, with all lines marked dirty
    let seqno = term.current_seqno();
    term.set_mode("?1049", false);
    assert_all_contents(&term, file!(), line!(), &["M", "o", "n", "k", "e", "y"]);
    term.assert_dirty_lines(seqno, &[0, 1, 2, 3, 4], None);
    assert_eq!(term.screen().visible_row_to_stable_row(0), 1);
}

#[test]
fn test_region_scrollback_limit() {
    // Ensure scrollback is truncated properly, when it reaches the line limit
    let mut term = TestTerm::new(4, 1, 2);
    term.print("1\n2\n3\n4");
    term.set_scroll_region(0, 1);
    term.cup(0, 1);

    let seqno = term.current_seqno();
    term.print("A\nB\nC\nD");
    assert_all_contents(&term, file!(), line!(), &["A", "B", "C", "D", "3", "4"]);
    term.assert_dirty_lines(seqno, &[0, 1, 2, 3, 4, 5], None);
    assert_eq!(term.screen().visible_row_to_stable_row(4), 7);
}

#[test]
fn test_hyperlinks() {
    let mut term = TestTerm::new(3, 5, 0);
    let link = Arc::new(Hyperlink::new("http://example.com"));
    term.hyperlink(&link);
    term.print("hello");
    term.hyperlink_off();

    let mut linked = CellAttributes::default();
    linked.set_hyperlink(Some(Arc::clone(&link)));

    assert_lines_equal(
        file!(),
        line!(),
        &term.screen().visible_lines(),
        &[
            Line::from_text("hello", &linked, SEQ_ZERO, None),
            "".into(),
            "".into(),
        ],
        Compare::TEXT | Compare::ATTRS,
    );

    term.hyperlink(&link);
    term.print("he");
    // Resetting pen should not reset the link
    term.print("\x1b[m");
    term.print("y!!");

    assert_lines_equal(
        file!(),
        line!(),
        &term.screen().visible_lines(),
        &[
            Line::from_text_with_wrapped_last_col("hello", &linked, SEQ_ZERO),
            Line::from_text("hey!!", &linked, SEQ_ZERO, None),
            "".into(),
        ],
        Compare::TEXT | Compare::ATTRS,
    );

    let otherlink = Arc::new(Hyperlink::new_with_id("http://example.com/other", "w00t"));

    // Switching link and turning it off
    term.hyperlink(&otherlink);
    term.print("wo");
    // soft reset also disables hyperlink attribute
    term.soft_reset();
    term.print("00t");

    let mut partial_line = Line::from_text("wo00t", &CellAttributes::default(), SEQ_ZERO, None);
    partial_line.set_cell(
        0,
        Cell::new(
            'w',
            CellAttributes::default()
                .set_hyperlink(Some(Arc::clone(&otherlink)))
                .clone(),
        ),
        SEQ_ZERO,
    );
    partial_line.set_cell(
        1,
        Cell::new(
            'o',
            CellAttributes::default()
                .set_hyperlink(Some(Arc::clone(&otherlink)))
                .clone(),
        ),
        SEQ_ZERO,
    );

    assert_lines_equal(
        file!(),
        line!(),
        &term.screen().visible_lines(),
        &[
            Line::from_text_with_wrapped_last_col("hello", &linked, SEQ_ZERO),
            Line::from_text_with_wrapped_last_col("hey!!", &linked, SEQ_ZERO),
            partial_line,
        ],
        Compare::TEXT | Compare::ATTRS,
    );
}

/// base64 of the crate's sample triangle simple-glyph record.
const SAMPLE_TRIANGLE_B64: &str = "AAEBRQCWAqMDUgACAAABAQEBRQFe/1EAlgAAArw=";

#[test]
fn glyph_protocol_glossary_starts_empty_and_reachable() {
    // The per-session glossary lives on TerminalState and is reachable via the
    // public accessor the GUI renderer uses. With no registrations yet it is
    // empty (no pre-seeding — apps must register).
    let term = TestTerm::new(3, 10, 0);
    assert!(term
        .glyph_glossary()
        .lock()
        .unwrap()
        .codepoints()
        .is_empty());
}

#[test]
fn glyph_protocol_register_via_apc() {
    // End-to-end: feed a register APC through the escape parser + performer
    // and confirm it lands in the glossary.
    let mut term = TestTerm::new(3, 10, 0);
    term.print(format!(
        "\x1b_25a1;r;cp=E000;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    let g = term.glyph_glossary();
    let g = g.lock().unwrap();
    assert!(g.contains(0xE000), "register APC should add U+E000");
    assert!(!g.contains(0xE001));
}

#[test]
fn glyph_protocol_register_rejects_non_pua() {
    // A non-PUA codepoint must not be registered (security contract §9).
    let mut term = TestTerm::new(3, 10, 0);
    term.print(format!(
        "\x1b_25a1;r;cp=41;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    assert!(term
        .glyph_glossary()
        .lock()
        .unwrap()
        .codepoints()
        .is_empty());
}

#[test]
fn glyph_protocol_clear_via_apc() {
    let mut term = TestTerm::new(3, 10, 0);
    term.print(format!(
        "\x1b_25a1;r;cp=E000;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    assert!(term.glyph_glossary().lock().unwrap().contains(0xE000));
    term.print("\x1b_25a1;c;cp=E000\x1b\\".to_string());
    assert!(!term.glyph_glossary().lock().unwrap().contains(0xE000));
}

/// base64 of a two-layer colrv0 container (red + blue squares).
const SAMPLE_COLR_B64: &str = "AAMAIgABAAAAAAPoA+gAAwAAAQEBAQAAA+gAAPwYAAAAAAPoAAAAIgABADIAMgH0A7YAAwAAAQEBAQAyAcIAAP4+ADIAAAOEAAAAIgABAfQAMgO2A7YAAwAAAQEBAQH0AcIAAP4+ADIAAAOEAAAAHAAAAAEAAAAOAAAAFAACAAAAAAACAAEAAAACAAEAFgAAAAIAAQACAAAADgAAKCj///9QKP8=";

#[test]
fn glyph_protocol_register_color_via_apc() {
    // A colrv0 container registers as a Color glyph in the glossary.
    let mut term = TestTerm::new(3, 10, 0);
    term.print(format!(
        "\x1b_25a1;r;cp=E001;fmt=colrv0;upm=1000;{SAMPLE_COLR_B64}\x1b\\"
    ));
    let g = term.glyph_glossary();
    let g = g.lock().unwrap();
    assert!(g.contains(0xE001), "colrv0 register should add U+E001");
    assert!(matches!(
        g.get(0xE001),
        Some(wezterm_glyph_protocol::RegisteredGlyph::Color { .. })
    ));
}

#[test]
fn glyph_protocol_ris_clears_glossary() {
    // RIS (ESC c) clears the glossary (spec §6.4).
    let mut term = TestTerm::new(3, 10, 0);
    term.print(format!(
        "\x1b_25a1;r;cp=E000;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    assert!(term.glyph_glossary().lock().unwrap().contains(0xE000));
    term.print("\x1bc".to_string()); // RIS
    assert!(
        term.glyph_glossary().lock().unwrap().is_empty(),
        "RIS must clear the glossary"
    );
}

#[test]
fn glyph_protocol_overwrite_bumps_version() {
    // Re-registering a codepoint bumps its version so the atlas drops the
    // stale sprite (spec §7.3).
    let mut term = TestTerm::new(3, 10, 0);
    term.print(format!(
        "\x1b_25a1;r;cp=E000;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    let v0 = term
        .glyph_glossary()
        .lock()
        .unwrap()
        .version(0xE000)
        .unwrap();
    term.print(format!(
        "\x1b_25a1;r;cp=E000;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    let v1 = term
        .glyph_glossary()
        .lock()
        .unwrap()
        .version(0xE000)
        .unwrap();
    assert!(
        v1 > v0,
        "overwrite must bump the version ({} -> {})",
        v0,
        v1
    );
}

#[test]
fn glyph_protocol_width_override_advances_two_cells() {
    // A width=2 registration makes its codepoint occupy two cells: the
    // cursor advances by 2 and the second cell is a continuation of the
    // wide grapheme (spec §6.1).
    let mut term = TestTerm::new(3, 10, 0);
    term.print(format!(
        "\x1b_25a1;r;cp=E000;upm=1000;width=2;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    // Print the registered codepoint (U+E000).
    term.print("\u{E000}".to_string());
    assert_eq!(
        term.cursor_pos().x,
        2,
        "width=2 glyph must advance the cursor by two cells"
    );
    // The grapheme lives in cell 0 and is two columns wide; the next
    // visible cell starts at column 2 (cell 1 is its continuation).
    let lines = term.screen().visible_lines();
    let first = lines[0].visible_cells().next().expect("a cell at col 0");
    assert_eq!(first.str(), "\u{E000}");
    assert_eq!(first.width(), 2, "the wide grapheme spans two columns");
}

#[test]
fn glyph_protocol_width_one_advances_one_cell() {
    // A width=1 (default) registration advances a single cell, even though
    // PUA codepoints are UAX#11 Ambiguous.
    let mut term = TestTerm::new(3, 10, 0);
    term.print(format!(
        "\x1b_25a1;r;cp=E001;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    term.print("\u{E001}".to_string());
    assert_eq!(term.cursor_pos().x, 1, "default width advances one cell");
}

#[test]
fn glyph_protocol_support_reply_wire_format() {
    // The `s` verb advertises exactly the formats this build renders.
    let mut term = TestTerm::new(3, 10, 0);
    term.print("\x1b_25a1;s\x1b\\".to_string());
    assert_eq!(
        String::from_utf8(term.wait_writer_output()).unwrap(),
        "\x1b_25a1;s;fmt=glyf,colrv0\x1b\\"
    );
}

#[test]
fn glyph_protocol_query_reply_reflects_glossary() {
    let mut term = TestTerm::new(3, 10, 0);
    term.print(format!(
        "\x1b_25a1;r;cp=E000;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    let _ = term.wait_writer_output(); // drop the register ACK

    term.print("\x1b_25a1;q;cp=E000\x1b\\".to_string());
    assert_eq!(
        String::from_utf8(term.wait_writer_output()).unwrap(),
        "\x1b_25a1;q;cp=e000;status=glossary\x1b\\"
    );
    // An unregistered PUA codepoint reports empty coverage.
    term.print("\x1b_25a1;q;cp=E001\x1b\\".to_string());
    assert_eq!(
        String::from_utf8(term.wait_writer_output()).unwrap(),
        "\x1b_25a1;q;cp=e001;status=\x1b\\"
    );
}

#[test]
fn glyph_protocol_register_error_replies() {
    let mut term = TestTerm::new(3, 10, 0);
    // Non-PUA codepoint → out_of_namespace.
    term.print(format!(
        "\x1b_25a1;r;cp=41;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    assert_eq!(
        String::from_utf8(term.wait_writer_output()).unwrap(),
        "\x1b_25a1;r;cp=41;status=1;reason=out_of_namespace\x1b\\"
    );
    // colrv1 is not advertised → malformed_payload.
    term.print(format!(
        "\x1b_25a1;r;cp=E000;fmt=colrv1;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    assert_eq!(
        String::from_utf8(term.wait_writer_output()).unwrap(),
        "\x1b_25a1;r;cp=e000;status=1;reason=malformed_payload\x1b\\"
    );
}

#[test]
fn glyph_protocol_reply_levels_gate_output() {
    let mut term = TestTerm::new(3, 10, 0);
    // reply=0 → silent on success.
    term.print(format!(
        "\x1b_25a1;r;cp=E000;reply=0;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    term.expect_no_writer_output();
    // reply=2 → silent on success, but emits failures.
    term.print(format!(
        "\x1b_25a1;r;cp=E001;reply=2;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    term.expect_no_writer_output();
    term.print("\x1b_25a1;r;cp=41;reply=2;upm=1000;AAEB\x1b\\".to_string());
    assert!(
        !term.wait_writer_output().is_empty(),
        "reply=2 must still emit failures"
    );
}

#[test]
fn glyph_protocol_disabled_ignores_apc() {
    // With enable_glyph_protocol = false, register/clear/support are all
    // no-ops: nothing lands in the glossary and `s` produces no PTY reply,
    // so clients fall back via the spec's detection timeout (§3.3).
    let mut term = TestTerm::new_with_glyph_protocol(3, 10, 0, false);
    term.print("\x1b_25a1;s\x1b\\".to_string());
    term.print(format!(
        "\x1b_25a1;r;cp=E000;upm=1000;{SAMPLE_TRIANGLE_B64}\x1b\\"
    ));
    assert!(
        term.glyph_glossary()
            .lock()
            .unwrap()
            .codepoints()
            .is_empty(),
        "disabled protocol must not register anything"
    );
    // No reply of any kind should have been written back to the PTY.
    term.expect_no_writer_output();
}
