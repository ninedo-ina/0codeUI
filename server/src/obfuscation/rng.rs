//! 轻量随机数发生器：避免为混淆引入额外依赖。
//!
//! 只用于生成不可预测的类名 / 包装节点 / 属性顺序，不用于密码学场景。
//! 使用时间、进程号与原子计数器混合种子，xorshift64* 扩展状态。

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 混淆用随机数发生器。
pub(crate) struct Rng {
    state: u64,
}

impl Rng {
    /// 创建带独立种子的发生器。
    pub(crate) fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos() as u64)
            .unwrap_or(0);
        let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id() as u64;
        let mut state = nanos ^ counter.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (pid << 32);
        if state == 0 {
            state = 0x6A09_E667_F3BC_C908;
        }
        Self { state }
    }

    /// 下一个 64 位随机数。
    pub(crate) fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// `0..upper` 范围内的随机数（upper > 0）。
    pub(crate) fn below(&mut self, upper: usize) -> usize {
        if upper == 0 {
            return 0;
        }
        (self.next_u64() % upper as u64) as usize
    }

    /// 原地洗牌。
    pub(crate) fn shuffle<T>(&mut self, values: &mut [T]) {
        if values.len() < 2 {
            return;
        }
        for index in (1..values.len()).rev() {
            let other = self.below(index + 1);
            values.swap(index, other);
        }
    }

    /// 生成指定长度的十六进制串。
    pub(crate) fn hex(&mut self, len: usize) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut output = String::with_capacity(len);
        for _ in 0..len {
            output.push(HEX[(self.next_u64() & 0x0f) as usize] as char);
        }
        output
    }

    /// 生成合法标识符（首字母固定为 `c`）。
    pub(crate) fn class_name(&mut self) -> String {
        format!("c{}", self.hex(10))
    }

    /// 生成合法 JS 标识符（首字母固定为 `v`）。
    pub(crate) fn js_identifier(&mut self) -> String {
        format!("v{}", self.hex(8))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_names_are_valid_and_varied() {
        let mut rng = Rng::new();
        let first = rng.class_name();
        let second = rng.class_name();
        assert!(first.starts_with('c'));
        assert!(first.chars().all(|ch| ch.is_ascii_alphanumeric()));
        assert_ne!(first, second);
    }

    #[test]
    fn shuffle_keeps_all_values() {
        let mut rng = Rng::new();
        let mut values = vec![1, 2, 3, 4, 5];
        rng.shuffle(&mut values);
        values.sort_unstable();
        assert_eq!(values, vec![1, 2, 3, 4, 5]);
    }
}
