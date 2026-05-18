//! crossterm `MouseEvent` -> xterm マウスレポートバイト列への変換。
//!
//! ccnest はマルチプレクサなので、フォーカス先ペインの内側アプリ
//! (Claude Code 等) が DECSET 1000/1002/1003/1006 でマウス報告を要求して
//! いるとき、ホストで受け取った `MouseEvent` をそのアプリが理解できる
//! マウスレポートに変換して PTY へ書き戻す必要がある。
//!
//! ここは `keymap` と同様、App 非依存の純粋関数 + 表テストに留める。
//! 「どのイベントを転送するか」のモードゲーティングも本モジュールが持ち、
//! 呼び側 (`event::try_forward_mouse`) は encode の `Option` を見て
//! 「転送 or 飲み込み」を判断するだけにする。

use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

pub use vt100::{MouseProtocolEncoding, MouseProtocolMode};

/// 報告 1 件分の素のボタンコード。
///
/// `cb` は **修飾ビットも +32 オフセットも未適用** の基底値。
/// - 通常ボタン: Left=0 / Middle=1 / Right=2
/// - Drag/Moved: 上記 + 32 (motion bit)
/// - ホイール: 上=64 下=65 左=66 右=67
///
/// `release` が true のとき、SGR は終端を `m` にし cb はボタンコードのまま、
/// X11/UTF-8 は cb をリリース番兵 `3` に置き換える。
#[derive(Clone, Copy)]
struct Report {
    cb: u32,
    release: bool,
}

fn button_code(b: &MouseButton) -> u32 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn classify(kind: &MouseEventKind) -> Report {
    use MouseEventKind::*;
    match kind {
        Down(b) => Report {
            cb: button_code(b),
            release: false,
        },
        Up(b) => Report {
            cb: button_code(b),
            release: true,
        },
        Drag(b) => Report {
            cb: button_code(b) + 32,
            release: false,
        },
        Moved => Report {
            cb: 3 + 32,
            release: false,
        },
        ScrollUp => Report {
            cb: 64,
            release: false,
        },
        ScrollDown => Report {
            cb: 65,
            release: false,
        },
        ScrollLeft => Report {
            cb: 66,
            release: false,
        },
        ScrollRight => Report {
            cb: 67,
            release: false,
        },
    }
}

fn modifier_bits(m: KeyModifiers) -> u32 {
    let mut bits = 0;
    if m.contains(KeyModifiers::SHIFT) {
        bits += 4;
    }
    if m.contains(KeyModifiers::ALT) {
        bits += 8;
    }
    if m.contains(KeyModifiers::CONTROL) {
        bits += 16;
    }
    bits
}

/// `mode` がこの種類のイベントを報告対象とするか。
fn reported(mode: MouseProtocolMode, kind: &MouseEventKind) -> bool {
    use MouseEventKind::*;
    use MouseProtocolMode as M;
    let is_scroll = matches!(kind, ScrollUp | ScrollDown | ScrollLeft | ScrollRight);
    match mode {
        M::None => false,
        M::Press => matches!(kind, Down(_)) || is_scroll,
        M::PressRelease => matches!(kind, Down(_) | Up(_)) || is_scroll,
        M::ButtonMotion => matches!(kind, Down(_) | Up(_) | Drag(_)) || is_scroll,
        M::AnyMotion => true,
    }
}

/// X11 (Default) 1 バイト = 値 + 32。座標は 223 までしか表現できないので
/// バイトが 255 を超えないようにクランプする。
fn x11_byte(v: u32) -> u8 {
    (v + 32).min(255) as u8
}

/// UTF-8 (DECSET 1005) は値 + 32 のコードポイントを UTF-8 で出す。
/// 値が 127 以下なら X11 と同一の 1 バイトになる。
fn push_utf8(out: &mut Vec<u8>, v: u32) {
    let cp = v + 32;
    if let Some(c) = char::from_u32(cp) {
        let mut buf = [0u8; 4];
        out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    } else {
        out.push(b'?');
    }
}

/// `me` を `mode`/`enc` に従ったマウスレポートにエンコードする。
///
/// `col`/`row` は **0 始まりのペインローカルセル座標**
/// (`event.rs` の既存 `lx`/`ly` をそのまま渡す)。ワイヤ上は端末仕様どおり
/// 1 始まりへ +1 する。
///
/// `mode` がこの種類を報告しない場合 (例: `Press` での `Up`) は `None` を
/// 返す。呼び側はこれを「アプリがマウスを掴んでいるので転送せず飲み込む」
/// 判断に使う (ローカル UI を誤発火させない)。
#[must_use]
pub fn encode_mouse_report(
    mode: MouseProtocolMode,
    enc: MouseProtocolEncoding,
    me: &MouseEvent,
    col: u16,
    row: u16,
) -> Option<Vec<u8>> {
    if !reported(mode, &me.kind) {
        return None;
    }
    let rep = classify(&me.kind);
    let mods = modifier_bits(me.modifiers);
    let cx = col as u32 + 1; // 端末は 1 始まり
    let cy = row as u32 + 1;

    match enc {
        MouseProtocolEncoding::Sgr => {
            // SGR(1006): cb は生値 (オフセットなし)。リリースは終端 'm' で
            // cb はボタンコードのまま (3 ではない)。座標上限なし。
            let cb = rep.cb + mods;
            let final_byte = if rep.release { 'm' } else { 'M' };
            Some(format!("\x1b[<{cb};{cx};{cy}{final_byte}").into_bytes())
        }
        MouseProtocolEncoding::Default => {
            // X11: ESC [ M の後に (cb|cx|cy)+32 を各 1 バイト。
            // リリースはどのボタンか区別不能なので cb=3。
            let cb = if rep.release { 3 + mods } else { rep.cb + mods };
            let mut v = Vec::with_capacity(6);
            v.extend_from_slice(b"\x1b[M");
            v.push(x11_byte(cb));
            v.push(x11_byte(cx));
            v.push(x11_byte(cy));
            Some(v)
        }
        MouseProtocolEncoding::Utf8 => {
            // 1005: X11 と同じ値だが cx/cy/cb を UTF-8 で出す (cap なし)。
            let cb = if rep.release { 3 + mods } else { rep.cb + mods };
            let mut v = Vec::with_capacity(6);
            v.extend_from_slice(b"\x1b[M");
            push_utf8(&mut v, cb);
            push_utf8(&mut v, cx);
            push_utf8(&mut v, cy);
            Some(v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

    fn me(kind: MouseEventKind, col: u16, row: u16, m: KeyModifiers) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: m,
        }
    }

    fn sgr(kind: MouseEventKind, col: u16, row: u16, m: KeyModifiers) -> Vec<u8> {
        encode_mouse_report(
            MouseProtocolMode::AnyMotion,
            MouseProtocolEncoding::Sgr,
            &me(kind, col, row, m),
            col,
            row,
        )
        .unwrap()
    }

    fn x11(kind: MouseEventKind, col: u16, row: u16) -> Vec<u8> {
        encode_mouse_report(
            MouseProtocolMode::AnyMotion,
            MouseProtocolEncoding::Default,
            &me(kind, col, row, KeyModifiers::NONE),
            col,
            row,
        )
        .unwrap()
    }

    #[test]
    fn sgr_table() {
        use MouseButton::*;
        use MouseEventKind::*;
        let n = KeyModifiers::NONE;
        assert_eq!(sgr(Down(Left), 0, 0, n), b"\x1b[<0;1;1M");
        assert_eq!(sgr(Down(Left), 9, 4, n), b"\x1b[<0;10;5M");
        assert_eq!(sgr(Down(Right), 0, 0, n), b"\x1b[<2;1;1M");
        assert_eq!(sgr(Down(Middle), 0, 0, n), b"\x1b[<1;1;1M");
        assert_eq!(sgr(Up(Left), 9, 4, n), b"\x1b[<0;10;5m");
        assert_eq!(sgr(Drag(Left), 2, 3, n), b"\x1b[<32;3;4M");
        assert_eq!(sgr(Moved, 2, 3, n), b"\x1b[<35;3;4M");
        assert_eq!(sgr(ScrollUp, 5, 6, n), b"\x1b[<64;6;7M");
        assert_eq!(sgr(ScrollDown, 5, 6, n), b"\x1b[<65;6;7M");
        assert_eq!(
            sgr(Down(Left), 0, 0, KeyModifiers::CONTROL),
            b"\x1b[<16;1;1M"
        );
        assert_eq!(sgr(Down(Left), 0, 0, KeyModifiers::ALT), b"\x1b[<8;1;1M");
        assert_eq!(sgr(ScrollUp, 0, 0, KeyModifiers::CONTROL), b"\x1b[<80;1;1M");
    }

    #[test]
    fn x11_table() {
        use MouseButton::*;
        use MouseEventKind::*;
        assert_eq!(x11(Down(Left), 0, 0), b"\x1b[M\x20\x21\x21"); // [32,33,33]
        assert_eq!(x11(Down(Left), 9, 4), b"\x1b[M\x20\x2a\x25"); // [32,42,37]
        assert_eq!(x11(Up(Left), 0, 0), b"\x1b[M\x23\x21\x21"); // [35,33,33]
        assert_eq!(x11(ScrollUp, 0, 0), b"\x1b[M\x60\x21\x21"); // [96,33,33]
        assert_eq!(x11(Down(Left), 300, 0), b"\x1b[M\x20\xff\x21"); // [32,255,33] cap
    }

    #[test]
    fn utf8_small_equals_x11() {
        // 値が 127 以下なら UTF-8 は X11 と同一の 1 バイト列。
        let k = MouseEventKind::Down(MouseButton::Left);
        let u = encode_mouse_report(
            MouseProtocolMode::AnyMotion,
            MouseProtocolEncoding::Utf8,
            &me(k, 3, 4, KeyModifiers::NONE),
            3,
            4,
        )
        .unwrap();
        assert_eq!(u, x11(k, 3, 4));
    }

    #[test]
    fn gating_table() {
        use MouseButton::Left;
        use MouseEventKind::*;
        use MouseProtocolEncoding::Sgr;
        use MouseProtocolMode as M;
        let n = KeyModifiers::NONE;
        let enc = |mode, kind| encode_mouse_report(mode, Sgr, &me(kind, 1, 1, n), 1, 1);

        assert!(enc(M::None, Down(Left)).is_none());
        assert!(enc(M::Press, Up(Left)).is_none());
        assert!(enc(M::Press, Drag(Left)).is_none());
        assert!(enc(M::PressRelease, Drag(Left)).is_none());
        assert!(enc(M::PressRelease, Moved).is_none());
        assert!(enc(M::ButtonMotion, Moved).is_none());
        assert!(enc(M::ButtonMotion, Drag(Left)).is_some());
        assert!(enc(M::AnyMotion, Moved).is_some());
        // ホイールはどのモードでも (None 以外) 報告される。
        assert!(enc(M::Press, ScrollUp).is_some());
    }
}
