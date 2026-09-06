//! イベントループの「起こし方」と、スレッド間で比較できる時刻スタンプ。
//!
//! - [`now_us`]: プロセス内の単調時刻 (マイクロ秒)。`Instant` は Atomic に
//!   載らないので、reader スレッド → イベントループへ渡す時刻はこれで表す。
//! - [`OutputStamp`]: PTY reader スレッドが「最後に vt100 parser へ出力を
//!   反映した時刻」を書き込む共有スタンプ。`CCNEST_LATENCY_TRACE` で
//!   「キー書き込み → エコー到着 → 描画完了」を計測するために使う。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

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
