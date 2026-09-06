//! イベントループの「起こし方」と、スレッド間で比較できる時刻スタンプ。
//!
//! - [`now_us`]: プロセス内の単調時刻 (マイクロ秒)。`Instant` は Atomic に
//!   載らないので、reader スレッド → イベントループへ渡す時刻はこれで表す。
//! - [`OutputStamp`]: PTY reader スレッドが「最後に vt100 parser へ出力を
//!   反映した時刻」を書き込む共有スタンプ。`CCNEST_LATENCY_TRACE` で
//!   「キー書き込み → エコー到着 → 描画完了」を計測するために使う。
//! - [`LoopMsg`] / [`OutputWaker`]: イベントループを起こす唯一のチャネル。
//!   入力ポンプスレッド (crossterm) と各 PTY reader スレッドが送信し、
//!   `run_event_loop` が `recv_timeout` 一本で「入力 or 出力 or タイムアウト」を
//!   待つ。かつては `event::poll(30ms)` のタイムアウトでしか出力を描けず、
//!   エコーが最大 30ms (Windows のタイマー分解能で実質 31〜47ms) 遅れていた。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Instant;

use crossterm::event::Event;

use crate::pane::PaneId;

/// イベントループを起こすメッセージ。
#[derive(Debug)]
pub enum LoopMsg {
    /// 入力ポンプスレッドが crossterm から読んだイベント (1 回の drain ぶん)。
    /// 1 通に複数イベントが入るのは、コンソール入力キューに同時に積まれて
    /// いたもの (ペーストや IME 確定) で、人間の 1 打鍵は通常 1 イベント。
    Input(Vec<Event>),
    /// ペインの PTY 出力が vt100 parser に反映された。
    Output(PaneId),
    /// 入力ポンプが読み取りエラーで終了した (以後キー入力は届かない)。
    InputClosed,
}

pub type WakeTx = mpsc::Sender<LoopMsg>;
pub type WakeRx = mpsc::Receiver<LoopMsg>;

/// reader スレッドから「出力あり」をループへ通知する。
///
/// 通知は「未処理が常に 1 通以下」になるよう間引く: 一度送ったらループが
/// [`disarm`](Self::disarm) するまで再送しない。ストリーミング中に 4KB
/// チャンクごとにキューを膨らませず、ループが 1 周で 1 回だけ描けばよい。
#[derive(Clone, Debug)]
pub struct OutputWaker {
    tx: WakeTx,
    pane: PaneId,
    armed: Arc<AtomicBool>,
}

impl OutputWaker {
    pub fn new(tx: WakeTx, pane: PaneId) -> Self {
        Self {
            tx,
            pane,
            armed: Arc::new(AtomicBool::new(false)),
        }
    }

    /// reader スレッドが parser 更新直後 (ロックの外) に呼ぶ。
    pub fn notify(&self) {
        if !self.armed.swap(true, Ordering::AcqRel) {
            // 受信側が消えていたら (終了中) 黙って捨てる。
            let _ = self.tx.send(LoopMsg::Output(self.pane));
        }
    }

    /// ループが `LoopMsg::Output` を受け取ったら呼ぶ。以降の出力で再度通知される。
    pub fn disarm(&self) {
        self.armed.store(false, Ordering::Release);
    }
}

/// プロセス起動 (正確には初回呼び出し) からの経過マイクロ秒。
pub fn now_us() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_micros() as u64
}

/// reader スレッドが更新し、イベントループが読む「最終出力時刻」。
/// `0` は「まだ一度も出力が反映されていない」。
#[derive(Clone, Debug, Default)]
pub struct OutputStamp(Arc<AtomicU64>);

impl OutputStamp {
    pub fn new() -> Self {
        Self::default()
    }

    /// reader スレッドが `parser.process()` の直後に呼ぶ。
    pub fn mark(&self) {
        self.0.store(now_us(), Ordering::Release);
    }

    /// 最後に出力が parser へ反映された時刻 (us)。
    pub fn last_us(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_us_is_monotonic() {
        let a = now_us();
        let b = now_us();
        assert!(b >= a);
    }

    #[test]
    fn output_waker_sends_once_until_disarmed() {
        let (tx, rx) = mpsc::channel();
        let w = OutputWaker::new(tx, 7);
        w.notify();
        w.notify();
        w.notify();
        assert!(matches!(rx.try_recv(), Ok(LoopMsg::Output(7))));
        assert!(rx.try_recv().is_err(), "重複通知は間引かれる");
        w.disarm();
        w.notify();
        assert!(matches!(rx.try_recv(), Ok(LoopMsg::Output(7))));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn output_waker_clone_shares_arming_state() {
        let (tx, rx) = mpsc::channel();
        let a = OutputWaker::new(tx, 1);
        let b = a.clone();
        a.notify();
        b.notify();
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "clone 側も同じ armed を見る");
        b.disarm();
        a.notify();
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn output_stamp_starts_at_zero_and_marks_current_time() {
        let s = OutputStamp::new();
        assert_eq!(s.last_us(), 0);
        let before = now_us();
        s.mark();
        assert!(s.last_us() >= before);
        // clone は同じスタンプを共有する (reader / loop で分け持つ前提)。
        let c = s.clone();
        s.mark();
        assert_eq!(c.last_us(), s.last_us());
    }
}

// Version History
// ver0.1 - 2026-09-06 - Initial: now_us / OutputStamp (latency trace), LoopMsg
//                       / WakeTx / WakeRx / OutputWaker (single wake channel
//                       for input pump + PTY reader threads; notifications are
//                       throttled to one outstanding message per pane).
