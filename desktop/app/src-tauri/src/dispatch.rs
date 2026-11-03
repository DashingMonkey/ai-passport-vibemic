//! 键入与按键注入(enigo)。全部动作:
//!
//! - type_text:识别文本键入当前焦点(enigo.text 走 Unicode 事件,中文可用)
//! - confirm / cancel:UP=确认(Enter 或 Ctrl+Enter,见 SendKey),DOWN=Esc
//! - cancel_twice:DOWN 双击=停止,连发两记 Esc(「双击 Esc 中断」类软件)
//! - start_delete / stop_delete:OK 双击=删一个字符,第二下按住=连删到松手
//!
//! 注入器进程级常驻:enigo 默认 Drop 时抬起所有按住的键,临时实例会把
//! 保持的组合键立刻抬起,这里由我们管理生命周期,
//! release_all() 借 Drop 兜底断连场景。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use enigo::{Direction, Enigo, Key, Keyboard, Settings};

use crate::state::SendKey;

static ENIGO: Mutex<Option<Enigo>> = Mutex::new(None);

/// 双击两记之间的间隔:目标软件要求两次是独立按键、且落在它的序列窗口内
/// (DSH 的 `stopSequenceMs` 默认 500ms);贴得太近的连击在个别宿主上会被并成一次。
const DOUBLE_TAP_GAP_MS: u64 = 60;

/// 连删:第二下按住超过它才开始"连续"删。取 400ms —— 远大于"点两下"的手速
/// (几百毫秒级),又明显短于"我要一路删过去"的自觉按住。
const DELETE_HOLD_DELAY_MS: u64 = 400;
/// 连删间隔 ≈10 字/秒:再快容易删过头,再慢按住太累。
const DELETE_REPEAT_MS: u64 = 100;
/// 连删安全上限:松开事件万一丢失,退格不能无限发下去(300 × 100ms ≈ 30s)。
const DELETE_MAX_REPEATS: u32 = 300;
/// 连删的分段睡眠粒度:松手后最多再多删一个字符就停。
const DELETE_SLEEP_SLICE_MS: u64 = 10;

/// 连删代次:start_delete / stop_delete 各递增一次,在跑的循环看到代次变了就退出。
/// 用代次而不是布尔标志,是为了让"新的一轮连删"和"上一轮的循环"互不干扰。
static DELETE_GEN: AtomicU64 = AtomicU64::new(0);

fn with_enigo(f: impl FnOnce(&mut Enigo) -> Result<(), String>) -> Result<(), String> {
    let mut guard = ENIGO.lock().map_err(|e| e.to_string())?;
    if guard.is_none() {
        *guard = Some(Enigo::new(&Settings::default()).map_err(|e| e.to_string())?);
    }
    f(guard.as_mut().unwrap())
}

/// 兜底释放(设备断连等异常路径):丢掉实例,enigo 的 Drop 会抬起所有仍按住的键。
pub fn release_all() {
    if let Ok(mut guard) = ENIGO.lock() {
        *guard = None;
    }
}

/// 把识别出的文本键入当前焦点窗口。
pub fn type_text(text: &str) -> Result<(), String> {
    if text.is_empty() {
        return Ok(());
    }
    // UIA 在专用线程内限时读取,不要占着注入器锁等待外部应用。
    let context = crate::text_context::before_typing();
    with_enigo(|e| {
        let text = crate::text_context::prepare_text(text, context);
        e.text(&text).map_err(|e| e.to_string())
    })
}

/// UP 单击 = 确认/发送:Enter(默认)或 Ctrl+Enter(按软件习惯配置)。
///
/// Ctrl+Enter 必须保证 Control 一定被抬起:失败路径上早退会把 Ctrl 卡在按下态
/// (注入器是进程级常驻,不 Drop),之后用户所有按键都会变成 Ctrl+组合键,
/// 直到设备断连才由 release_all() 兜底。
pub fn confirm(key: SendKey) -> Result<(), String> {
    match key {
        SendKey::Enter => with_enigo(|e| {
            e.key(Key::Return, Direction::Click).map_err(|e| e.to_string())
        }),
        SendKey::CtrlEnter => with_enigo(|e| {
            e.key(Key::Control, Direction::Press)
                .map_err(|e| e.to_string())?;
            let typed = e.key(Key::Return, Direction::Click);
            // 无论 Return 成功与否都抬起 Ctrl,再决定返回什么错误。
            let released = e.key(Key::Control, Direction::Release);
            typed.map_err(|e| e.to_string())?;
            released.map_err(|e| e.to_string())
        }),
    }
}

/// DOWN 单击 = 取消:Esc。
pub fn cancel() -> Result<(), String> {
    with_enigo(|e| e.key(Key::Escape, Direction::Click).map_err(|e| e.to_string()))
}

/// DOWN 双击 = 停止:连发两记 Esc。
///
/// 两记之间持有注入器锁并 sleep:间隔必须落在两次 Esc 之间,不能被别的注入插进来
/// 打断序列(目标软件按"两次独立按键 + 间隔"判定)。
pub fn cancel_twice() -> Result<(), String> {
    with_enigo(|e| {
        e.key(Key::Escape, Direction::Click).map_err(|e| e.to_string())?;
        std::thread::sleep(std::time::Duration::from_millis(DOUBLE_TAP_GAP_MS));
        e.key(Key::Escape, Direction::Click).map_err(|e| e.to_string())
    })
}

/// OK 双击 = 删除:先删掉一个字符;第二下继续按住,则每 [`DELETE_REPEAT_MS`] 再删一个,
/// 直到 [`stop_delete`](松手、断链或下一次按键)。
///
/// 删键注入跑在自己的线程里:设备端那条宏执行器是 FIFO 且不可打断,持续循环会把
/// 后续宏全堵住;也不能占着设备线程(它还要处理按键事件与音频帧)。
pub fn start_delete() -> Result<(), String> {
    let generation = DELETE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::Builder::new()
        .name("vibemic-delete".into())
        .spawn(move || {
            // 双击的含义就是"删一个":按下第二下的那一刻就该看到一个字消失。
            if !delete_once(generation) {
                return;
            }
            // 只点两下(已经松手)到此为止;按住不放才进入连删。
            if !sleep_while_current(generation, DELETE_HOLD_DELAY_MS) {
                return;
            }
            for _ in 0..DELETE_MAX_REPEATS {
                if !delete_once(generation) {
                    return;
                }
                if !sleep_while_current(generation, DELETE_REPEAT_MS) {
                    return;
                }
            }
            // 走到这儿说明按住超过 30s:多半是松开事件丢了,停下并留痕。
            if DELETE_GEN.load(Ordering::SeqCst) == generation {
                crate::diag::log("连删达到上限(松开事件可能丢失),已停止");
                DELETE_GEN.fetch_add(1, Ordering::SeqCst);
            }
        })
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// 结束连删:OK 松手、链路断开、以及任何一次新的 OK 按下都调它(幂等)。
pub fn stop_delete() {
    DELETE_GEN.fetch_add(1, Ordering::SeqCst);
}

/// 发一个退格,返回"是否该继续"。代次变了说明这一轮已作废,连这一个都不发。
fn delete_once(generation: u64) -> bool {
    if DELETE_GEN.load(Ordering::SeqCst) != generation {
        return false;
    }
    match with_enigo(|e| e.key(Key::Backspace, Direction::Click).map_err(|e| e.to_string())) {
        Ok(()) => true,
        Err(e) => {
            crate::diag::log(&format!("删除键注入失败: {e}"));
            false
        }
    }
}

/// 分段睡眠:期间代次变了(松手/断链/新按键)立刻醒并返回 false。
/// 不整段睡是为了"松手就停" —— 整段睡会在松手之后再多删好几个字符。
fn sleep_while_current(generation: u64, total_ms: u64) -> bool {
    let mut left = total_ms;
    while left > 0 {
        if DELETE_GEN.load(Ordering::SeqCst) != generation {
            return false;
        }
        let slice = DELETE_SLEEP_SLICE_MS.min(left);
        std::thread::sleep(std::time::Duration::from_millis(slice));
        left -= slice;
    }
    DELETE_GEN.load(Ordering::SeqCst) == generation
}
