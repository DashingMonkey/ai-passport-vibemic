//! Best-effort punctuation at the actual caret, never from a previous transcript.
//! Unsupported controls/OS versions, selections and timeouts silently fall back.
use std::borrow::Cow;

fn needs_separator(before: &str, incoming: &str) -> bool {
    // Web editors can leave zero-width layout/IME characters after deletion.
    // They are not evidence that the user has entered a preceding sentence.
    let previous = before.trim_end_matches(|c: char| {
        (c.is_whitespace() || is_format_marker(c)) && !is_line_break(c)
    });
    let Some(last) = previous.chars().next_back() else {
        return false;
    };
    let Some(first) = incoming.chars().next() else {
        return false;
    };
    !is_line_break(last)
        && last != '\u{fffc}' // Embedded object/placeholder, not preceding prose.
        && !is_punctuation(last)
        && !first.is_whitespace()
        && !is_punctuation(first)
}

fn is_format_marker(c: char) -> bool {
    matches!(
        c,
        '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}'
    )
}

fn is_line_break(c: char) -> bool {
    matches!(c, '\r' | '\n' | '\u{2028}' | '\u{2029}')
}

fn is_punctuation(c: char) -> bool {
    c.is_ascii_punctuation()
        || "。，！？；：、…～．｡､﹐﹑﹒﹔﹕﹖﹗“”‘’「」『』（）【】《》〈〉—".contains(c)
}

/// UIA GetEnclosingElement returns the innermost node containing a range;
/// a paragraph/link inside an editor is valid too. Only ascend from that node:
/// accepting an ancestor of the editor would admit unrelated page/history text.
#[cfg(any(target_os = "windows", test))]
fn belongs_to_input<T>(
    mut node: T,
    mut is_input: impl FnMut(&T) -> Option<bool>,
    mut parent: impl FnMut(&T) -> Option<T>,
) -> bool {
    // Broken providers can return cycles or arbitrarily deep trees. The outer
    // query deadline still bounds the caller's wait for slow COM calls.
    for _ in 0..32 {
        match is_input(&node) {
            Some(true) => return true,
            Some(false) => {}
            None => return false,
        }
        let Some(next) = parent(&node) else {
            return false;
        };
        node = next;
    }
    false
}

#[cfg(target_os = "windows")]
pub(crate) use platform::{before_typing, Context};

#[cfg(not(target_os = "windows"))]
pub(crate) struct Context;

#[cfg(not(target_os = "windows"))]
pub(crate) fn before_typing() -> Option<Context> {
    None
}

pub(crate) fn prepare_text(text: &str, context: Option<Context>) -> Cow<'_, str> {
    #[cfg(target_os = "windows")]
    if let Some(context) = context {
        if context.is_current() && needs_separator(&context.before, text) {
            crate::diag::log("[punctuation] added separator");
            return Cow::Owned(format!("。{text}"));
        }
        crate::diag::log("[punctuation] unchanged: boundary/empty text/focus changed");
    }
    #[cfg(not(target_os = "windows"))]
    let _ = context;
    Cow::Borrowed(text)
}

// At most one pending native query, even if a third-party UIA provider hangs.
// A late result is dropped with its per-request receiver, never reused later.
#[cfg(any(target_os = "windows", test))]
mod bounded {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    };
    use std::time::Duration;

    pub(super) struct Probe<T> {
        tx: mpsc::SyncSender<mpsc::Sender<Option<T>>>,
        busy: Arc<AtomicBool>,
    }

    impl<T: Send + 'static> Probe<T> {
        pub(super) fn new(query: impl Fn() -> Option<T> + Send + 'static) -> Option<Self> {
            let (tx, rx) = mpsc::sync_channel::<mpsc::Sender<Option<T>>>(1);
            let busy = Arc::new(AtomicBool::new(false));
            let running = busy.clone();
            std::thread::Builder::new()
                .name("vibemic-text-context".into())
                .spawn(move || {
                    while let Ok(reply) = rx.recv() {
                        let result = query();
                        running.store(false, Ordering::Release);
                        let _ = reply.send(result);
                    }
                })
                .ok()?;
            Some(Self { tx, busy })
        }

        pub(super) fn read(&self, timeout: Duration) -> Option<T> {
            if self
                .busy
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
            {
                return None;
            }
            let (tx, rx) = mpsc::channel();
            if self.tx.try_send(tx).is_err() {
                self.busy.store(false, Ordering::Release);
                return None;
            }
            rx.recv_timeout(timeout).ok().flatten()
        }
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::belongs_to_input;
    use super::bounded::Probe;
    use std::{sync::OnceLock, time::Duration};
    use windows::Win32::{
        System::{
            Com::{
                CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
                COINIT_MULTITHREADED,
            },
            Variant::{VariantClear, VT_BOOL},
        },
        UI::{Accessibility::*, WindowsAndMessaging::*},
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct Focus {
        foreground: usize,
        focused: usize,
        caret: usize,
        rect: [i32; 4],
    }

    impl Focus {
        fn current() -> Option<Self> {
            unsafe {
                let foreground = GetForegroundWindow();
                if foreground.is_invalid() {
                    return None;
                }
                let thread = GetWindowThreadProcessId(foreground, None);
                if thread == 0 {
                    return None;
                }
                let mut info = GUITHREADINFO {
                    cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
                    ..Default::default()
                };
                GetGUIThreadInfo(thread, &mut info).ok()?;
                if info.hwndFocus.is_invalid() {
                    return None;
                }
                Some(Self {
                    foreground: foreground.0 as usize,
                    focused: info.hwndFocus.0 as usize,
                    caret: info.hwndCaret.0 as usize,
                    rect: [
                        info.rcCaret.left,
                        info.rcCaret.top,
                        info.rcCaret.right,
                        info.rcCaret.bottom,
                    ],
                })
            }
        }
    }

    pub(crate) struct Context {
        focus: Focus,
        pub(super) before: String,
    }

    impl Context {
        pub(super) fn is_current(&self) -> bool {
            Focus::current() == Some(self.focus)
        }
    }

    pub(crate) fn before_typing() -> Option<Context> {
        static PROBE: OnceLock<Option<Probe<Context>>> = OnceLock::new();
        let focus = Focus::current()?;
        let context = PROBE
            .get_or_init(|| Probe::new(query))
            .as_ref()?
            .read(Duration::from_millis(200));
        let Some(context) = context else {
            crate::diag::log("[punctuation] unchanged: context unavailable/busy/timeout");
            return None;
        };
        (context.focus == focus && context.is_current()).then_some(context)
    }

    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() }
        }
    }

    fn query() -> Option<Context> {
        // All COM interfaces stay in this MTA worker and are dropped before COM teardown.
        // Only a short text suffix and plain window identifiers leave the worker.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok().ok()?;
            let _apartment = Apartment;
            let focus = Focus::current()?;
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
            let element = automation.GetFocusedElement().ok()?;
            if element.CurrentIsPassword().ok()?.as_bool()
                || !element.CurrentHasKeyboardFocus().ok()?.as_bool()
            {
                return None;
            }
            let control = element.CurrentControlType().ok()?;
            if control != UIA_EditControlTypeId && control != UIA_DocumentControlTypeId {
                return None;
            }
            let pattern: IUIAutomationTextPattern =
                match element.GetCurrentPatternAs(UIA_TextPatternId) {
                    Ok(pattern) => pattern,
                    Err(error) => {
                        crate::diag::log(&format!(
                            "[punctuation] skipped: TextPattern unavailable ({:?})",
                            error.code()
                        ));
                        return None;
                    }
                };
            let document = pattern.DocumentRange().ok()?;
            // A range inside a paragraph can be enclosed by that paragraph rather
            // than by the focused editor. Confirm ancestry, not strict identity.
            // See the Windows GetEnclosingElement contract (text-provider children).
            let enclosing = document.GetEnclosingElement().ok()?;
            let walker = automation.RawViewWalker().ok()?;
            if !belongs_to_input(
                enclosing,
                |node| {
                    automation
                        .CompareElements(&element, node)
                        .ok()
                        .map(|same| same.as_bool())
                },
                |node| walker.GetParentElement(node).ok(),
            ) {
                crate::diag::log("[punctuation] skipped: input range ownership unconfirmed");
                return None;
            }
            let selection = pattern.GetSelection().ok()?;
            if selection.Length().ok()? != 1 {
                return None;
            }
            let caret = selection.GetElement(0).ok()?;
            if caret
                .CompareEndpoints(
                    TextPatternRangeEndpoint_Start,
                    &caret,
                    TextPatternRangeEndpoint_End,
                )
                .ok()?
                != 0
            {
                return None; // Replacing selected text must not acquire a leading period.
            }
            // A collapsed caret can sit at the start of an empty editor while
            // MoveEndpointByUnit still traverses into the surrounding web page.
            // Confirm the insertion belongs to this input before moving anything.
            if caret
                .CompareEndpoints(
                    TextPatternRangeEndpoint_Start,
                    &document,
                    TextPatternRangeEndpoint_Start,
                )
                .ok()?
                <= 0
            {
                crate::diag::log("[punctuation] skipped: caret at input start/outside input");
                return None;
            }
            let preceding = caret.Clone().ok()?;
            if caret
                .CompareEndpoints(
                    TextPatternRangeEndpoint_End,
                    &document,
                    TextPatternRangeEndpoint_End,
                )
                .ok()?
                > 0
            {
                // Chromium may place a collapsed caret just after a text-node
                // endpoint with no intervening characters (observed in DSH).
                // Normalize that equivalent position only inside this same input.
                let caret_owner = caret.GetEnclosingElement().ok()?;
                if !belongs_to_input(
                    caret_owner,
                    |node| {
                        automation
                            .CompareElements(&element, node)
                            .ok()
                            .map(|same| same.as_bool())
                    },
                    |node| walker.GetParentElement(node).ok(),
                ) {
                    crate::diag::log("[punctuation] skipped: caret outside input");
                    return None;
                }
                let gap = caret.Clone().ok()?;
                gap.MoveEndpointByRange(
                    TextPatternRangeEndpoint_Start,
                    &document,
                    TextPatternRangeEndpoint_End,
                )
                .ok()?;
                if !gap.GetText(1).ok()?.is_empty() {
                    crate::diag::log("[punctuation] skipped: text beyond input boundary");
                    return None;
                }
                // Moving End before Start collapses the clone at the new End.
                // Keep the original caret unchanged for the final selection check.
                preceding
                    .MoveEndpointByRange(
                        TextPatternRangeEndpoint_End,
                        &document,
                        TextPatternRangeEndpoint_End,
                    )
                    .ok()?;
                crate::diag::log("[punctuation] normalized empty end marker");
            }
            let mut readonly = caret.GetAttributeValue(UIA_IsReadOnlyAttributeId).ok()?;
            let writable = readonly.Anonymous.Anonymous.vt == VT_BOOL
                && !readonly.Anonymous.Anonymous.Anonymous.boolVal.as_bool();
            let _ = VariantClear(&mut readonly);
            if !writable {
                return None;
            }
            let moved = preceding
                .MoveEndpointByUnit(TextPatternRangeEndpoint_Start, TextUnit_Character, -16)
                .ok()?;
            if moved >= 0 {
                return None;
            }
            if preceding
                .CompareEndpoints(
                    TextPatternRangeEndpoint_Start,
                    &document,
                    TextPatternRangeEndpoint_Start,
                )
                .ok()?
                < 0
            {
                preceding
                    .MoveEndpointByRange(
                        TextPatternRangeEndpoint_Start,
                        &document,
                        TextPatternRangeEndpoint_Start,
                    )
                    .ok()?;
            }
            // A range can span read-only page content even though its caret is
            // editable. Mixed/unknown writability must never justify punctuation.
            let mut prefix_readonly = preceding
                .GetAttributeValue(UIA_IsReadOnlyAttributeId)
                .ok()?;
            let prefix_writable = prefix_readonly.Anonymous.Anonymous.vt == VT_BOOL
                && !prefix_readonly
                    .Anonymous
                    .Anonymous
                    .Anonymous
                    .boolVal
                    .as_bool();
            let _ = VariantClear(&mut prefix_readonly);
            if !prefix_writable {
                crate::diag::log("[punctuation] skipped: preceding range not wholly editable");
                return None;
            }
            // 16 Unicode characters need at most 32 UTF-16 code units. Reject a
            // provider expanding farther rather than examining a truncated prefix.
            let before = preceding.GetText(33).ok()?;
            if before.len() > 32 {
                return None;
            }
            let before = before.to_string();
            let current = automation.GetFocusedElement().ok()?;
            if !automation
                .CompareElements(&element, &current)
                .ok()?
                .as_bool()
            {
                return None;
            }
            let current_selection = pattern.GetSelection().ok()?;
            if current_selection.Length().ok()? != 1 {
                return None;
            }
            let current_caret = current_selection.GetElement(0).ok()?;
            for endpoint in [TextPatternRangeEndpoint_Start, TextPatternRangeEndpoint_End] {
                if caret
                    .CompareEndpoints(endpoint, &current_caret, endpoint)
                    .ok()?
                    != 0
                {
                    return None;
                }
            }
            if Focus::current() != Some(focus) {
                return None;
            }
            Some(Context { focus, before })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn separates_existing_text_including_manual_input() {
        for previous in ["我觉得可以", "第一句。第二句", "hello", "做好了  "] {
            assert!(needs_separator(previous, "明天开始做"), "{previous}");
        }
    }

    #[test]
    fn preserves_boundaries_and_does_not_double_punctuate() {
        for previous in [
            "",
            "  ",
            "\n",
            "上一段\r\n  ",
            "上一段\u{2029}",
            "你好。",
            "你好？ ",
            "hello!",
            "等一下，",
            "如下：",
            "好了；",
            "等等…",
            "“你好。”",
        ] {
            assert!(!needs_separator(previous, "下一句"), "{previous:?}");
        }
        for incoming in ["", "，但是", "。下一句", "\n下一段"] {
            assert!(!needs_separator("上一句", incoming));
        }
    }

    #[test]
    fn unavailable_context_preserves_transcript() {
        assert_eq!(prepare_text("明天开始做", None), "明天开始做");
    }

    #[test]
    fn empty_web_editor_markers_do_not_count_as_a_sentence() {
        for previous in [
            "\u{200b}",
            " \u{feff}\u{200b}",
            "\u{200c}\u{200d}\u{2060}",
            "\n\u{200b}",
            "\u{fffc}",
        ] {
            assert!(!needs_separator(previous, "第一句话"), "{previous:?}");
        }
        assert!(needs_separator("之前的文字\u{200b}", "第二句话"));
        assert!(!needs_separator("之前的文字。\u{200b}", "第二句话"));
    }

    #[test]
    fn input_scope_accepts_editor_and_nested_text_but_not_page_or_sibling() {
        // page(0) -> editor(1) -> paragraph(2) -> text(3)
        // page(0) -> another_editor(4) -> text(5)
        let parents = [None, Some(0), Some(1), Some(2), Some(0), Some(4)];
        for (node, expected) in [
            (0, false),
            (1, true),
            (2, true),
            (3, true),
            (4, false),
            (5, false),
        ] {
            assert_eq!(
                belongs_to_input(node, |n| Some(*n == 1), |n| parents[*n]),
                expected
            );
        }
    }

    #[test]
    fn input_scope_fails_closed_on_provider_errors_and_cycles() {
        assert!(!belongs_to_input(2, |_| None, |_| Some(1)));
        assert!(!belongs_to_input(2, |n| Some(*n == 1), |_| None));
        let mut visited = 0;
        assert!(!belongs_to_input(
            2,
            |n| Some(*n == 1),
            |n| {
                visited += 1;
                Some(*n)
            }
        ));
        assert_eq!(visited, 32);
    }

    #[test]
    fn failed_provider_silently_falls_back() {
        let probe = bounded::Probe::<String>::new(|| None).unwrap();
        assert!(probe.read(Duration::from_secs(1)).is_none());
    }

    #[test]
    fn blocked_provider_has_bounded_wait_and_no_backlog_or_stale_result() {
        let (release, wait) = mpsc::channel();
        let probe = bounded::Probe::new(move || wait.recv().ok()).unwrap();
        assert!(probe.read(Duration::from_millis(10)).is_none());
        // Still blocked: skip immediately, never enqueue another native query.
        assert!(probe.read(Duration::ZERO).is_none());
        release.send(42).unwrap();
        // Stop the provider: no later read may receive the old value 42.
        drop(release);
        assert!(probe.read(Duration::from_secs(1)).is_none());
    }
}
